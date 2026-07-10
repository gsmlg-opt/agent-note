use note_embedding::Embedder;
use note_storage::Storage;
use std::sync::Arc;

pub trait EmbeddingJobNotifier: Send + Sync {
    fn wake(&self);
}

/// One struct holding a pooled DB handle and the shared embedder instance (docs/design.md §6).
/// Constructed once per process (Axum) or per stdio invocation — passed explicitly, no globals.
pub struct Context {
    pub storage: Arc<Storage>,
    pub embedder: Arc<dyn Embedder>,
    embedding_job_notifier: Option<Arc<dyn EmbeddingJobNotifier>>,
}

impl Context {
    pub fn new(storage: Arc<Storage>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            storage,
            embedder,
            embedding_job_notifier: None,
        }
    }

    pub fn with_embedding_job_notifier(
        storage: Arc<Storage>,
        embedder: Arc<dyn Embedder>,
        embedding_job_notifier: Arc<dyn EmbeddingJobNotifier>,
    ) -> Self {
        Self {
            storage,
            embedder,
            embedding_job_notifier: Some(embedding_job_notifier),
        }
    }

    pub fn wake_embedding_jobs(&self) {
        if let Some(notifier) = &self.embedding_job_notifier {
            notifier.wake();
        }
    }
}
