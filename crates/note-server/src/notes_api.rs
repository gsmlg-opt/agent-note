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
    .map_err(|e| {
        // Validation failures are the caller's fault (400); anything else (DB txn, insert,
        // embedder) is an infra failure (500). save_note preserves the typed ValidationError
        // in the anyhow chain, so we downcast to tell them apart.
        let status = if e.downcast_ref::<note_core::ValidationError>().is_some() {
            axum::http::StatusCode::BAD_REQUEST
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use note_embedding::StubEmbedder;
    use note_storage::Storage;
    use tower::ServiceExt;

    // Builds the real /api/notes router over a fresh temp DB + stub embedder so tests exercise the
    // actual HTTP surface (routing, JSON extractor, status codes, DTO serialization) via oneshot.
    async fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(StubEmbedder)));
        (notes_router().with_state(ctx), dir)
    }

    fn post(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn save_note_returns_200_with_id() {
        let (app, _dir) = test_app().await;
        let resp = app
            .oneshot(post("/api/notes", r#"{"title":"T","content":"C","labels":[]}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn empty_title_returns_400() {
        let (app, _dir) = test_app().await;
        let resp = app
            .oneshot(post("/api/notes", r#"{"title":"","content":"C","labels":[]}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // An unknown label key is auto-created on save (docs/design.md §6), so this succeeds (200)
    // rather than 400 — only genuinely invalid input (e.g. empty title) is a 400.
    #[tokio::test]
    async fn unknown_label_key_is_auto_created_returns_200() {
        let (app, _dir) = test_app().await;
        let resp = app
            .oneshot(post(
                "/api/notes",
                r#"{"title":"T","content":"C","labels":[["nope","v"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_returns_200_json_array() {
        let (app, _dir) = test_app().await;
        // Seed a note, then search for it. Router is Clone, so clone for the first request since
        // oneshot consumes the service.
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Find","content":"unique text","labels":[]}"#,
            ))
            .await
            .unwrap();
        let resp = app
            .oneshot(post(
                "/api/notes/search",
                r#"{"query":"unique text","limit":5}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // The StubEmbedder is deterministic and the query equals the seeded content, so the seeded
        // note must be found — assert a real hit so the seed step is load-bearing, not decorative.
        let hits = arr.as_array().expect("response is a JSON array");
        assert!(
            hits.iter().any(|r| r.get("title").and_then(|v| v.as_str()) == Some("Find")),
            "expected the seeded note in results, got {arr}"
        );
    }
}
