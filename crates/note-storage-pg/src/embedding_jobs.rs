use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_storage::{
    EmbeddingDashboardStatus, EmbeddingJob, EmbeddingRepository, NoteChunk,
    ProcessingEmbeddingNote, StorageError, StorageErrorKind, StorageResult, UpsertNoteChunk,
};

#[async_trait::async_trait]
impl EmbeddingRepository for PgSession {
    async fn embedding_dashboard_status(&self) -> StorageResult<EmbeddingDashboardStatus> {
        let mut connection = self.connection().await?;
        let (embedded_note_count, processing_id, processing_title): (
            i64,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "WITH embedded AS (
                 SELECT COUNT(*) AS note_count
                 FROM notes n
                 WHERE n.deleted_at IS NULL
                   AND EXISTS (
                       SELECT 1 FROM note_chunks c
                       WHERE c.note_id = n.id AND c.note_revision = n.note_revision
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM note_chunks c
                       WHERE c.note_id = n.id
                         AND (c.note_revision != n.note_revision OR c.status != 'embedded')
                   )
             ),
             processing AS (
                 SELECT n.id, n.title
                 FROM embedding_jobs j
                 JOIN notes n ON n.id = j.note_id
                 WHERE j.status = 'processing' AND n.deleted_at IS NULL
                 ORDER BY j.updated_at, j.id
                 LIMIT 1
             )
             SELECT embedded.note_count, processing.id, processing.title
             FROM embedded
             LEFT JOIN processing ON TRUE",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query embedding dashboard status", error))?;
        let embedded_note_count = usize::try_from(embedded_note_count).map_err(|_| {
            StorageError::new(
                StorageErrorKind::Operation,
                "decode embedding dashboard count",
            )
        })?;
        let processing_note = match (processing_id, processing_title) {
            (Some(id), Some(title)) => Some(ProcessingEmbeddingNote { id, title }),
            (None, None) => None,
            _ => {
                return Err(StorageError::new(
                    StorageErrorKind::Operation,
                    "decode processing embedding note",
                ));
            }
        };

        Ok(EmbeddingDashboardStatus {
            embedded_note_count,
            processing_note,
        })
    }

    async fn list_note_chunks(&self, note_id: &str) -> StorageResult<Vec<NoteChunk>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, NoteChunkRow>(
            "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
             FROM note_chunks
             WHERE note_id = $1
             ORDER BY chunk_idx",
        )
        .bind(note_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note chunks", error))?;
        Ok(rows.into_iter().map(NoteChunkRow::into_chunk).collect())
    }

    async fn get_note_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<Option<NoteChunk>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, NoteChunkRow>(
            "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
             FROM note_chunks
             WHERE note_id = $1 AND chunk_idx = $2",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note chunk", error))
        .map(|row| row.map(NoteChunkRow::into_chunk))
    }

    async fn upsert_note_chunk(&self, chunk: UpsertNoteChunk<'_>) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO note_chunks (
                 note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (note_id, chunk_idx) DO UPDATE SET
                 chunk_hash = EXCLUDED.chunk_hash,
                 content = EXCLUDED.content,
                 note_revision = EXCLUDED.note_revision,
                 status = EXCLUDED.status,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(chunk.note_id)
        .bind(chunk.chunk_idx)
        .bind(chunk.content_hash)
        .bind(chunk.content)
        .bind(chunk.note_revision)
        .bind(chunk.status)
        .bind(chunk.updated_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("upsert note chunk", error))?;
        Ok(())
    }

    async fn mark_note_chunk_status(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        note_revision: i64,
        status: &str,
        updated_at: i64,
    ) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE note_chunks
             SET status = $4, updated_at = $5
             WHERE note_id = $1
               AND chunk_idx = $2
               AND chunk_hash = $3
               AND note_revision = $6",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .bind(content_hash)
        .bind(status)
        .bind(updated_at)
        .bind(note_revision)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("mark note chunk status", error))?;
        Ok(result.rows_affected())
    }

    async fn delete_note_chunks_from(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query("DELETE FROM note_chunks WHERE note_id = $1 AND chunk_idx >= $2")
            .bind(note_id)
            .bind(min_chunk_idx)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("delete trailing note chunks", error))?;
        Ok(result.rows_affected())
    }

    async fn chunk_embedding_exists(&self, note_id: &str, chunk_idx: i64) -> StorageResult<bool> {
        let mut connection = self.connection().await?;
        sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1
                 FROM note_chunk_embeddings
                 WHERE note_id = $1 AND chunk_idx = $2
             )",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query chunk embedding existence", error))
    }

    async fn enqueue_embedding_job(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        content: &str,
        note_revision: i64,
        now: i64,
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO embedding_jobs (
                 note_id, chunk_idx, chunk_hash, content, note_revision,
                 status, attempts, error, created_at, updated_at
             )
             VALUES ($1, $2, $3, $4, $5, 'pending', 0, NULL, $6, $6)
             ON CONFLICT (note_id, chunk_idx, chunk_hash) DO UPDATE SET
                 content = EXCLUDED.content,
                 note_revision = EXCLUDED.note_revision,
                 status = 'pending',
                 error = NULL,
                 updated_at = EXCLUDED.updated_at
             WHERE embedding_jobs.status <> 'processing'",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .bind(content_hash)
        .bind(content)
        .bind(note_revision)
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("enqueue embedding job", error))?;
        Ok(())
    }

    async fn delete_stale_embedding_jobs_for_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
        current_hash: &str,
    ) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "DELETE FROM embedding_jobs
             WHERE note_id = $1
               AND chunk_idx = $2
               AND chunk_hash <> $3
               AND status <> 'processing'",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .bind(current_hash)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("delete stale embedding jobs", error))?;
        Ok(result.rows_affected())
    }

    async fn delete_embedding_jobs_from_chunk(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "DELETE FROM embedding_jobs
             WHERE note_id = $1
               AND chunk_idx >= $2
               AND status <> 'processing'",
        )
        .bind(note_id)
        .bind(min_chunk_idx)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("delete trailing embedding jobs", error))?;
        Ok(result.rows_affected())
    }

    async fn claim_pending_embedding_jobs(
        &self,
        limit: usize,
        now: i64,
    ) -> StorageResult<Vec<EmbeddingJob>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = checked_limit(limit, "claim embedding jobs")?;
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, EmbeddingJobRow>(
            "WITH candidates AS MATERIALIZED (
                 SELECT id
                 FROM embedding_jobs
                 WHERE status = 'pending'
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT $1
             ),
             claimed AS (
                 UPDATE embedding_jobs AS job
                 SET status = 'processing',
                     attempts = job.attempts + 1,
                     error = NULL,
                     updated_at = $2
                 FROM candidates
                 WHERE job.id = candidates.id
                 RETURNING job.id, job.note_id, job.chunk_idx, job.chunk_hash,
                           job.content, job.note_revision, job.attempts, job.created_at
             )
             SELECT id, note_id, chunk_idx, chunk_hash, content, note_revision, attempts
             FROM claimed
             ORDER BY created_at, id",
        )
        .bind(limit)
        .bind(now)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("claim pending embedding jobs", error))?;
        Ok(rows.into_iter().map(EmbeddingJobRow::into_job).collect())
    }

    async fn delete_embedding_job(&self, id: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query("DELETE FROM embedding_jobs WHERE id = $1")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("delete embedding job", error))?;
        Ok(result.rows_affected())
    }

    async fn fail_embedding_job(
        &self,
        id: i64,
        attempts: i64,
        max_attempts: i64,
        error: &str,
        now: i64,
    ) -> StorageResult<u64> {
        let status = if attempts >= max_attempts {
            "failed"
        } else {
            "pending"
        };
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE embedding_jobs
             SET status = $2, error = $3, updated_at = $4
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|driver_error| map_sqlx_error("fail embedding job", driver_error))?;
        Ok(result.rows_affected())
    }

    async fn requeue_processing_embedding_jobs(&self, now: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE embedding_jobs
             SET status = 'pending', updated_at = $1
             WHERE status = 'processing'",
        )
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("requeue processing embedding jobs", error))?;
        Ok(result.rows_affected())
    }

    async fn reset_embeddings_for_regeneration(&self, now: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        sqlx::query("DELETE FROM note_chunk_embeddings")
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("clear chunk embeddings for regeneration", error))?;
        sqlx::query("DELETE FROM embedding_jobs")
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("clear embedding jobs for regeneration", error))?;
        sqlx::query(
            "UPDATE note_chunks AS chunks
             SET status = 'pending', updated_at = $1
             FROM notes
             WHERE notes.id = chunks.note_id
               AND notes.deleted_at IS NULL",
        )
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("mark active chunks pending for regeneration", error))?;
        let result = sqlx::query(
            "INSERT INTO embedding_jobs (
                 note_id, chunk_idx, chunk_hash, content, note_revision,
                 status, attempts, error, created_at, updated_at
             )
             SELECT
                 chunks.note_id, chunks.chunk_idx, chunks.chunk_hash, chunks.content,
                 chunks.note_revision, 'pending', 0, NULL, $1, $1
             FROM note_chunks AS chunks
             JOIN notes ON notes.id = chunks.note_id
             WHERE notes.deleted_at IS NULL",
        )
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("queue embeddings for regeneration", error))?;
        Ok(result.rows_affected())
    }
}

#[derive(sqlx::FromRow)]
struct NoteChunkRow {
    note_id: String,
    chunk_idx: i64,
    chunk_hash: String,
    content: String,
    note_revision: i64,
    status: String,
    updated_at: i64,
}

impl NoteChunkRow {
    fn into_chunk(self) -> NoteChunk {
        NoteChunk {
            note_id: self.note_id,
            chunk_idx: self.chunk_idx,
            content_hash: self.chunk_hash,
            content: self.content,
            note_revision: self.note_revision,
            status: self.status,
            updated_at: self.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobRow {
    id: i64,
    note_id: String,
    chunk_idx: i64,
    chunk_hash: String,
    content: String,
    note_revision: i64,
    attempts: i64,
}

impl EmbeddingJobRow {
    fn into_job(self) -> EmbeddingJob {
        EmbeddingJob {
            id: self.id,
            note_id: self.note_id,
            chunk_idx: self.chunk_idx,
            content_hash: self.chunk_hash,
            content: self.content,
            note_revision: self.note_revision,
            attempts: self.attempts,
        }
    }
}

fn checked_limit(limit: usize, operation: &str) -> StorageResult<i64> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorKind::Operation,
            format!("{operation} limit exceeds the supported range"),
        )
    })
}
