// crates/note-core/src/fusion.rs
use crate::types::NoteId;
use std::collections::HashMap;

/// Weighted Reciprocal Rank Fusion:
/// score(note) = sum(weight / (k + one_based_rank)).
pub fn weighted_rrf_fuse(rankings: &[(f32, &[NoteId])], k: f32) -> Vec<(NoteId, f32)> {
    let mut scores: HashMap<NoteId, f32> = HashMap::new();
    for &(weight, ranking) in rankings {
        for (idx, note_id) in ranking.iter().enumerate() {
            let rank = (idx + 1) as f32;
            *scores.entry(note_id.clone()).or_insert(0.0) += weight / (k + rank);
        }
    }
    let mut fused: Vec<(NoteId, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_fusion_gives_title_three_times_the_same_rank_content_score() {
        let title = vec!["title-only".to_string()];
        let content = vec!["content-only".to_string()];
        let result = weighted_rrf_fuse(&[(3.0, title.as_slice()), (1.0, content.as_slice())], 60.0);
        let get = |id: &str| result.iter().find(|(nid, _)| nid == id).unwrap().1;

        assert_eq!(result[0].0, "title-only");
        assert!((get("title-only") - 3.0 / 61.0).abs() < 1e-6);
        assert!((get("content-only") - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn weighted_fusion_accumulates_scores_across_rankings() {
        let title = vec!["shared".to_string()];
        let content = vec!["content-only".to_string(), "shared".to_string()];
        let result = weighted_rrf_fuse(&[(3.0, title.as_slice()), (1.0, content.as_slice())], 60.0);
        let shared_score = result
            .iter()
            .find(|(note_id, _)| note_id == "shared")
            .unwrap()
            .1;

        assert!((shared_score - (3.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-6);
    }

    #[test]
    fn weighted_fusion_breaks_equal_scores_by_note_id() {
        let title = vec!["b".to_string()];
        let content = vec!["a".to_string()];
        let result = weighted_rrf_fuse(&[(1.0, title.as_slice()), (1.0, content.as_slice())], 60.0);
        let ids: Vec<&str> = result.iter().map(|(id, _)| id.as_str()).collect();

        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn empty_weighted_rankings_produce_empty_result() {
        assert!(weighted_rrf_fuse(&[], 60.0).is_empty());
    }
}
