use axum::{extract::State, routing::post, Json, Router};
use note_pipelines::{save_note, search_notes, Context, SaveNoteInput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SaveNoteRequest {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct SaveNoteResponse {
    pub id: String,
}

async fn save_note_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<SaveNoteRequest>,
) -> Result<Json<SaveNoteResponse>, (axum::http::StatusCode, String)> {
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: req.title,
            content: req.content,
            labels: req.labels,
        },
    )
    .await
    .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(SaveNoteResponse { id: note.id }))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct SearchResultDto {
    pub id: String,
    pub title: String,
    pub score: f32, // fused RRF score — label as such in any client UI, not "similarity" (docs/design.md §7)
}

async fn search_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<SearchQuery>,
) -> Result<Json<Vec<SearchResultDto>>, (axum::http::StatusCode, String)> {
    let results = search_notes(&ctx, &req.query, req.limit)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        results
            .into_iter()
            .map(|r| SearchResultDto {
                id: r.note.id,
                title: r.note.title,
                score: r.score,
            })
            .collect(),
    ))
}

pub fn notes_router() -> Router<Arc<Context>> {
    Router::new()
        .route("/api/notes", post(save_note_handler))
        // POST (not GET) because search takes a JSON body: browsers' Fetch API forbids a body on
        // GET, so the Wasm frontend (gloo-net) can't call a GET-with-body search. POST-with-body is
        // the standard pattern for structured search params.
        .route("/api/notes/search", post(search_handler))
}
