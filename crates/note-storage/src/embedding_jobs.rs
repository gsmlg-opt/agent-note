use libsql::Connection;

pub use crate::records::{
    EmbeddingDashboardStatus, EmbeddingJob, NoteChunk, ProcessingEmbeddingNote, UpsertNoteChunk,
};

pub async fn embedding_dashboard_status(
    conn: &Connection,
) -> anyhow::Result<EmbeddingDashboardStatus> {
    let mut count_rows = conn
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
        .await?;
    let embedded_note_count = count_rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?
        .unwrap_or(0)
        .max(0) as usize;

    let mut processing_rows = conn
        .query(
            "SELECT n.id, n.title
             FROM embedding_jobs j
             JOIN notes n ON n.id = j.note_id
             WHERE j.status = 'processing' AND n.deleted_at IS NULL
             ORDER BY j.updated_at, j.id
             LIMIT 1",
            (),
        )
        .await?;
    let processing_note = match processing_rows.next().await? {
        Some(row) => Some(ProcessingEmbeddingNote {
            id: row.get::<String>(0)?,
            title: row.get::<String>(1)?,
        }),
        None => None,
    };

    Ok(EmbeddingDashboardStatus {
        embedded_note_count,
        processing_note,
    })
}

pub async fn list_note_chunks(conn: &Connection, note_id: &str) -> anyhow::Result<Vec<NoteChunk>> {
    let mut rows = conn
        .query(
            "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
             FROM note_chunks
             WHERE note_id = ?1
             ORDER BY chunk_idx",
            libsql::params![note_id],
        )
        .await?;
    let mut chunks = Vec::new();
    while let Some(row) = rows.next().await? {
        chunks.push(NoteChunk {
            note_id: row.get::<String>(0)?,
            chunk_idx: row.get::<i64>(1)?,
            content_hash: row.get::<String>(2)?,
            content: row.get::<String>(3)?,
            note_revision: row.get::<i64>(4)?,
            status: row.get::<String>(5)?,
            updated_at: row.get::<i64>(6)?,
        });
    }
    Ok(chunks)
}

pub async fn get_note_chunk(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
) -> anyhow::Result<Option<NoteChunk>> {
    let mut rows = conn
        .query(
            "SELECT note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
             FROM note_chunks
             WHERE note_id = ?1 AND chunk_idx = ?2",
            libsql::params![note_id, chunk_idx],
        )
        .await?;
    let row = match rows.next().await? {
        Some(row) => row,
        None => return Ok(None),
    };
    Ok(Some(NoteChunk {
        note_id: row.get::<String>(0)?,
        chunk_idx: row.get::<i64>(1)?,
        content_hash: row.get::<String>(2)?,
        content: row.get::<String>(3)?,
        note_revision: row.get::<i64>(4)?,
        status: row.get::<String>(5)?,
        updated_at: row.get::<i64>(6)?,
    }))
}

pub async fn upsert_note_chunk(
    conn: &Connection,
    chunk: UpsertNoteChunk<'_>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO note_chunks (note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(note_id, chunk_idx) DO UPDATE SET
             chunk_hash = excluded.chunk_hash,
             content = excluded.content,
             note_revision = excluded.note_revision,
             status = excluded.status,
             updated_at = excluded.updated_at",
        libsql::params![
            chunk.note_id,
            chunk.chunk_idx,
            chunk.content_hash,
            chunk.content,
            chunk.note_revision,
            chunk.status,
            chunk.updated_at
        ],
    )
    .await?;
    Ok(())
}

pub async fn mark_note_chunk_status(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
    content_hash: &str,
    note_revision: i64,
    status: &str,
    updated_at: i64,
) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "UPDATE note_chunks
             SET status = ?4, updated_at = ?5
             WHERE note_id = ?1 AND chunk_idx = ?2 AND chunk_hash = ?3 AND note_revision = ?6",
            libsql::params![
                note_id,
                chunk_idx,
                content_hash,
                status,
                updated_at,
                note_revision
            ],
        )
        .await?;
    Ok(affected)
}

pub async fn delete_note_chunks_from(
    conn: &Connection,
    note_id: &str,
    min_chunk_idx: i64,
) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "DELETE FROM note_chunks WHERE note_id = ?1 AND chunk_idx >= ?2",
            libsql::params![note_id, min_chunk_idx],
        )
        .await?;
    Ok(affected)
}

