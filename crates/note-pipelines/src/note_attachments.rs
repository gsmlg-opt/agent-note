use crate::Context;
use note_core::{
    is_relative_attachment_path, normalize_attachment_path, validate_attachments, Note,
    NoteAttachment,
};
use note_storage::{AttachmentMetadataUpdate, TransactionMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutNoteAttachmentResult {
    pub attachment: NoteAttachment,
    pub created: bool,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteNoteAttachmentResult {
    Applied { revision: i64 },
    Absent,
}

impl std::ops::Not for DeleteNoteAttachmentResult {
    type Output = bool;

    fn not(self) -> Self::Output {
        matches!(self, Self::Absent)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttachmentMutationError {
    NoteNotFound(String),
    AttachmentNotFound {
        note_id: String,
        attachment_id: String,
    },
    AttachmentPathChange {
        attachment_id: String,
    },
    AttachmentPathCollision {
        path: String,
    },
}

impl std::fmt::Display for AttachmentMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoteNotFound(note_id) => write!(f, "note not found: {note_id}"),
            Self::AttachmentNotFound {
                note_id,
                attachment_id,
            } => write!(
                f,
                "attachment not found: note {note_id}, attachment {attachment_id}"
            ),
            Self::AttachmentPathChange { attachment_id } => write!(
                f,
                "attachment path cannot change for existing id: {attachment_id}"
            ),
            Self::AttachmentPathCollision { path } => {
                write!(f, "attachment path is already in use: {path}")
            }
        }
    }
}

impl std::error::Error for AttachmentMutationError {}

pub async fn hydrate_note_attachments(ctx: &Context, note: &mut Note) -> anyhow::Result<()> {
    for attachment in &mut note.attachments {
        attachment.content = read_attachment(ctx, &note.id, attachment).await?;
    }
    Ok(())
}

pub async fn get_note_attachment(
    ctx: &Context,
    note_id: &str,
    requested_path: &str,
) -> anyhow::Result<Option<NoteAttachment>> {
    if requested_path.trim().is_empty() || !is_relative_attachment_path(requested_path) {
        return Ok(None);
    }
    let requested_path = normalize_attachment_path(requested_path);
    if requested_path.is_empty() {
        return Ok(None);
    }
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    drop(session);
    let Some(mut attachment) = note.attachments.into_iter().find(|attachment| {
        is_relative_attachment_path(&attachment.path)
            && normalize_attachment_path(&attachment.path) == requested_path
    }) else {
        return Ok(None);
    };

    attachment.content = read_attachment(ctx, note_id, &attachment).await?;
    Ok(Some(attachment))
}

pub async fn put_note_attachment(
    ctx: &Context,
    note_id: &str,
    expected_revision: i64,
    mut attachment: NoteAttachment,
) -> anyhow::Result<PutNoteAttachmentResult> {
    validate_attachments(std::slice::from_ref(&attachment)).map_err(anyhow::Error::new)?;
    attachment.id = canonical_attachment_id(&attachment.id).to_string();
    let published = crate::generated_attachments::publish_generated_attachments(
        ctx.attachments(),
        note_id,
        std::slice::from_ref(&attachment),
    )
    .await?;
    let transaction = match ctx.storage().begin(TransactionMode::Immediate).await {
        Ok(transaction) => transaction,
        Err(error) => {
            return Err(published
                .cleanup_with_primary(ctx.attachments(), error.into())
                .await);
        }
    };
    let transaction_result = async {
        let Some(note) = transaction.get_note(note_id).await? else {
            return Err(crate::NoteMutationError::NotFound(note_id.to_string()).into());
        };
        if note.revision != expected_revision {
            return Err(crate::NoteMutationError::StaleRevision {
                note_id: note_id.to_string(),
                expected_revision,
                current_revision: note.revision,
            }
            .into());
        }
        let updated_at = attachment_mutation_timestamp(note.updated_at);
        let normalized_path = normalize_attachment_path(&attachment.path);
        let existing_index = note.attachments.iter().position(|existing| {
            canonical_attachment_id(&existing.id) == canonical_attachment_id(&attachment.id)
        });
        let created = existing_index.is_none();

        if let Some(index) = existing_index {
            if normalize_attachment_path(&note.attachments[index].path) != normalized_path {
                return Err(anyhow::Error::new(
                    AttachmentMutationError::AttachmentPathChange {
                        attachment_id: attachment.id.clone(),
                    },
                ));
            }
        }
        if note.attachments.iter().any(|existing| {
            canonical_attachment_id(&existing.id) != canonical_attachment_id(&attachment.id)
                && normalize_attachment_path(&existing.path) == normalized_path
        }) {
            return Err(anyhow::Error::new(
                AttachmentMutationError::AttachmentPathCollision {
                    path: normalized_path,
                },
            ));
        }

        let mut metadata = note.attachments;
        let mut stored_attachment = published.metadata[0].clone();
        let replaced = existing_index.map(|index| metadata[index].clone());
        match existing_index {
            Some(index) => {
                stored_attachment.id = metadata[index].id.clone();
                metadata[index] = stored_attachment.clone();
            }
            None => metadata.push(stored_attachment.clone()),
        }
        let affected = transaction
            .update_note_attachments(AttachmentMetadataUpdate {
                id: note_id,
                expected_revision,
                attachments: &metadata,
                updated_at,
            })
            .await?;
        let revision = crate::mutation_error::mutation_revision(note_id, affected)?;
        if let Some(replaced) = replaced {
            let cleanup_now = chrono::Utc::now().timestamp();
            crate::generated_attachments::enqueue_attachment_cleanup(
                transaction.as_ref(),
                note_id,
                std::slice::from_ref(&replaced),
                cleanup_now,
            )
            .await?;
        }
        anyhow::Ok(PutNoteAttachmentResult {
            attachment: stored_attachment,
            created,
            revision,
        })
    }
    .await;

    let result = match transaction_result {
        Ok(result) => match transaction.commit().await {
            Ok(()) => result,
            Err(error) => {
                return Err(anyhow::Error::from(error).context(
                    "attachment transaction commit failed; generated object retained because commit outcome is unknown",
                ));
            }
        },
        Err(error) => {
            return match transaction.rollback().await {
                Ok(()) => Err(published
                    .cleanup_with_primary(ctx.attachments(), error)
                    .await),
                Err(rollback_error) => Err(error.context(format!(
                    "transaction rollback failed; generated object retained because rollback outcome is unknown: {rollback_error}"
                ))),
            };
        }
    };
    crate::generated_attachments::run_attachment_cleanup_once(ctx).await;
    Ok(result)
}

pub async fn get_note_attachment_by_id(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<Option<NoteAttachment>> {
    let attachment_id = canonical_attachment_id(attachment_id);
    for _ in 0..2 {
        let Some(attachment) = resolve_attachment_metadata(ctx, note_id, attachment_id).await?
        else {
            return Ok(None);
        };
        let resolved_locator = AttachmentReadLocator::from(&attachment);
        let content = match read_attachment(ctx, note_id, &attachment).await {
            Ok(content) => content,
            Err(read_error) => {
                let Some(confirmed) =
                    resolve_attachment_metadata(ctx, note_id, attachment_id).await?
                else {
                    return Ok(None);
                };
                if AttachmentReadLocator::from(&confirmed) == resolved_locator {
                    return Err(read_error);
                }
                continue;
            }
        };
        let Some(mut confirmed) = resolve_attachment_metadata(ctx, note_id, attachment_id).await?
        else {
            continue;
        };
        if AttachmentReadLocator::from(&confirmed) == resolved_locator {
            confirmed.content = content;
            return Ok(Some(confirmed));
        }
    }
    Err(anyhow::Error::new(
        AttachmentMutationError::AttachmentPathChange {
            attachment_id: attachment_id.to_string(),
        },
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum AttachmentReadLocator {
    Object(String),
    Legacy(String),
}

impl From<&NoteAttachment> for AttachmentReadLocator {
    fn from(attachment: &NoteAttachment) -> Self {
        match &attachment.storage {
            Some(storage) => Self::Object(storage.object_key.clone()),
            None => Self::Legacy(normalize_attachment_path(&attachment.path)),
        }
    }
}

async fn read_attachment(
    ctx: &Context,
    note_id: &str,
    attachment: &NoteAttachment,
) -> anyhow::Result<Vec<u8>> {
    match &attachment.storage {
        Some(storage) => ctx.attachments().read_object(&storage.object_key).await,
        None => {
            ctx.attachments()
                .read_legacy(note_id, &attachment.path)
                .await
        }
    }
}

pub async fn delete_note_attachment(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
    expected_revision: i64,
) -> anyhow::Result<DeleteNoteAttachmentResult> {
    let attachment_id = canonical_attachment_id(attachment_id);
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let Some(note) = transaction.get_note(note_id).await? else {
            return Err(crate::NoteMutationError::NotFound(note_id.to_string()).into());
        };
        if note.revision != expected_revision {
            return Err(crate::NoteMutationError::StaleRevision {
                note_id: note_id.to_string(),
                expected_revision,
                current_revision: note.revision,
            }
            .into());
        }
        let Some(existing_index) = note.attachments.iter().position(|attachment| {
            canonical_attachment_id(&attachment.id) == canonical_attachment_id(attachment_id)
        }) else {
            return Ok(DeleteNoteAttachmentResult::Absent);
        };

        let updated_at = attachment_mutation_timestamp(note.updated_at);
        let mut metadata = note.attachments;
        let removed = metadata.remove(existing_index);
        let affected = transaction
            .update_note_attachments(AttachmentMetadataUpdate {
                id: note_id,
                expected_revision,
                attachments: &metadata,
                updated_at,
            })
            .await?;
        let revision = crate::mutation_error::mutation_revision(note_id, affected)?;
        crate::generated_attachments::enqueue_attachment_cleanup(
            transaction.as_ref(),
            note_id,
            std::slice::from_ref(&removed),
            chrono::Utc::now().timestamp(),
        )
        .await?;
        Ok(DeleteNoteAttachmentResult::Applied { revision })
    }
    .await;
    let result = crate::save_note::finish_transaction(transaction, transaction_result).await?;
    if matches!(result, DeleteNoteAttachmentResult::Applied { .. }) {
        crate::generated_attachments::run_attachment_cleanup_once(ctx).await;
    }
    Ok(result)
}

async fn resolve_attachment_metadata(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<Option<NoteAttachment>> {
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    Ok(note.attachments.into_iter().find(|attachment| {
        canonical_attachment_id(&attachment.id) == canonical_attachment_id(attachment_id)
    }))
}

fn canonical_attachment_id(attachment_id: &str) -> &str {
    attachment_id.trim()
}

fn attachment_mutation_timestamp(stored_updated_at: i64) -> i64 {
    chrono::Utc::now()
        .timestamp()
        .max(stored_updated_at.saturating_add(1))
}
