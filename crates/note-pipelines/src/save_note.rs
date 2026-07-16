use crate::context::Context;
use note_core::{
    resolve_duplicate_rule, validate_label_value, validate_note_input, DuplicateNoteError, Label,
    LabelValueType, Note, NoteAttachment, NoteInput, ValidationError,
};
use std::collections::HashMap;

pub struct SaveNoteInput {
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub labels: Vec<(String, String)>,
}

pub async fn save_note(ctx: &Context, input: SaveNoteInput) -> anyhow::Result<Note> {
    let conn = ctx.storage.connect()?;

    let existing_label_keys = note_storage::list_label_keys(&conn).await?;
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
        if !key.is_empty()
            && !existing_keys.iter().any(|k| k == key)
            && !missing_keys.iter().any(|k| k == key)
        {
            missing_keys.push(key.clone());
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let note_revision = 1;
    let chunks = crate::chunk::chunk_content(&input.content);
    let prepared_attachments =
        crate::attachment_files::prepare_note_attachments(ctx, &id, &input.attachments)?;

    // The note row, labels, chunk hashes, and embedding jobs are committed atomically. Actual
    // embedding is deliberately out-of-process: save returns once the durable queue request exists.
    let tx_result = async {
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let config = note_storage::get_system_config(&tx).await?;
        if config.duplicate_check.enabled {
            for rule in &config.duplicate_check.rules {
                let Some(labels) = resolve_duplicate_rule(rule, &input.labels) else {
                    continue;
                };
                if let Some(existing_note_id) =
                    note_storage::find_note_with_labels(&tx, &labels).await?
                {
                    return Err(anyhow::Error::new(DuplicateNoteError {
                        existing_note_id,
                        labels,
                    }));
                }
            }
        }
        for key in &missing_keys {
            note_storage::insert_label_key_if_missing(&tx, key, "").await?;
        }
        // A missing key may have been created with a non-text type before this immediate
        // transaction acquired the write lock. Validate against the catalog state protected by
        // this transaction before persisting any label values.
        let current_label_types: HashMap<String, LabelValueType> =
            note_storage::list_label_keys(&tx)
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
        note_storage::insert_note_with_attachments(
            &tx,
            &id,
            &input.title,
            &input.content,
            prepared_attachments.metadata(),
            now,
            now,
            note_revision,
        )
        .await?;
        let queued = crate::sync_note_embedding_jobs(&tx, &id, &chunks, note_revision, now).await?;
        for (key, value) in &input.labels {
            note_storage::attach_label(&tx, &id, key, value).await?;
        }
        let resolved_labels: Vec<Label> = note_storage::labels_for_note(&tx, &id).await?;
        tx.commit().await?;
        anyhow::Ok((queued, resolved_labels))
    }
    .await;

    let (queued, resolved_labels) = match tx_result {
        Ok(result) => result,
        Err(error) => {
            crate::attachment_files::cleanup_prepared_note_attachments(&prepared_attachments);
            return Err(error);
        }
    };
    crate::attachment_files::commit_note_attachments(prepared_attachments)?;
    if queued > 0 {
        ctx.wake_embedding_jobs();
    }

    Ok(Note {
        id,
        title: input.title,
        content: input.content,
        attachments: input.attachments,
        labels: resolved_labels,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    })
}
