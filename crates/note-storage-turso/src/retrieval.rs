use crate::connection::map_turso_error;
use crate::TursoSession;
use note_storage::{
    RetrievalRepository, StorageError, StorageErrorKind, StorageResult, EMBEDDING_DIMENSION,
};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
};

const TITLE_FTS_CANDIDATE_SQL: &str = "SELECT id, fts_score(title, ?1) AS score
                                      FROM notes
                                      WHERE fts_match(title, ?1)
                                      ORDER BY score DESC
                                      LIMIT ?2";
const ALLOWED_TITLE_CANDIDATE_SQL: &str = "SELECT n.id, n.title
                                           FROM json_each(?1) AS allowed
                                           CROSS JOIN notes AS n
                                           WHERE n.id = CAST(allowed.value AS TEXT)
                                             AND n.deleted_at IS NULL";
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

    async fn dense_search(
        &self,
        query: &[f32],
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>> {
        if allowed_note_ids.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let json = vector_to_json(query)?;
        let limit = checked_limit(limit, "dense search")?;
        let allowed_note_ids = allowed_note_ids
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Operation,
                    "serialize dense search allowed note ids",
                    error,
                )
            })?;
        let mut rows = if let Some(allowed_note_ids) = allowed_note_ids {
            self.connection
                .query(
                    "SELECT e.note_id
                     FROM note_chunk_embeddings AS e
                     JOIN notes AS n ON n.id = e.note_id
                     WHERE n.deleted_at IS NULL
                       AND e.note_id IN (SELECT value FROM json_each(?2))
                     GROUP BY e.note_id
                     ORDER BY MIN(vector_distance_cos(e.embedding, vector(?1))) ASC, e.note_id ASC
                     LIMIT ?3",
                    turso::params![json, allowed_note_ids, limit],
                )
                .await
        } else {
            self.connection
                .query(
                    "SELECT e.note_id
                     FROM note_chunk_embeddings AS e
                     JOIN notes AS n ON n.id = e.note_id
                     WHERE n.deleted_at IS NULL
                     GROUP BY e.note_id
                     ORDER BY MIN(vector_distance_cos(e.embedding, vector(?1))) ASC, e.note_id ASC
                     LIMIT ?2",
                    turso::params![json, limit],
                )
                .await
        }
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

    async fn title_search(
        &self,
        query: &str,
        limit: usize,
        allowed_note_ids: Option<&[String]>,
    ) -> StorageResult<Vec<String>> {
        if allowed_note_ids.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if let Some(allowed_note_ids) = allowed_note_ids {
            // Pinned Turso 0.7 loses bounded FTS ORDER/LIMIT when an allowed-ID predicate is
            // added. Rank only the constrained collection by case-insensitive whole-token
            // frequency; the None path below retains the existing FTS/BM25 ranking.
            let query_terms = title_term_set(query);
            if query_terms.is_empty() {
                return Ok(Vec::new());
            }
            let allowed_note_ids = unique_allowed_note_ids(allowed_note_ids);
            let allowed_note_ids = serde_json::to_string(&allowed_note_ids).map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Operation,
                    "serialize title search allowed note ids",
                    error,
                )
            })?;
            let mut rows = self
                .connection
                .query(
                    ALLOWED_TITLE_CANDIDATE_SQL,
                    turso::params![allowed_note_ids],
                )
                .await
                .map_err(|error| map_turso_error("query allowed title search candidates", error))?;
            let retained_limit = limit.min(MAX_TITLE_FTS_CANDIDATES);
            let mut candidates = BinaryHeap::with_capacity(retained_limit);
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read allowed title search candidates", error))?
            {
                let id = row.get::<String>(0).map_err(|error| {
                    map_turso_error("decode allowed title search note id", error)
                })?;
                let title = row.get::<String>(1).map_err(|error| {
                    map_turso_error("decode allowed title search note title", error)
                })?;
                if let Some(candidate) = score_title_candidate(id, &title, &query_terms) {
                    retain_top_title_candidate(&mut candidates, candidate, retained_limit);
                }
            }
            return Ok(ranked_title_ids(candidates));
        }
        let Some(query) = normalize_title_query(query) else {
            return Ok(Vec::new());
        };
        let (requested_limit, physical_candidate_limit) = title_search_limits(limit);
        let candidate_limit = checked_limit(physical_candidate_limit, "title search")?;
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

#[derive(Debug, Eq, PartialEq)]
struct RankedTitleCandidate {
    id: String,
    score: usize,
}

impl RankedTitleCandidate {
    fn is_better_than(&self, other: &Self) -> bool {
        self.score > other.score || (self.score == other.score && self.id < other.id)
    }
}

impl Ord for RankedTitleCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl PartialOrd for RankedTitleCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn unique_allowed_note_ids(allowed_note_ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::with_capacity(allowed_note_ids.len());
    let mut unique = Vec::with_capacity(allowed_note_ids.len());
    for id in allowed_note_ids {
        if seen.insert(id.as_str()) {
            unique.push(id.as_str());
        }
    }
    unique
}

fn title_term_set(value: &str) -> HashSet<String> {
    title_terms(value).into_iter().collect()
}

