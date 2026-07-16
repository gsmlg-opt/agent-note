use crate::Context;
use note_core::{Note, NoteAttachment};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub struct PreparedNoteAttachments {
    final_dir: PathBuf,
    temp_dir: Option<PathBuf>,
    metadata: Vec<NoteAttachment>,
}

impl PreparedNoteAttachments {
    pub fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }
}

pub fn prepare_note_attachments(
    ctx: &Context,
    note_id: &str,
    attachments: &[NoteAttachment],
) -> anyhow::Result<PreparedNoteAttachments> {
    let final_dir = ctx.attachments_dir().join(note_id);
    let metadata = attachments
        .iter()
        .map(|attachment| NoteAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
            content: Vec::new(),
        })
        .collect::<Vec<_>>();

    if attachments.is_empty() {
        return Ok(PreparedNoteAttachments {
            final_dir,
            temp_dir: None,
            metadata,
        });
    }

    fs::create_dir_all(ctx.attachments_dir())?;
    let temp_dir = ctx
        .attachments_dir()
        .join(format!(".{note_id}-{}.tmp", uuid::Uuid::new_v4()));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;

    let write_result = (|| -> anyhow::Result<()> {
        for attachment in attachments {
            let path = attachment_path(&temp_dir, &attachment.path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, &attachment.content)?;
        }
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }

    Ok(PreparedNoteAttachments {
        final_dir,
        temp_dir: Some(temp_dir),
        metadata,
    })
}

pub fn commit_note_attachments(prepared: PreparedNoteAttachments) -> anyhow::Result<()> {
    if prepared.final_dir.exists() {
        fs::remove_dir_all(&prepared.final_dir)?;
    }
    if let Some(temp_dir) = prepared.temp_dir {
        fs::rename(temp_dir, prepared.final_dir)?;
    }
    Ok(())
}

pub fn cleanup_prepared_note_attachments(prepared: &PreparedNoteAttachments) {
    if let Some(temp_dir) = &prepared.temp_dir {
        let _ = fs::remove_dir_all(temp_dir);
    }
}

pub fn remove_note_attachments(ctx: &Context, note_id: &str) -> anyhow::Result<()> {
    let dir = ctx.attachments_dir().join(note_id);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

pub fn hydrate_note_attachments(ctx: &Context, note: &mut Note) -> anyhow::Result<()> {
    for attachment in &mut note.attachments {
        let path = attachment_path(&ctx.attachments_dir().join(&note.id), &attachment.path)?;
        attachment.content = fs::read(path)?;
    }
    Ok(())
}

fn attachment_path(root: &Path, attachment_path: &str) -> anyhow::Result<PathBuf> {
    let mut path = PathBuf::from(root);
    let raw_path = Path::new(attachment_path.trim_start_matches("./"));
    for component in raw_path.components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => anyhow::bail!("invalid attachment path: {attachment_path}"),
        }
    }
    Ok(path)
}
