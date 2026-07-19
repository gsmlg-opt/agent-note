use crate::Context;
use note_attachments::{PreparedAttachmentMutation, PreparedAttachmentSet};
use note_core::{
    is_relative_attachment_path, normalize_attachment_path, validate_attachments, Note,
    NoteAttachment,
};
use note_storage::{AttachmentMetadataUpdate, StorageTransaction, TransactionMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutNoteAttachmentResult {
    pub attachment: NoteAttachment,
    pub created: bool,
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
    ctx.attachments()
        .hydrate(&note.id, &mut note.attachments)
        .await
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

    attachment.content = ctx.attachments().read(note_id, &attachment.path).await?;
    Ok(Some(attachment))
}

pub async fn put_note_attachment(
    ctx: &Context,
    note_id: &str,
    attachment: NoteAttachment,
) -> anyhow::Result<PutNoteAttachmentResult> {
    validate_attachments(std::slice::from_ref(&attachment)).map_err(anyhow::Error::new)?;
    let prepared = ctx.attachments().prepare_put(note_id, &attachment).await?;
    let transaction = match ctx.storage().begin(TransactionMode::Immediate).await {
        Ok(transaction) => transaction,
        Err(error) => {
            return Err(abort_mutation_with_primary(prepared, error.into()).await);
        }
    };
    let transaction_result = async {
        let Some(note) = transaction.get_note(note_id).await? else {
            return Err(anyhow::Error::new(AttachmentMutationError::NoteNotFound(
                note_id.to_string(),
            )));
        };
        let normalized_path = normalize_attachment_path(&attachment.path);
        let existing_index = note
            .attachments
            .iter()
            .position(|existing| existing.id == attachment.id);
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
            existing.id != attachment.id
                && normalize_attachment_path(&existing.path) == normalized_path
        }) {
            return Err(anyhow::Error::new(
                AttachmentMutationError::AttachmentPathCollision {
                    path: normalized_path,
                },
            ));
        }

        let mut metadata = note.attachments;
        let mut stored_attachment = attachment.clone();
        stored_attachment.content.clear();
        match existing_index {
            Some(index) => metadata[index] = stored_attachment.clone(),
            None => metadata.push(stored_attachment.clone()),
        }
        let affected = transaction
            .update_note_attachments(AttachmentMetadataUpdate {
                id: note_id,
                attachments: &metadata,
                updated_at: chrono::Utc::now().timestamp(),
            })
            .await?;
        if affected == 0 {
            return Err(anyhow::Error::new(AttachmentMutationError::NoteNotFound(
                note_id.to_string(),
            )));
        }
        anyhow::Ok(PutNoteAttachmentResult {
            attachment: stored_attachment,
            created,
        })
    }
    .await;

    let result = crate::save_note::finish_transaction(transaction, transaction_result).await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            return Err(abort_mutation_with_primary(prepared, error).await);
        }
    };
    prepared.publish().await?;
    Ok(result)
}

