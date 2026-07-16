use axum::{
    body::Body,
    extract::{Path, Query, State},
    response::{Html, Response},
    routing::{get as route_get, post},
    Json, Router,
};
use note_core::{Note, NoteAttachment, NoteListItem};
use note_pipelines::{
    count_notes, delete_note, embedding_dashboard_status, get_note, label_note_counts,
    list_deleted_note_summaries, list_label_keys, list_note_summaries, permanently_delete_note,
    restore_notes, save_note, search_notes_filtered, update_note, Context, ListNotesParams,
    SaveNoteInput,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

const DEFAULT_LIST_LIMIT: i64 = 10;
const MAX_LIST_LIMIT: i64 = 1000;
const DASHBOARD_CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
pub struct SaveNoteRequest {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<NoteAttachment>,
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
    #[serde(default)]
    pub attachment_base: Option<String>,
}

#[derive(Serialize)]
pub struct NoteDto {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Note> for NoteDto {
    fn from(note: Note) -> Self {
        Self {
            id: note.id,
            title: note.title,
            content: note.content,
            attachments: note.attachments,
            labels: note
                .labels
                .iter()
                .map(|label| (label.key.clone(), label.value.clone()))
                .collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
}

#[derive(Serialize)]
pub struct NoteListDto {
    pub id: String,
    pub title: String,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<NoteListItem> for NoteListDto {
    fn from(note: NoteListItem) -> Self {
        Self {
            id: note.id,
            title: note.title,
            labels: note
                .labels
                .iter()
                .map(|label| (label.key.clone(), label.value.clone()))
                .collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
}

#[derive(Serialize)]
pub struct TrashNoteDto {
    pub id: String,
    pub title: String,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: i64,
}

impl From<NoteListItem> for TrashNoteDto {
    fn from(note: NoteListItem) -> Self {
        Self {
            id: note.id,
            title: note.title,
            labels: note
                .labels
                .iter()
                .map(|label| (label.key.clone(), label.value.clone()))
                .collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            deleted_at: note
                .deleted_at
                .expect("trash queries only return deleted notes"),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct DashboardLabelDto {
    pub key: String,
    pub description: String,
    pub value_type: String,
    pub count: usize,
}

#[derive(Clone, Serialize)]
pub struct DashboardNoteDto {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Serialize)]
pub struct DashboardEmbeddingNoteDto {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Serialize)]
pub struct DashboardDto {
    pub note_count: usize,
    pub embedded_note_count: usize,
    pub embedding_note: Option<DashboardEmbeddingNoteDto>,
    pub label_count: usize,
    pub last_updated_at: Option<i64>,
    pub labels: Vec<DashboardLabelDto>,
    pub recent_updates: Vec<DashboardNoteDto>,
}

#[derive(Clone)]
struct DashboardCacheEntry {
    expires_at: Instant,
    value: DashboardDto,
}

static DASHBOARD_CACHE: OnceLock<RwLock<Option<DashboardCacheEntry>>> = OnceLock::new();

fn dashboard_cache() -> &'static RwLock<Option<DashboardCacheEntry>> {
    DASHBOARD_CACHE.get_or_init(|| RwLock::new(None))
}

pub(crate) fn invalidate_dashboard_cache() {
    if let Some(cache) = DASHBOARD_CACHE.get() {
        if let Ok(mut guard) = cache.try_write() {
            *guard = None;
        }
    }
}

async fn load_dashboard(ctx: &Context) -> anyhow::Result<DashboardDto> {
    let note_count = count_notes(ctx, None).await?;
    let labels = list_label_keys(ctx).await?;
    let label_count = labels.len();
    let label_counts = label_note_counts(ctx)
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();
    let mut labels = labels
        .into_iter()
        .map(|label| DashboardLabelDto {
            count: *label_counts.get(&label.key).unwrap_or(&0),
            key: label.key,
            description: label.description,
            value_type: label.value_type.as_str().to_string(),
        })
        .collect::<Vec<_>>();
    labels.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));

    let recent = list_note_summaries(
        ctx,
        ListNotesParams {
            limit: Some(5),
            offset: Some(0),
            label: None,
        },
    )
    .await?;
    let recent_updates = recent
        .into_iter()
        .map(|note| DashboardNoteDto {
            id: note.id,
            title: note.title,
            updated_at: note.updated_at,
        })
        .collect::<Vec<_>>();
    let last_updated_at = recent_updates.first().map(|note| note.updated_at);

    let mut dashboard = DashboardDto {
        note_count,
        embedded_note_count: 0,
        embedding_note: None,
        label_count,
        last_updated_at,
        labels,
        recent_updates,
    };
    refresh_dashboard_embedding_status(ctx, &mut dashboard).await?;
    Ok(dashboard)
}

async fn refresh_dashboard_embedding_status(
    ctx: &Context,
    dashboard: &mut DashboardDto,
) -> anyhow::Result<()> {
    let status = embedding_dashboard_status(ctx).await?;
    dashboard.embedded_note_count = status.embedded_note_count;
    dashboard.embedding_note = status
        .processing_note
        .map(|note| DashboardEmbeddingNoteDto {
            id: note.id,
            title: note.title,
        });
    Ok(())
}

async fn dashboard_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<DashboardDto>, (axum::http::StatusCode, String)> {
    let now = Instant::now();
    if let Some(cached) = dashboard_cache().read().await.clone() {
        if cached.expires_at > now {
            let mut value = cached.value;
            refresh_dashboard_embedding_status(&ctx, &mut value)
                .await
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Ok(Json(value));
        }
    }

    let value = load_dashboard(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *dashboard_cache().write().await = Some(DashboardCacheEntry {
        expires_at: Instant::now() + DASHBOARD_CACHE_TTL,
        value: value.clone(),
    });
    Ok(Json(value))
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
            attachments: req.attachments,
            labels: req.labels,
        },
    )
    .await
    .map_err(|e| {
        // Validation failures are the caller's fault (400); anything else (DB txn, insert,
        // enqueue) is an infra failure (500). save_note preserves the typed ValidationError
        // in the anyhow chain, so we downcast to tell them apart.
        let status = if e.downcast_ref::<note_core::DuplicateNoteError>().is_some() {
            axum::http::StatusCode::CONFLICT
        } else if e.downcast_ref::<note_core::ValidationError>().is_some() {
            axum::http::StatusCode::BAD_REQUEST
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;
    invalidate_dashboard_cache();
    Ok(Json(SaveNoteResponse { id: note.id }))
}

#[derive(Deserialize, Default)]
pub struct ListNotesQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub label: Option<String>,
}

fn normalized_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(0, MAX_LIST_LIMIT)
}

fn normalized_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

async fn list_notes_handler(
    State(ctx): State<Arc<Context>>,
    Query(req): Query<ListNotesQuery>,
) -> Result<Json<Vec<NoteListDto>>, (axum::http::StatusCode, String)> {
    let notes = list_note_summaries(
        &ctx,
        ListNotesParams {
            limit: Some(normalized_limit(req.limit)),
            offset: Some(normalized_offset(req.offset)),
            label: req.label,
        },
    )
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(notes.into_iter().map(Into::into).collect()))
}

#[derive(Serialize)]
pub struct CountNotesResponse {
    pub total: usize,
}

async fn count_notes_handler(
    State(ctx): State<Arc<Context>>,
    Query(req): Query<ListNotesQuery>,
) -> Result<Json<CountNotesResponse>, (axum::http::StatusCode, String)> {
    let total = count_notes(&ctx, req.label)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(CountNotesResponse { total }))
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

async fn get_attachment_handler(
    State(ctx): State<Arc<Context>>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Response, (axum::http::StatusCode, String)> {
    let note = get_note(&ctx, &id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                axum::http::StatusCode::NOT_FOUND,
                "note not found".to_string(),
            )
        })?;
    let requested_path = normalize_attachment_path(&path);
    let attachment = note
        .attachments
        .into_iter()
        .find(|attachment| normalize_attachment_path(&attachment.path) == requested_path)
        .ok_or_else(|| {
            (
                axum::http::StatusCode::NOT_FOUND,
                "attachment not found".to_string(),
            )
        })?;

    Response::builder()
        .header(axum::http::header::CONTENT_TYPE, attachment.mime)
        .body(Body::from(attachment.content))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn normalize_attachment_path(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
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
            attachments: req.attachments,
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
        Some(note) => {
            invalidate_dashboard_cache();
            Ok(Json(NoteDto::from(note)))
        }
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
        true => {
            invalidate_dashboard_cache();
            Ok(axum::http::StatusCode::NO_CONTENT)
        }
        false => Err((axum::http::StatusCode::NOT_FOUND, "note not found".into())),
    }
}

async fn list_deleted_notes_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<TrashNoteDto>>, (axum::http::StatusCode, String)> {
    let notes = list_deleted_note_summaries(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(notes.into_iter().map(TrashNoteDto::from).collect()))
}

#[derive(Deserialize)]
struct RestoreNotesRequest {
    ids: Vec<String>,
}

async fn restore_notes_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<RestoreNotesRequest>,
) -> Result<axum::http::StatusCode, (axum::http::StatusCode, String)> {
    if req.ids.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "at least one note id is required".into(),
        ));
    }
    match restore_notes(&ctx, &req.ids)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        true => {
            invalidate_dashboard_cache();
            Ok(axum::http::StatusCode::NO_CONTENT)
        }
        false => Err((
            axum::http::StatusCode::NOT_FOUND,
            "one or more notes were not found in Trash".into(),
        )),
    }
}

