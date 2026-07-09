use crate::context::Context;
use note_core::{parse_label_selectors, rrf_fuse, SearchResult};
use std::collections::HashSet;

const RRF_K: f32 = 60.0; // docs/design.md §5 — tunable, revisit if fusion balance looks off

pub async fn search_notes(
    ctx: &Context,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    search_notes_filtered(ctx, query, limit, None).await
}

pub async fn search_notes_filtered(
    ctx: &Context,
    query: &str,
    limit: usize,
    label: Option<String>,
) -> anyhow::Result<Vec<SearchResult>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let (dense, sparse) = ctx.embedder.embed(query).await?;
    let token_ids: Vec<i64> = sparse.keys().copied().collect();

    // Dense ANN and sparse postings are independent read-only queries. docs/design.md §6 calls
    // them "parallel", but we run them sequentially on one connection: a single libsql Connection
    // serializes work internally, so a true concurrent join would need two connections plus a
    // tokio runtime dependency this crate (like its sibling save_note) otherwise doesn't take.
    // The parallelism is a perf nice-to-have, not a correctness requirement — revisit if query
    // latency matters. Correctness (RRF-fused ranking) is unaffected by the ordering here.
    let conn = ctx.storage.connect()?;
    let selectors = label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    let allowed_note_ids = if selectors.is_empty() {
        None
    } else {
        let ids = note_storage::list_notes(&conn, &selectors, None, None)
            .await?
            .into_iter()
            .map(|note| note.id)
            .collect::<HashSet<_>>();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Some(ids)
    };
    let retrieval_limit = if allowed_note_ids.is_some() {
        limit.saturating_mul(32).max(128)
    } else {
        limit
    };

    let dense_ranking = note_storage::dense_ann_query(&conn, &dense, retrieval_limit).await?;
    // Guard the empty case: sparse_postings_query builds `WHERE token_id IN (...)`, which is invalid
    // SQL when there are no tokens. The current StubEmbedder never yields an empty sparse map, but the
    // Embedder contract doesn't guarantee it, so fall back to a dense-only ranking rather than error.
    let sparse_ranking = if token_ids.is_empty() {
        Vec::new()
    } else {
        note_storage::sparse_postings_query(&conn, &token_ids, retrieval_limit).await?
    };

    let fused = rrf_fuse(&[dense_ranking, sparse_ranking], RRF_K);

    let mut results = vec![];
    for (note_id, score) in fused {
        if allowed_note_ids
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&note_id))
        {
            continue;
        }
        // A None here means an index row (dense/sparse) outlived its note row. save_note writes all
        // three tables in one atomic transaction (see save_note.rs), so this is unreachable today;
        // it would only occur under index/note divergence (e.g. a future delete path with a bug).
        if let Some(note) = note_storage::get_note(&conn, &note_id).await? {
            results.push(SearchResult { note, score });
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}
