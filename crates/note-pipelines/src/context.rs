use note_embedding::Embedder;
use note_storage::Storage;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub trait EmbeddingJobNotifier: Send + Sync {
    fn wake(&self);
}

/// One struct holding a pooled DB handle and the shared embedder instance (docs/design.md §6).
/// Constructed once per process (Axum) or per stdio invocation — passed explicitly, no globals.
pub struct Context {
    pub storage: Arc<Storage>,
    pub embedder: Arc<dyn Embedder>,
    attachments_dir: PathBuf,
    embedding_job_notifier: Option<Arc<dyn EmbeddingJobNotifier>>,
}

impl Context {
    pub fn new(storage: Arc<Storage>, embedder: Arc<dyn Embedder>) -> Self {
        Self::with_attachment_dir(storage, embedder, default_attachment_dir())
    }

    pub fn with_attachment_dir(
        storage: Arc<Storage>,
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
        storage: Arc<Storage>,
        embedder: Arc<dyn Embedder>,
        embedding_job_notifier: Arc<dyn EmbeddingJobNotifier>,
    ) -> Self {
        Self::with_embedding_job_notifier_and_attachment_dir(
            storage,
            embedder,
            embedding_job_notifier,
            default_attachment_dir(),
        )
    }

    pub fn with_embedding_job_notifier_and_attachment_dir(
        storage: Arc<Storage>,
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

    pub fn attachments_dir(&self) -> &Path {
        &self.attachments_dir
    }

    pub fn wake_embedding_jobs(&self) {
        if let Some(notifier) = &self.embedding_job_notifier {
            notifier.wake();
        }
    }
}

fn default_attachment_dir() -> PathBuf {
    std::env::var("NOTE_ATTACHMENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("attachments"))
}
