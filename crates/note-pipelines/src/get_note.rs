use crate::context::Context;
use note_core::Note;

pub async fn get_note(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let conn = ctx.storage.connect()?;
    let Some(mut note) = note_storage::get_note(&conn, id).await? else {
        return Ok(None);
    };
    crate::attachment_files::hydrate_note_attachments(ctx, &mut note)?;
    Ok(Some(note))
}
