use crate::context::Context;
use note_core::{validate_note_input, Label, Note, NoteInput};

pub struct SaveNoteInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
}

pub async fn save_note(ctx: &Context, input: SaveNoteInput) -> anyhow::Result<Note> {
    let conn = ctx.storage.connect()?;

    let known_keys: Vec<String> = note_storage::list_label_keys(&conn)
        .await?
        .into_iter()
        .map(|k| k.key)
        .collect();

    validate_note_input(
        &NoteInput {
            title: input.title.clone(),
            content: input.content.clone(),
            labels: input.labels.clone(),
        },
        &known_keys,
    )
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let (dense, sparse) = ctx.embedder.embed(&input.content).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let weights: Vec<(i64, f64)> = sparse.into_iter().map(|(k, v)| (k, v as f64)).collect();

    // docs/design.md §3: notes, notes_embeddings and notes_sparse_weights (plus label attachments)
    // must be written in one atomic transaction — a partial write silently degrades recall and is a
    // correctness bug. libsql's `Transaction` derefs to `Connection`, so we pass `&tx` straight into
    // the existing `&Connection`-taking storage functions and only `commit()` after every write
    // succeeds. Any early `?` return drops `tx`, whose Drop rolls the transaction back, so nothing
    // partial ever persists.
    let tx = conn.transaction().await?;
    note_storage::insert_note(&tx, &id, &input.title, &input.content, now, now).await?;
    note_storage::insert_embedding(&tx, &id, &dense).await?;
    note_storage::insert_sparse_weights(&tx, &id, &weights).await?;
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