pub async fn chunk_embedding_exists(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx = ?2 LIMIT 1",
            libsql::params![note_id, chunk_idx],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

pub async fn enqueue_embedding_job(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
    content_hash: &str,
    content: &str,
    note_revision: i64,
    now: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO embedding_jobs (
             note_id, chunk_idx, chunk_hash, content, note_revision, status, attempts, error, created_at, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, NULL, ?6, ?6)
         ON CONFLICT(note_id, chunk_idx, chunk_hash) DO UPDATE SET
             content = excluded.content,
             note_revision = excluded.note_revision,
             status = 'pending',
             error = NULL,
             updated_at = excluded.updated_at
         WHERE embedding_jobs.status != 'processing'",
        libsql::params![
            note_id,
            chunk_idx,
            content_hash,
            content,
            note_revision,
            now
        ],
    )
    .await?;
    Ok(())
}

pub async fn delete_stale_embedding_jobs_for_chunk(
    conn: &Connection,
    note_id: &str,
    chunk_idx: i64,
    current_hash: &str,
) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "DELETE FROM embedding_jobs
             WHERE note_id = ?1
               AND chunk_idx = ?2
               AND chunk_hash != ?3
               AND status != 'processing'",
            libsql::params![note_id, chunk_idx, current_hash],
        )
        .await?;
    Ok(affected)
}

pub async fn delete_embedding_jobs_from_chunk(
    conn: &Connection,
    note_id: &str,
    min_chunk_idx: i64,
) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "DELETE FROM embedding_jobs
             WHERE note_id = ?1 AND chunk_idx >= ?2 AND status != 'processing'",
            libsql::params![note_id, min_chunk_idx],
        )
        .await?;
    Ok(affected)
}

pub async fn claim_pending_embedding_jobs(
    conn: &Connection,
    limit: usize,
    now: i64,
) -> anyhow::Result<Vec<EmbeddingJob>> {
    let mut rows = conn
        .query(
            "SELECT id, note_id, chunk_idx, chunk_hash, content, note_revision, attempts
             FROM embedding_jobs
             WHERE status = 'pending'
             ORDER BY created_at, id
             LIMIT ?1",
            libsql::params![limit as i64],
        )
        .await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(EmbeddingJob {
            id: row.get::<i64>(0)?,
            note_id: row.get::<String>(1)?,
            chunk_idx: row.get::<i64>(2)?,
            content_hash: row.get::<String>(3)?,
            content: row.get::<String>(4)?,
            note_revision: row.get::<i64>(5)?,
            attempts: row.get::<i64>(6)?,
        });
    }
    drop(rows);

    let mut claimed = Vec::new();
    for mut job in candidates {
        let affected = conn
            .execute(
                "UPDATE embedding_jobs
                 SET status = 'processing',
                     attempts = attempts + 1,
                     error = NULL,
                     updated_at = ?2
                 WHERE id = ?1 AND status = 'pending'",
                libsql::params![job.id, now],
            )
            .await?;
        if affected > 0 {
            job.attempts += 1;
            claimed.push(job);
        }
    }

    Ok(claimed)
}

pub async fn delete_embedding_job(conn: &Connection, id: i64) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "DELETE FROM embedding_jobs WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    Ok(affected)
}

pub async fn fail_embedding_job(
    conn: &Connection,
    id: i64,
    attempts: i64,
    max_attempts: i64,
    error: &str,
    now: i64,
) -> anyhow::Result<u64> {
    let status = if attempts >= max_attempts {
        "failed"
    } else {
        "pending"
    };
    let affected = conn
        .execute(
            "UPDATE embedding_jobs
             SET status = ?2, error = ?3, updated_at = ?4
             WHERE id = ?1",
            libsql::params![id, status, error, now],
        )
        .await?;
    Ok(affected)
}

pub async fn requeue_processing_embedding_jobs(conn: &Connection, now: i64) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "UPDATE embedding_jobs
             SET status = 'pending', updated_at = ?1
             WHERE status = 'processing'",
            libsql::params![now],
        )
        .await?;
    Ok(affected)
}
