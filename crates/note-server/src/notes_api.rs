use axum::{
    body::Body,
    extract::{Path, Query, State},
    response::{Html, Response},
    routing::get as route_get,
    Json,
};
use note_core::{Note, NoteAttachment, NoteListItem};
use note_pipelines::{
    count_notes, delete_note, embedding_dashboard_status, get_note, get_note_attachment,
    get_note_markdown, label_note_counts, list_deleted_note_summaries, list_label_keys,
    list_note_summaries, permanently_delete_note, restore_notes, save_note, search_notes_filtered,
    update_note, Context, ListNotesParams, SaveNoteInput,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

const DEFAULT_LIST_LIMIT: i64 = 10;
const MAX_LIST_LIMIT: i64 = 1000;
const DASHBOARD_CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SaveNoteRequest {
    pub title: String,
    pub content: String,
    #[serde(default)]
    #[schema(default = json!([]))]
    pub attachments: Vec<AttachmentRequest>,
    #[serde(default)]
    #[schema(schema_with = crate::openapi::label_pairs_with_empty_default_schema)]
    pub labels: Vec<(String, String)>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[schema(
    description = "Each attachment request requires at least one of content or content_base64. If both are present, they must decode to identical bytes."
)]
pub struct AttachmentRequest {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[serde(default)]
    #[schema(default = "")]
    pub description: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    #[schema(
        content_encoding = "base64",
        content_media_type = "application/octet-stream"
    )]
    pub content_base64: Option<String>,
}

struct AttachmentRequestDocumentation;

impl utoipa::PartialSchema for AttachmentRequestDocumentation {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        let content_requirement = utoipa::openapi::schema::AnyOfBuilder::new()
            .item(
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::Object)
                    .property(
                        "content",
                        utoipa::openapi::schema::Object::with_type(
                            utoipa::openapi::schema::Type::String,
                        ),
                    )
                    .required("content"),
            )
            .item(
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::Object)
                    .property(
                        "content_base64",
                        utoipa::openapi::schema::Object::with_type(
                            utoipa::openapi::schema::Type::String,
                        ),
                    )
                    .required("content_base64"),
            );

        utoipa::openapi::schema::AllOfBuilder::new()
            .item(<AttachmentRequest as utoipa::PartialSchema>::schema())
            .item(content_requirement)
            .into()
    }
}

impl utoipa::ToSchema for AttachmentRequestDocumentation {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AttachmentRequest")
    }
}

#[derive(utoipa::ToSchema)]
#[schema(as = SaveNoteRequest)]
#[allow(dead_code)]
struct SaveNoteRequestDocumentation {
    title: String,
    content: String,
    #[schema(default = json!([]), required = false)]
    attachments: Vec<AttachmentRequestDocumentation>,
    #[schema(
        schema_with = crate::openapi::label_pairs_with_empty_default_schema,
        required = false
    )]
    labels: Vec<(String, String)>,
}

impl TryFrom<AttachmentRequest> for NoteAttachment {
    type Error = note_core::AttachmentContentError;

