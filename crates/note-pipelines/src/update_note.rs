use crate::{context::Context, SaveNoteInput};
use note_attachments::PreparedAttachmentSet;
use note_core::{
    validate_label_value, validate_note_input, Label, LabelValueType, Note, NoteAttachment,
    NoteInput, ValidationError,
};
use note_storage::{NoteFieldsUpdate, NoteUpdate, TransactionMode};
use std::collections::{HashMap, HashSet};

pub const TRASH_RETENTION_DAYS: i64 = 90;
pub const TRASH_RETENTION_SECONDS: i64 = TRASH_RETENTION_DAYS * 24 * 60 * 60;

pub struct UpdateNoteFieldsInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
}

pub async fn update_note(
    ctx: &Context,
    id: &str,
    input: SaveNoteInput,
) -> anyhow::Result<Option<Note>> {
    update_note_inner(
        ctx,
        id,
        UpdateNoteFieldsInput {
            title: input.title,
            content: input.content,
            labels: input.labels,
        },
        Some(input.attachments),
    )
    .await
}

pub async fn update_note_fields(
    ctx: &Context,
    id: &str,
    input: UpdateNoteFieldsInput,
) -> anyhow::Result<Option<Note>> {
    update_note_inner(ctx, id, input, None).await
}

async fn update_note_inner(
    ctx: &Context,
    id: &str,
    input: UpdateNoteFieldsInput,
    attachments: Option<Vec<NoteAttachment>>,
) -> anyhow::Result<Option<Note>> {
    let session = ctx.storage().session().await?;

    let existing = match session.get_note(id).await? {
        Some(note) => note,
        None => return Ok(None),
    };

    let existing_label_keys = session.list_label_keys().await?;
    let existing_keys: Vec<String> = existing_label_keys.iter().map(|k| k.key.clone()).collect();
    let label_value_types: HashMap<String, LabelValueType> = existing_label_keys
        .into_iter()
        .map(|label_key| (label_key.key, label_key.value_type))
        .collect();

    let mut known_for_validation = existing_keys.clone();
    for (key, _) in &input.labels {
        if !known_for_validation.iter().any(|k| k == key) {
            known_for_validation.push(key.clone());
        }
    }

    validate_note_input(
        &NoteInput {
            title: input.title.clone(),
            content: input.content.clone(),
            attachments: attachments.clone().unwrap_or_default(),
            labels: input.labels.clone(),
        },
        &known_for_validation,
    )
    .map_err(anyhow::Error::new)?;

    for (key, value) in &input.labels {
        let value_type = label_value_types
            .get(key)
            .copied()
            .unwrap_or(LabelValueType::Text);
        if !validate_label_value(value_type, value) {
            return Err(anyhow::Error::new(ValidationError::InvalidLabelValue {
                key: key.clone(),
                value: value.clone(),
                value_type,
            }));
        }
    }

    let mut missing_keys: Vec<String> = Vec::new();
    for (key, _) in &input.labels {
        if !key.is_empty()
            && !existing_keys.iter().any(|k| k == key)
            && !missing_keys.iter().any(|k| k == key)
        {
            missing_keys.push(key.clone());
        }
    }

    let now = chrono::Utc::now().timestamp();
    let note_revision = session.get_note_revision(id).await?.unwrap_or(1) + 1;
    drop(session);
    let chunks = crate::chunk::chunk_content(&input.content);
    let mut prepared_attachments = match attachments.as_deref() {
        Some(attachments) => Some(ctx.attachments().prepare(id, attachments).await?),
        None => None,
    };
    let attachment_metadata = prepared_attachments
        .as_ref()
        .map(|prepared| prepared.metadata().to_vec());

    let transaction = match ctx.storage().begin(TransactionMode::Deferred).await {
        Ok(transaction) => transaction,
        Err(error) => {
            return Err(abort_optional(prepared_attachments.take(), error.into()).await);
        }
    };
    let transaction_result = async {
        let affected = match attachment_metadata.as_deref() {
            Some(attachments) => {
                transaction
                    .update_note(NoteUpdate {
                        id,
                        title: &input.title,
                        content: &input.content,
                        attachments,
                        updated_at: now,
                        note_revision,
                    })
                    .await?
            }
            None => {
                transaction
                    .update_note_fields(NoteFieldsUpdate {
                        id,
                        title: &input.title,
                        content: &input.content,
                        updated_at: now,
                        note_revision,
                    })
                    .await?
            }
        };
        if affected == 0 {
            return anyhow::Ok(None);
        }
        for key in &missing_keys {
            transaction.insert_label_key(key, "").await?;
        }
        transaction.clear_note_labels(id).await?;
        let queued =
            crate::sync_note_embedding_jobs(transaction.as_ref(), id, &chunks, note_revision, now)
                .await?;
        for (key, value) in &input.labels {
            transaction.attach_label(id, key, value).await?;
        }
        let resolved_labels: Vec<Label> = transaction.labels_for_note(id).await?;
        anyhow::Ok(Some((queued, resolved_labels)))
    }
    .await;

    let transaction_result = match transaction_result {
        Ok(Some(result)) => Ok(result),
        Ok(None) => {
            if let Err(error) = transaction.rollback().await {
                return Err(abort_optional(prepared_attachments.take(), error.into()).await);
            }
            if let Some(prepared) = prepared_attachments.take() {
                prepared.abort().await?;
            }
            return Ok(None);
        }
        Err(error) => Err(error),
    };
    let finalized = crate::save_note::finish_transaction(transaction, transaction_result).await;
    let (queued, resolved_labels) = match finalized {
        Ok(result) => result,
        Err(error) => {
            return Err(abort_optional(prepared_attachments.take(), error).await);
        }
    };
    if let Some(prepared) = prepared_attachments.take() {
        prepared.publish().await?;
    }
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }

    match attachments {
        Some(attachments) => Ok(Some(Note {
            id: id.to_string(),
            title: input.title,
            content: input.content,
            attachments,
            labels: resolved_labels,
            created_at: existing.created_at,
            updated_at: now,
            deleted_at: None,
        })),
        None => crate::get_note_metadata(ctx, id).await,
    }
}

