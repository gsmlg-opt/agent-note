use crate::context::Context;
use note_core::Note;

pub async fn list_notes(ctx: &Context) -> anyhow::Result<Vec<Note>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_notes(&conn).await
}
