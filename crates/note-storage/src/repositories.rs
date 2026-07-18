use crate::{
    ActiveNoteSource, BackendInfo, EmbeddingDashboardStatus, EmbeddingJob, NewNote, NoteChunk,
    NoteUpdate, StorageResult, UpsertNoteChunk,
};

#[async_trait::async_trait]
pub trait NotesRepository: Send + Sync {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()>;
    async fn get_note_revision(&self, id: &str) -> StorageResult<Option<i64>>;
    async fn note_exists(&self, id: &str) -> StorageResult<bool>;
    async fn get_note(&self, id: &str) -> StorageResult<Option<note_core::Note>>;
    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>>;
    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<u64>;
    async fn soft_delete_note(&self, id: &str, deleted_at: i64) -> StorageResult<u64>;
    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>>;
    async fn restore_note(&self, id: &str, note_revision: i64) -> StorageResult<u64>;
    async fn permanently_delete_note(&self, id: &str) -> StorageResult<u64>;
    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>>;
    async fn clear_note_search_data(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_labels(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_chunk_derived(&self, id: &str, chunk_idx: i64) -> StorageResult<()>;
    async fn clear_note_chunks_from_derived(
        &self,
        id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<()>;
    async fn list_notes(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::Note>>;
    async fn list_all_notes(&self) -> StorageResult<Vec<note_core::Note>>;
    async fn list_note_summaries(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn list_deleted_note_summaries(&self) -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn count_notes(&self, selectors: &[note_core::LabelSelector]) -> StorageResult<usize>;
    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>>;
}

#[async_trait::async_trait]
pub trait LabelRepository: Send + Sync {
    async fn insert_label_key(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn insert_label_key_if_missing(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn insert_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn list_label_keys(&self) -> StorageResult<Vec<note_core::LabelKey>>;
    async fn update_label_key(&self, key: &str, description: &str) -> StorageResult<()>;
    async fn update_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn delete_label_key(&self, key: &str) -> StorageResult<()>;
    async fn attach_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<()>;
    async fn labels_for_note(&self, note_id: &str) -> StorageResult<Vec<note_core::Label>>;
    async fn label_note_counts(&self) -> StorageResult<Vec<(String, usize)>>;
    async fn find_note_with_labels(
        &self,
        labels: &[(String, String)],
    ) -> StorageResult<Option<String>>;
}

#[async_trait::async_trait]
pub trait EmbeddingRepository: Send + Sync {
    async fn embedding_dashboard_status(&self) -> StorageResult<EmbeddingDashboardStatus>;
    async fn list_note_chunks(&self, note_id: &str) -> StorageResult<Vec<NoteChunk>>;
    async fn get_note_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<Option<NoteChunk>>;
    async fn upsert_note_chunk(&self, chunk: UpsertNoteChunk<'_>) -> StorageResult<()>;
    async fn mark_note_chunk_status(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        note_revision: i64,
        status: &str,
        updated_at: i64,
    ) -> StorageResult<u64>;
    async fn delete_note_chunks_from(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn chunk_embedding_exists(&self, note_id: &str, chunk_idx: i64) -> StorageResult<bool>;
    async fn enqueue_embedding_job(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        content: &str,
        note_revision: i64,
        now: i64,
    ) -> StorageResult<()>;
    async fn delete_stale_embedding_jobs_for_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
        current_hash: &str,
    ) -> StorageResult<u64>;
    async fn delete_embedding_jobs_from_chunk(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn claim_pending_embedding_jobs(
        &self,
        limit: usize,
        now: i64,
    ) -> StorageResult<Vec<EmbeddingJob>>;
    async fn delete_embedding_job(&self, id: i64) -> StorageResult<u64>;
    async fn fail_embedding_job(
        &self,
        id: i64,
        attempts: i64,
        max_attempts: i64,
        error: &str,
        now: i64,
    ) -> StorageResult<u64>;
    async fn requeue_processing_embedding_jobs(&self, now: i64) -> StorageResult<u64>;
    /// Removes every derived vector, marks active chunks pending, replaces all
    /// embedding jobs with one pending job per active chunk, and returns the
    /// number of queued jobs. Call this inside a storage transaction together
    /// with `SettingsRepository::set_embedding_fingerprint`.
    async fn reset_embeddings_for_regeneration(&self, now: i64) -> StorageResult<u64>;
}

#[async_trait::async_trait]
pub trait RetrievalRepository: Send + Sync {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()>;
    async fn dense_search(&self, query: &[f32], limit: usize) -> StorageResult<Vec<String>>;
    async fn title_search(&self, query: &str, limit: usize) -> StorageResult<Vec<String>>;
}

#[async_trait::async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_system_config(&self) -> StorageResult<note_core::SystemConfig>;
    async fn set_system_config(&self, config: &note_core::SystemConfig) -> StorageResult<()>;
    async fn get_embedding_fingerprint(&self) -> StorageResult<Option<String>>;
    async fn set_embedding_fingerprint(&self, fingerprint: &str) -> StorageResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    Deferred,
    Immediate,
}

pub trait StorageSession:
    NotesRepository
    + LabelRepository
    + EmbeddingRepository
    + RetrievalRepository
    + SettingsRepository
    + Send
    + Sync
{
}

impl<T> StorageSession for T where
    T: NotesRepository
        + LabelRepository
        + EmbeddingRepository
        + RetrievalRepository
        + SettingsRepository
        + Send
        + Sync
{
}

#[async_trait::async_trait]
pub trait StorageTransaction: StorageSession {
    async fn commit(self: Box<Self>) -> StorageResult<()>;
    async fn rollback(self: Box<Self>) -> StorageResult<()>;
}

#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>>;
    async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>>;
    async fn info(&self) -> StorageResult<BackendInfo>;
}
