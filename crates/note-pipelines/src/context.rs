use note_embedding::Embedder;
use note_storage::StorageBackend;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub trait EmbeddingJobNotifier: Send + Sync {
    fn wake(&self);
}

/// Shared storage and embedding dependencies constructed at the application composition root.
/// Passed explicitly through pipeline and transport layers; no globals.
pub struct Context {
    storage: Arc<dyn StorageBackend>,
    pub embedder: Arc<dyn Embedder>,
    attachments_dir: PathBuf,
    embedding_job_notifier: Option<Arc<dyn EmbeddingJobNotifier>>,
}

impl Context {
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn Embedder>,
        attachments_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            storage,
            embedder,
            attachments_dir: attachments_dir.into(),
            embedding_job_notifier: None,
        }
    }

    pub fn with_embedding_job_notifier(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn Embedder>,
        embedding_job_notifier: Arc<dyn EmbeddingJobNotifier>,
        attachments_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            storage,
            embedder,
            attachments_dir: attachments_dir.into(),
            embedding_job_notifier: Some(embedding_job_notifier),
        }
    }

    pub(crate) fn storage(&self) -> &dyn StorageBackend {
        self.storage.as_ref()
    }

    pub fn attachments_dir(&self) -> &Path {
        &self.attachments_dir
    }

    pub fn wake_embedding_jobs(&self) {
        if let Some(notifier) = &self.embedding_job_notifier {
            notifier.wake();
        }
    }
}
