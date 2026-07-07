use crate::context::Context;
use note_core::Note;

pub async fn get_note(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let conn = ctx.storage.connect()?;
    note_storage::get_note(&conn, id).await
}
