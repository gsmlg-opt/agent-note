use crate::{context::Context, SaveNoteInput};
use note_core::{
    validate_label_key, validate_label_value, validate_note_input, Label, LabelValueType, Note,
    NoteAttachment, NoteInput, ValidationError,
};
use note_storage::{NoteFieldsUpdate, NoteMutationResult, NoteUpdate, TransactionMode};
use std::collections::{HashMap, HashSet};

pub const TRASH_RETENTION_DAYS: i64 = 90;
pub const TRASH_RETENTION_SECONDS: i64 = TRASH_RETENTION_DAYS * 24 * 60 * 60;

pub struct UpdateNoteFieldsInput {
    pub expected_revision: i64,
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
}

pub async fn update_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    input: SaveNoteInput,
) -> anyhow::Result<Option<Note>> {
    update_note_inner(
        ctx,
        id,
        UpdateNoteFieldsInput {
            expected_revision,
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
        None => return Err(crate::NoteMutationError::NotFound(id.to_string()).into()),
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
        if !existing_keys.iter().any(|k| k == key) && !missing_keys.iter().any(|k| k == key) {
            validate_label_key(key).map_err(anyhow::Error::new)?;
            missing_keys.push(key.clone());
        }
    }

    let now = chrono::Utc::now().timestamp();
    let expected_revision = input.expected_revision;
    drop(session);
    let chunks = crate::chunk::chunk_content(&input.content);
    let published_attachments = match attachments.as_deref() {
        Some(attachments) => Some(
            crate::generated_attachments::publish_generated_attachments(
                ctx.attachments(),
                id,
                attachments,
            )
            .await?,
        ),
        None => None,
    };
    let attachment_metadata = published_attachments
        .as_ref()
        .map(|published| published.metadata.as_slice());

    let transaction = match ctx.storage().begin(TransactionMode::Immediate).await {
        Ok(transaction) => transaction,
        Err(error) => {
            return match published_attachments {
                Some(published) => Err(published
                    .cleanup_with_primary(ctx.attachments(), error.into())
                    .await),
                None => Err(error.into()),
            };
        }
    };
    let transaction_result = async {
        let current = transaction
            .get_note(id)
            .await?
            .ok_or_else(|| crate::NoteMutationError::NotFound(id.to_string()))?;
        let mutation = match attachment_metadata.as_deref() {
            Some(attachments) => {
                transaction
                    .update_note(NoteUpdate {
                        id,
                        title: &input.title,
                        content: &input.content,
                        attachments,
                        updated_at: now,
                        expected_revision,
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
                        expected_revision,
                    })
                    .await?
            }
        };
        let note_revision = crate::mutation_error::mutation_revision(id, mutation)?;
        if attachment_metadata.is_some() {
            crate::generated_attachments::enqueue_attachment_cleanup(
                transaction.as_ref(),
                id,
                &current.attachments,
                now,
            )
            .await?;
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
        anyhow::Ok((queued, resolved_labels, note_revision))
    }
    .await;

    let (queued, resolved_labels, note_revision) = match transaction_result {
        Ok(result) => match transaction.commit().await {
            Ok(()) => result,
            Err(error) => {
                let error = anyhow::Error::from(error);
                return if published_attachments.is_some() {
                    Err(error.context(
                        "note transaction commit failed; generated attachment objects retained because commit outcome is unknown",
                    ))
                } else {
                    Err(error)
                };
            }
        },
        Err(error) => {
            return match transaction.rollback().await {
                Ok(()) => match published_attachments {
                    Some(published) => Err(published
                        .cleanup_with_primary(ctx.attachments(), error)
                        .await),
                    None => Err(error),
                },
                Err(rollback_error) => Err(error.context(format!(
                    "transaction rollback failed; generated attachment objects retained because rollback outcome is unknown: {rollback_error}"
                ))),
            };
        }
    };
    if published_attachments.is_some() {
        crate::generated_attachments::run_attachment_cleanup_once(ctx).await;
    }
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }

    match attachments {
        Some(_) => Ok(Some(Note {
            id: id.to_string(),
            title: input.title,
            content: input.content,
            attachments: published_attachments
                .expect("published attachments exist for a full update")
                .attachments,
            labels: resolved_labels,
            created_at: existing.created_at,
            updated_at: now,
            revision: note_revision,
            deleted_at: None,
        })),
        None => Ok(Some(crate::get_note_metadata(ctx, id).await?.ok_or_else(
            || crate::NoteMutationError::NotFound(id.to_string()),
        )?)),
    }
}

pub async fn delete_note(ctx: &Context, id: &str, expected_revision: i64) -> anyhow::Result<bool> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let result = transaction
            .soft_delete_note(id, expected_revision, chrono::Utc::now().timestamp())
            .await?;
        let revision = crate::mutation_error::mutation_revision(id, result)?;
        transaction.clear_note_search_data(id).await?;
        anyhow::Ok(revision > expected_revision)
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await
}

