use axum::{
    extract::{Path, State},
    response::Html,
    routing::{get as route_get, post},
    Json, Router,
};
use note_core::Note;
use note_pipelines::{
    delete_note, get_note, list_notes, save_note, search_notes, update_note, Context,
    ListNotesParams, SaveNoteInput,
};
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

#[derive(Deserialize)]
pub struct RenderRequest {
    pub content: String,
}

#[derive(Serialize)]
pub struct NoteDto {
    pub id: String,
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
}

impl From<Note> for NoteDto {
    fn from(note: Note) -> Self {
        Self {
            id: note.id,
            title: note.title,
            content: note.content,
            labels: note
                .labels
                .iter()
                .map(|label| (label.key.clone(), label.value.clone()))
                .collect(),
            created_at: note.created_at,
        }
    }
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
        // enqueue) is an infra failure (500). save_note preserves the typed ValidationError
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

async fn list_notes_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<NoteDto>>, (axum::http::StatusCode, String)> {
    let notes = list_notes(&ctx, ListNotesParams::default())
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(notes.into_iter().map(Into::into).collect()))
}

async fn get_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<NoteDto>, (axum::http::StatusCode, String)> {
    match get_note(&ctx, &id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        Some(note) => Ok(Json(NoteDto::from(note))),
        None => Err((axum::http::StatusCode::NOT_FOUND, "note not found".into())),
    }
}

async fn update_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<SaveNoteRequest>,
) -> Result<Json<NoteDto>, (axum::http::StatusCode, String)> {
    let note = update_note(
        &ctx,
        &id,
        SaveNoteInput {
            title: req.title,
            content: req.content,
            labels: req.labels,
        },
    )
    .await
    .map_err(|e| {
        let status = if e.downcast_ref::<note_core::ValidationError>().is_some() {
            axum::http::StatusCode::BAD_REQUEST
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;

    match note {
        Some(note) => Ok(Json(NoteDto::from(note))),
        None => Err((axum::http::StatusCode::NOT_FOUND, "note not found".into())),
    }
}

async fn delete_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, (axum::http::StatusCode, String)> {
    match delete_note(&ctx, &id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        true => Ok(axum::http::StatusCode::NO_CONTENT),
        false => Err((axum::http::StatusCode::NOT_FOUND, "note not found".into())),
    }
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

async fn render_handler(Json(req): Json<RenderRequest>) -> Html<String> {
    Html(crate::render::render_markdown_html(&req.content))
}

pub fn notes_router() -> Router<Arc<Context>> {
    Router::new()
        .route(
            "/api/notes",
            post(save_note_handler).get(list_notes_handler),
        )
        .route(
            "/api/notes/{id}",
            route_get(get_note_handler)
                .put(update_note_handler)
                .delete(delete_note_handler),
        )
        // POST (not GET) because search takes a JSON body: browsers' Fetch API forbids a body on
        // GET, so the Wasm frontend (gloo-net) can't call a GET-with-body search. POST-with-body is
        // the standard pattern for structured search params.
        .route("/api/notes/search", post(search_handler))
        .route("/api/render", post(render_handler))
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
    async fn test_app() -> (Router, Arc<Context>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(StubEmbedder)));
        (notes_router().with_state(ctx.clone()), ctx, dir)
    }

    fn post(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn put(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn delete(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    async fn save_note_id(app: Router, body: &str) -> String {
        let resp = app.oneshot(post("/api/notes", body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json.get("id")
            .and_then(|v| v.as_str())
            .expect("response has id")
            .to_string()
    }

    #[tokio::test]
    async fn save_note_returns_200_with_id() {
        let (app, _ctx, _dir) = test_app().await;
        let resp = app
            .oneshot(post(
                "/api/notes",
                r#"{"title":"T","content":"C","labels":[]}"#,
            ))
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
        let (app, _ctx, _dir) = test_app().await;
        let resp = app
            .oneshot(post(
                "/api/notes",
                r#"{"title":"","content":"C","labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // An unknown label key is auto-created on save (docs/design.md §6), so this succeeds (200)
    // rather than 400 — only genuinely invalid input (e.g. empty title) is a 400.
    #[tokio::test]
    async fn unknown_label_key_is_auto_created_returns_200() {
        let (app, _ctx, _dir) = test_app().await;
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
    async fn list_notes_returns_newest_first() {
        let (app, _ctx, _dir) = test_app().await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"First","content":"C1","labels":[]}"#,
            ))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Second","content":"C2","labels":[]}"#,
            ))
            .await
            .unwrap();

        let resp = app.oneshot(get("/api/notes")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let notes = arr.as_array().expect("response is a JSON array");
        assert_eq!(notes.len(), 2);
        assert_eq!(
            notes[0].get("title").and_then(|v| v.as_str()),
            Some("Second")
        );
    }

    #[tokio::test]
    async fn get_note_by_id_returns_note_or_404() {
        let (app, _ctx, _dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Fetch Me","content":"C","labels":[]}"#,
        )
        .await;

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("title").and_then(|v| v.as_str()), Some("Fetch Me"));

        let resp = app.oneshot(get("/api/notes/does-not-exist")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_note_by_id_replaces_note_or_404() {
        let (app, _ctx, _dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Old","content":"Old content","labels":[]}"#,
        )
        .await;

        let resp = app
            .clone()
            .oneshot(put(
                &format!("/api/notes/{id}"),
                r#"{"title":"New","content":"New content","labels":[["topic","rust"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("title").and_then(|v| v.as_str()), Some("New"));

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("title").and_then(|v| v.as_str()), Some("New"));

        let resp = app
            .oneshot(put(
                "/api/notes/nope",
                r#"{"title":"New","content":"New content","labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_note_by_id_removes_note_or_404() {
        let (app, _ctx, _dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Delete Me","content":"C","labels":[]}"#,
        )
        .await;

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app.oneshot(delete("/api/notes/nope")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_returns_200_json_array() {
        let (app, ctx, _dir) = test_app().await;
        // Seed a note, then search for it. Router is Clone, so clone for the first request since
        // oneshot consumes the service.
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Find","content":"unique text","labels":[]}"#,
            ))
            .await
            .unwrap();
        note_pipelines::drain_embedding_jobs(&ctx, 10)
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
            hits.iter()
                .any(|r| r.get("title").and_then(|v| v.as_str()) == Some("Find")),
            "expected the seeded note in results, got {arr}"
        );
    }
}
