use crate::context::Context;
use note_core::{try_parse_label_selectors, Note, NoteListItem};

pub const DEFAULT_LIST_LIMIT: i64 = 10;
pub const MAX_LIST_LIMIT: i64 = 1_000;

pub fn normalized_list_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(0, MAX_LIST_LIMIT)
}

pub fn normalized_list_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

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
        .map(try_parse_label_selectors)
        .transpose()
        .map_err(anyhow::Error::new)?
        .unwrap_or_default();
    let mut notes = session
        .list_notes(&selectors, params.limit, params.offset)
        .await?;
    drop(session);
    for note in &mut notes {
        crate::hydrate_note_attachments(ctx, note).await?;
    }
    Ok(notes)
}

pub async fn list_all_notes(ctx: &Context) -> anyhow::Result<Vec<Note>> {
    let session = ctx.storage().session().await?;
    let mut notes = session.list_all_notes().await?;
    drop(session);
    for note in &mut notes {
        crate::hydrate_note_attachments(ctx, note).await?;
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
        .map(try_parse_label_selectors)
        .transpose()
        .map_err(anyhow::Error::new)?
        .unwrap_or_default();
    Ok(session
        .list_note_summaries(&selectors, params.limit, params.offset)
        .await?)
}

pub async fn count_notes(ctx: &Context, label: Option<String>) -> anyhow::Result<usize> {
    let session = ctx.storage().session().await?;
    let selectors = label
        .as_deref()
        .map(try_parse_label_selectors)
        .transpose()
        .map_err(anyhow::Error::new)?
        .unwrap_or_default();
    Ok(session.count_notes(&selectors).await?)
}

pub async fn list_deleted_note_summaries(ctx: &Context) -> anyhow::Result<Vec<NoteListItem>> {
    let session = ctx.storage().session().await?;
    Ok(session.list_deleted_note_summaries().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_pagination_normalizes_defaults_bounds_and_offsets() {
        assert_eq!(normalized_list_limit(None), 10);
        assert_eq!(normalized_list_limit(Some(0)), 0);
        assert_eq!(normalized_list_limit(Some(25)), 25);
        assert_eq!(normalized_list_limit(Some(-1)), 0);
        assert_eq!(normalized_list_limit(Some(2_000)), 1_000);

        assert_eq!(normalized_list_offset(None), 0);
        assert_eq!(normalized_list_offset(Some(-1)), 0);
        assert_eq!(normalized_list_offset(Some(25)), 25);
    }
}
