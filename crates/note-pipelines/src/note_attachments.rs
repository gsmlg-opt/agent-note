use crate::Context;
use note_attachments::PreparedAttachmentSet;
use note_core::{is_relative_attachment_path, normalize_attachment_path, Note, NoteAttachment};

pub async fn hydrate_note_attachments(ctx: &Context, note: &mut Note) -> anyhow::Result<()> {
    ctx.attachments()
        .hydrate(&note.id, &mut note.attachments)
        .await
}

pub async fn get_note_attachment(
    ctx: &Context,
    note_id: &str,
    requested_path: &str,
) -> anyhow::Result<Option<NoteAttachment>> {
    if requested_path.trim().is_empty() || !is_relative_attachment_path(requested_path) {
        return Ok(None);
    }
    let requested_path = normalize_attachment_path(requested_path);
    if requested_path.is_empty() {
        return Ok(None);
    }
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    drop(session);
    let Some(mut attachment) = note.attachments.into_iter().find(|attachment| {
        is_relative_attachment_path(&attachment.path)
            && normalize_attachment_path(&attachment.path) == requested_path
    }) else {
        return Ok(None);
    };

    attachment.content = ctx.attachments().read(note_id, &attachment.path).await?;
    Ok(Some(attachment))
}

pub(crate) async fn abort_with_primary(
    prepared: Box<dyn PreparedAttachmentSet>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match prepared.abort().await {
        Ok(()) => primary,
        Err(abort_error) => primary.context(format!("attachment abort also failed: {abort_error}")),
    }
}
