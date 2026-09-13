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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedReadError {
    Unsupported,
    LimitExceeded,
    Missing,
    StorageFailure,
}

impl std::fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "bounded attachment reads are not supported by this store",
            Self::LimitExceeded => "attachment object exceeds bounded read limit",
            Self::Missing => "attachment object does not exist",
            Self::StorageFailure => "attachment bounded read failed",
        })
    }
}

impl std::error::Error for BoundedReadError {}

#[async_trait::async_trait]
pub trait AttachmentStore: Send + Sync {
    async fn put_immutable(&self, _request: PutObjectRequest) -> anyhow::Result<StoredObject> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    async fn read_object(&self, _object_key: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("immutable attachment objects are not supported by this store")
    }

    /// Reads at most `max_bytes` from one immutable object.
    ///
    /// Implementations must enforce the bound while streaming. This default
    /// intentionally does not delegate to the unbounded compatibility API.
    async fn read_object_bounded(
        &self,
        _object_key: &str,
        _max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        Err(BoundedReadError::Unsupported.into())
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

    /// Reads an attachment from the legacy note-id plus user-path namespace.
    async fn read_legacy(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>>;

    /// Reads at most `max_bytes` from the legacy note/path namespace.
    /// The safe default never falls back to the unbounded compatibility API.
    async fn read_legacy_bounded(
        &self,
        _note_id: &str,
        _path: &str,
        _max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        Err(BoundedReadError::Unsupported.into())
    }

    fn info(&self) -> AttachmentStoreInfo;
}
