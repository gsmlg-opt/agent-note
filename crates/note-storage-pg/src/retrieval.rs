use crate::connection::map_sqlx_error;
use crate::settings::embedding_fingerprint_on_connection;
use crate::PgSession;
use note_storage::{
    embedding_dimension_for_fingerprint, RetrievalRepository, StorageError, StorageErrorKind,
    StorageResult,
};
use pgvector::Vector;

const TITLE_SEARCH_SQL: &str = include_str!("title_search.sql");

#[async_trait::async_trait]
impl RetrievalRepository for PgSession {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        let fingerprint = embedding_fingerprint_on_connection(&mut connection).await?;
        let dimension = embedding_dimension_for_fingerprint(fingerprint.as_deref())?;
        validate_vector(embedding, dimension)?;
        let embedding = Vector::from(embedding.to_vec());
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

    async fn dense_search(
        &self,
        query: &[f32],
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>> {
        let allowed_note_ids = allowed_note_ids.map(|ids| ids.to_vec());
        if allowed_note_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(Vec::new());
        }
        let mut connection = self.connection().await?;
        let fingerprint = embedding_fingerprint_on_connection(&mut connection).await?;
        let dimension = embedding_dimension_for_fingerprint(fingerprint.as_deref())?;
        validate_vector(query, dimension)?;
        let limit = checked_limit(limit, "dense search")?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = Vector::from(query.to_vec());
        sqlx::query_scalar(
            "SELECT embedding.note_id
             FROM note_chunk_embeddings AS embedding
             JOIN notes ON notes.id = embedding.note_id
             WHERE notes.deleted_at IS NULL
               AND ($2::text[] IS NULL OR embedding.note_id = ANY($2))
             GROUP BY embedding.note_id
             ORDER BY MIN(embedding.embedding <=> $1) ASC, embedding.note_id ASC
             LIMIT $3",
        )
        .bind(query)
        .bind(allowed_note_ids)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query exact dense search", error))
    }

    async fn title_search(
        &self,
        query: &str,
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>> {
        let allowed_note_ids = allowed_note_ids.map(|ids| ids.to_vec());
        if allowed_note_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(Vec::new());
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(query) = normalize_title_query(query) else {
            return Ok(Vec::new());
        };
        let limit = checked_limit(limit, "title search")?;

        let mut connection = self.connection().await?;
        sqlx::query_scalar(TITLE_SEARCH_SQL)
            .bind(query)
            .bind(allowed_note_ids)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query title search", error))
    }
}

fn validate_vector(vector: &[f32], dimension: usize) -> StorageResult<()> {
    if vector.len() != dimension {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            format!(
                "embedding vector must contain exactly {dimension} components, got {}",
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
