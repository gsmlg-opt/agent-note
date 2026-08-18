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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectRequest {
    pub object_key: String,
    pub bytes: Vec<u8>,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub object_key: String,
    pub size_bytes: u64,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub size_bytes: u64,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteObjectOutcome {
    Deleted,
    AlreadyAbsent,
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
    async fn put_immutable(&self, _request: PutObjectRequest) -> anyhow::Result<StoredObject> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    async fn read_object(&self, _object_key: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    async fn head_object(&self, _object_key: &str) -> anyhow::Result<ObjectMetadata> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    async fn delete_object(&self, _object_key: &str) -> anyhow::Result<DeleteObjectOutcome> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    async fn delete_legacy(
        &self,
        _note_id: &str,
        _user_path: &str,
    ) -> anyhow::Result<DeleteObjectOutcome> {
        anyhow::bail!("legacy attachment objects are not supported by this store")
    }

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

    /// Reads from the legacy note-id plus user-path namespace.
    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>>;

    /// Reads an attachment from the legacy note-id plus user-path namespace.
    async fn read_legacy(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.read(note_id, path).await
    }

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
