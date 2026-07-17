use crate::connection::map_turso_error;
use crate::TursoSession;
use note_storage::{
    RetrievalRepository, StorageError, StorageErrorKind, StorageResult, EMBEDDING_DIMENSION,
};
use std::collections::HashSet;

const TITLE_FTS_CANDIDATE_SQL: &str = "SELECT id, fts_score(title, ?1) AS score
                                      FROM notes
                                      WHERE fts_match(title, ?1)
                                      ORDER BY score DESC
                                      LIMIT ?2";
const MAX_TITLE_FTS_CANDIDATES: usize = 4_096;

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
        let Some(query) = normalize_title_query(query) else {
            return Ok(Vec::new());
        };
        let requested_limit = title_candidate_limit(limit);
        let candidate_limit = checked_limit(requested_limit, "title search")?;
        let mut rows = self
            .connection
            .query(
                TITLE_FTS_CANDIDATE_SQL,
                turso::params![query, candidate_limit],
            )
            .await
            .map_err(|error| map_turso_error("query title search", error))?;
        let mut candidates = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read title search", error))?
        {
            let id = row
                .get::<String>(0)
                .map_err(|error| map_turso_error("decode title search note id", error))?;
            let score = row
                .get::<f64>(1)
                .map_err(|error| map_turso_error("decode title search score", error))?;
            candidates.push((id, score));
        }
        drop(rows);

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = (1..=candidates.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let active_sql =
            format!("SELECT id FROM notes WHERE deleted_at IS NULL AND id IN ({placeholders})");
        let params = candidates
            .iter()
            .map(|(id, _)| turso::Value::from(id.clone()))
            .collect::<Vec<_>>();
        let mut rows = self
            .connection
            .query(&active_sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query active title search candidates", error))?;
        let mut active_ids = HashSet::with_capacity(candidates.len());
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read active title search candidates", error))?
        {
            active_ids.insert(
                row.get::<String>(0).map_err(|error| {
                    map_turso_error("decode active title search note id", error)
                })?,
            );
        }

        candidates.retain(|(id, _)| active_ids.contains(id));
        candidates.sort_by(|(left_id, left_score), (right_id, right_score)| {
            right_score
                .total_cmp(left_score)
                .then_with(|| left_id.cmp(right_id))
        });
        Ok(candidates
            .into_iter()
            .take(requested_limit)
            .map(|(id, _)| id)
            .collect())
    }
}

fn normalize_title_query(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!(r#""{term}""#))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn title_candidate_limit(limit: usize) -> usize {
    limit.min(MAX_TITLE_FTS_CANDIDATES)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TursoStorage;

    #[test]
    fn title_candidate_limit_is_capped_at_storage_boundary() {
        assert_eq!(title_candidate_limit(17), 17);
        assert_eq!(title_candidate_limit(usize::MAX), 4_096);
    }

    #[tokio::test]
    async fn title_candidate_query_uses_bounded_ordered_fts_plan_without_sql_sorter() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TursoStorage::open(dir.path().join("plan.db"))
            .await
            .unwrap();
        let session = storage.connect().await.unwrap();
        let mut rows = session
            .connection
            .query(
                format!("EXPLAIN {TITLE_FTS_CANDIDATE_SQL}"),
                turso::params![r#""rust""#, 10_i64],
            )
            .await
            .unwrap();
        let mut plan = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            plan.push((
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
                row.get::<i64>(3).unwrap(),
                row.get::<i64>(4).unwrap(),
            ));
        }

        let query = plan
            .iter()
            .position(|(opcode, _, _, _)| opcode == "IndexMethodQuery")
            .unwrap_or_else(|| panic!("FTS plan contains IndexMethodQuery: {plan:#?}"));
        let pattern = plan[..query]
            .iter()
            .rev()
            .find_map(|(opcode, value, _, _)| (opcode == "Integer").then_some(*value))
            .expect("FTS plan loads a pattern index before IndexMethodQuery");

        assert_eq!(
            pattern, 1,
            "expected COMBINED_ORDERED_LIMIT FTS pattern 1, plan: {plan:#?}"
        );
        assert!(
            !plan.iter().any(|(opcode, _, _, _)| opcode == "SorterOpen"),
            "bounded FTS plan must not open a SQL sorter: {plan:#?}"
        );
    }
}
