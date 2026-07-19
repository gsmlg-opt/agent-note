mod filesystem;
mod path;
#[cfg_attr(not(test), allow(dead_code))]
mod s3;

pub use filesystem::FilesystemAttachmentStore;
pub use s3::{S3AttachmentConfig, S3AttachmentStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentStoreInfo {
    pub engine: String,
    pub location: Option<String>,
}

#[async_trait::async_trait]
pub trait PreparedAttachmentSet: Send {
    fn metadata(&self) -> &[note_core::NoteAttachment];

    async fn publish(self: Box<Self>) -> anyhow::Result<()>;

    async fn abort(self: Box<Self>) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
pub trait PreparedAttachmentMutation: Send {
    async fn publish(self: Box<Self>) -> anyhow::Result<()>;

    async fn abort(self: Box<Self>) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
pub trait AttachmentStore: Send + Sync {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[note_core::NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>>;

    async fn prepare_put(
        &self,
        note_id: &str,
        attachment: &note_core::NoteAttachment,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>>;

    async fn prepare_delete(
        &self,
        note_id: &str,
        path: &str,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>>;

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>>;

    async fn hydrate(
        &self,
        note_id: &str,
        attachments: &mut [note_core::NoteAttachment],
    ) -> anyhow::Result<()> {
        for attachment in attachments {
            attachment.content = self.read(note_id, &attachment.path).await?;
        }
        Ok(())
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()>;

    fn info(&self) -> AttachmentStoreInfo;
}