fn title_terms(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn score_title_candidate(
    id: String,
    title: &str,
    query_terms: &HashSet<String>,
) -> Option<RankedTitleCandidate> {
    let score = title_terms(title)
        .iter()
        .filter(|term| query_terms.contains(*term))
        .count();
    (score > 0).then_some(RankedTitleCandidate { id, score })
}

fn retain_top_title_candidate(
    candidates: &mut BinaryHeap<RankedTitleCandidate>,
    candidate: RankedTitleCandidate,
    limit: usize,
) {
    debug_assert!(limit > 0);
    if candidates.len() < limit {
        candidates.push(candidate);
        return;
    }
    if candidates
        .peek()
        .is_some_and(|worst| candidate.is_better_than(worst))
    {
        *candidates.peek_mut().expect("nonempty candidate heap") = candidate;
    }
}

fn ranked_title_ids(candidates: BinaryHeap<RankedTitleCandidate>) -> Vec<String> {
    let mut candidates = candidates.into_vec();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
        .into_iter()
        .map(|candidate| candidate.id)
        .collect()
}

fn normalize_title_query(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!(r#""{term}""#))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn title_search_limits(limit: usize) -> (usize, usize) {
    (
        limit.min(MAX_TITLE_FTS_CANDIDATES),
        MAX_TITLE_FTS_CANDIDATES,
    )
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
    fn title_search_uses_fixed_physical_pool_and_capped_return_limit() {
        assert_eq!(title_search_limits(2), (2, 4_096));
        assert_eq!(title_search_limits(usize::MAX), (4_096, 4_096));
    }

    #[test]
    fn filtered_title_scoring_uses_case_insensitive_whole_token_frequency() {
        let query_terms = title_term_set("NeEdLe");
        let repeated = score_title_candidate(
            "repeated".into(),
            "NEEDLE, needle! pre-needle",
            &query_terms,
        )
        .unwrap();
        let single =
            score_title_candidate("single".into(), "A single Needle boundary", &query_terms)
                .unwrap();

        assert_eq!(repeated.score, 3);
        assert_eq!(single.score, 1);
        assert!(repeated.is_better_than(&single));
        assert!(score_title_candidate("substring".into(), "needles", &query_terms).is_none());
        assert!(score_title_candidate("zero".into(), "unrelated", &query_terms).is_none());
    }

    #[test]
    fn allowed_title_ids_are_deduplicated_in_input_order_before_serialization() {
        let ids = vec![
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "a".to_string(),
        ];
        let unique = unique_allowed_note_ids(&ids);

        assert_eq!(unique, vec!["b", "a", "c"]);
        assert_eq!(serde_json::to_string(&unique).unwrap(), r#"["b","a","c"]"#);
    }

    #[test]
    fn filtered_title_top_k_heap_stays_bounded_and_returns_exact_best_order() {
        let limit = 7;
        let mut heap = BinaryHeap::new();
        let mut expected = Vec::new();
        for index in 0..10_000 {
            let candidate = RankedTitleCandidate {
                id: format!("candidate-{index:05}"),
                score: index % 13,
            };
            expected.push((candidate.id.clone(), candidate.score));
            retain_top_title_candidate(&mut heap, candidate, limit);
            assert!(heap.len() <= limit);
        }
        expected.sort_by(|(left_id, left_score), (right_id, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_id.cmp(right_id))
        });
        expected.truncate(limit);

        assert_eq!(
            ranked_title_ids(heap),
            expected.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
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
                turso::params![r#""rust""#, MAX_TITLE_FTS_CANDIDATES as i64],
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

    #[tokio::test]
    async fn allowed_title_candidate_query_uses_json_ids_and_primary_key_lookups() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TursoStorage::open(dir.path().join("allowed-plan.db"))
            .await
            .unwrap();
        let session = storage.connect().await.unwrap();
        let mut rows = session
            .connection
            .query(
                format!("EXPLAIN {ALLOWED_TITLE_CANDIDATE_SQL}"),
                turso::params![r#"["allowed"]"#],
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

        assert!(
            !plan
                .iter()
                .any(|(opcode, _, _, _)| opcode == "IndexMethodQuery"),
            "allowed title lookup must not use the FTS index: {plan:#?}"
        );
        assert!(
            !plan.iter().any(|(opcode, _, _, _)| opcode == "SorterOpen"),
            "allowed title lookup must not open a SQL sorter: {plan:#?}"
        );
        assert!(
            !plan.iter().any(|(opcode, _, _, _)| opcode == "Rewind"),
            "allowed title lookup must not scan the notes table or ID index: {plan:#?}"
        );

        let json_scan = plan
            .iter()
            .position(|(opcode, _, _, _)| opcode == "VFilter")
            .unwrap_or_else(|| panic!("allowed title lookup scans json_each: {plan:#?}"));
        let cast = plan
            .iter()
            .position(|(opcode, _, _, _)| opcode == "Cast")
            .unwrap_or_else(|| panic!("allowed JSON values receive TEXT affinity: {plan:#?}"));
        let id_seek = plan
            .iter()
            .position(|(opcode, _, _, _)| opcode == "SeekGE")
            .unwrap_or_else(|| panic!("allowed title lookup seeks the notes ID index: {plan:#?}"));
        assert!(
            json_scan < cast && cast < id_seek,
            "json_each must drive notes ID index lookups: {plan:#?}"
        );
        assert!(
            plan.iter().any(|(opcode, _, _, _)| opcode == "IdxGT"),
            "allowed title lookup bounds each ID index seek: {plan:#?}"
        );
    }
}