async fn permanently_delete_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, (axum::http::StatusCode, String)> {
    match permanently_delete_note(&ctx, &id)
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
    #[serde(default)]
    pub label: Option<String>,
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
    let results = search_notes_filtered(&ctx, &req.query, req.limit, req.label)
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
    let mut html = crate::render::render_markdown_html(&req.content);
    if let Some(base) = req.attachment_base {
        html = rewrite_relative_attachment_urls(&html, &base);
    }
    Html(html)
}

fn rewrite_relative_attachment_urls(html: &str, base: &str) -> String {
    let base = base.trim_end_matches('/');
    html.replace("href=\"./", &format!("href=\"{base}/"))
        .replace("src=\"./", &format!("src=\"{base}/"))
}

pub fn notes_router() -> Router<Arc<Context>> {
    Router::new()
        .route(
            "/api/notes",
            post(save_note_handler).get(list_notes_handler),
        )
        .route("/api/notes/count", route_get(count_notes_handler))
        .route("/api/trash", route_get(list_deleted_notes_handler))
        .route("/api/trash/restore", post(restore_notes_handler))
        .route(
            "/api/trash/{id}",
            axum::routing::delete(permanently_delete_note_handler),
        )
        .route("/api/dashboard", route_get(dashboard_handler))
        .route(
            "/api/notes/{id}",
            route_get(get_note_handler)
                .put(update_note_handler)
                .delete(delete_note_handler),
        )
        .route(
            "/api/notes/{id}/attachments/{*path}",
            route_get(get_attachment_handler),
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
        let ctx = Arc::new(Context::with_attachment_dir(
            Arc::new(storage),
            Arc::new(StubEmbedder),
            dir.path().join("attachments"),
        ));
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

    #[tokio::test]
    async fn duplicate_labels_return_conflict() {
        let (app, ctx, _dir) = test_app().await;
        note_pipelines::update_system_config(
            &ctx,
            &note_core::SystemConfig {
                duplicate_check: note_core::DuplicateCheckConfig {
                    enabled: true,
                    rules: vec![note_core::DuplicateCheckRule {
                        terms: vec![
                            note_core::DuplicateCheckTerm {
                                key: "skill-name".to_string(),
                                value: None,
                            },
                            note_core::DuplicateCheckTerm {
                                key: "version".to_string(),
                                value: None,
                            },
                        ],
                    }],
                },
            },
        )
        .await
        .unwrap();
        let body = r#"{"title":"T","content":"C","labels":[["skill-name","zddi-hooks"],["version","1.0.0"]]}"#;
        let first = app.clone().oneshot(post("/api/notes", body)).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let duplicate = app.oneshot(post("/api/notes", body)).await.unwrap();
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        let bytes = duplicate.into_body().collect().await.unwrap().to_bytes();
        let message = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(message.contains("skill-name=zddi-hooks + version=1.0.0"));
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
        assert!(notes[0].get("content").is_none());
    }

    #[tokio::test]
    async fn list_notes_defaults_to_ten_and_counts_total() {
        let (app, _ctx, _dir) = test_app().await;
        for idx in 0..12 {
            app.clone()
                .oneshot(post(
                    "/api/notes",
                    &format!(r#"{{"title":"N{idx}","content":"C{idx}","labels":[]}}"#),
                ))
                .await
                .unwrap();
        }

        let resp = app.clone().oneshot(get("/api/notes")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let notes: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(notes.len(), 10);
        assert!(notes.iter().all(|note| note.get("content").is_none()));

        let resp = app.oneshot(get("/api/notes/count")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("total").and_then(|v| v.as_u64()), Some(12));
    }

    #[tokio::test]
    async fn list_notes_clamps_limit_to_one_thousand() {
        let (app, ctx, _dir) = test_app().await;
        let conn = ctx.storage.connect().unwrap();
        for idx in 0..1002 {
            note_storage::insert_note(
                &conn,
                &format!("note-{idx}"),
                &format!("N{idx}"),
                &format!("C{idx}"),
                idx,
                idx,
                1,
            )
            .await
            .unwrap();
        }

        let resp = app.oneshot(get("/api/notes?limit=2000")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let notes: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(notes.len(), 1000);
    }

    #[tokio::test]
    async fn dashboard_uses_summary_data_and_invalidates_after_note_write() {
        invalidate_dashboard_cache();
        let (app, ctx, _dir) = test_app().await;

        let resp = app.clone().oneshot(get("/api/dashboard")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("note_count").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            json.get("embedded_note_count").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert!(json.get("embedding_note").unwrap().is_null());

        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Dashboard","content":"C","labels":[["type","note"]]}"#,
            ))
            .await
            .unwrap();

        let resp = app.clone().oneshot(get("/api/dashboard")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("note_count").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(json.get("label_count").and_then(|v| v.as_u64()), Some(1));
        let labels = json
            .get("labels")
            .and_then(|v| v.as_array())
            .expect("labels array");
        let label = labels
            .iter()
            .find(|label| label.get("key").and_then(|v| v.as_str()) == Some("type"))
            .expect("type label");
        assert_eq!(label.get("count").and_then(|v| v.as_u64()), Some(1));
        let recent = json
            .get("recent_updates")
            .and_then(|v| v.as_array())
            .expect("recent updates");
        assert_eq!(
            recent[0].get("title").and_then(|v| v.as_str()),
            Some("Dashboard")
        );

        let conn = ctx.storage.connect().unwrap();
        assert_eq!(
            note_storage::claim_pending_embedding_jobs(&conn, 1, 2000)
                .await
                .unwrap()
                .len(),
            1
        );
        let resp = app.clone().oneshot(get("/api/dashboard")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["embedding_note"]["title"].as_str(), Some("Dashboard"));
        assert_eq!(json["embedded_note_count"].as_u64(), Some(0));

        note_pipelines::requeue_processing_embedding_jobs(&ctx)
            .await
            .unwrap();
        assert_eq!(
            note_pipelines::drain_embedding_jobs(&ctx, 10)
                .await
                .unwrap(),
            1
        );
        let resp = app.oneshot(get("/api/dashboard")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["embedding_note"].is_null());
        assert_eq!(json["embedded_note_count"].as_u64(), Some(1));
        invalidate_dashboard_cache();
    }

    #[tokio::test]
    async fn list_notes_filters_by_multiple_labels() {
        let (app, _ctx, _dir) = test_app().await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Prod Tech","content":"C1","labels":[["type","tech"],["env","prod"]]}"#,
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Prod Note","content":"C2","labels":[["type","note"],["env","prod"]]}"#,
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(get("/api/notes?label=type%3Dtech%26env%3Dprod"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let notes = arr.as_array().expect("response is a JSON array");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes[0].get("title").and_then(|v| v.as_str()),
            Some("Prod Tech")
        );
    }

    #[tokio::test]
    async fn list_notes_filters_by_typed_comparison() {
        let (app, ctx, _dir) = test_app().await;
        note_pipelines::define_label_key_with_type(
            &ctx,
            "version",
            "Release version",
            note_core::LabelValueType::Version,
        )
        .await
        .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Old","content":"C1","labels":[["version","1.2.0"]]}"#,
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"New","content":"C2","labels":[["version","1.10.0"]]}"#,
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(get("/api/notes?label=version%3E%3D1.10.0"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let notes = arr.as_array().expect("response is a JSON array");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].get("title").and_then(|v| v.as_str()), Some("New"));
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
    async fn note_attachments_roundtrip_and_render_as_relative_files() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"With attachment","content":"See [meta](./meta.json)","attachments":[{"id":"meta","path":"./meta.json","mime":"application/json","description":"metadata","content":"{\"ok\":true}"}],"labels":[]}"#,
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
        let attachments = json
            .get("attachments")
            .and_then(|v| v.as_array())
            .expect("attachments array");
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0].get("description").and_then(|v| v.as_str()),
            Some("metadata")
        );

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}/attachments/meta.json")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], br#"{"ok":true}"#);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("attachments").join(&id).join("meta.json"))
                .unwrap(),
            r#"{"ok":true}"#
        );

        let render_body = format!(
            r#"{{"content":"See [meta](./meta.json)","attachment_base":"/api/notes/{id}/attachments"}}"#
        );
        let resp = app
            .oneshot(post("/api/render", &render_body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = resp.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&html).unwrap();
        assert!(html.contains(&format!(r#"href="/api/notes/{id}/attachments/meta.json""#)));
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
    async fn delete_note_by_id_lists_deleted_note_in_trash_or_404() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Delete Me","content":"C","attachments":[{"id":"proof","path":"./proof.txt","mime":"text/plain","description":"proof","content":"keep me"}],"labels":[["status","deleted"]]}"#,
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

        let resp = app.clone().oneshot(get("/api/notes")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let notes: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(notes.as_array().unwrap().len(), 0);

        let resp = app.clone().oneshot(get("/api/notes/count")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let count: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(count["total"], 0);

        assert_eq!(
            std::fs::read_to_string(dir.path().join("attachments").join(&id).join("proof.txt"))
                .unwrap(),
            "keep me"
        );

        let resp = app.clone().oneshot(get("/api/trash")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let deleted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let deleted = deleted.as_array().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0]["id"], id);
        assert_eq!(deleted[0]["title"], "Delete Me");
        assert_eq!(deleted[0]["labels"][0][0], "status");
        assert!(deleted[0]["deleted_at"].as_i64().is_some());

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"ids":["{id}"]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app.clone().oneshot(get("/api/trash")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let deleted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(deleted.as_array().unwrap().is_empty());
        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"ids":["{id}"]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/trash/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(!dir.path().join("attachments").join(&id).exists());

        let resp = app.clone().oneshot(get("/api/trash")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let deleted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(deleted.as_array().unwrap().is_empty());

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/trash/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app.oneshot(delete("/api/notes/nope")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn batch_restore_is_atomic_and_requeues_embeddings() {
        let (app, ctx, _dir) = test_app().await;
        let first = save_note_id(
            app.clone(),
            r#"{"title":"First","content":"First content","labels":[]}"#,
        )
        .await;
        let second = save_note_id(
            app.clone(),
            r#"{"title":"Second","content":"Second content","labels":[]}"#,
        )
        .await;
        for id in [&first, &second] {
            let resp = app
                .clone()
                .oneshot(delete(&format!("/api/notes/{id}")))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        }

        let resp = app
            .clone()
            .oneshot(post("/api/trash/restore", r#"{"ids":[]}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"ids":["{first}","missing"]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(note_pipelines::get_note(&ctx, &first)
            .await
            .unwrap()
            .is_none());

        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"ids":["{first}","{second}","{first}"]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = app.clone().oneshot(get("/api/notes/count")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let count: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(count["total"], 2);
        let resp = app.oneshot(get("/api/trash")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let deleted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(deleted.as_array().unwrap().is_empty());

        let conn = ctx.storage.connect().unwrap();
        let jobs = note_storage::claim_pending_embedding_jobs(&conn, 10, 3000)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().any(|job| job.note_id == first));
        assert!(jobs.iter().any(|job| job.note_id == second));
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

    #[tokio::test]
    async fn search_filters_by_label() {
        let (app, ctx, _dir) = test_app().await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Rust","content":"shared searchable content","labels":[["topic","rust"]]}"#,
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Ops","content":"shared searchable content","labels":[["topic","ops"]]}"#,
            ))
            .await
            .unwrap();
        note_pipelines::drain_embedding_jobs(&ctx, 10)
            .await
            .unwrap();

        let resp = app
            .oneshot(post(
                "/api/notes/search",
                r#"{"query":"shared searchable content","limit":5,"label":"topic=rust"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let hits = arr.as_array().expect("response is a JSON array");
        assert!(
            hits.iter()
                .any(|r| r.get("title").and_then(|v| v.as_str()) == Some("Rust")),
            "expected Rust in filtered results, got {arr}"
        );
        assert!(
            !hits
                .iter()
                .any(|r| r.get("title").and_then(|v| v.as_str()) == Some("Ops")),
            "expected Ops to be filtered out, got {arr}"
        );
    }

    #[tokio::test]
    async fn search_filters_by_typed_comparison() {
        let (app, ctx, _dir) = test_app().await;
        note_pipelines::define_label_key_with_type(
            &ctx,
            "priority",
            "Priority score",
            note_core::LabelValueType::Number,
        )
        .await
        .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Low","content":"shared searchable content","labels":[["priority","2"]]}"#,
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"High","content":"shared searchable content","labels":[["priority","10"]]}"#,
            ))
            .await
            .unwrap();
        note_pipelines::drain_embedding_jobs(&ctx, 10)
            .await
            .unwrap();

        let resp = app
            .oneshot(post(
                "/api/notes/search",
                r#"{"query":"shared searchable content","limit":5,"label":"priority>5"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let hits = arr.as_array().expect("response is a JSON array");
        assert!(
            hits.iter()
                .any(|r| r.get("title").and_then(|v| v.as_str()) == Some("High")),
            "expected High in filtered results, got {arr}"
        );
        assert!(
            !hits
                .iter()
                .any(|r| r.get("title").and_then(|v| v.as_str()) == Some("Low")),
            "expected Low to be filtered out, got {arr}"
        );
    }
}