async fn abort_optional(
    prepared: Option<Box<dyn PreparedAttachmentSet>>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match prepared {
        Some(prepared) => crate::note_attachments::abort_with_primary(prepared, primary).await,
        None => primary,
    }
}

pub async fn delete_note(ctx: &Context, id: &str) -> anyhow::Result<bool> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let deleted = transaction
            .soft_delete_note(id, chrono::Utc::now().timestamp())
            .await?
            > 0;
        if deleted {
            transaction.clear_note_search_data(id).await?;
        }
        anyhow::Ok(deleted)
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await
}

pub async fn permanently_delete_note(ctx: &Context, id: &str) -> anyhow::Result<bool> {
    let session = ctx.storage().session().await?;
    let deleted = session.permanently_delete_note(id).await? > 0;
    drop(session);
    if deleted {
        ctx.attachments().remove_note(id).await?;
    }
    Ok(deleted)
}

pub async fn restore_notes(ctx: &Context, ids: &[String]) -> anyhow::Result<bool> {
    let mut seen = HashSet::new();
    let ids = ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(false);
    }

    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let mut notes = Vec::with_capacity(ids.len());
        for id in &ids {
            let Some((content, note_revision)) = transaction
                .get_deleted_note_content_and_revision(id)
                .await?
            else {
                return anyhow::Ok(None);
            };
            notes.push((id, content, note_revision.saturating_add(1)));
        }

        let now = chrono::Utc::now().timestamp();
        let mut queued = 0;
        for (id, content, note_revision) in notes {
            transaction.restore_note(id, note_revision).await?;
            let chunks = crate::chunk::chunk_content(&content);
            queued += crate::sync_note_embedding_jobs(
                transaction.as_ref(),
                id,
                &chunks,
                note_revision,
                now,
            )
            .await?;
        }
        anyhow::Ok(Some(queued))
    }
    .await;

    let transaction_result = match transaction_result {
        Ok(Some(queued)) => Ok(queued),
        Ok(None) => {
            transaction.rollback().await?;
            return Ok(false);
        }
        Err(error) => Err(error),
    };
    let queued = crate::save_note::finish_transaction(transaction, transaction_result).await?;
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }
    Ok(true)
}

pub async fn purge_expired_deleted_notes(ctx: &Context, now: i64) -> anyhow::Result<usize> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let cutoff = now.saturating_sub(TRASH_RETENTION_SECONDS);
        let ids = transaction.list_expired_deleted_note_ids(cutoff).await?;
        for id in &ids {
            transaction.permanently_delete_note(id).await?;
        }
        anyhow::Ok(ids)
    }
    .await;
    let ids = crate::save_note::finish_transaction(transaction, transaction_result).await?;
    for id in &ids {
        ctx.attachments().remove_note(id).await?;
    }
    Ok(ids.len())
}
