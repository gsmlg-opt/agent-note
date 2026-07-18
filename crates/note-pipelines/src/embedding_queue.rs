use crate::{chunk_content, Context};
use note_embedding::DenseVector;
use note_storage::{EmbeddingJob, TransactionMode};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write;

const CHUNK_STATUS_PENDING: &str = "pending";
const CHUNK_STATUS_EMBEDDED: &str = "embedded";
const CHUNK_STATUS_FAILED: &str = "failed";
const MAX_EMBEDDING_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEmbeddingJobStatus {
    Completed,
    Stale,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedEmbeddingJob {
    pub job_id: i64,
    pub note_id: String,
    pub chunk_idx: i64,
    pub status: ProcessEmbeddingJobStatus,
}

pub fn chunk_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("writing to String cannot fail");
    }
    out
}

pub async fn sync_note_embedding_jobs(
    storage: &dyn note_storage::StorageSession,
    note_id: &str,
    chunks: &[String],
    note_revision: i64,
    now: i64,
) -> anyhow::Result<usize> {
    let existing_chunks = storage.list_note_chunks(note_id).await?;
    let existing_by_idx: HashMap<i64, note_storage::NoteChunk> = existing_chunks
        .into_iter()
        .map(|chunk| (chunk.chunk_idx, chunk))
        .collect();

    let mut queued = 0;
    for (idx, content) in chunks.iter().enumerate() {
        let idx = idx as i64;
        let hash = chunk_hash(content);
        let existing = existing_by_idx.get(&idx);
        let current_hash_known = existing
            .map(|chunk| chunk.content_hash == hash)
            .unwrap_or(true);
        let has_embedding = storage.chunk_embedding_exists(note_id, idx).await?;

        storage
            .delete_stale_embedding_jobs_for_chunk(note_id, idx, &hash)
            .await?;

        if current_hash_known && has_embedding {
            storage
                .upsert_note_chunk(note_storage::UpsertNoteChunk {
                    note_id,
                    chunk_idx: idx,
                    content_hash: &hash,
                    content,
                    note_revision,
                    status: CHUNK_STATUS_EMBEDDED,
                    updated_at: now,
                })
                .await?;
            continue;
        }

        storage
            .upsert_note_chunk(note_storage::UpsertNoteChunk {
                note_id,
                chunk_idx: idx,
                content_hash: &hash,
                content,
                note_revision,
                status: CHUNK_STATUS_PENDING,
                updated_at: now,
            })
            .await?;
        storage.clear_note_chunk_derived(note_id, idx).await?;
        storage
            .enqueue_embedding_job(note_id, idx, &hash, content, note_revision, now)
            .await?;
        queued += 1;
    }

    let new_len = chunks.len() as i64;
    storage
        .clear_note_chunks_from_derived(note_id, new_len)
        .await?;
    storage
        .delete_embedding_jobs_from_chunk(note_id, new_len)
        .await?;
    storage.delete_note_chunks_from(note_id, new_len).await?;

    Ok(queued)
}

pub async fn enqueue_missing_chunk_embeddings(ctx: &Context) -> anyhow::Result<usize> {
    let session = ctx.storage().session().await?;
    let notes = session.list_active_note_sources().await?;
    drop(session);

    let mut queued = 0;
    for note in notes {
        let chunks = chunk_content(&note.content);
        let now = chrono::Utc::now().timestamp();
        let transaction = ctx.storage().begin(TransactionMode::Deferred).await?;
        let transaction_result = sync_note_embedding_jobs(
            transaction.as_ref(),
            &note.id,
            &chunks,
            note.note_revision,
            now,
        )
        .await;
        queued += crate::save_note::finish_transaction(transaction, transaction_result).await?;
    }

    Ok(queued)
}

pub async fn backfill_chunk_embeddings(ctx: &Context) -> anyhow::Result<usize> {
    enqueue_missing_chunk_embeddings(ctx).await
}

