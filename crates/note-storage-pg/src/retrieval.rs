use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_storage::{
    RetrievalRepository, StorageError, StorageErrorKind, StorageResult, EMBEDDING_DIMENSION,
};
use pgvector::Vector;

#[async_trait::async_trait]
impl RetrievalRepository for PgSession {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()> {
        validate_vector(embedding)?;
        let embedding = Vector::from(embedding.to_vec());
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding)
             VALUES ($1, $2, $3)
             ON CONFLICT (note_id, chunk_idx) DO UPDATE
             SET embedding = EXCLUDED.embedding",
        )
        .bind(note_id)
        .bind(chunk_idx)
        .bind(embedding)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert chunk embedding", error))?;
        Ok(())
    }

    async fn dense_search(&self, query: &[f32], limit: usize) -> StorageResult<Vec<String>> {
        validate_vector(query)?;
        let limit = checked_limit(limit, "dense search")?;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let query = Vector::from(query.to_vec());
        let mut connection = self.connection().await?;
        sqlx::query_scalar(
            "SELECT embedding.note_id
             FROM note_chunk_embeddings AS embedding
             JOIN notes ON notes.id = embedding.note_id
             WHERE notes.deleted_at IS NULL
             GROUP BY embedding.note_id
             ORDER BY MIN(embedding.embedding <=> $1) ASC, embedding.note_id ASC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query exact dense search", error))
    }

    async fn title_search(&self, query: &str, limit: usize) -> StorageResult<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(query) = normalize_title_query(query) else {
            return Ok(Vec::new());
        };
        let limit = checked_limit(limit, "title search")?;

        let mut connection = self.connection().await?;
        sqlx::query_scalar(
            "WITH query AS (
                 SELECT to_tsquery('simple'::regconfig, $1) AS terms
             )
             SELECT notes.id
             FROM notes
             CROSS JOIN query
             WHERE notes.deleted_at IS NULL
               AND notes.title_fts @@ query.terms
             ORDER BY ts_rank_cd(notes.title_fts, query.terms) DESC, notes.id ASC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query title search", error))
    }
}

fn validate_vector(vector: &[f32]) -> StorageResult<()> {
    if vector.len() != EMBEDDING_DIMENSION {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            format!(
                "embedding vector must contain exactly {EMBEDDING_DIMENSION} components, got {}",
                vector.len()
            ),
        ));
    }
    if vector.iter().any(|component| !component.is_finite()) {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "embedding vector components must all be finite",
        ));
    }
    Ok(())
}

fn normalize_title_query(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("'{term}'"))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" | "))
}

fn checked_limit(limit: usize, operation: &str) -> StorageResult<i64> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorKind::Operation,
            format!("{operation} limit exceeds the supported range"),
        )
    })
}
