use crate::context::Context;
use note_core::{parse_label_selectors, Note, NoteListItem};

#[derive(Debug, Default, Clone)]
pub struct ListNotesParams {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub label: Option<String>,
}

pub async fn list_notes(ctx: &Context, params: ListNotesParams) -> anyhow::Result<Vec<Note>> {
    let conn = ctx.storage.connect()?;
    let selectors = params
        .label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    let mut notes =
        note_storage::list_notes(&conn, &selectors, params.limit, params.offset).await?;
    for note in &mut notes {
        crate::attachment_files::hydrate_note_attachments(ctx, note)?;
    }
    Ok(notes)
}

pub async fn list_all_notes(ctx: &Context) -> anyhow::Result<Vec<Note>> {
    let conn = ctx.storage.connect()?;
    let mut notes = note_storage::list_all_notes(&conn).await?;
    for note in &mut notes {
        crate::attachment_files::hydrate_note_attachments(ctx, note)?;
    }
    Ok(notes)
}

pub async fn list_note_summaries(
    ctx: &Context,
    params: ListNotesParams,
) -> anyhow::Result<Vec<NoteListItem>> {
    let conn = ctx.storage.connect()?;
    let selectors = params
        .label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    note_storage::list_note_summaries(&conn, &selectors, params.limit, params.offset).await
}

pub async fn count_notes(ctx: &Context, label: Option<String>) -> anyhow::Result<usize> {
    let conn = ctx.storage.connect()?;
    let selectors = label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    note_storage::count_notes(&conn, &selectors).await
}

pub async fn list_deleted_note_summaries(ctx: &Context) -> anyhow::Result<Vec<NoteListItem>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_deleted_note_summaries(&conn).await
}
