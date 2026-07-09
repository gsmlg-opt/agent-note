use crate::{context::Context, SaveNoteInput};
use note_core::{
    validate_label_value, validate_note_input, Label, LabelValueType, Note, NoteInput,
    ValidationError,
};
use std::collections::HashMap;

pub async fn update_note(
    ctx: &Context,
    id: &str,
    input: SaveNoteInput,
) -> anyhow::Result<Option<Note>> {
    let conn = ctx.storage.connect()?;

    let existing = match note_storage::get_note(&conn, id).await? {
        Some(note) => note,
        None => return Ok(None),
    };

    let existing_label_keys = note_storage::list_label_keys(&conn).await?;
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
    let chunks = crate::chunk::chunk_content(&input.content);

    let tx = conn.transaction().await?;
    let affected = note_storage::update_note(&tx, id, &input.title, &input.content, now).await?;
    if affected == 0 {
        return Ok(None);
    }
    for key in &missing_keys {
        note_storage::insert_label_key(&tx, key, "").await?;
    }
    note_storage::clear_note_labels(&tx, id).await?;
    crate::sync_note_embedding_jobs(&tx, id, &chunks, now).await?;
    for (key, value) in &input.labels {
        note_storage::attach_label(&tx, id, key, value).await?;
    }
    let resolved_labels: Vec<Label> = note_storage::labels_for_note(&tx, id).await?;
    tx.commit().await?;

    Ok(Some(Note {
        id: id.to_string(),
        title: input.title,
        content: input.content,
        labels: resolved_labels,
        created_at: existing.created_at,
        updated_at: now,
    }))
}

pub async fn delete_note(ctx: &Context, id: &str) -> anyhow::Result<bool> {
    let conn = ctx.storage.connect()?;
    Ok(note_storage::delete_note(&conn, id).await? > 0)
}
