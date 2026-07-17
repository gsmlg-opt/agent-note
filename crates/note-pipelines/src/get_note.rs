use crate::context::Context;
use note_core::Note;

pub async fn get_note(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let session = ctx.storage().session().await?;
    let Some(mut note) = session.get_note(id).await? else {
        return Ok(None);
    };
    crate::attachment_files::hydrate_note_attachments(ctx, &mut note)?;
    Ok(Some(note))
}

pub async fn get_note_markdown(ctx: &Context, id: &str) -> anyhow::Result<Option<String>> {
    let session = ctx.storage().session().await?;
    Ok(session.get_note_content(id).await?)
}
