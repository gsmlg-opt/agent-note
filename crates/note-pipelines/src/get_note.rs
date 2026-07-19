use crate::context::Context;
use note_core::Note;

pub async fn get_note(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let session = ctx.storage().session().await?;
    let note = session.get_note(id).await?;
    drop(session);
    let Some(mut note) = note else {
        return Ok(None);
    };
    crate::hydrate_note_attachments(ctx, &mut note).await?;
    Ok(Some(note))
}

pub async fn get_note_metadata(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let session = ctx.storage().session().await?;
    Ok(session.get_note(id).await?)
}

pub async fn get_note_markdown(ctx: &Context, id: &str) -> anyhow::Result<Option<String>> {
    let session = ctx.storage().session().await?;
    Ok(session.get_note_content(id).await?)
}