pub async fn requeue_processing_embedding_jobs(ctx: &Context) -> anyhow::Result<usize> {
    let session = ctx.storage().session().await?;
    let now = chrono::Utc::now().timestamp();
    Ok(session.requeue_processing_embedding_jobs(now).await? as usize)
}

pub async fn embedding_dashboard_status(
    ctx: &Context,
) -> anyhow::Result<note_storage::EmbeddingDashboardStatus> {
    let session = ctx.storage().session().await?;
    Ok(session.embedding_dashboard_status().await?)
}

pub async fn process_next_embedding_job(
    ctx: &Context,
) -> anyhow::Result<Option<ProcessedEmbeddingJob>> {
    let now = chrono::Utc::now().timestamp();
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        let mut jobs = transaction.claim_pending_embedding_jobs(1, now).await?;
        anyhow::Ok(jobs.pop())
    }
    .await;
    let job = crate::save_note::finish_transaction(transaction, transaction_result).await?;

    let Some(job) = job else {
        return Ok(None);
    };

    match ctx.embedder.embed(&job.content).await {
        Ok(dense) => complete_embedding_job(ctx, job, dense).await.map(Some),
        Err(error) => fail_embedding_job(ctx, job, &error.to_string())
            .await
            .map(Some),
    }
}

pub async fn drain_embedding_jobs(ctx: &Context, limit: usize) -> anyhow::Result<usize> {
    let mut processed = 0;
    while processed < limit {
        if process_next_embedding_job(ctx).await?.is_none() {
            break;
        }
        processed += 1;
    }
    Ok(processed)
}

