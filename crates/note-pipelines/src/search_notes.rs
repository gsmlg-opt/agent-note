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
    let sparse_ranking = note_storage::sparse_postings_query(&conn, &token_ids, limit).await?;

    let fused = rrf_fuse(&[dense_ranking, sparse_ranking], RRF_K);

    let mut results = vec![];
    for (note_id, score) in fused.into_iter().take(limit) {
        if let Some(note) = note_storage::get_note(&conn, &note_id).await? {
            results.push(SearchResult { note, score });
        }
    }
    Ok(results)
}
