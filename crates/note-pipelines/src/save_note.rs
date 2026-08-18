use crate::context::Context;
use note_core::{
    resolve_duplicate_rule, validate_label_key, validate_label_value, validate_note_input,
    DuplicateNoteError, Label, LabelValueType, Note, NoteAttachment, NoteInput, ValidationError,
};
use note_storage::{NewNote, StorageTransaction, TransactionMode};
use std::collections::HashMap;

pub struct SaveNoteInput {
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub labels: Vec<(String, String)>,
}

pub(crate) async fn finish_transaction<T>(
    transaction: Box<dyn StorageTransaction>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            transaction.commit().await?;
            Ok(value)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(error.context(format!(
                "transaction rollback also failed: {rollback_error}"
            ))),
        },
    }
}

pub async fn save_note(ctx: &Context, input: SaveNoteInput) -> anyhow::Result<Note> {
    let session = ctx.storage().session().await?;

    let existing_label_keys = session.list_label_keys().await?;
    let existing_keys: Vec<String> = existing_label_keys.iter().map(|k| k.key.clone()).collect();
    let label_value_types: HashMap<String, LabelValueType> = existing_label_keys
        .into_iter()
        .map(|label_key| (label_key.key, label_key.value_type))
        .collect();

    // Auto-create semantics: a label key the note references but the catalog doesn't have yet is
    // created on save (with an empty description) rather than rejected. So for validation we treat
    // every referenced key as "known" — validate_note_input then only enforces non-empty
    // title/content. (This intentionally overrides the earlier "unknown key rejects the request"
    // rule; see docs/design.md §6.)
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
            attachments: input.attachments.clone(),
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

    // Distinct referenced keys not yet in the catalog — these get auto-created in the transaction.
    let mut missing_keys: Vec<String> = Vec::new();
    for (key, _) in &input.labels {
        if !existing_keys.iter().any(|k| k == key) && !missing_keys.iter().any(|k| k == key) {
            validate_label_key(key).map_err(anyhow::Error::new)?;
            missing_keys.push(key.clone());
        }
    }
    drop(session);

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let note_revision = 1;
    let chunks = crate::chunk::chunk_content(&input.content);
    let published_attachments = crate::generated_attachments::publish_generated_attachments(
        ctx.attachments(),
        &id,
        &input.attachments,
    )
    .await?;

    // The note row, labels, chunk hashes, and embedding jobs are committed atomically. Actual
    // embedding is deliberately out-of-process: save returns once the durable queue request exists.
    let transaction = match ctx.storage().begin(TransactionMode::Immediate).await {
        Ok(transaction) => transaction,
        Err(error) => {
            return Err(published_attachments
                .cleanup_with_primary(ctx.attachments(), error.into())
                .await);
        }
    };
    let transaction_result = async {
        let config = transaction.get_system_config().await?;
        if config.duplicate_check.enabled {
            for rule in &config.duplicate_check.rules {
                let Some(labels) = resolve_duplicate_rule(rule, &input.labels) else {
                    continue;
                };
                if let Some(existing_note_id) = transaction.find_note_with_labels(&labels).await? {
                    return Err(anyhow::Error::new(DuplicateNoteError {
                        existing_note_id,
                        labels,
                    }));
                }
            }
        }
        for key in &missing_keys {
            transaction.insert_label_key_if_missing(key, "").await?;
        }
        // A missing key may have been created with a non-text type before this immediate
        // transaction acquired the write lock. Validate against the catalog state protected by
        // this transaction before persisting any label values.
        let current_label_types: HashMap<String, LabelValueType> = transaction
            .list_label_keys()
            .await?
            .into_iter()
            .map(|label_key| (label_key.key, label_key.value_type))
            .collect();
        for (key, value) in &input.labels {
            let value_type = current_label_types
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
        transaction
            .insert_note(NewNote {
                id: &id,
                title: &input.title,
                content: &input.content,
                attachments: &published_attachments.metadata,
                created_at: now,
                updated_at: now,
                note_revision,
                deleted_at: None,
            })
            .await?;
        let queued =
            crate::sync_note_embedding_jobs(transaction.as_ref(), &id, &chunks, note_revision, now)
                .await?;
        for (key, value) in &input.labels {
            transaction.attach_label(&id, key, value).await?;
        }
        let resolved_labels: Vec<Label> = transaction.labels_for_note(&id).await?;
        anyhow::Ok((queued, resolved_labels))
    }
    .await;

    let (queued, resolved_labels) = match transaction_result {
        Ok(result) => match transaction.commit().await {
            Ok(()) => result,
            Err(error) => {
                return Err(anyhow::Error::from(error).context(
                    "note transaction commit failed; generated attachment objects retained because commit outcome is unknown",
                ));
            }
        },
        Err(error) => {
            return match transaction.rollback().await {
                Ok(()) => Err(published_attachments
                    .cleanup_with_primary(ctx.attachments(), error)
                    .await),
                Err(rollback_error) => Err(error.context(format!(
                    "transaction rollback failed; generated attachment objects retained because rollback outcome is unknown: {rollback_error}"
                ))),
            };
        }
    };
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }

    Ok(Note {
        id,
        title: input.title,
        content: input.content,
        attachments: published_attachments.attachments,
        labels: resolved_labels,
        created_at: now,
        updated_at: now,
        revision: note_revision,
        deleted_at: None,
    })
}
