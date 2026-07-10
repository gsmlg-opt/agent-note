use crate::{
    enqueue_missing_chunk_embeddings, list_notes, parse_label_value_type, Context, ListNotesParams,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
    pub created_at: i64,
    pub updated_at: i64,
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct ImportStats {
    pub notes_added: usize,
    pub notes_skipped: usize,
    pub label_keys_added: usize,
    pub embedding_jobs_queued: usize,
}

pub async fn export_data(ctx: &Context) -> anyhow::Result<ExportData> {
    let conn = ctx.storage.connect()?;
    let label_keys = note_storage::list_label_keys(&conn)
        .await?
        .into_iter()
        .map(|label_key| ExportLabelKey {
            key: label_key.key,
            description: label_key.description,
            value_type: label_key.value_type.as_str().to_string(),
        })
        .collect();
    let notes = list_notes(ctx, ListNotesParams::default())
        .await?
        .into_iter()
        .map(|note| ExportNote {
            id: note.id,
            title: note.title,
            content: note.content,
            created_at: note.created_at,
            updated_at: note.updated_at,
            labels: note
                .labels
                .into_iter()
                .map(|label| (label.key, label.value))
                .collect(),
        })
        .collect();

    Ok(ExportData {
        version: 1,
        label_keys,
        notes,
    })
}

pub async fn import_data(ctx: &Context, data: ExportData) -> anyhow::Result<ImportStats> {
    let conn = ctx.storage.connect()?;
    let mut known_keys: HashSet<String> = note_storage::list_label_keys(&conn)
        .await?
        .into_iter()
        .map(|label_key| label_key.key)
        .collect();
    let mut stats = ImportStats::default();

    let tx = conn.transaction().await?;
    for label_key in data.label_keys {
        if known_keys.insert(label_key.key.clone()) {
            let value_type = parse_label_value_type(&label_key.value_type)?;
            note_storage::insert_label_key_with_type(
                &tx,
                &label_key.key,
                &label_key.description,
                value_type,
            )
            .await?;
            stats.label_keys_added += 1;
        }
    }

    for note in data.notes {
        if note_storage::get_note(&tx, &note.id).await?.is_some() {
            stats.notes_skipped += 1;
            continue;
        }

        for (key, _) in &note.labels {
            if !key.is_empty() && known_keys.insert(key.clone()) {
                note_storage::insert_label_key(&tx, key, "").await?;
                stats.label_keys_added += 1;
            }
        }

        note_storage::insert_note(
            &tx,
            &note.id,
            &note.title,
            &note.content,
            note.created_at,
            note.updated_at,
            1,
        )
        .await?;
        for (key, value) in &note.labels {
            note_storage::attach_label(&tx, &note.id, key, value).await?;
        }
        stats.notes_added += 1;
    }
    tx.commit().await?;

    stats.embedding_jobs_queued = enqueue_missing_chunk_embeddings(ctx).await?;
    Ok(stats)
}

pub async fn export_json(ctx: &Context) -> anyhow::Result<String> {
    let data = export_data(ctx).await?;
    serde_json::to_string_pretty(&data).map_err(anyhow::Error::new)
}

pub async fn import_json(ctx: &Context, input: &str) -> anyhow::Result<ImportStats> {
    let data: ExportData = serde_json::from_str(input)?;
    import_data(ctx, data).await
}

fn default_label_value_type() -> String {
    "text".to_string()
}
