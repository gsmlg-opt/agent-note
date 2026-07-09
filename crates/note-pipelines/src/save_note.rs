use crate::context::Context;
use note_core::{
    validate_label_value, validate_note_input, Label, LabelValueType, Note, NoteInput,
    ValidationError,
};
use std::collections::HashMap;

pub struct SaveNoteInput {
    pub title: String,
    pub content: String,
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
    let chunks = crate::chunk::chunk_content(&input.content);

    // The note row, labels, chunk hashes, and embedding jobs are committed atomically. Actual
    // embedding is deliberately out-of-process: save returns once the durable queue request exists.
    let tx = conn.transaction().await?;
    for key in &missing_keys {
        note_storage::insert_label_key(&tx, key, "").await?;
    }
    note_storage::insert_note(&tx, &id, &input.title, &input.content, now, now).await?;
    crate::sync_note_embedding_jobs(&tx, &id, &chunks, now).await?;
    for (key, value) in &input.labels {
        note_storage::attach_label(&tx, &id, key, value).await?;
    }
    let resolved_labels: Vec<Label> = note_storage::labels_for_note(&tx, &id).await?;
    tx.commit().await?;

    Ok(Note {
        id,
        title: input.title,
        content: input.content,
        labels: resolved_labels,
        created_at: now,
        updated_at: now,
    })
}
