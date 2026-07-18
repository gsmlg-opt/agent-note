use crate::{
    path::{canonical_attachment_path, note_directory},
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentSet,
};
use note_core::NoteAttachment;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};
use tokio::fs;

pub struct FilesystemAttachmentStore {
    root: PathBuf,
}

struct PreparedFilesystemSet {
    final_dir: PathBuf,
    temp_dir: Option<PathBuf>,
    metadata: Vec<NoteAttachment>,
}

impl FilesystemAttachmentStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait::async_trait]
impl AttachmentStore for FilesystemAttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
        let final_dir = note_directory(&self.root, note_id)?;
        let metadata = attachments
            .iter()
            .map(|attachment| NoteAttachment {
                id: attachment.id.clone(),
                path: attachment.path.clone(),
                mime: attachment.mime.clone(),
                description: attachment.description.clone(),
                content: Vec::new(),
            })
            .collect();

        if attachments.is_empty() {
            return Ok(Box::new(PreparedFilesystemSet {
                final_dir,
                temp_dir: None,
                metadata,
            }));
        }

        fs::create_dir_all(&self.root).await?;
        let temp_dir = self
            .root
            .join(format!(".{note_id}-{}.tmp", uuid::Uuid::new_v4()));
        fs::create_dir(&temp_dir).await?;

        let write_result = async {
            for attachment in attachments {
                let path = canonical_attachment_path(&temp_dir, &attachment.path)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await?;
                }
                fs::write(path, &attachment.content).await?;
            }
            anyhow::Ok(())
        }
        .await;

        if let Err(error) = write_result {
            let _ = remove_directory_if_present(&temp_dir).await;
            return Err(error);
        }

        Ok(Box::new(PreparedFilesystemSet {
            final_dir,
            temp_dir: Some(temp_dir),
            metadata,
        }))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let note_dir = note_directory(&self.root, note_id)?;
        let path = canonical_attachment_path(&note_dir, path)?;
        Ok(fs::read(path).await?)
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        let note_dir = note_directory(&self.root, note_id)?;
        remove_directory_if_present(&note_dir).await
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "filesystem".to_string(),
            location: Some(self.root.to_string_lossy().into_owned()),
        }
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentSet for PreparedFilesystemSet {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let Self {
            final_dir,
            temp_dir,
            metadata: _,
        } = *self;

        remove_directory_if_present(&final_dir).await?;
        if let Some(temp_dir) = temp_dir {
            fs::rename(temp_dir, final_dir).await?;
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        if let Some(temp_dir) = self.temp_dir.as_ref() {
            remove_directory_if_present(temp_dir).await?;
        }
        Ok(())
    }
}

async fn remove_directory_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
