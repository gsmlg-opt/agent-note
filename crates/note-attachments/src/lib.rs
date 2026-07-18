mod filesystem;
mod path;

pub use filesystem::FilesystemAttachmentStore;

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
pub trait AttachmentStore: Send + Sync {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[note_core::NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>>;

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
