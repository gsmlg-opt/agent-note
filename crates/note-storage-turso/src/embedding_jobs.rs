use crate::connection::map_turso_error;
use crate::TursoSession;
use note_storage::{
    EmbeddingDashboardStatus, EmbeddingJob, EmbeddingRepository, NoteChunk,
    ProcessingEmbeddingNote, StorageError, StorageErrorKind, StorageResult, UpsertNoteChunk,
};

#[async_trait::async_trait]
impl EmbeddingRepository for TursoSession {
    async fn embedding_dashboard_status(&self) -> StorageResult<EmbeddingDashboardStatus> {
        let mut count_rows = self
            .connection
            .query(
                "SELECT COUNT(*)
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
                   )",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query embedding dashboard count", error))?;
        let embedded_note_count = count_rows
            .next()
            .await
            .map_err(|error| map_turso_error("read embedding dashboard count", error))?
            .map(|row| {
                row.get::<i64>(0)
                    .map_err(|error| map_turso_error("decode embedding dashboard count", error))
            })
            .transpose()?
            .unwrap_or(0)
            .max(0) as usize;
        drop(count_rows);

        let mut processing_rows = self
            .connection
            .query(
                "SELECT n.id, n.title
                 FROM embedding_jobs j
                 JOIN notes n ON n.id = j.note_id
                 WHERE j.status = 'processing' AND n.deleted_at IS NULL
                 ORDER BY j.updated_at, j.id
                 LIMIT 1",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query processing embedding note", error))?;
        let processing_note = match processing_rows
            .next()
            .await
            .map_err(|error| map_turso_error("read processing embedding note", error))?
        {
            Some(row) => Some(ProcessingEmbeddingNote {
                id: row
                    .get::<String>(0)
                    .map_err(|error| map_turso_error("decode processing note id", error))?,
                title: row
                    .get::<String>(1)
                    .map_err(|error| map_turso_error("decode processing note title", error))?,
            }),
            None => None,
        };

        Ok(EmbeddingDashboardStatus {
            embedded_note_count,
            processing_note,
        })
    }

    async fn list_note_chunks(&self, note_id: &str) -> StorageResult<Vec<NoteChunk>> {
        let mut rows = self
            .connection
            .query(
                "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
                 FROM note_chunks
                 WHERE note_id = ?1
                 ORDER BY chunk_idx",
                turso::params![note_id],
            )
            .await
            .map_err(|error| map_turso_error("query note chunks", error))?;
        let mut chunks = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note chunks", error))?
        {
            chunks.push(decode_note_chunk(&row)?);
        }
        Ok(chunks)
    }

    async fn get_note_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<Option<NoteChunk>> {
        let mut rows = self
            .connection
            .query(
                "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
                 FROM note_chunks
                 WHERE note_id = ?1 AND chunk_idx = ?2",
                turso::params![note_id, chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("query note chunk", error))?;
        rows.next()
            .await
            .map_err(|error| map_turso_error("read note chunk", error))?
            .map(|row| decode_note_chunk(&row))
            .transpose()
    }

    async fn upsert_note_chunk(&self, chunk: UpsertNoteChunk<'_>) -> StorageResult<()> {
        self.connection
            .execute(
                "INSERT INTO note_chunks (
                     note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(note_id, chunk_idx) DO UPDATE SET
                     chunk_hash = excluded.chunk_hash,
                     content = excluded.content,
                     note_revision = excluded.note_revision,
                     status = excluded.status,
                     updated_at = excluded.updated_at",
                turso::params![
                    chunk.note_id,
                    chunk.chunk_idx,
                    chunk.content_hash,
                    chunk.content,
                    chunk.note_revision,
                    chunk.status,
                    chunk.updated_at
                ],
            )
            .await
            .map_err(|error| map_turso_error("upsert note chunk", error))?;
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
        self.connection
            .execute(
                "UPDATE note_chunks
                 SET status = ?4, updated_at = ?5
                 WHERE note_id = ?1
                   AND chunk_idx = ?2
                   AND chunk_hash = ?3
                   AND note_revision = ?6",
                turso::params![
                    note_id,
                    chunk_idx,
                    content_hash,
                    status,
                    updated_at,
                    note_revision
                ],
            )
            .await
            .map_err(|error| map_turso_error("mark note chunk status", error))
    }

    async fn delete_note_chunks_from(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64> {
        self.connection
            .execute(
                "DELETE FROM note_chunks WHERE note_id = ?1 AND chunk_idx >= ?2",
                turso::params![note_id, min_chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("delete trailing note chunks", error))
    }

    async fn chunk_embedding_exists(&self, note_id: &str, chunk_idx: i64) -> StorageResult<bool> {
        let mut rows = self
            .connection
            .query(
                "SELECT 1
                 FROM note_chunk_embeddings
                 WHERE note_id = ?1 AND chunk_idx = ?2
                 LIMIT 1",
                turso::params![note_id, chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("query chunk embedding existence", error))?;
        rows.next()
            .await
            .map(|row| row.is_some())
            .map_err(|error| map_turso_error("read chunk embedding existence", error))
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
        self.connection
            .execute(
                "INSERT INTO embedding_jobs (
                     note_id, chunk_idx, chunk_hash, content, note_revision,
                     status, attempts, error, created_at, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, NULL, ?6, ?6)
                 ON CONFLICT(note_id, chunk_idx, chunk_hash) DO UPDATE SET
                     content = excluded.content,
                     note_revision = excluded.note_revision,
                     status = 'pending',
                     error = NULL,
                     updated_at = excluded.updated_at
                 WHERE embedding_jobs.status != 'processing'",
                turso::params![
                    note_id,
                    chunk_idx,
                    content_hash,
                    content,
                    note_revision,
                    now
                ],
            )
            .await
            .map_err(|error| map_turso_error("enqueue embedding job", error))?;
        Ok(())
    }

    async fn delete_stale_embedding_jobs_for_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
        current_hash: &str,
    ) -> StorageResult<u64> {
        self.connection
            .execute(
                "DELETE FROM embedding_jobs
                 WHERE note_id = ?1
                   AND chunk_idx = ?2
                   AND chunk_hash != ?3
                   AND status != 'processing'",
                turso::params![note_id, chunk_idx, current_hash],
            )
            .await
            .map_err(|error| map_turso_error("delete stale embedding jobs", error))
    }

    async fn delete_embedding_jobs_from_chunk(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64> {
        self.connection
            .execute(
                "DELETE FROM embedding_jobs
                 WHERE note_id = ?1
                   AND chunk_idx >= ?2
                   AND status != 'processing'",
                turso::params![note_id, min_chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("delete trailing embedding jobs", error))
    }

    async fn claim_pending_embedding_jobs(
        &self,
        limit: usize,
        now: i64,
    ) -> StorageResult<Vec<EmbeddingJob>> {
        let limit = checked_limit(limit, "claim embedding jobs")?;
        let mut rows = self
            .connection
            .query(
                "SELECT id, note_id, chunk_idx, chunk_hash, content, note_revision, attempts
                 FROM embedding_jobs
                 WHERE status = 'pending'
                 ORDER BY created_at, id
                 LIMIT ?1",
                turso::params![limit],
            )
            .await
            .map_err(|error| map_turso_error("query pending embedding jobs", error))?;
        let mut candidates = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read pending embedding jobs", error))?
        {
            candidates.push(decode_embedding_job(&row)?);
        }
        drop(rows);

        let mut claimed = Vec::new();
        for mut job in candidates {
            let affected = self
                .connection
                .execute(
                    "UPDATE embedding_jobs
                     SET status = 'processing',
                         attempts = attempts + 1,
                         error = NULL,
                         updated_at = ?2
                     WHERE id = ?1 AND status = 'pending'",
                    turso::params![job.id, now],
                )
                .await
                .map_err(|error| map_turso_error("claim pending embedding job", error))?;
            if affected > 0 {
                job.attempts += 1;
                claimed.push(job);
            }
        }
        Ok(claimed)
    }

    async fn delete_embedding_job(&self, id: i64) -> StorageResult<u64> {
        self.connection
            .execute(
                "DELETE FROM embedding_jobs WHERE id = ?1",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("delete embedding job", error))
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
        self.connection
            .execute(
                "UPDATE embedding_jobs
                 SET status = ?2, error = ?3, updated_at = ?4
                 WHERE id = ?1",
                turso::params![id, status, error, now],
            )
            .await
            .map_err(|driver_error| map_turso_error("fail embedding job", driver_error))
    }

    async fn requeue_processing_embedding_jobs(&self, now: i64) -> StorageResult<u64> {
        self.connection
            .execute(
                "UPDATE embedding_jobs
                 SET status = 'pending', updated_at = ?1
                 WHERE status = 'processing'",
                turso::params![now],
            )
            .await
            .map_err(|error| map_turso_error("requeue processing embedding jobs", error))
    }

    async fn reset_embeddings_for_regeneration(&self, now: i64) -> StorageResult<u64> {
        self.connection
            .execute("DELETE FROM note_chunk_embeddings", ())
            .await
            .map_err(|error| map_turso_error("clear chunk embeddings for regeneration", error))?;
        self.connection
            .execute("DELETE FROM embedding_jobs", ())
            .await
            .map_err(|error| map_turso_error("clear embedding jobs for regeneration", error))?;
        self.connection
            .execute(
                "UPDATE note_chunks
                 SET status = 'pending', updated_at = ?1
                 WHERE EXISTS (
                     SELECT 1 FROM notes
                     WHERE notes.id = note_chunks.note_id
                       AND notes.deleted_at IS NULL
                 )",
                turso::params![now],
            )
            .await
            .map_err(|error| {
                map_turso_error("mark active chunks pending for regeneration", error)
            })?;
        self.connection
            .execute(
                "INSERT INTO embedding_jobs (
                     note_id, chunk_idx, chunk_hash, content, note_revision,
                     status, attempts, error, created_at, updated_at
                 )
                 SELECT
                     chunks.note_id, chunks.chunk_idx, chunks.chunk_hash, chunks.content,
                     chunks.note_revision, 'pending', 0, NULL, ?1, ?1
                 FROM note_chunks AS chunks
                 JOIN notes ON notes.id = chunks.note_id
                 WHERE notes.deleted_at IS NULL",
                turso::params![now],
            )
            .await
            .map_err(|error| map_turso_error("queue embeddings for regeneration", error))
    }
}

fn decode_note_chunk(row: &turso::Row) -> StorageResult<NoteChunk> {
    Ok(NoteChunk {
        note_id: row
            .get::<String>(0)
            .map_err(|error| map_turso_error("decode note chunk note id", error))?,
        chunk_idx: row
            .get::<i64>(1)
            .map_err(|error| map_turso_error("decode note chunk index", error))?,
        content_hash: row
            .get::<String>(2)
            .map_err(|error| map_turso_error("decode note chunk hash", error))?,
        content: row
            .get::<String>(3)
            .map_err(|error| map_turso_error("decode note chunk content", error))?,
        note_revision: row
            .get::<i64>(4)
            .map_err(|error| map_turso_error("decode note chunk revision", error))?,
        status: row
            .get::<String>(5)
            .map_err(|error| map_turso_error("decode note chunk status", error))?,
        updated_at: row
            .get::<i64>(6)
            .map_err(|error| map_turso_error("decode note chunk update time", error))?,
    })
}

fn decode_embedding_job(row: &turso::Row) -> StorageResult<EmbeddingJob> {
    Ok(EmbeddingJob {
        id: row
            .get::<i64>(0)
            .map_err(|error| map_turso_error("decode embedding job id", error))?,
        note_id: row
            .get::<String>(1)
            .map_err(|error| map_turso_error("decode embedding job note id", error))?,
        chunk_idx: row
            .get::<i64>(2)
            .map_err(|error| map_turso_error("decode embedding job chunk index", error))?,
        content_hash: row
            .get::<String>(3)
            .map_err(|error| map_turso_error("decode embedding job hash", error))?,
        content: row
            .get::<String>(4)
            .map_err(|error| map_turso_error("decode embedding job content", error))?,
        note_revision: row
            .get::<i64>(5)
            .map_err(|error| map_turso_error("decode embedding job revision", error))?,
        attempts: row
            .get::<i64>(6)
            .map_err(|error| map_turso_error("decode embedding job attempts", error))?,
    })
}

fn checked_limit(limit: usize, operation: &str) -> StorageResult<i64> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorKind::Operation,
            format!("{operation} limit exceeds the supported range"),
        )
    })
}
