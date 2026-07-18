use crate::Context;
use note_attachments::PreparedAttachmentSet;
use note_core::{Note, NoteAttachment};
use std::path::{Component, Path};

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
    let requested_path = normalize_attachment_path(requested_path)?;
    let session = ctx.storage().session().await?;
    let Some(note) = session.get_note(note_id).await? else {
        return Ok(None);
    };
    let Some(mut attachment) = note.attachments.into_iter().find(|attachment| {
        normalize_attachment_path(&attachment.path).is_ok_and(|path| path == requested_path)
    }) else {
        return Ok(None);
    };

    attachment.content = ctx.attachments().read(note_id, &attachment.path).await?;
    Ok(Some(attachment))
}

fn normalize_attachment_path(path: &str) -> anyhow::Result<Vec<String>> {
    let mut normalized = Vec::new();
    for component in Path::new(path.trim_start_matches("./")).components() {
        match component {
            Component::Normal(part) => normalized.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => anyhow::bail!("invalid attachment path: {path}"),
        }
    }
    if normalized.is_empty() {
        anyhow::bail!("invalid attachment path: {path}");
    }
    Ok(normalized)
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
