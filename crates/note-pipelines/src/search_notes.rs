use crate::context::Context;
use note_core::{rrf_fuse, SearchResult};

const RRF_K: f32 = 60.0; // docs/design.md §5 — tunable, revisit if fusion balance looks off

pub async fn search_notes(
    ctx: &Context,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let (dense, sparse) = ctx.embedder.embed(query).await?;
    let token_ids: Vec<i64> = sparse.keys().copied().collect();

    // Dense ANN and sparse postings are independent read-only queries. docs/design.md §6 calls
    // them "parallel", but we run them sequentially on one connection: a single libsql Connection
    // serializes work internally, so a true concurrent join would need two connections plus a
    // tokio runtime dependency this crate (like its sibling save_note) otherwise doesn't take.
    // The parallelism is a perf nice-to-have, not a correctness requirement — revisit if query
    // latency matters. Correctness (RRF-fused ranking) is unaffected by the ordering here.
    let conn = ctx.storage.connect()?;
    let dense_ranking = note_storage::dense_ann_query(&conn, &dense, limit).await?;
    // Guard the empty case: sparse_postings_query builds `WHERE token_id IN (...)`, which is invalid
    // SQL when there are no tokens. The current StubEmbedder never yields an empty sparse map, but the
    // Embedder contract doesn't guarantee it, so fall back to a dense-only ranking rather than error.
    let sparse_ranking = if token_ids.is_empty() {
        Vec::new()
    } else {
        note_storage::sparse_postings_query(&conn, &token_ids, limit).await?
    };

    let fused = rrf_fuse(&[dense_ranking, sparse_ranking], RRF_K);

    let mut results = vec![];
    for (note_id, score) in fused.into_iter().take(limit) {
        // A None here means an index row (dense/sparse) outlived its note row. save_note writes all
        // three tables in one atomic transaction (see save_note.rs), so this is unreachable today;
        // it would only occur under index/note divergence (e.g. a future delete path with a bug).
        if let Some(note) = note_storage::get_note(&conn, &note_id).await? {
            results.push(SearchResult { note, score });
        }
    }
    Ok(results)
}
