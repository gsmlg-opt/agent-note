use crate::connection::map_turso_error;
use crate::TursoSession;
use note_storage::{
    RetrievalRepository, StorageError, StorageErrorKind, StorageResult, EMBEDDING_DIMENSION,
};

#[async_trait::async_trait]
impl RetrievalRepository for TursoSession {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()> {
        let json = vector_to_json(embedding)?;
        self.connection
            .execute(
                "INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding)
                 VALUES (?1, ?2, vector(?3))",
                turso::params![note_id, chunk_idx, json],
            )
            .await
            .map_err(|error| map_turso_error("insert chunk embedding", error))?;
        Ok(())
    }

    async fn dense_search(&self, query: &[f32], limit: usize) -> StorageResult<Vec<String>> {
        let json = vector_to_json(query)?;
        let limit = checked_limit(limit, "dense search")?;
        let mut rows = self
            .connection
            .query(
                "SELECT note_id
                 FROM note_chunk_embeddings
                 GROUP BY note_id
                 ORDER BY MIN(vector_distance_cos(embedding, vector(?1))) ASC, note_id ASC
                 LIMIT ?2",
                turso::params![json, limit],
            )
            .await
            .map_err(|error| map_turso_error("query exact dense search", error))?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read exact dense search", error))?
        {
            ids.push(
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode dense search note id", error))?,
            );
        }
        Ok(ids)
    }

    async fn title_search(&self, query: &str, limit: usize) -> StorageResult<Vec<String>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let limit = checked_limit(limit, "title search")?;
        let mut rows = self
            .connection
            .query(
                "SELECT id, fts_score(title, ?1) AS score
                 FROM notes
                 WHERE fts_match(title, ?1) AND deleted_at IS NULL
                 ORDER BY score DESC, id ASC
                 LIMIT ?2",
                turso::params![query, limit],
            )
            .await
            .map_err(|error| map_turso_error("query title search", error))?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read title search", error))?
        {
            ids.push(
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode title search note id", error))?,
            );
        }
        Ok(ids)
    }
}

fn vector_to_json(vector: &[f32]) -> StorageResult<String> {
    validate_vector(vector)?;
    let components = vector.iter().map(ToString::to_string).collect::<Vec<_>>();
    Ok(format!("[{}]", components.join(",")))
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

fn checked_limit(limit: usize, operation: &str) -> StorageResult<i64> {
    i64::try_from(limit).map_err(|_| {
        StorageError::new(
            StorageErrorKind::Operation,
            format!("{operation} limit exceeds the supported range"),
        )
    })
}
