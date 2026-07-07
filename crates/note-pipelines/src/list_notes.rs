use crate::context::Context;
use note_core::{parse_label_selectors, Note};

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
    note_storage::list_notes(&conn, &selectors, params.limit, params.offset).await
}