pub async fn permanently_delete_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
) -> anyhow::Result<bool> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let snapshot = transaction
            .get_deleted_note_snapshot(id)
            .await?
            .ok_or_else(|| crate::NoteMutationError::NotFound(id.to_string()))?;
        crate::mutation_error::mutation_revision(
            id,
            transaction
                .permanently_delete_note(id, expected_revision)
                .await?,
        )?;
        crate::generated_attachments::enqueue_attachment_cleanup(
            transaction.as_ref(),
            id,
            &snapshot.attachments,
            chrono::Utc::now().timestamp(),
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await?;
    crate::generated_attachments::run_attachment_cleanup_once(ctx).await;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreNoteInput {
    pub id: String,
    pub expected_revision: i64,
}

pub async fn restore_notes(ctx: &Context, entries: &[RestoreNoteInput]) -> anyhow::Result<bool> {
    let mut seen = HashSet::new();
    let entries = entries
        .iter()
        .filter(|entry| seen.insert(entry.id.clone()))
        .cloned()
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(false);
    }

    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let mut notes = Vec::with_capacity(entries.len());
        for entry in &entries {
            let Some((content, _)) = transaction
                .get_deleted_note_content_and_revision(&entry.id)
                .await?
            else {
                return Err(crate::NoteMutationError::NotFound(entry.id.clone()).into());
            };
            notes.push((entry, content));
        }

        let now = chrono::Utc::now().timestamp();
        let mut queued = 0;
        let mut applied = Vec::with_capacity(notes.len());
        for (entry, content) in notes {
            let result = transaction
                .restore_note(&entry.id, entry.expected_revision)
                .await?;
            let note_revision = crate::mutation_error::mutation_revision(&entry.id, result)?;
            let chunks = crate::chunk::chunk_content(&content);
            queued += crate::sync_note_embedding_jobs(
                transaction.as_ref(),
                &entry.id,
                &chunks,
                note_revision,
                now,
            )
            .await?;
            applied.push((entry.id.clone(), note_revision));
        }
        anyhow::Ok((queued, applied))
    }
    .await;

    let transaction_result = match transaction_result {
        Ok(result) => Ok(result),
        Err(error) => Err(error),
    };
    let (queued, applied) =
        crate::save_note::finish_transaction(transaction, transaction_result).await?;
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }
    Ok(!applied.is_empty())
}

pub async fn purge_expired_deleted_notes(ctx: &Context, now: i64) -> anyhow::Result<usize> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let cutoff = now.saturating_sub(TRASH_RETENTION_SECONDS);
        let candidates = transaction.list_expired_deleted_note_ids(cutoff).await?;
        let mut deleted = 0;
        for id in &candidates {
            let Some(snapshot) = transaction.get_deleted_note_snapshot(id).await? else {
                continue;
            };
            let result = transaction
                .permanently_delete_note(id, snapshot.revision)
                .await?;
            if matches!(result, NoteMutationResult::Applied { .. }) {
                crate::generated_attachments::enqueue_attachment_cleanup(
                    transaction.as_ref(),
                    id,
                    &snapshot.attachments,
                    now,
                )
                .await?;
                deleted += 1;
            }
        }
        anyhow::Ok(deleted)
    }
    .await;
    let deleted = crate::save_note::finish_transaction(transaction, transaction_result).await?;
    crate::generated_attachments::run_attachment_cleanup_once(ctx).await;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_only_cleans_up_applied_note_deletions() {
        assert_eq!(
            applied_deleted_note_id(
                "applied",
                NoteMutationResult::Applied {
                    value: (),
                    revision: 2,
                },
            ),
            Some("applied".to_string())
        );
        assert_eq!(
            applied_deleted_note_id(
                "conflict",
                NoteMutationResult::Conflict {
                    expected_revision: 1,
                    current_revision: 2,
                },
            ),
            None
        );
        assert_eq!(
            applied_deleted_note_id("missing", NoteMutationResult::NotFound),
            None
        );
    }
}
