use crate::{enqueue_missing_chunk_embeddings, parse_label_value_type, Context};
use note_core::{decode_attachment_content, encode_attachment_content, NoteAttachment};
use note_storage::{NewNote, TransactionMode};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
pub struct ExportData {
    pub version: u32,
    pub label_keys: Vec<ExportLabelKey>,
    pub notes: Vec<ExportNote>,
}

#[derive(Serialize, Deserialize)]
pub struct ExportLabelKey {
    pub key: String,
    pub description: String,
    #[serde(default = "default_label_value_type")]
    pub value_type: String,
}

#[derive(Serialize, Deserialize)]
pub struct ExportNote {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ExportAttachment>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    pub labels: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
pub struct ExportAttachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}

impl From<NoteAttachment> for ExportAttachment {
    fn from(attachment: NoteAttachment) -> Self {
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content: std::str::from_utf8(&attachment.content)
                .ok()
                .map(str::to_owned),
            content_base64: Some(encode_attachment_content(&attachment.content)),
        }
    }
}

impl ExportAttachment {
    fn into_note_attachment(self) -> anyhow::Result<NoteAttachment> {
        let content =
            decode_attachment_content(self.content.as_deref(), self.content_base64.as_deref())
                .map_err(|error| {
                    anyhow::anyhow!("invalid content for attachment {}: {error}", self.id)
                })?;
        Ok(NoteAttachment {
            id: self.id,
            path: self.path,
            mime: self.mime,
            description: self.description,
            content,
        })
    }
}

#[derive(Debug, Default)]
pub struct ImportStats {
    pub notes_added: usize,
    pub notes_skipped: usize,
    pub label_keys_added: usize,
    pub embedding_jobs_queued: usize,
}

pub async fn export_data(ctx: &Context) -> anyhow::Result<ExportData> {
    let session = ctx.storage().session().await?;
    let label_keys = session
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label_key| ExportLabelKey {
            key: label_key.key,
            description: label_key.description,
            value_type: label_key.value_type.as_str().to_string(),
        })
        .collect();
    let mut notes = session.list_all_notes().await?;
    for note in &mut notes {
        crate::attachment_files::hydrate_note_attachments(ctx, note)?;
    }
    let notes = notes
        .into_iter()
        .map(|note| ExportNote {
            id: note.id,
            title: note.title,
            content: note.content,
            attachments: note
                .attachments
                .into_iter()
                .map(ExportAttachment::from)
                .collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            deleted_at: note.deleted_at,
            labels: note
                .labels
                .into_iter()
                .map(|label| (label.key, label.value))
                .collect(),
        })
        .collect();

    Ok(ExportData {
        version: 2,
        label_keys,
        notes,
    })
}

pub async fn import_data(ctx: &Context, data: ExportData) -> anyhow::Result<ImportStats> {
    if !matches!(data.version, 1 | 2) {
        anyhow::bail!("unsupported export version: {}", data.version);
    }
    let session = ctx.storage().session().await?;
    let mut known_keys: HashSet<String> = session
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label_key| label_key.key)
        .collect();
    drop(session);
    let mut stats = ImportStats::default();
    let mut prepared_attachments = Vec::new();

    let transaction = ctx.storage().begin(TransactionMode::Deferred).await?;
    let transaction_result = async {
        for label_key in data.label_keys {
            if known_keys.insert(label_key.key.clone()) {
                let value_type = parse_label_value_type(&label_key.value_type)?;
                transaction
                    .insert_label_key_with_type(&label_key.key, &label_key.description, value_type)
                    .await?;
                stats.label_keys_added += 1;
            }
        }

        for note in data.notes {
            if transaction.note_exists(&note.id).await? {
                stats.notes_skipped += 1;
                continue;
            }

            for (key, _) in &note.labels {
                if !key.is_empty() && known_keys.insert(key.clone()) {
                    transaction.insert_label_key(key, "").await?;
                    stats.label_keys_added += 1;
                }
            }

            let attachments = note
                .attachments
                .into_iter()
                .map(ExportAttachment::into_note_attachment)
                .collect::<anyhow::Result<Vec<_>>>()?;
            let prepared =
                crate::attachment_files::prepare_note_attachments(ctx, &note.id, &attachments)?;
            prepared_attachments.push(prepared);
            let prepared = prepared_attachments
                .last()
                .expect("prepared attachment was just pushed");
            transaction
                .insert_note(NewNote {
                    id: &note.id,
                    title: &note.title,
                    content: &note.content,
                    attachments: prepared.metadata(),
                    created_at: note.created_at,
                    updated_at: note.updated_at,
                    note_revision: 1,
                    deleted_at: note.deleted_at,
                })
                .await?;
            for (key, value) in &note.labels {
                transaction.attach_label(&note.id, key, value).await?;
            }
            stats.notes_added += 1;
        }
        anyhow::Ok(())
    }
    .await;

    if let Err(error) = crate::save_note::finish_transaction(transaction, transaction_result).await
    {
        for prepared in &prepared_attachments {
            crate::attachment_files::cleanup_prepared_note_attachments(prepared);
        }
        return Err(error);
    }

    for prepared in prepared_attachments {
        crate::attachment_files::commit_note_attachments(prepared)?;
    }

    stats.embedding_jobs_queued = enqueue_missing_chunk_embeddings(ctx).await?;
    Ok(stats)
}

pub async fn export_json(ctx: &Context) -> anyhow::Result<String> {
    let data = export_data(ctx).await?;
    serde_json::to_string_pretty(&data).map_err(anyhow::Error::new)
}

pub fn create_backup_archive(data: &ExportData) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec_pretty(data)?;
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    );
    header.set_cksum();
    archive.append_data(&mut header, "notes.json", io::Cursor::new(json))?;
    archive.finish()?;
    let encoder = archive.into_inner()?;
    encoder.finish().map_err(anyhow::Error::new)
}

pub async fn import_json(ctx: &Context, input: &str) -> anyhow::Result<ImportStats> {
    let data: ExportData = serde_json::from_str(input)?;
    import_data(ctx, data).await
}

fn default_label_value_type() -> String {
    "text".to_string()
}
