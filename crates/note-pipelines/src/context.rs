use note_attachments::AttachmentStore;
use note_embedding::{Embedder, EmbeddingBackendInfo};
use note_storage::StorageBackend;
use std::sync::Arc;

pub trait EmbeddingJobNotifier: Send + Sync {
    fn wake(&self);
}

pub trait NoteMutationNotifier: Send + Sync {
    fn note_mutated(&self);
}

/// Shared storage and embedding dependencies constructed at the application composition root.
/// Passed explicitly through pipeline and transport layers; no globals.
pub struct Context {
    storage: Arc<dyn StorageBackend>,
    pub embedder: Arc<dyn Embedder>,
    embedding_info: EmbeddingBackendInfo,
    attachments: Arc<dyn AttachmentStore>,
    embedding_job_notifier: Option<Arc<dyn EmbeddingJobNotifier>>,
    note_mutation_notifier: Option<Arc<dyn NoteMutationNotifier>>,
}

impl Context {
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn Embedder>,
        attachments: Arc<dyn AttachmentStore>,
    ) -> Self {
        Self {
            storage,
            embedder,
            embedding_info: EmbeddingBackendInfo::local_bge_m3(),
            attachments,
            embedding_job_notifier: None,
            note_mutation_notifier: None,
        }
    }

    pub fn with_embedding_job_notifier(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn Embedder>,
        embedding_info: EmbeddingBackendInfo,
        embedding_job_notifier: Arc<dyn EmbeddingJobNotifier>,
        attachments: Arc<dyn AttachmentStore>,
    ) -> Self {
        Self {
            storage,
            embedder,
            embedding_info,
            attachments,
            embedding_job_notifier: Some(embedding_job_notifier),
            note_mutation_notifier: None,
        }
    }

    pub fn with_note_mutation_notifier(
        mut self,
        note_mutation_notifier: Arc<dyn NoteMutationNotifier>,
    ) -> Self {
        self.note_mutation_notifier = Some(note_mutation_notifier);
        self
    }

    pub(crate) fn storage(&self) -> &dyn StorageBackend {
        self.storage.as_ref()
    }

    pub fn attachments(&self) -> &dyn AttachmentStore {
        self.attachments.as_ref()
    }

    pub fn embedding_info(&self) -> &EmbeddingBackendInfo {
        &self.embedding_info
    }

    pub fn wake_embedding_jobs(&self) {
        if let Some(notifier) = &self.embedding_job_notifier {
            notifier.wake();
        }
    }

    pub fn notify_note_mutated(&self) {
        if let Some(notifier) = &self.note_mutation_notifier {
            notifier.note_mutated();
        }
    }
}
