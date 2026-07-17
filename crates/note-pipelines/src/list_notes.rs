use crate::context::Context;
use note_core::{parse_label_selectors, Note, NoteListItem};

#[derive(Debug, Default, Clone)]
pub struct ListNotesParams {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub label: Option<String>,
}

pub async fn list_notes(ctx: &Context, params: ListNotesParams) -> anyhow::Result<Vec<Note>> {
    let session = ctx.storage().session().await?;
    let selectors = params
        .label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    let mut notes = session
        .list_notes(&selectors, params.limit, params.offset)
        .await?;
    for note in &mut notes {
        crate::attachment_files::hydrate_note_attachments(ctx, note)?;
    }
    Ok(notes)
}

pub async fn list_all_notes(ctx: &Context) -> anyhow::Result<Vec<Note>> {
    let session = ctx.storage().session().await?;
    let mut notes = session.list_all_notes().await?;
    for note in &mut notes {
        crate::attachment_files::hydrate_note_attachments(ctx, note)?;
    }
    Ok(notes)
}

pub async fn list_note_summaries(
    ctx: &Context,
    params: ListNotesParams,
) -> anyhow::Result<Vec<NoteListItem>> {
    let session = ctx.storage().session().await?;
    let selectors = params
        .label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    Ok(session
        .list_note_summaries(&selectors, params.limit, params.offset)
        .await?)
}

pub async fn count_notes(ctx: &Context, label: Option<String>) -> anyhow::Result<usize> {
    let session = ctx.storage().session().await?;
    let selectors = label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    Ok(session.count_notes(&selectors).await?)
}

pub async fn list_deleted_note_summaries(ctx: &Context) -> anyhow::Result<Vec<NoteListItem>> {
    let session = ctx.storage().session().await?;
    Ok(session.list_deleted_note_summaries().await?)
}
