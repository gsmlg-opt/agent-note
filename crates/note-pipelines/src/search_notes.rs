use crate::context::Context;
use note_core::{parse_label_selectors, weighted_rrf_fuse, NoteListItem, SearchResult};
use std::collections::HashSet;

const RRF_K: f32 = 60.0;
const TITLE_RRF_WEIGHT: f32 = 3.0;
const CONTENT_RRF_WEIGHT: f32 = 1.0;
const RETRIEVAL_OVERFETCH_FACTOR: usize = 32;
const MIN_RETRIEVAL_CANDIDATES: usize = 128;
const MAX_RETRIEVAL_CANDIDATES: usize = 4_096;

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
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let dense = ctx.embedder.embed(query).await?;

    // Use one session for filtering, retrieval, and note metadata collection.
    let session = ctx.storage().session().await?;
    let selectors = label
        .as_deref()
        .map(parse_label_selectors)
        .unwrap_or_default();
    let allowed_note_ids = if selectors.is_empty() {
        None
    } else {
        let ids = session
            .list_notes(&selectors, None, None)
            .await?
            .into_iter()
            .map(|note| note.id)
            .collect::<HashSet<_>>();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Some(ids)
    };
    let retrieval_limit = limit
        .saturating_mul(RETRIEVAL_OVERFETCH_FACTOR)
        .clamp(MIN_RETRIEVAL_CANDIDATES, MAX_RETRIEVAL_CANDIDATES);

    let title_ranking = session.title_search(query, retrieval_limit).await?;
    let dense_ranking = session.dense_search(&dense, retrieval_limit).await?;
    let fused = weighted_rrf_fuse(
        &[
            (TITLE_RRF_WEIGHT, title_ranking.as_slice()),
            (CONTENT_RRF_WEIGHT, dense_ranking.as_slice()),
        ],
        RRF_K,
    );

    let mut notes = vec![];
    for (note_id, score) in fused {
        if allowed_note_ids
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&note_id))
        {
            continue;
        }
        // A None here means a title/dense retrieval row outlived its note row; it would only occur
        // under index/note divergence (e.g. a future delete path with a bug).
        if let Some(note) = session.get_note(&note_id).await? {
            notes.push(SearchResult {
                note: NoteListItem {
                    id: note.id,
                    title: note.title,
                    labels: note.labels,
                    created_at: note.created_at,
                    updated_at: note.updated_at,
                    deleted_at: note.deleted_at,
                },
                score,
            });
            if notes.len() >= limit {
                break;
            }
        }
    }
    Ok(notes)
}
