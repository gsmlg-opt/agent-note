use crate::{chunk_content, Context};
use note_embedding::{DenseVector, SparseVector};
use note_storage::EmbeddingJob;
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
    conn: &libsql::Connection,
    note_id: &str,
    chunks: &[String],
    now: i64,
) -> anyhow::Result<usize> {
    let existing_chunks = note_storage::list_note_chunks(conn, note_id).await?;
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
            .map(|chunk| chunk.chunk_hash == hash)
            .unwrap_or(true);
        let has_embedding = note_storage::chunk_embedding_exists(conn, note_id, idx).await?;

        note_storage::delete_stale_embedding_jobs_for_chunk(conn, note_id, idx, &hash).await?;

        if current_hash_known && has_embedding {
            note_storage::upsert_note_chunk(
                conn,
                note_id,
                idx,
                &hash,
                content,
                CHUNK_STATUS_EMBEDDED,
                now,
            )
            .await?;
            continue;
        }

        note_storage::upsert_note_chunk(
            conn,
            note_id,
            idx,
            &hash,
            content,
            CHUNK_STATUS_PENDING,
            now,
        )
        .await?;
        note_storage::clear_note_chunk_derived(conn, note_id, idx).await?;
        note_storage::enqueue_embedding_job(conn, note_id, idx, &hash, content, now).await?;
        queued += 1;
    }

    let new_len = chunks.len() as i64;
    note_storage::clear_note_chunks_from_derived(conn, note_id, new_len).await?;
    note_storage::delete_embedding_jobs_from_chunk(conn, note_id, new_len).await?;
    note_storage::delete_note_chunks_from(conn, note_id, new_len).await?;

    Ok(queued)
}

pub async fn enqueue_missing_chunk_embeddings(ctx: &Context) -> anyhow::Result<usize> {
    let conn = ctx.storage.connect()?;
    let mut rows = conn.query("SELECT id, content FROM notes", ()).await?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next().await? {
        notes.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(rows);

    let mut queued = 0;
    for (id, content) in notes {
        let chunks = chunk_content(&content);
        let now = chrono::Utc::now().timestamp();
        let tx = conn.transaction().await?;
        queued += sync_note_embedding_jobs(&tx, &id, &chunks, now).await?;
        tx.commit().await?;
    }

    Ok(queued)
}

pub async fn backfill_chunk_embeddings(ctx: &Context) -> anyhow::Result<usize> {
    enqueue_missing_chunk_embeddings(ctx).await
}

pub async fn requeue_processing_embedding_jobs(ctx: &Context) -> anyhow::Result<usize> {
    let conn = ctx.storage.connect()?;
    let now = chrono::Utc::now().timestamp();
    Ok(note_storage::requeue_processing_embedding_jobs(&conn, now).await? as usize)
}

pub async fn process_next_embedding_job(
    ctx: &Context,
) -> anyhow::Result<Option<ProcessedEmbeddingJob>> {
    let conn = ctx.storage.connect()?;
    let now = chrono::Utc::now().timestamp();
    let mut jobs = note_storage::claim_pending_embedding_jobs(&conn, 1, now).await?;
    drop(conn);

    let Some(job) = jobs.pop() else {
        return Ok(None);
    };

    match ctx.embedder.embed(&job.content).await {
        Ok((dense, sparse)) => complete_embedding_job(ctx, job, dense, sparse)
            .await
            .map(Some),
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
    sparse: SparseVector,
) -> anyhow::Result<ProcessedEmbeddingJob> {
    let weights: Vec<(i64, f64)> = sparse.into_iter().map(|(k, v)| (k, v as f64)).collect();
    let conn = ctx.storage.connect()?;
    let now = chrono::Utc::now().timestamp();
    let tx = conn.transaction().await?;

    let current = note_storage::get_note_chunk(&tx, &job.note_id, job.chunk_idx).await?;
    let status = if current.as_ref().map(|chunk| chunk.chunk_hash.as_str())
        == Some(job.chunk_hash.as_str())
    {
        note_storage::clear_note_chunk_derived(&tx, &job.note_id, job.chunk_idx).await?;
        note_storage::insert_chunk_embedding(&tx, &job.note_id, job.chunk_idx, &dense).await?;
        note_storage::insert_chunk_sparse_weights(&tx, &job.note_id, job.chunk_idx, &weights)
            .await?;
        note_storage::mark_note_chunk_status(
            &tx,
            &job.note_id,
            job.chunk_idx,
            &job.chunk_hash,
            CHUNK_STATUS_EMBEDDED,
            now,
        )
        .await?;
        ProcessEmbeddingJobStatus::Completed
    } else {
        ProcessEmbeddingJobStatus::Stale
    };

    note_storage::delete_embedding_job(&tx, job.id).await?;
    tx.commit().await?;

    Ok(ProcessedEmbeddingJob {
        job_id: job.id,
        note_id: job.note_id,
        chunk_idx: job.chunk_idx,
        status,
    })
}

async fn fail_embedding_job(
    ctx: &Context,
    job: EmbeddingJob,
    error: &str,
) -> anyhow::Result<ProcessedEmbeddingJob> {
    let conn = ctx.storage.connect()?;
    let now = chrono::Utc::now().timestamp();
    let tx = conn.transaction().await?;
    note_storage::fail_embedding_job(
        &tx,
        job.id,
        job.attempts,
        MAX_EMBEDDING_ATTEMPTS,
        error,
        now,
    )
    .await?;
    if job.attempts >= MAX_EMBEDDING_ATTEMPTS {
        note_storage::mark_note_chunk_status(
            &tx,
            &job.note_id,
            job.chunk_idx,
            &job.chunk_hash,
            CHUNK_STATUS_FAILED,
            now,
        )
        .await?;
    }
    tx.commit().await?;

    Ok(ProcessedEmbeddingJob {
        job_id: job.id,
        note_id: job.note_id,
        chunk_idx: job.chunk_idx,
        status: ProcessEmbeddingJobStatus::Failed,
    })
}