async fn complete_embedding_job(
    ctx: &Context,
    job: EmbeddingJob,
    dense: DenseVector,
) -> anyhow::Result<ProcessedEmbeddingJob> {
    let now = chrono::Utc::now().timestamp();
    let transaction = ctx.storage().begin(TransactionMode::Deferred).await?;
    let transaction_result = async {
        let current = transaction
            .get_note_chunk(&job.note_id, job.chunk_idx)
            .await?;
        transaction.delete_embedding_job(job.id).await?;
        let status = if current.as_ref().is_some_and(|chunk| {
            chunk.content_hash == job.content_hash && chunk.note_revision == job.note_revision
        }) {
            transaction
                .clear_note_chunk_derived(&job.note_id, job.chunk_idx)
                .await?;
            transaction
                .insert_chunk_embedding(&job.note_id, job.chunk_idx, &dense)
                .await?;
            transaction
                .mark_note_chunk_status(
                    &job.note_id,
                    job.chunk_idx,
                    &job.content_hash,
                    job.note_revision,
                    CHUNK_STATUS_EMBEDDED,
                    now,
                )
                .await?;
            ProcessEmbeddingJobStatus::Completed
        } else {
            if let Some(current) = current {
                if current.content_hash == job.content_hash {
                    transaction
                        .enqueue_embedding_job(
                            &current.note_id,
                            current.chunk_idx,
                            &current.content_hash,
                            &current.content,
                            current.note_revision,
                            now,
                        )
                        .await?;
                }
            }
            ProcessEmbeddingJobStatus::Stale
        };

        anyhow::Ok(ProcessedEmbeddingJob {
            job_id: job.id,
            note_id: job.note_id.clone(),
            chunk_idx: job.chunk_idx,
            status,
        })
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await
}

async fn fail_embedding_job(
    ctx: &Context,
    job: EmbeddingJob,
    error: &str,
) -> anyhow::Result<ProcessedEmbeddingJob> {
    let now = chrono::Utc::now().timestamp();
    let transaction = ctx.storage().begin(TransactionMode::Deferred).await?;
    let transaction_result = async {
        transaction
            .fail_embedding_job(job.id, job.attempts, MAX_EMBEDDING_ATTEMPTS, error, now)
            .await?;
        if job.attempts >= MAX_EMBEDDING_ATTEMPTS {
            transaction
                .mark_note_chunk_status(
                    &job.note_id,
                    job.chunk_idx,
                    &job.content_hash,
                    job.note_revision,
                    CHUNK_STATUS_FAILED,
                    now,
                )
                .await?;
        }

        anyhow::Ok(ProcessedEmbeddingJob {
            job_id: job.id,
            note_id: job.note_id.clone(),
            chunk_idx: job.chunk_idx,
            status: ProcessEmbeddingJobStatus::Failed,
        })
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{save_note, update_note, SaveNoteInput};
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::{DenseVector, Embedder, StubEmbedder};
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use std::sync::Arc;
    use tokio::sync::Notify;

    struct PausingEmbedder {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl Embedder for PausingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<DenseVector> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(vec![0.1; 1024])
        }
    }

    #[tokio::test]
    async fn stale_revision_result_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let attachments = Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        ));
        let write_ctx = Context::new(storage.clone(), Arc::new(StubEmbedder), attachments.clone());
        let note = save_note(
            &write_ctx,
            SaveNoteInput {
                title: "original".into(),
                content: "old content".into(),
                attachments: vec![],
                labels: vec![],
            },
        )
        .await
        .unwrap();

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let process_ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(PausingEmbedder {
                started: started.clone(),
                release: release.clone(),
            }),
            attachments,
        ));
        let process_task = {
            let process_ctx = process_ctx.clone();
            tokio::spawn(async move {
                process_next_embedding_job(&process_ctx)
                    .await
                    .unwrap()
                    .unwrap()
            })
        };
        started.notified().await;

        update_note(
            &write_ctx,
            &note.id,
            SaveNoteInput {
                title: "updated".into(),
                content: "new content".into(),
                attachments: vec![],
                labels: vec![],
            },
        )
        .await
        .unwrap()
        .unwrap();
        release.notify_one();

        let processed = process_task.await.unwrap();
        assert_eq!(processed.status, ProcessEmbeddingJobStatus::Stale);

        let session = storage.session().await.unwrap();
        assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());
        assert!(session
            .dense_search(&vec![0.1; 1024], 10)
            .await
            .unwrap()
            .is_empty());
        let pending = session
            .claim_pending_embedding_jobs(10, chrono::Utc::now().timestamp())
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].note_id, note.id);
        assert_eq!(pending[0].content, "new content");
    }

    #[tokio::test]
    async fn same_content_new_revision_requeues_after_stale_result() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let attachments = Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        ));
        let write_ctx = Context::new(storage.clone(), Arc::new(StubEmbedder), attachments.clone());
        let note = save_note(
            &write_ctx,
            SaveNoteInput {
                title: "original".into(),
                content: "stable content".into(),
                attachments: vec![],
                labels: vec![("phase".into(), "draft".into())],
            },
        )
        .await
        .unwrap();

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let process_ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(PausingEmbedder {
                started: started.clone(),
                release: release.clone(),
            }),
            attachments,
        ));
        let process_task = {
            let process_ctx = process_ctx.clone();
            tokio::spawn(async move {
                process_next_embedding_job(&process_ctx)
                    .await
                    .unwrap()
                    .unwrap()
            })
        };
        started.notified().await;

        update_note(
            &write_ctx,
            &note.id,
            SaveNoteInput {
                title: "renamed".into(),
                content: "stable content".into(),
                attachments: vec![],
                labels: vec![("phase".into(), "published".into())],
            },
        )
        .await
        .unwrap()
        .unwrap();

        let session = storage.session().await.unwrap();
        assert_eq!(session.get_note_revision(&note.id).await.unwrap(), Some(2));
        let current_chunk = session.get_note_chunk(&note.id, 0).await.unwrap().unwrap();
        assert_eq!(current_chunk.note_revision, 2);
        assert_eq!(current_chunk.content_hash, chunk_hash("stable content"));
        drop(session);

        release.notify_one();
        let processed = process_task.await.unwrap();
        assert_eq!(processed.status, ProcessEmbeddingJobStatus::Stale);

        let replacement = process_next_embedding_job(&write_ctx)
            .await
            .unwrap()
            .expect("stale completion must leave a replacement job");
        assert_eq!(replacement.status, ProcessEmbeddingJobStatus::Completed);

        let session = storage.session().await.unwrap();
        assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());
    }
}