    fn try_from(attachment: AttachmentRequest) -> Result<Self, Self::Error> {
        let content = note_core::decode_attachment_content(
            attachment.content.as_deref(),
            attachment.content_base64.as_deref(),
        )?;
        Ok(Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content,
        })
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SaveNoteResponse {
    pub id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RenderRequest {
    pub content: String,
    #[serde(default)]
    pub attachment_base: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NoteDto {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<AttachmentResponse>,
    #[schema(schema_with = crate::openapi::label_pairs_schema)]
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AttachmentResponse {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[schema(default = "")]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[schema(
        content_encoding = "base64",
        content_media_type = "application/octet-stream"
    )]
    pub content_base64: String,
}

impl From<NoteAttachment> for AttachmentResponse {
    fn from(attachment: NoteAttachment) -> Self {
        let content = String::from_utf8(attachment.content.clone()).ok();
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content,
            content_base64: note_core::encode_attachment_content(&attachment.content),
        }
    }
}

impl From<Note> for NoteDto {
    fn from(note: Note) -> Self {
        Self {
            id: note.id,
            title: note.title,
            content: note.content,
            attachments: note.attachments.into_iter().map(Into::into).collect(),
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct NoteListDto {
    pub id: String,
    pub title: String,
    #[schema(schema_with = crate::openapi::label_pairs_schema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct TrashNoteDto {
    pub id: String,
    pub title: String,
    #[schema(schema_with = crate::openapi::label_pairs_schema)]
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

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct DashboardLabelDto {
    pub key: String,
    pub description: String,
    #[schema(schema_with = crate::openapi::label_value_type_schema)]
    pub value_type: String,
    pub count: usize,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct DashboardNoteDto {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct DashboardEmbeddingNoteDto {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
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

#[utoipa::path(
    get,
    path = "/api/dashboard",
    tag = "dashboard",
    responses(
        (status = 200, description = "Dashboard summary", body = DashboardDto),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    post,
    path = "/api/notes",
    tag = "notes",
    request_body = SaveNoteRequestDocumentation,
    responses(
        (status = 200, description = "Note saved", body = SaveNoteResponse),
        (status = 400, description = "Invalid note", body = String, content_type = "text/plain"),
        (status = 409, description = "Duplicate note", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn save_note_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<SaveNoteRequest>,
) -> Result<Json<SaveNoteResponse>, (axum::http::StatusCode, String)> {
    let attachments = decode_attachment_requests(req.attachments)?;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: req.title,
            content: req.content,
            attachments,
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

fn decode_attachment_requests(
    attachments: Vec<AttachmentRequest>,
) -> Result<Vec<NoteAttachment>, (axum::http::StatusCode, String)> {
    attachments
        .into_iter()
        .map(|attachment| {
            let id = attachment.id.clone();
            attachment.try_into().map_err(|error| {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("invalid attachment {id}: {error}"),
                )
            })
        })
        .collect()
}

#[derive(Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListNotesQuery {
    /// Maximum notes to return; omitted values are normalized to 10 and all values are clamped to 0..=1000.
    #[serde(default)]
    #[param(default = 10, minimum = 0, maximum = 1000)]
    pub limit: Option<i64>,
    /// Number of notes to skip; omitted and negative values are normalized to 0.
    #[serde(default)]
    #[param(default = 0, minimum = 0)]
    pub offset: Option<i64>,
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex).
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct CountNotesQuery {
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex).
    #[serde(default)]
    label: Option<String>,
    /// Accepted and validated for compatibility; values do not affect the count.
    #[serde(default)]
    limit: Option<i64>,
    /// Accepted and validated for compatibility; values do not affect the count.
    #[serde(default)]
    offset: Option<i64>,
}

fn normalized_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(0, MAX_LIST_LIMIT)
}

fn normalized_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

#[utoipa::path(
    get,
    path = "/api/notes",
    tag = "notes",
    params(ListNotesQuery),
    responses(
        (status = 200, description = "Notes", body = Vec<NoteListDto>),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct CountNotesResponse {
    pub total: usize,
}

#[utoipa::path(
    get,
    path = "/api/notes/count",
    tag = "notes",
    params(CountNotesQuery),
    responses(
        (status = 200, description = "Note count", body = CountNotesResponse),
        (status = 400, description = "Invalid compatibility query value", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn count_notes_handler(
    State(ctx): State<Arc<Context>>,
    Query(req): Query<ListNotesQuery>,
) -> Result<Json<CountNotesResponse>, (axum::http::StatusCode, String)> {
    let total = count_notes(&ctx, req.label)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(CountNotesResponse { total }))
}

#[utoipa::path(
    get,
    path = "/api/notes/{id}",
    tag = "notes",
    params(("id" = String, Path, description = "Note ID")),
    responses(
        (status = 200, description = "Note", body = NoteDto),
        (status = 404, description = "Note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    get,
    path = "/api/notes/{id}/raw",
    tag = "notes",
    params(
        ("id" = String, Path, description = "Note ID"),
        NoteContentQuery
    ),
    responses(
        (status = 200, description = "Raw Markdown or rendered HTML", content(
            (String = "text/markdown"),
            (String = "text/html")
        )),
        (status = 400, description = "Unsupported content type", body = String, content_type = "text/plain"),
        (status = 404, description = "Note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn get_note_raw_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Query(query): Query<NoteContentQuery>,
) -> Result<Response, (axum::http::StatusCode, String)> {
    let content = get_note_markdown(&ctx, &id)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                axum::http::StatusCode::NOT_FOUND,
                "note not found".to_string(),
            )
        })?;

    match query.output_type.as_deref() {
        None => markdown_response(content),
        Some("html") => {
            let attachment_base = format!("/api/notes/{id}/attachments");
            let mut content_html = yew_duskmoon::render_markdown_to_html_with_options(
                &content,
                yew_duskmoon::DmMarkdownOptions {
                    base_url: Some(attachment_base.clone()),
                    ..yew_duskmoon::DmMarkdownOptions::default()
                },
            );
            content_html = rewrite_relative_attachment_urls(&content_html, &attachment_base);
            let document = crate::render::render_embedded_markdown_document(&content_html);
            Response::builder()
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "text/html; charset=utf-8",
                )
                .header("x-content-type-options", "nosniff")
                .header(
                    "content-security-policy",
                    "default-src 'none'; img-src 'self' data: https:; style-src 'unsafe-inline'; frame-ancestors *; base-uri 'none'; form-action 'none'",
                )
                .body(Body::from(document))
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
        Some(output_type) => Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("unsupported content type: {output_type}"),
        )),
    }
}

#[derive(Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct NoteContentQuery {
    #[serde(rename = "type")]
    #[param(
        rename = "type",
        schema_with = crate::openapi::note_content_type_schema
    )]
    output_type: Option<String>,
}

#[utoipa::path(
    get,
    path = "/notes/{id}/content",
    tag = "notes",
    params(
        ("id" = String, Path, description = "Note ID"),
        NoteContentQuery
    ),
    responses(
        (status = 200, description = "Raw Markdown or rendered HTML", content(
            (String = "text/markdown"),
            (String = "text/html")
        )),
        (status = 400, description = "Unsupported content type", body = String, content_type = "text/plain"),
        (status = 404, description = "Note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn get_note_content_handler(
    state: State<Arc<Context>>,
    path: Path<String>,
    query: Query<NoteContentQuery>,
) -> Result<Response, (axum::http::StatusCode, String)> {
    get_note_raw_handler(state, path, query).await
}

fn markdown_response(content: String) -> Result<Response, (axum::http::StatusCode, String)> {
    Response::builder()
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )
        .header("x-content-type-options", "nosniff")
        .body(Body::from(content))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[utoipa::path(
    get,
    path = "/api/notes/{id}/attachments/{path}",
    tag = "notes",
    params(
        ("id" = String, Path, description = "Note ID"),
        ("path" = String, Path, description = "Attachment path")
    ),
    responses(
        (status = 200, description = "Attachment bytes using the stored MIME type", body = inline(crate::openapi::Binary), content_type = "*/*"),
        (status = 404, description = "Attachment not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn get_attachment_handler(
    State(ctx): State<Arc<Context>>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Response, (axum::http::StatusCode, String)> {
    let attachment = get_note_attachment(&ctx, &id, &path)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                axum::http::StatusCode::NOT_FOUND,
                "attachment not found".to_string(),
            )
        })?;

    Response::builder()
        .header(axum::http::header::CONTENT_TYPE, attachment.mime)
        .header("x-content-type-options", "nosniff")
        .body(Body::from(attachment.content))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[utoipa::path(
    put,
    path = "/api/notes/{id}",
    tag = "notes",
    params(("id" = String, Path, description = "Note ID")),
    request_body = SaveNoteRequestDocumentation,
    responses(
        (status = 200, description = "Updated note", body = NoteDto),
        (status = 400, description = "Invalid note", body = String, content_type = "text/plain"),
        (status = 404, description = "Note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn update_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<SaveNoteRequest>,
) -> Result<Json<NoteDto>, (axum::http::StatusCode, String)> {
    let attachments = decode_attachment_requests(req.attachments)?;
    let note = update_note(
        &ctx,
        &id,
        SaveNoteInput {
            title: req.title,
            content: req.content,
            attachments,
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

#[utoipa::path(
    delete,
    path = "/api/notes/{id}",
    tag = "notes",
    params(("id" = String, Path, description = "Note ID")),
    responses(
        (status = 204, description = "Note moved to Trash"),
        (status = 404, description = "Note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    get,
    path = "/api/trash",
    tag = "trash",
    responses(
        (status = 200, description = "Trashed notes", body = Vec<TrashNoteDto>),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn list_deleted_notes_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<TrashNoteDto>>, (axum::http::StatusCode, String)> {
    let notes = list_deleted_note_summaries(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(notes.into_iter().map(TrashNoteDto::from).collect()))
}

#[derive(Deserialize, utoipa::ToSchema)]
struct RestoreNotesRequest {
    #[schema(min_items = 1)]
    ids: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/api/trash/restore",
    tag = "trash",
    request_body = RestoreNotesRequest,
    responses(
        (status = 204, description = "Notes restored"),
        (status = 400, description = "Invalid restore request", body = String, content_type = "text/plain"),
        (status = 404, description = "Trashed note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    delete,
    path = "/api/trash/{id}",
    tag = "trash",
    params(("id" = String, Path, description = "Note ID")),
    responses(
        (status = 204, description = "Note permanently deleted"),
        (status = 404, description = "Trashed note not found", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex).
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchResultDto {
    pub id: String,
    pub title: String,
    pub score: f32, // fused RRF score — label as such in any client UI, not "similarity" (docs/design.md §7)
}

#[utoipa::path(
    post,
    path = "/api/notes/search",
    tag = "notes",
    request_body = SearchQuery,
    responses(
        (status = 200, description = "Search results", body = Vec<SearchResultDto>),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    post,
    path = "/api/render",
    tag = "rendering",
    request_body = RenderRequest,
    responses(
        (status = 200, description = "Rendered HTML", body = String, content_type = "text/html")
    )
)]
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

pub fn notes_router() -> OpenApiRouter<Arc<Context>> {
    #[derive(OpenApi)]
    #[openapi(paths(get_attachment_handler))]
    struct AttachmentOpenApi;

    OpenApiRouter::with_openapi(AttachmentOpenApi::openapi())
        .routes(routes!(save_note_handler, list_notes_handler))
        .routes(routes!(count_notes_handler))
        .routes(routes!(list_deleted_notes_handler))
        .routes(routes!(restore_notes_handler))
        .routes(routes!(permanently_delete_note_handler))
        .routes(routes!(dashboard_handler))
        .routes(routes!(
            get_note_handler,
            update_note_handler,
            delete_note_handler
        ))
        .routes(routes!(get_note_raw_handler))
        .routes(routes!(get_note_content_handler))
        .route(
            "/api/notes/{id}/attachments/{*path}",
            route_get(get_attachment_handler),
        )
        // POST (not GET) because search takes a JSON body: browsers' Fetch API forbids a body on
        // GET, so the Wasm frontend (gloo-net) can't call a GET-with-body search. POST-with-body is
        // the standard pattern for structured search params.
        .routes(routes!(search_handler))
        .routes(routes!(render_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, StatusCode};
    use axum::{body::Body, Router};
    use http_body_util::BodyExt;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::{StorageBackend, TransactionMode};
    use note_storage_turso::TursoStorage;
    use serde_json::Value;
    use tower::ServiceExt;

    fn note_openapi_document() -> Value {
        let (_, openapi) = notes_router().split_for_parts();
        serde_json::to_value(openapi).unwrap()
    }

    fn operation_parameter<'a>(operation: &'a Value, name: &str) -> &'a Value {
        operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap_or_else(|| panic!("missing parameter {name}"))
    }

    #[test]
    fn openapi_contains_all_note_operations() {
        let document = note_openapi_document();

        for (path, methods) in [
            ("/api/notes", &["get", "post"][..]),
            ("/api/notes/count", &["get"][..]),
            ("/api/notes/{id}", &["get", "put", "delete"][..]),
            ("/api/notes/{id}/raw", &["get"][..]),
            ("/notes/{id}/content", &["get"][..]),
            ("/api/notes/{id}/attachments/{path}", &["get"][..]),
            ("/api/notes/search", &["post"][..]),
            ("/api/render", &["post"][..]),
            ("/api/trash", &["get"][..]),
            ("/api/trash/restore", &["post"][..]),
            ("/api/trash/{id}", &["delete"][..]),
            ("/api/dashboard", &["get"][..]),
        ] {
            for method in methods {
                assert!(
                    document["paths"][path][method].is_object(),
                    "missing {method} {path}"
                );
            }
        }

        let raw_content =
            &document["paths"]["/api/notes/{id}/raw"]["get"]["responses"]["200"]["content"];
        assert_eq!(raw_content["text/markdown"]["schema"]["type"], "string");
        assert_eq!(raw_content["text/html"]["schema"]["type"], "string");

        let render_content =
            &document["paths"]["/api/render"]["post"]["responses"]["200"]["content"];
        assert_eq!(render_content["text/html"]["schema"]["type"], "string");

        let attachment_content = &document["paths"]["/api/notes/{id}/attachments/{path}"]["get"]
            ["responses"]["200"]["content"];
        assert_eq!(attachment_content["*/*"]["schema"]["type"], "string");
        assert_eq!(attachment_content["*/*"]["schema"]["format"], "binary");

        let plain_error =
            &document["paths"]["/api/notes"]["post"]["responses"]["400"]["content"]["text/plain"];
        assert_eq!(plain_error["schema"]["type"], "string");

        let empty_response = &document["paths"]["/api/notes/{id}"]["delete"]["responses"]["204"];
        assert!(
            empty_response.get("content").is_none(),
            "204 response must not publish a body"
        );
    }

    #[test]
    fn openapi_documents_list_and_count_queries() {
        let document = note_openapi_document();
        let list = &document["paths"]["/api/notes"]["get"];
        let limit = operation_parameter(list, "limit");
        assert_eq!(limit["required"], false);
        assert_eq!(limit["schema"]["default"], 10);
        assert_eq!(limit["schema"]["minimum"], 0);
        assert_eq!(limit["schema"]["maximum"], 1000);
        let limit_description = limit["description"].as_str().unwrap();
        assert!(limit_description.contains("normalized"));
        assert!(limit_description.contains("clamped"));

        let offset = operation_parameter(list, "offset");
        assert_eq!(offset["required"], false);
        assert_eq!(offset["schema"]["default"], 0);
        assert_eq!(offset["schema"]["minimum"], 0);
        assert!(offset["description"]
            .as_str()
            .unwrap()
            .contains("normalized"));

        assert_eq!(operation_parameter(list, "label")["required"], false);

        let count = &document["paths"]["/api/notes/count"]["get"];
        let count_parameters = count["parameters"].as_array().unwrap();
        assert_eq!(count_parameters.len(), 3);
        assert_eq!(operation_parameter(count, "label")["required"], false);
        for name in ["limit", "offset"] {
            let parameter = operation_parameter(count, name);
            assert_eq!(parameter["required"], false);
            let description = parameter["description"].as_str().unwrap();
            assert!(description.contains("compatibility"));
            assert!(description.contains("do not affect the count"));
        }

        let bad_query = &count["responses"]["400"]["content"]["text/plain"];
        assert_eq!(bad_query["schema"]["type"], "string");
    }

    #[test]
    fn openapi_documents_label_filter_operators() {
        let document = note_openapi_document();
        let list_label = operation_parameter(&document["paths"]["/api/notes"]["get"], "label")
            ["description"]
            .as_str()
            .unwrap_or_default();
        let count_label =
            operation_parameter(&document["paths"]["/api/notes/count"]["get"], "label")
                ["description"]
                .as_str()
                .unwrap_or_default();
        let search_label = document["components"]["schemas"]["SearchQuery"]["properties"]["label"]
            ["description"]
            .as_str()
            .unwrap_or_default();

        for description in [list_label, count_label, search_label] {
            for operator in ["^=", "$=", "~="] {
                assert!(description.contains(operator), "{description}");
            }
            assert!(description.contains("case-insensitive"), "{description}");
        }
    }

    #[test]
    fn openapi_documents_note_content_type_query() {
        let document = note_openapi_document();
        for path in ["/api/notes/{id}/raw", "/notes/{id}/content"] {
            let operation = &document["paths"][path]["get"];
            let content_type = operation_parameter(operation, "type");
            assert_eq!(content_type["in"], "query");
            assert_eq!(content_type["required"], false);
            assert_eq!(content_type["schema"]["enum"], serde_json::json!(["html"]));
            assert!(content_type["description"]
                .as_str()
                .or_else(|| content_type["schema"]["description"].as_str())
                .unwrap()
                .contains("omission returns Markdown"));
        }
    }

    #[test]
    fn openapi_requires_at_least_one_restore_id() {
        let document = note_openapi_document();
        assert_eq!(
            document["components"]["schemas"]["RestoreNotesRequest"]["properties"]["ids"]
                ["minItems"],
            1
        );
    }

    #[test]
    fn openapi_documents_attachment_transport_contract() {
        let document = note_openapi_document();
        let schemas = &document["components"]["schemas"];
        let request = &schemas["AttachmentRequest"];
        let request_properties = if request["properties"].is_object() {
            &request["properties"]
        } else {
            &request["allOf"][0]["properties"]
        };
        let response = &schemas["AttachmentResponse"];
        let save = &schemas["SaveNoteRequest"];

        assert_eq!(request_properties["description"]["default"], "");
        assert_eq!(response["properties"]["description"]["default"], "");
        assert_eq!(
            save["properties"]["attachments"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            save["properties"]["labels"]["default"],
            serde_json::json!([])
        );

        for properties in [request_properties, &response["properties"]] {
            let content_base64 = &properties["content_base64"];
            assert_eq!(content_base64["contentEncoding"], "base64");
            assert_eq!(
                content_base64["contentMediaType"],
                "application/octet-stream"
            );
        }

        assert_eq!(
            request["allOf"][1]["anyOf"],
            serde_json::json!([
                {
                    "type": "object",
                    "required": ["content"],
                    "properties": {"content": {"type": "string"}}
                },
                {
                    "type": "object",
                    "required": ["content_base64"],
                    "properties": {"content_base64": {"type": "string"}}
                }
            ])
        );

        let request_description = request["description"]
            .as_str()
            .or_else(|| request["allOf"][0]["description"].as_str())
            .unwrap();
        assert!(request_description.contains("at least one of content or content_base64"));
        assert!(request_description.contains("identical bytes"));
    }

    #[test]
    fn openapi_defaults_labels_only_on_save_requests() {
        let document = note_openapi_document();
        let schemas = &document["components"]["schemas"];

        assert_eq!(
            schemas["SaveNoteRequest"]["properties"]["labels"]["default"],
            serde_json::json!([])
        );
        for response in ["NoteDto", "NoteListDto", "TrashNoteDto"] {
            assert!(
                schemas[response]["properties"]["labels"]
                    .get("default")
                    .is_none(),
                "{response}.labels must not publish a default"
            );
        }
    }

    #[test]
    fn openapi_constrains_dashboard_label_value_types() {
        let document = note_openapi_document();
        assert_eq!(
            document["components"]["schemas"]["DashboardLabelDto"]["properties"]["value_type"]
                ["enum"],
            serde_json::json!(["text", "number", "version", "date", "datetime", "time"])
        );
    }

    // Builds the real /api/notes router over a fresh temp DB + stub embedder so tests exercise the
    // actual HTTP surface (routing, JSON extractor, status codes, DTO serialization) via oneshot.
    async fn test_app_with_backend() -> (
        Router,
        Arc<Context>,
        Arc<dyn StorageBackend>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(dir.path().join("t.db")).await.unwrap());
        let ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        (
            notes_router().with_state(ctx.clone()).into(),
            ctx,
            storage,
            dir,
        )
    }

    async fn test_app() -> (Router, Arc<Context>, tempfile::TempDir) {
        let (app, ctx, _storage, dir) = test_app_with_backend().await;
        (app, ctx, dir)
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
    async fn count_notes_rejects_invalid_pagination_query_values() {
        let (app, _ctx, _dir) = test_app().await;

        for uri in [
            "/api/notes/count?limit=invalid",
            "/api/notes/count?offset=invalid",
        ] {
            let response = app.clone().oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("text/plain; charset=utf-8"),
                "{uri}"
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let body = std::str::from_utf8(&body).unwrap();
            assert!(
                body.contains("Failed to deserialize query string"),
                "{uri}: {body}"
            );
        }
    }

    #[test]
    fn list_notes_clamps_limit_to_one_thousand() {
        assert_eq!(normalized_limit(Some(2000)), 1000);
    }

    #[tokio::test]
    async fn dashboard_uses_summary_data_and_invalidates_after_note_write() {
        invalidate_dashboard_cache();
        let (app, ctx, storage, _dir) = test_app_with_backend().await;

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

        let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
        assert_eq!(
            transaction
                .claim_pending_embedding_jobs(1, 2000)
                .await
                .unwrap()
                .len(),
            1
        );
        transaction.commit().await.unwrap();
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
    async fn get_note_raw_returns_exact_markdown_or_404() {
        let (app, _ctx, _dir) = test_app().await;
        let markdown = "# Raw Markdown\n\n<span style=\"color: #0065ff\">HTML</span>\n";
        let id = save_note_id(
            app.clone(),
            &serde_json::json!({
                "title": "Raw",
                "content": markdown,
                "labels": []
            })
            .to_string(),
        )
        .await;

        for uri in [
            format!("/api/notes/{id}/raw"),
            format!("/notes/{id}/content"),
        ] {
            let resp = app.clone().oneshot(get(&uri)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                resp.headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("text/markdown; charset=utf-8")
            );
            assert_eq!(
                resp.headers()
                    .get("x-content-type-options")
                    .and_then(|value| value.to_str().ok()),
                Some("nosniff")
            );
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&bytes[..], markdown.as_bytes());
        }

        let resp = app
            .oneshot(get("/notes/does-not-exist/content"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_note_content_html_returns_styled_embeddable_document() {
        let (app, _ctx, _dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            &serde_json::json!({
                "title": "Embedded",
                "content": "---\ntheme: zdns\n---\n\n# Embedded\n\n| Key | Value |\n| --- | --- |\n| color | `#0065ff` |\n\n![image](./image.png)\n\n<a id=\"raw\"></a>",
                "attachments": [{
                    "id": "image",
                    "path": "./image.png",
                    "mime": "image/png",
                    "content_base64": "iVBORw0KGgo="
                }],
                "labels": []
            })
            .to_string(),
        )
        .await;

        let resp = app
            .clone()
            .oneshot(get(&format!("/notes/{id}/content?type=html")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert!(resp
            .headers()
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("frame-ancestors *")));
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"));
        assert!(html.contains(".markdown-body table"));
        assert!(html.contains("<h1>Embedded</h1>"));
        assert!(html.contains("<table>"));
        assert!(html.contains(r#"class="dm-code-block""#));
        assert!(html.contains("theme: zdns"));
        assert!(html.contains(r#"class="dm-color-chip""#));
        assert!(html.contains("background-color:#0065ff;"));
        assert!(html.contains(&format!("src=\"/api/notes/{id}/attachments/image.png\"")));
        assert!(html.contains("<a id=\"raw\"></a>"));

        let resp = app
            .oneshot(get(&format!("/notes/{id}/content?type=pdf")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
        assert_eq!(
            attachments[0]
                .get("content_base64")
                .and_then(|v| v.as_str()),
            Some("eyJvayI6dHJ1ZX0=")
        );
        assert_eq!(
            attachments[0].get("content").and_then(|v| v.as_str()),
            Some(r#"{"ok":true}"#)
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
        assert_eq!(
            resp.headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
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
    async fn binary_attachments_roundtrip_as_base64_and_download_as_raw_bytes() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Binary","content":"![blob](./blob.bin)","attachments":[{"id":"blob","path":"./blob.bin","mime":"application/octet-stream","description":"raw bytes","content_base64":"AJ+Slv8="}],"labels":[]}"#,
        )
        .await;

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let attachment = &json["attachments"][0];
        assert_eq!(attachment["content_base64"], "AJ+Slv8=");
        assert!(attachment.get("content").is_none());

        json["title"] = serde_json::Value::String("Binary updated".into());
        let resp = app
            .clone()
            .oneshot(put(
                &format!("/api/notes/{id}"),
                &serde_json::to_string(&json).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(get(&format!("/api/notes/{id}/attachments/blob.bin")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/octet-stream")
        );
        assert_eq!(
            resp.headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], &[0, 0x9f, 0x92, 0x96, 0xff]);
        assert_eq!(
            std::fs::read(dir.path().join("attachments").join(&id).join("blob.bin")).unwrap(),
            [0, 0x9f, 0x92, 0x96, 0xff]
        );
    }

    #[tokio::test]
    async fn attachment_download_does_not_read_unrequested_siblings() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Two files","content":"C","attachments":[{"id":"good","path":"./good.txt","mime":"text/plain","content":"available"},{"id":"missing","path":"./missing.txt","mime":"text/plain","content":"remove me"}],"labels":[]}"#,
        )
        .await;
        std::fs::remove_file(dir.path().join("attachments").join(&id).join("missing.txt")).unwrap();

        let response = app
            .oneshot(get(&format!("/api/notes/{id}/attachments/good.txt")))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"available");
    }

    #[tokio::test]
    async fn invalid_or_ambiguous_attachment_content_is_rejected() {
        let (app, _ctx, dir) = test_app().await;
        let invalid = app
            .clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Invalid","content":"C","attachments":[{"id":"blob","path":"./blob.bin","mime":"application/octet-stream","content_base64":"not base64 !!"}],"labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let mismatched = app
            .clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Mismatch","content":"C","attachments":[{"id":"blob","path":"./blob.bin","mime":"application/octet-stream","content":"abc","content_base64":"eHl6"}],"labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);

        let resp = app.oneshot(get("/api/notes/count")).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let count: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(count["total"], 0);
        assert!(!dir.path().join("attachments").exists());
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
        let (app, ctx, storage, _dir) = test_app_with_backend().await;
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

        let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
        let jobs = transaction
            .claim_pending_embedding_jobs(10, 3000)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
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
    async fn search_prefers_a_title_match_over_an_exact_content_match() {
        let (app, ctx, _dir) = test_app().await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Needle handbook","content":"Alpha unrelated body","labels":[]}"#,
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Other handbook","content":"Needle","labels":[]}"#,
            ))
            .await
            .unwrap();
        note_pipelines::drain_embedding_jobs(&ctx, 10)
            .await
            .unwrap();
        let response = app
            .oneshot(post("/api/notes/search", r#"{"query":"Needle","limit":1}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let results: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(results[0]["title"], "Needle handbook");
    }

    #[tokio::test]
    async fn search_treats_malformed_fts_syntax_as_literal_title_text() {
        let (app, _ctx, _dir) = test_app().await;
        app.clone()
            .oneshot(post(
                "/api/notes",
                r#"{"title":"Foo reference","content":"unrelated body","labels":[]}"#,
            ))
            .await
            .unwrap();

        let response = app
            .oneshot(post("/api/notes/search", r#"{"query":"foo:","limit":5}"#))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let results: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            results
                .as_array()
                .unwrap()
                .iter()
                .any(|result| result["title"] == "Foo reference"),
            "expected literal title hit, got {results}"
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
