// crates/note-core/src/fusion.rs
use crate::types::NoteId;
use std::collections::HashMap;

/// Reciprocal Rank Fusion (docs/design.md §5): RRF(note) = Σ over retrievers r: 1 / (k + rank_r(note))
/// `rankings` is one ranked candidate list per retriever (e.g. dense, sparse); rank is 1-based position.
pub fn rrf_fuse(rankings: &[Vec<NoteId>], k: f32) -> Vec<(NoteId, f32)> {
    let mut scores: HashMap<NoteId, f32> = HashMap::new();
    for ranking in rankings {
        for (idx, note_id) in ranking.iter().enumerate() {
            let rank = (idx + 1) as f32;
            *scores.entry(note_id.clone()).or_insert(0.0) += 1.0 / (k + rank);
        }
    }
    let mut fused: Vec<(NoteId, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_ranking_uses_1_based_rank() {
        let ranking = vec!["a".to_string(), "b".to_string()];
        let result = rrf_fuse(&[ranking], 60.0);
        let get = |id: &str| result.iter().find(|(nid, _)| nid == id).unwrap().1;
        assert!((get("a") - 1.0 / 61.0).abs() < 1e-6);
        assert!((get("b") - 1.0 / 62.0).abs() < 1e-6);
    }

    #[test]
    fn combines_scores_across_rankings() {
        let dense = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let sparse = vec!["b".to_string(), "a".to_string(), "d".to_string()];
        let result = rrf_fuse(&[dense, sparse], 60.0);
        let get = |id: &str| result.iter().find(|(nid, _)| nid == id).unwrap().1;

        assert!((get("a") - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-6); // rank1 dense + rank2 sparse
        assert!((get("b") - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-6); // rank2 dense + rank1 sparse
        assert!((get("c") - (1.0 / 63.0)).abs() < 1e-6); // rank3 dense only
        assert!((get("d") - (1.0 / 63.0)).abs() < 1e-6); // rank3 sparse only
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn empty_rankings_produce_empty_result() {
        let result = rrf_fuse(&[], 60.0);
        assert!(result.is_empty());
    }

    #[test]
    fn result_is_sorted_descending_by_score() {
        let dense = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = rrf_fuse(&[dense], 60.0);
        for pair in result.windows(2) {
            assert!(pair[0].1 >= pair[1].1);
        }
    }
}
