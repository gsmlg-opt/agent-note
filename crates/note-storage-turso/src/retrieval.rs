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

    async fn insert_chunk_sparse_weights(
        &self,
        note_id: &str,
        chunk_idx: i64,
        weights: &[(i64, f64)],
    ) -> StorageResult<()> {
        for (token_id, weight) in weights {
            self.connection
                .execute(
                    "INSERT INTO note_chunk_sparse (note_id, chunk_idx, token_id, weight)
                     VALUES (?1, ?2, ?3, ?4)",
                    turso::params![note_id, chunk_idx, *token_id, *weight],
                )
                .await
                .map_err(|error| map_turso_error("insert chunk sparse weight", error))?;
        }
        Ok(())
    }

    async fn sparse_postings_query(
        &self,
        token_ids: &[i64],
        limit: usize,
    ) -> StorageResult<Vec<String>> {
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = checked_limit(limit, "sparse postings query")?;
        let placeholders = (1..=token_ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>();
        let limit_param = token_ids.len() + 1;
        let sql = format!(
            "SELECT note_id, MAX(chunk_total) AS total FROM (
                 SELECT note_id, chunk_idx, SUM(weight) AS chunk_total
                 FROM note_chunk_sparse
                 WHERE token_id IN ({})
                 GROUP BY note_id, chunk_idx
             )
             GROUP BY note_id
             ORDER BY total DESC, note_id ASC
             LIMIT ?{limit_param}",
            placeholders.join(",")
        );
        let mut params = token_ids
            .iter()
            .copied()
            .map(turso::Value::from)
            .collect::<Vec<_>>();
        params.push(limit.into());

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query sparse postings", error))?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read sparse postings", error))?
        {
            ids.push(
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode sparse posting note id", error))?,
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