pub async fn get_note_attachment_by_id(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<Option<NoteAttachment>> {
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    let Some(attachment) = note
        .attachments
        .into_iter()
        .find(|attachment| attachment.id == attachment_id)
    else {
        return Ok(None);
    };
    drop(session);

    get_note_attachment(ctx, note_id, &attachment.path).await
}

pub async fn delete_note_attachment(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<bool> {
    let Some(mut captured_path) = resolve_attachment_path(ctx, note_id, attachment_id).await?
    else {
        return Ok(false);
    };

    for attempt in 0..2 {
        let prepared = ctx
            .attachments()
            .prepare_delete(note_id, &captured_path)
            .await?;
        let transaction = match ctx.storage().begin(TransactionMode::Immediate).await {
            Ok(transaction) => transaction,
            Err(error) => {
                return Err(abort_mutation_with_primary(prepared, error.into()).await);
            }
        };
        let transaction_result = delete_attachment_metadata(
            transaction.as_ref(),
            note_id,
            attachment_id,
            &captured_path,
        )
        .await;

        match transaction_result {
            Ok(DeleteMetadataResult::Deleted) => {
                if let Err(error) = transaction.commit().await {
                    return Err(abort_mutation_with_primary(prepared, error.into()).await);
                }
                prepared.publish().await?;
                return Ok(true);
            }
            Ok(DeleteMetadataResult::Absent) => {
                rollback_and_abort(transaction, prepared).await?;
                return Ok(false);
            }
            Ok(DeleteMetadataResult::PathChanged) => {
                rollback_and_abort(transaction, prepared).await?;
                if attempt == 1 {
                    return Err(anyhow::Error::new(
                        AttachmentMutationError::AttachmentPathChange {
                            attachment_id: attachment_id.to_string(),
                        },
                    ));
                }
                let Some(resolved_path) =
                    resolve_attachment_path(ctx, note_id, attachment_id).await?
                else {
                    return Ok(false);
                };
                captured_path = resolved_path;
                continue;
            }
            Err(error) => {
                let error = rollback_with_primary(transaction, error).await;
                return Err(abort_mutation_with_primary(prepared, error).await);
            }
        }
    }

    Err(anyhow::Error::new(
        AttachmentMutationError::AttachmentPathChange {
            attachment_id: attachment_id.to_string(),
        },
    ))
}

enum DeleteMetadataResult {
    Deleted,
    Absent,
    PathChanged,
}

async fn delete_attachment_metadata(
    transaction: &dyn note_storage::StorageTransaction,
    note_id: &str,
    attachment_id: &str,
    captured_path: &str,
) -> anyhow::Result<DeleteMetadataResult> {
    let Some(note) = transaction.get_note(note_id).await? else {
        return Ok(DeleteMetadataResult::Absent);
    };
    let Some(existing) = note
        .attachments
        .iter()
        .find(|attachment| attachment.id == attachment_id)
    else {
        return Ok(DeleteMetadataResult::Absent);
    };
    if normalize_attachment_path(&existing.path) != normalize_attachment_path(captured_path) {
        return Ok(DeleteMetadataResult::PathChanged);
    }

    let metadata = note
        .attachments
        .into_iter()
        .filter(|attachment| attachment.id != attachment_id)
        .collect::<Vec<_>>();
    let affected = transaction
        .update_note_attachments(AttachmentMetadataUpdate {
            id: note_id,
            attachments: &metadata,
            updated_at: chrono::Utc::now().timestamp(),
        })
        .await?;
    if affected == 0 {
        return Ok(DeleteMetadataResult::Absent);
    }
    Ok(DeleteMetadataResult::Deleted)
}

async fn resolve_attachment_path(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<Option<String>> {
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    Ok(note
        .attachments
        .into_iter()
        .find(|attachment| attachment.id == attachment_id)
        .map(|attachment| attachment.path))
}

async fn rollback_and_abort(
    transaction: Box<dyn StorageTransaction>,
    prepared: Box<dyn PreparedAttachmentMutation>,
) -> anyhow::Result<()> {
    match transaction.rollback().await {
        Ok(()) => prepared.abort().await,
        Err(error) => Err(abort_mutation_with_primary(prepared, error.into()).await),
    }
}

async fn rollback_with_primary(
    transaction: Box<dyn StorageTransaction>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match transaction.rollback().await {
        Ok(()) => primary,
        Err(rollback_error) => primary.context(format!(
            "transaction rollback also failed: {rollback_error}"
        )),
    }
}

async fn abort_mutation_with_primary(
    prepared: Box<dyn PreparedAttachmentMutation>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match prepared.abort().await {
        Ok(()) => primary,
        Err(abort_error) => primary.context(format!(
            "attachment mutation abort also failed: {abort_error}"
        )),
    }
}

pub(crate) async fn abort_with_primary(
    prepared: Box<dyn PreparedAttachmentSet>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match prepared.abort().await {
        Ok(()) => primary,
        Err(abort_error) => primary.context(format!("attachment abort also failed: {abort_error}")),
    }
}
