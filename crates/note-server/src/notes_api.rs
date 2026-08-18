use axum::{
    body::Body,
    extract::{
        rejection::{JsonRejection, QueryRejection},
        FromRef, FromRequest, FromRequestParts, Path, Query, Request, State,
    },
    http::{request::Parts, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get as route_get,
    Json,
};
use note_core::{Note, NoteAttachment, NoteListItem};
use note_pipelines::{
    bulk_update_note_labels, count_notes, delete_note, embedding_dashboard_status,
    get_note_attachment, get_note_markdown, get_note_metadata, label_note_counts,
    list_deleted_note_summaries, list_label_keys, list_note_summaries, normalized_list_limit,
    normalized_list_offset, permanently_delete_note, restore_notes, save_note,
    search_notes_filtered, update_note, BulkUpdateNoteLabelsInput,
    BulkUpdateNoteLabelsValidationError, Context, ListNotesParams, NoteMutationNotifier,
    RestoreNoteInput, SaveNoteInput,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

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
pub struct UpdateNoteRequest {
    pub expected_revision: i64,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentRequest>,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct ExpectedRevisionQuery {
    /// Revision returned by the latest read of this note.
    expected_revision: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct NoteMutationApiError {
    pub code: String,
    pub message: String,
    #[schema(example = json!({}))]
    pub details: serde_json::Value,
    pub retryable: bool,
    #[serde(skip)]
    #[schema(ignore)]
    status: StatusCode,
}

impl NoteMutationApiError {
    fn expected_revision_required() -> Self {
        Self {
            code: "expected_revision_required".into(),
            message: "expected_revision is required".into(),
            details: serde_json::json!({}),
            retryable: false,
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".into(),
            message: message.into(),
            details: serde_json::json!({}),
            retryable: false,
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn not_found(note_id: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: "note not found".into(),
            details: serde_json::json!({"note_id": note_id.into()}),
            retryable: false,
            status: StatusCode::NOT_FOUND,
        }
    }

    fn storage_failure() -> Self {
        Self {
            code: "storage_failure".into(),
            message: "note storage operation failed".into(),
            details: serde_json::json!({}),
            retryable: true,
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn from_anyhow(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<note_pipelines::NoteMutationError>() {
            return Self::from(error.clone());
        }
        if error.downcast_ref::<note_core::ValidationError>().is_some()
            || error
                .downcast_ref::<note_core::LabelKeyValidationError>()
                .is_some()
        {
            return Self::invalid_input(error.to_string());
        }
        Self::storage_failure()
    }
}

impl From<note_pipelines::NoteMutationError> for NoteMutationApiError {
    fn from(error: note_pipelines::NoteMutationError) -> Self {
        let (message, status) = match &error {
            note_pipelines::NoteMutationError::NotFound(_) => {
                ("note not found", StatusCode::NOT_FOUND)
            }
            note_pipelines::NoteMutationError::StaleRevision { .. } => {
                ("the note changed after it was read", StatusCode::CONFLICT)
            }
            note_pipelines::NoteMutationError::StaleContentTag { .. } => (
                "the note content changed after it was read",
                StatusCode::CONFLICT,
            ),
        };
        Self {
            code: error.code().into(),
            message: message.into(),
            details: serde_json::to_value(error.details())
                .unwrap_or_else(|_| serde_json::json!({})),
            retryable: error.retryable(),
            status,
        }
    }
}

impl IntoResponse for NoteMutationApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

struct NoteJson<T>(T);

impl<S, T> FromRequest<S> for NoteJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = NoteMutationApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(note_json_rejection)
    }
}

fn note_json_rejection(rejection: JsonRejection) -> NoteMutationApiError {
    if rejection.body_text().contains("expected_revision") {
        NoteMutationApiError::expected_revision_required()
    } else {
        NoteMutationApiError::invalid_input("Invalid JSON request body")
    }
}

struct NoteQuery<T>(T);

impl<S, T> FromRequestParts<S> for NoteQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = NoteMutationApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(note_query_rejection)
    }
}

fn note_query_rejection(rejection: QueryRejection) -> NoteMutationApiError {
    if rejection.body_text().contains("expected_revision") {
        NoteMutationApiError::expected_revision_required()
    } else {
        NoteMutationApiError::invalid_input("Invalid query parameters")
    }
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

#[derive(utoipa::ToSchema)]
#[schema(as = UpdateNoteRequest)]
#[allow(dead_code)]
struct UpdateNoteRequestDocumentation {
    expected_revision: i64,
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
            storage: None,
        })
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SaveNoteResponse {
    pub id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BulkUpdateNoteLabelsRequest {
    #[schema(min_length = 1)]
    selector: String,
    #[schema(schema_with = crate::openapi::nonempty_label_pairs_schema)]
    set: Vec<(String, String)>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct BulkUpdateNoteLabelsResponse {
    matched: usize,
    updated: usize,
    unchanged: usize,
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
    pub attachments: Vec<AttachmentMetadataResponse>,
    #[schema(schema_with = crate::openapi::label_pairs_schema)]
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AttachmentMetadataResponse {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[schema(default = "")]
    pub description: String,
}

impl From<NoteAttachment> for AttachmentMetadataResponse {
    fn from(attachment: NoteAttachment) -> Self {
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
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
            revision: note.revision,
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
    pub revision: i64,
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
            revision: note.revision,
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
    pub revision: i64,
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
            revision: note.revision,
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

#[cfg(test)]
static BULK_DASHBOARD_CACHE_INVALIDATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub struct DashboardCacheInvalidator;

impl NoteMutationNotifier for DashboardCacheInvalidator {
    fn note_mutated(&self) {
        invalidate_dashboard_cache();
        #[cfg(test)]
        BULK_DASHBOARD_CACHE_INVALIDATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

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
        } else if e.downcast_ref::<note_core::ValidationError>().is_some()
            || e.downcast_ref::<note_core::LabelKeyValidationError>()
                .is_some()
        {
            axum::http::StatusCode::BAD_REQUEST
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;
    invalidate_dashboard_cache();
    Ok(Json(SaveNoteResponse { id: note.id }))
}

#[utoipa::path(
    post,
    path = "/api/notes/bulk-labels",
    tag = "notes",
    request_body = BulkUpdateNoteLabelsRequest,
    responses(
        (status = 200, description = "Bulk note label update result", body = BulkUpdateNoteLabelsResponse),
        (status = 400, description = "Invalid bulk label update", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn bulk_update_note_labels_handler(
    State(ctx): State<Arc<Context>>,
    req: Result<Json<BulkUpdateNoteLabelsRequest>, JsonRejection>,
) -> Result<Json<BulkUpdateNoteLabelsResponse>, (axum::http::StatusCode, String)> {
    let Json(req) = req.map_err(|_| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "invalid bulk note label update request".into(),
        )
    })?;
    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: req.selector,
            set: req.set,
        },
    )
    .await
    .map_err(map_bulk_update_note_labels_error)?;
    Ok(Json(BulkUpdateNoteLabelsResponse {
        matched: result.matched,
        updated: result.updated,
        unchanged: result.unchanged,
    }))
}

fn map_bulk_update_note_labels_error(error: anyhow::Error) -> (axum::http::StatusCode, String) {
    let caller_message = error
        .downcast_ref::<BulkUpdateNoteLabelsValidationError>()
        .map(ToString::to_string)
        .or_else(|| {
            error
                .downcast_ref::<note_core::ValidationError>()
                .map(ToString::to_string)
        })
        .or_else(|| {
            error
                .downcast_ref::<note_core::LabelKeyValidationError>()
                .map(ToString::to_string)
        });
    if caller_message
        .as_ref()
        .is_some_and(|message| error.to_string() == *message)
    {
        return (axum::http::StatusCode::BAD_REQUEST, caller_message.unwrap());
    }
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "bulk note label update failed".into(),
    )
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
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex). Keys cannot contain
    /// selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`;
    /// the `&` separator is also reserved in operands.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct CountNotesQuery {
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex). Keys cannot contain
    /// selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`;
    /// the `&` separator is also reserved in operands.
    #[serde(default)]
    label: Option<String>,
    /// Accepted and validated for compatibility; values do not affect the count.
    #[serde(default)]
    limit: Option<i64>,
    /// Accepted and validated for compatibility; values do not affect the count.
    #[serde(default)]
    offset: Option<i64>,
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
            limit: Some(normalized_list_limit(req.limit)),
            offset: Some(normalized_list_offset(req.offset)),
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
    match get_note_metadata(&ctx, &id)
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
    request_body = UpdateNoteRequestDocumentation,
    responses(
        (status = 200, description = "Updated note", body = NoteDto),
        (status = 400, description = "Invalid note or missing expected revision", body = NoteMutationApiError),
        (status = 404, description = "Note not found", body = NoteMutationApiError),
        (status = 409, description = "Stale note revision", body = NoteMutationApiError),
        (status = 500, description = "Server error", body = NoteMutationApiError)
    )
)]
async fn update_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    NoteJson(req): NoteJson<UpdateNoteRequest>,
) -> Result<Json<NoteDto>, NoteMutationApiError> {
    let attachments = decode_attachment_requests(req.attachments)
        .map_err(|(_, message)| NoteMutationApiError::invalid_input(message))?;
    let note = update_note(
        &ctx,
        &id,
        req.expected_revision,
        SaveNoteInput {
            title: req.title,
            content: req.content,
            attachments,
            labels: req.labels,
        },
    )
    .await
    .map_err(NoteMutationApiError::from_anyhow)?;

    match note {
        Some(note) => {
            invalidate_dashboard_cache();
            Ok(Json(NoteDto::from(note)))
        }
        None => Err(NoteMutationApiError::not_found(id)),
    }
}

#[utoipa::path(
    delete,
    path = "/api/notes/{id}",
    tag = "notes",
    params(("id" = String, Path, description = "Note ID"), ExpectedRevisionQuery),
    responses(
        (status = 204, description = "Note moved to Trash"),
        (status = 400, description = "Missing expected revision", body = NoteMutationApiError),
        (status = 404, description = "Note not found", body = NoteMutationApiError),
        (status = 409, description = "Stale note revision", body = NoteMutationApiError),
        (status = 500, description = "Server error", body = NoteMutationApiError)
    )
)]
async fn delete_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    NoteQuery(query): NoteQuery<ExpectedRevisionQuery>,
) -> Result<axum::http::StatusCode, NoteMutationApiError> {
    match delete_note(&ctx, &id, query.expected_revision)
        .await
        .map_err(NoteMutationApiError::from_anyhow)?
    {
        true => {
            invalidate_dashboard_cache();
            Ok(axum::http::StatusCode::NO_CONTENT)
        }
        false => Err(NoteMutationApiError::not_found(id)),
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
    notes: Vec<RestoreNoteInputRequest>,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct RestoreNoteInputRequest {
    id: String,
    expected_revision: i64,
}

#[utoipa::path(
    post,
    path = "/api/trash/restore",
    tag = "trash",
    request_body = RestoreNotesRequest,
    responses(
        (status = 204, description = "Notes restored"),
        (status = 400, description = "Invalid restore request", body = NoteMutationApiError),
        (status = 404, description = "Trashed note not found", body = NoteMutationApiError),
        (status = 409, description = "Stale note revision", body = NoteMutationApiError),
        (status = 500, description = "Server error", body = NoteMutationApiError)
    )
)]
async fn restore_notes_handler(
    State(ctx): State<Arc<Context>>,
    NoteJson(req): NoteJson<RestoreNotesRequest>,
) -> Result<axum::http::StatusCode, NoteMutationApiError> {
    if req.notes.is_empty() {
        return Err(NoteMutationApiError::invalid_input(
            "at least one note id is required",
        ));
    }
    let entries = req
        .notes
        .into_iter()
        .map(|note| RestoreNoteInput {
            id: note.id,
            expected_revision: note.expected_revision,
        })
        .collect::<Vec<_>>();
    match restore_notes(&ctx, &entries)
        .await
        .map_err(NoteMutationApiError::from_anyhow)?
    {
        true => {
            invalidate_dashboard_cache();
            Ok(axum::http::StatusCode::NO_CONTENT)
        }
        false => Err(NoteMutationApiError::invalid_input(
            "at least one note id is required",
        )),
    }
}

#[utoipa::path(
    delete,
    path = "/api/trash/{id}",
    tag = "trash",
    params(("id" = String, Path, description = "Note ID"), ExpectedRevisionQuery),
    responses(
        (status = 204, description = "Note permanently deleted"),
        (status = 400, description = "Missing expected revision", body = NoteMutationApiError),
        (status = 404, description = "Trashed note not found", body = NoteMutationApiError),
        (status = 409, description = "Stale note revision", body = NoteMutationApiError),
        (status = 500, description = "Server error", body = NoteMutationApiError)
    )
)]
async fn permanently_delete_note_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    NoteQuery(query): NoteQuery<ExpectedRevisionQuery>,
) -> Result<axum::http::StatusCode, NoteMutationApiError> {
    match permanently_delete_note(&ctx, &id, query.expected_revision)
        .await
        .map_err(NoteMutationApiError::from_anyhow)?
    {
        true => Ok(axum::http::StatusCode::NO_CONTENT),
        false => Err(NoteMutationApiError::not_found(id)),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex). Keys cannot contain
    /// selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`;
    /// the `&` separator is also reserved in operands.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchResultDto {
    pub id: String,
    pub title: String,
    pub revision: i64,
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
                revision: r.note.revision,
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

pub fn notes_router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Arc<Context>: FromRef<S>,
{
    #[derive(OpenApi)]
    #[openapi(paths(get_attachment_handler))]
    struct AttachmentOpenApi;

    OpenApiRouter::with_openapi(AttachmentOpenApi::openapi())
        .routes(routes!(save_note_handler, list_notes_handler))
        .routes(routes!(bulk_update_note_labels_handler))
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
    use note_attachments::{
        AttachmentStore, AttachmentStoreInfo, FilesystemAttachmentStore,
        PreparedAttachmentMutation, PreparedAttachmentSet,
    };
    use note_embedding::StubEmbedder;
    use note_storage::{
        ActiveNoteSource, AttachmentMetadataUpdate, BackendInfo, EmbeddingDashboardStatus,
        EmbeddingJob, EmbeddingRepository, LabelRepository, NewNote, NoteChunk, NoteFieldsUpdate,
        NoteMutationResult, NoteUpdate, NotesRepository, OrgRepository, RetrievalRepository,
        SettingsRepository, StorageBackend, StorageError, StorageErrorKind, StorageResult,
        StorageSession, StorageTransaction, TransactionMode, UpsertNoteChunk,
    };
    use note_storage_turso::TursoStorage;
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    static BULK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct RecordingReadAttachmentStore {
        inner: FilesystemAttachmentStore,
        read_calls: AtomicUsize,
    }

    struct FailingBeginStorageBackend {
        inner: Arc<dyn StorageBackend>,
    }

    struct RollbackFailingStorageBackend {
        inner: Arc<dyn StorageBackend>,
    }

    struct RollbackFailingTransaction {
        inner: Box<dyn StorageTransaction>,
    }

    #[async_trait::async_trait]
    impl StorageBackend for FailingBeginStorageBackend {
        async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
            self.inner.session().await
        }

        async fn begin(
            &self,
            _mode: TransactionMode,
        ) -> StorageResult<Box<dyn StorageTransaction>> {
            Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "private repository failure detail",
            ))
        }

        async fn info(&self) -> StorageResult<BackendInfo> {
            self.inner.info().await
        }
    }

    #[async_trait::async_trait]
    impl StorageBackend for RollbackFailingStorageBackend {
        async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
            self.inner.session().await
        }

        async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>> {
            Ok(Box::new(RollbackFailingTransaction {
                inner: self.inner.begin(mode).await?,
            }))
        }

        async fn info(&self) -> StorageResult<BackendInfo> {
            self.inner.info().await
        }
    }

    macro_rules! impl_forward_repository {
        ($repository:path { $(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty;)* }) => {
            #[async_trait::async_trait]
            impl $repository for RollbackFailingTransaction {
                $(
                    async fn $name(&self, $($arg: $ty),*) -> StorageResult<$result> {
                        self.inner.$name($($arg),*).await
                    }
                )*
            }
        };
    }

    impl_forward_repository! {
        NotesRepository {
            fn insert_note(note: NewNote<'_>) -> ();
            fn get_note_revision(id: &str) -> Option<i64>;
            fn note_exists(id: &str) -> bool;
            fn get_note(id: &str) -> Option<note_core::Note>;
            fn get_note_content(id: &str) -> Option<String>;
            fn update_note(note: NoteUpdate<'_>) -> NoteMutationResult<()>;
            fn update_note_fields(note: NoteFieldsUpdate<'_>) -> NoteMutationResult<()>;
            fn update_note_attachments(note: AttachmentMetadataUpdate<'_>) -> NoteMutationResult<()>;
            fn soft_delete_note(id: &str, expected_revision: i64, deleted_at: i64) -> NoteMutationResult<()>;
            fn get_deleted_note_content_and_revision(id: &str) -> Option<(String, i64)>;
            fn restore_note(id: &str, expected_revision: i64) -> NoteMutationResult<()>;
            fn permanently_delete_note(id: &str, expected_revision: i64) -> NoteMutationResult<()>;
            fn list_expired_deleted_note_ids(cutoff: i64) -> Vec<String>;
            fn clear_note_search_data(id: &str) -> ();
            fn clear_note_labels(id: &str) -> ();
            fn clear_note_chunk_derived(id: &str, chunk_idx: i64) -> ();
            fn clear_note_chunks_from_derived(id: &str, min_chunk_idx: i64) -> ();
            fn list_notes(
                selectors: &[note_core::LabelSelector],
                limit: Option<i64>,
                offset: Option<i64>,
            ) -> Vec<note_core::Note>;
            fn list_all_notes() -> Vec<note_core::Note>;
            fn list_note_summaries(
                selectors: &[note_core::LabelSelector],
                limit: Option<i64>,
                offset: Option<i64>,
            ) -> Vec<note_core::NoteListItem>;
            fn list_deleted_note_summaries() -> Vec<note_core::NoteListItem>;
            fn count_notes(selectors: &[note_core::LabelSelector]) -> usize;
            fn matching_note_ids(selectors: &[note_core::LabelSelector]) -> Vec<String>;
            fn matching_note_ids_for_update(selectors: &[note_core::LabelSelector]) -> Vec<String>;
            fn advance_note_updated_at(id: &str, now: i64) -> u64;
            fn list_active_note_sources() -> Vec<ActiveNoteSource>;
        }
    }

    impl_forward_repository! {
        LabelRepository {
            fn insert_label_key(key: &str, description: &str) -> ();
            fn insert_label_key_if_missing(key: &str, description: &str) -> ();
            fn insert_label_key_with_type(
                key: &str,
                description: &str,
                value_type: note_core::LabelValueType,
            ) -> ();
            fn list_label_keys() -> Vec<note_core::LabelKey>;
            fn update_label_key(key: &str, description: &str) -> ();
            fn update_label_key_with_type(
                key: &str,
                description: &str,
                value_type: note_core::LabelValueType,
            ) -> ();
            fn delete_label_key(key: &str) -> ();
            fn attach_label(note_id: &str, key: &str, value: &str) -> ();
            fn set_note_label(note_id: &str, key: &str, value: &str) -> bool;
            fn labels_for_note(note_id: &str) -> Vec<note_core::Label>;
            fn label_note_counts() -> Vec<(String, usize)>;
            fn find_note_with_labels(labels: &[(String, String)]) -> Option<String>;
        }
    }

    impl_forward_repository! {
        EmbeddingRepository {
            fn embedding_dashboard_status() -> EmbeddingDashboardStatus;
            fn list_note_chunks(note_id: &str) -> Vec<NoteChunk>;
            fn get_note_chunk(note_id: &str, chunk_idx: i64) -> Option<NoteChunk>;
            fn upsert_note_chunk(chunk: UpsertNoteChunk<'_>) -> ();
            fn mark_note_chunk_status(
                note_id: &str,
                chunk_idx: i64,
                content_hash: &str,
                note_revision: i64,
                status: &str,
                updated_at: i64,
            ) -> u64;
            fn delete_note_chunks_from(note_id: &str, min_chunk_idx: i64) -> u64;
            fn chunk_embedding_exists(note_id: &str, chunk_idx: i64) -> bool;
            fn enqueue_embedding_job(
                note_id: &str,
                chunk_idx: i64,
                content_hash: &str,
                content: &str,
                note_revision: i64,
                now: i64,
            ) -> ();
            fn delete_stale_embedding_jobs_for_chunk(
                note_id: &str,
                chunk_idx: i64,
                current_hash: &str,
            ) -> u64;
            fn delete_embedding_jobs_from_chunk(note_id: &str, min_chunk_idx: i64) -> u64;
            fn claim_pending_embedding_jobs(limit: usize, now: i64) -> Vec<EmbeddingJob>;
            fn delete_embedding_job(id: i64) -> u64;
            fn fail_embedding_job(
                id: i64,
                attempts: i64,
                max_attempts: i64,
                error: &str,
                now: i64,
            ) -> u64;
            fn requeue_processing_embedding_jobs(now: i64) -> u64;
            fn reset_embeddings_for_regeneration(now: i64) -> u64;
        }
    }

    impl_forward_repository! {
        RetrievalRepository {
            fn insert_chunk_embedding(note_id: &str, chunk_idx: i64, embedding: &[f32]) -> ();
            fn dense_search(
                query: &[f32],
                limit: usize,
                allowed_note_ids: Option<&[String]>,
            ) -> Vec<String>;
            fn title_search(
                query: &str,
                limit: usize,
                allowed_note_ids: Option<&[String]>,
            ) -> Vec<String>;
        }
    }

    impl_forward_repository! {
        SettingsRepository {
            fn get_system_config() -> note_core::SystemConfig;
            fn set_system_config(config: &note_core::SystemConfig) -> ();
            fn get_embedding_fingerprint() -> Option<String>;
            fn set_embedding_fingerprint(fingerprint: &str) -> ();
        }
    }

    impl OrgRepository for RollbackFailingTransaction {}

    #[async_trait::async_trait]
    impl StorageTransaction for RollbackFailingTransaction {
        async fn commit(self: Box<Self>) -> StorageResult<()> {
            self.inner.commit().await
        }

        async fn rollback(self: Box<Self>) -> StorageResult<()> {
            self.inner.rollback().await?;
            Err(StorageError::new(
                StorageErrorKind::Transaction,
                "ROLLBACK-SECRET-42",
            ))
        }
    }

    impl RecordingReadAttachmentStore {
        fn new(root: impl Into<std::path::PathBuf>) -> Self {
            Self {
                inner: FilesystemAttachmentStore::new(root.into()),
                read_calls: AtomicUsize::new(0),
            }
        }

        fn read_calls(&self) -> usize {
            self.read_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl AttachmentStore for RecordingReadAttachmentStore {
        async fn prepare(
            &self,
            note_id: &str,
            attachments: &[NoteAttachment],
        ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
            self.inner.prepare(note_id, attachments).await
        }

        async fn prepare_put(
            &self,
            note_id: &str,
            attachment: &NoteAttachment,
        ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
            self.inner.prepare_put(note_id, attachment).await
        }

        async fn prepare_delete(
            &self,
            note_id: &str,
            path: &str,
        ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
            self.inner.prepare_delete(note_id, path).await
        }

        async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
            self.read_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.read(note_id, path).await
        }

        async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
            self.inner.remove_note(note_id).await
        }

        fn info(&self) -> AttachmentStoreInfo {
            self.inner.info()
        }
    }

    fn note_openapi_document() -> Value {
        let (_, openapi) = notes_router::<Arc<Context>>().split_for_parts();
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
            ("/api/notes/bulk-labels", &["post"][..]),
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
    fn openapi_documents_exact_bulk_label_contract() {
        let document = note_openapi_document();
        let schemas = &document["components"]["schemas"];
        let request = &schemas["BulkUpdateNoteLabelsRequest"];
        let response = &schemas["BulkUpdateNoteLabelsResponse"];

        let mut request_properties = request["properties"]
            .as_object()
            .expect("bulk request properties")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        request_properties.sort_unstable();
        assert_eq!(request_properties, vec!["selector", "set"]);
        assert_eq!(request["additionalProperties"], false);
        assert_eq!(request["required"], serde_json::json!(["selector", "set"]));
        assert_eq!(request["properties"]["selector"]["type"], "string");
        assert_eq!(request["properties"]["selector"]["minLength"], 1);

        let set = &request["properties"]["set"];
        assert_eq!(set["type"], "array");
        assert_eq!(set["minItems"], 1);
        assert_eq!(set["items"]["type"], "array");
        assert_eq!(set["items"]["minItems"], 2);
        assert_eq!(set["items"]["maxItems"], 2);
        assert_eq!(set["items"]["items"]["type"], "string");

        let mut response_properties = response["properties"]
            .as_object()
            .expect("bulk response properties")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        response_properties.sort_unstable();
        assert_eq!(response_properties, vec!["matched", "unchanged", "updated"]);
        assert_eq!(
            response["required"],
            serde_json::json!(["matched", "updated", "unchanged"])
        );
        for field in ["matched", "updated", "unchanged"] {
            assert_eq!(response["properties"][field]["type"], "integer");
        }

        let operation = &document["paths"]["/api/notes/bulk-labels"]["post"];
        assert_eq!(operation["tags"], serde_json::json!(["notes"]));
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/BulkUpdateNoteLabelsRequest"
        );
        assert_eq!(
            operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/BulkUpdateNoteLabelsResponse"
        );
        let mut statuses = operation["responses"]
            .as_object()
            .expect("bulk response statuses")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        statuses.sort_unstable();
        assert_eq!(statuses, vec!["200", "400", "500"]);
        for status in ["400", "500"] {
            assert_eq!(
                operation["responses"][status]["content"]["text/plain"]["schema"]["type"],
                "string"
            );
        }
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
            document["components"]["schemas"]["RestoreNotesRequest"]["properties"]["notes"]
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
        let response = &schemas["AttachmentMetadataResponse"];
        let response_properties = response["properties"]
            .as_object()
            .expect("attachment metadata response properties");
        let save = &schemas["SaveNoteRequest"];

        assert_eq!(request_properties["description"]["default"], "");
        assert_eq!(response_properties["description"]["default"], "");
        let mut response_property_keys = response_properties
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        response_property_keys.sort_unstable();
        assert_eq!(
            response_property_keys,
            vec!["description", "id", "mime", "path"]
        );
        assert_eq!(
            schemas["NoteDto"]["properties"]["attachments"]["items"]["$ref"],
            "#/components/schemas/AttachmentMetadataResponse"
        );
        assert_eq!(
            document["paths"]["/api/notes/{id}"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/NoteDto"
        );
        assert_eq!(
            document["paths"]["/api/notes/{id}"]["put"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/NoteDto"
        );
        assert_eq!(
            save["properties"]["attachments"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            save["properties"]["labels"]["default"],
            serde_json::json!([])
        );

        let content_base64 = &request_properties["content_base64"];
        assert_eq!(content_base64["contentEncoding"], "base64");
        assert_eq!(
            content_base64["contentMediaType"],
            "application/octet-stream"
        );

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
    fn openapi_documents_required_revisions_and_structured_mutation_errors() {
        let document = note_openapi_document();
        let schemas = &document["components"]["schemas"];
        assert!(schemas["UpdateNoteRequest"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("expected_revision")));
        assert!(schemas["SearchResultDto"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("revision")));
        assert_eq!(
            operation_parameter(
                &document["paths"]["/api/notes/{id}"]["delete"],
                "expected_revision"
            )["required"],
            true
        );
        assert_eq!(
            operation_parameter(
                &document["paths"]["/api/trash/{id}"]["delete"],
                "expected_revision"
            )["required"],
            true
        );
        assert_eq!(
            document["paths"]["/api/notes/{id}"]["put"]["responses"]["409"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/NoteMutationApiError"
        );
        assert_eq!(
            schemas["NoteMutationApiError"]["required"],
            serde_json::json!(["code", "message", "details", "retryable"])
        );
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
        let ctx = Arc::new(
            Context::new(
                storage.clone(),
                Arc::new(StubEmbedder),
                Arc::new(FilesystemAttachmentStore::new(
                    dir.path().join("attachments"),
                )),
            )
            .with_note_mutation_notifier(Arc::new(DashboardCacheInvalidator)),
        );
        (
            notes_router::<Arc<Context>>()
                .with_state(ctx.clone())
                .into(),
            ctx,
            storage,
            dir,
        )
    }

    async fn test_app() -> (Router, Arc<Context>, tempfile::TempDir) {
        let (app, ctx, _storage, dir) = test_app_with_backend().await;
        (app, ctx, dir)
    }

    fn test_app_from_backend(
        storage: Arc<dyn StorageBackend>,
        attachments: impl Into<std::path::PathBuf>,
    ) -> Router {
        let ctx = Arc::new(
            Context::new(
                storage,
                Arc::new(StubEmbedder),
                Arc::new(FilesystemAttachmentStore::new(attachments.into())),
            )
            .with_note_mutation_notifier(Arc::new(DashboardCacheInvalidator)),
        );
        notes_router::<Arc<Context>>().with_state(ctx).into()
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
    async fn bulk_label_update_adds_replaces_and_reports_noops() {
        let _bulk_guard = BULK_TEST_LOCK.lock().await;
        let (app, _ctx, _dir) = test_app().await;
        let mut ids = Vec::new();
        for body in [
            r#"{"title":"Add","content":"Body","labels":[["type","ietf-rfc"],["owner","protocols"]]}"#,
            r#"{"title":"Replace","content":"Body","labels":[["type","ietf-rfc"],["owner","protocols"],["project","old"]]}"#,
            r#"{"title":"Noop","content":"Body","labels":[["type","ietf-rfc"],["owner","protocols"],["project","IETF-RFC"]]}"#,
        ] {
            ids.push(save_note_id(app.clone(), body).await);
        }

        let response = app
            .clone()
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=ietf-rfc","set":[["project","IETF-RFC"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            serde_json::json!({"matched": 3, "updated": 2, "unchanged": 1})
        );

        for id in ids {
            let response = app
                .clone()
                .oneshot(get(&format!("/api/notes/{id}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let note: Value = serde_json::from_slice(&body).unwrap();
            assert!(note["labels"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(["project", "IETF-RFC"])));
            assert!(note["labels"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(["owner", "protocols"])));
        }
    }

    #[tokio::test]
    async fn bulk_label_update_zero_matches_does_not_create_catalog_entries() {
        let (app, ctx, _dir) = test_app().await;

        let response = app
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=not-present","set":[["project","IETF-RFC"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            serde_json::json!({"matched": 0, "updated": 0, "unchanged": 0})
        );
        assert!(list_label_keys(&ctx).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn bulk_label_update_rejects_caller_errors_and_unknown_fields() {
        let (app, ctx, _dir) = test_app().await;
        note_pipelines::define_label_key_with_type(
            &ctx,
            "priority",
            "Priority score",
            note_core::LabelValueType::Number,
        )
        .await
        .unwrap();
        save_note_id(
            app.clone(),
            r#"{"title":"Typed","content":"Body","labels":[["type","ietf-rfc"]]}"#,
        )
        .await;

        for (body, expected_message) in [
            (
                r#"{"selector":"   ","set":[["project","IETF-RFC"]]}"#,
                "selector must not be empty",
            ),
            (
                r#"{"selector":"type=ietf-rfc&&owner=protocols","set":[["project","IETF-RFC"]]}"#,
                "label selector is malformed",
            ),
            (
                r#"{"selector":"type=ietf-rfc","set":[]}"#,
                "at least one label assignment is required",
            ),
            (
                r#"{"selector":"type=ietf-rfc","set":[["project","one"],["project","two"]]}"#,
                "duplicate label assignment key: project",
            ),
            (
                r#"{"selector":"type=ietf-rfc","set":[["bad$key","value"]]}"#,
                "label key must not contain selector-reserved character: $",
            ),
            (
                r#"{"selector":"type=ietf-rfc","set":[["priority","urgent"]]}"#,
                "invalid value for label priority: urgent is not number",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(post("/api/notes/bulk-labels", body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            let response_body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                std::str::from_utf8(&response_body).unwrap(),
                expected_message
            );
        }

        for body in [
            r#"{"selector":"type=ietf-rfc","set":[["project","IETF-RFC"]],"unknown":true}"#,
            r#"{"selector":"type=ietf-rfc","set":[["project","IETF-RFC"]]"#,
        ] {
            let response = app
                .clone()
                .oneshot(post("/api/notes/bulk-labels", body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            let response_body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                &response_body[..],
                b"invalid bulk note label update request"
            );
        }
    }

    #[tokio::test]
    async fn bulk_label_update_invalidates_dashboard_only_when_a_note_changes() {
        let _bulk_guard = BULK_TEST_LOCK.lock().await;
        let (app, _ctx, _dir) = test_app().await;
        save_note_id(
            app.clone(),
            r#"{"title":"Cache","content":"Body","labels":[["type","ietf-rfc"],["project","IETF-RFC"]]}"#,
        )
        .await;

        let invalidations_before = BULK_DASHBOARD_CACHE_INVALIDATIONS.load(Ordering::SeqCst);

        let response = app
            .clone()
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=ietf-rfc","set":[["project","IETF-RFC"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            BULK_DASHBOARD_CACHE_INVALIDATIONS.load(Ordering::SeqCst),
            invalidations_before
        );

        let response = app
            .clone()
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=missing","set":[["project","other"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            BULK_DASHBOARD_CACHE_INVALIDATIONS.load(Ordering::SeqCst),
            invalidations_before
        );

        let response = app
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=ietf-rfc","set":[["project","changed"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            BULK_DASHBOARD_CACHE_INVALIDATIONS.load(Ordering::SeqCst),
            invalidations_before + 1
        );
    }

    #[tokio::test]
    async fn bulk_label_update_sanitizes_storage_failures() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(dir.path().join("t.db")).await.unwrap());
        let failing: Arc<dyn StorageBackend> =
            Arc::new(FailingBeginStorageBackend { inner: backend });
        let app = test_app_from_backend(failing, dir.path().join("attachments"));

        let response = app
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=ietf-rfc","set":[["project","IETF-RFC"]]}"#,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"bulk note label update failed");
        assert!(!String::from_utf8_lossy(&body).contains("private repository failure detail"));
    }

    #[tokio::test]
    async fn bulk_label_update_sanitizes_typed_validation_with_rollback_failure() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(dir.path().join("t.db")).await.unwrap());
        let base_app = test_app_from_backend(backend.clone(), dir.path().join("base-attachments"));
        let base_ctx = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("setup-attachments"),
            )),
        ));
        note_pipelines::define_label_key_with_type(
            &base_ctx,
            "priority",
            "Priority score",
            note_core::LabelValueType::Number,
        )
        .await
        .unwrap();
        save_note_id(
            base_app,
            r#"{"title":"Typed rollback","content":"Body","labels":[["type","ietf-rfc"]]}"#,
        )
        .await;
        let failing: Arc<dyn StorageBackend> =
            Arc::new(RollbackFailingStorageBackend { inner: backend });
        let app = test_app_from_backend(failing, dir.path().join("failing-attachments"));

        let response = app
            .oneshot(post(
                "/api/notes/bulk-labels",
                r#"{"selector":"type=ietf-rfc","set":[["priority","urgent"]]}"#,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"bulk note label update failed");
        assert!(!String::from_utf8_lossy(&body).contains("ROLLBACK-SECRET-42"));
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
    async fn selector_reserved_label_key_returns_400() {
        let (app, _ctx, _dir) = test_app().await;
        let resp = app
            .oneshot(post(
                "/api/notes",
                r#"{"title":"T","content":"C","labels":[["project$name","v"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
        assert!(attachments[0].get("content_base64").is_none());
        assert!(attachments[0].get("content").is_none());

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
    async fn get_note_returns_attachment_metadata_without_reading_files() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Proof","content":"See [proof](./proof.txt)","attachments":[{"id":"proof","path":"./proof.txt","mime":"text/plain","description":"proof","content":"evidence"}],"labels":[]}"#,
        )
        .await;
        std::fs::remove_file(dir.path().join("attachments").join(&id).join("proof.txt")).unwrap();

        let resp = app.oneshot(get(&format!("/api/notes/{id}"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["attachments"],
            serde_json::json!([{
                "id": "proof",
                "path": "./proof.txt",
                "mime": "text/plain",
                "description": "proof"
            }])
        );
    }

    #[tokio::test]
    async fn get_note_does_not_read_attachment_content() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(dir.path().join("t.db")).await.unwrap());
        let attachments = Arc::new(RecordingReadAttachmentStore::new(
            dir.path().join("attachments"),
        ));
        let ctx = Arc::new(Context::new(
            storage,
            Arc::new(StubEmbedder),
            attachments.clone(),
        ));
        let app: Router = notes_router::<Arc<Context>>().with_state(ctx).into();
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Proof","content":"See [proof](./proof.txt)","attachments":[{"id":"proof","path":"./proof.txt","mime":"text/plain","description":"proof","content":"evidence"}],"labels":[]}"#,
        )
        .await;
        assert_eq!(
            attachments.read_calls(),
            0,
            "saving must not read attachments"
        );

        let response = app.oneshot(get(&format!("/api/notes/{id}"))).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            attachments.read_calls(),
            0,
            "GET /api/notes/{{id}} must not read attachment content"
        );
    }

    #[tokio::test]
    async fn binary_attachments_accept_base64_requests_and_download_as_raw_bytes() {
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
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let attachment = &json["attachments"][0];
        assert_eq!(
            attachment,
            &serde_json::json!({
                "id": "blob",
                "path": "./blob.bin",
                "mime": "application/octet-stream",
                "description": "raw bytes"
            })
        );
        assert!(attachment.get("content_base64").is_none());
        assert!(attachment.get("content").is_none());

        let update_body = serde_json::json!({
            "expected_revision": 1,
            "title": "Binary updated",
            "content": "![blob](./blob.bin)",
            "attachments": [{
                "id": "blob",
                "path": "./blob.bin",
                "mime": "application/octet-stream",
                "description": "raw bytes",
                "content_base64": "AJ+Slv8="
            }],
            "labels": []
        })
        .to_string();
        let resp = app
            .clone()
            .oneshot(put(&format!("/api/notes/{id}"), &update_body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let attachment = &json["attachments"][0];
        assert_eq!(
            attachment,
            &serde_json::json!({
                "id": "blob",
                "path": "./blob.bin",
                "mime": "application/octet-stream",
                "description": "raw bytes"
            })
        );
        assert!(attachment.get("content_base64").is_none());
        assert!(attachment.get("content").is_none());

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
                r#"{"expected_revision":1,"title":"New","content":"New content","labels":[["topic","rust"]]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("title").and_then(|v| v.as_str()), Some("New"));

        let stale = app
            .clone()
            .oneshot(put(
                &format!("/api/notes/{id}"),
                r#"{"expected_revision":1,"title":"Stale","content":"stale","labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let body: serde_json::Value =
            serde_json::from_slice(&stale.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(body["code"], "stale_revision");
        assert_eq!(body["details"]["note_id"], id);
        assert_eq!(body["details"]["expected_revision"], 1);
        assert_eq!(body["details"]["current_revision"], 2);
        assert_eq!(body["retryable"], false);

        let missing_revision = app
            .clone()
            .oneshot(put(
                &format!("/api/notes/{id}"),
                r#"{"title":"Unsafe","content":"unsafe","labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(missing_revision.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_slice(
            &missing_revision
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(body["code"], "expected_revision_required");

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
                r#"{"expected_revision":1,"title":"New","content":"New content","labels":[]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(body["code"], "not_found");
        assert_eq!(body["details"]["note_id"], "nope");
    }

    #[tokio::test]
    async fn delete_note_by_id_lists_deleted_note_in_trash_or_404() {
        let (app, _ctx, dir) = test_app().await;
        let id = save_note_id(
            app.clone(),
            r#"{"title":"Delete Me","content":"C","attachments":[{"id":"proof","path":"./proof.txt","mime":"text/plain","description":"proof","content":"keep me"}],"labels":[["status","deleted"]]}"#,
        )
        .await;

        let missing_revision = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}")))
            .await
            .unwrap();
        assert_eq!(missing_revision.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_slice(
            &missing_revision
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(body["code"], "expected_revision_required");

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}?expected_revision=1")))
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
            .oneshot(delete(&format!("/api/notes/{id}?expected_revision=2")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"notes":[{{"id":"{id}","expected_revision":2}}]}}"#),
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
                &format!(r#"{{"notes":[{{"id":"{id}","expected_revision":3}}]}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/notes/{id}?expected_revision=3")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app
            .clone()
            .oneshot(delete(&format!("/api/trash/{id}?expected_revision=4")))
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
            .oneshot(delete(&format!("/api/trash/{id}?expected_revision=4")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .oneshot(delete("/api/notes/nope?expected_revision=1"))
            .await
            .unwrap();
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
                .oneshot(delete(&format!("/api/notes/{id}?expected_revision=1")))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        }

        let resp = app
            .clone()
            .oneshot(post("/api/trash/restore", r#"{"notes":[]}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = app
            .clone()
            .oneshot(post(
                "/api/trash/restore",
                &format!(r#"{{"notes":[{{"id":"{first}","expected_revision":2}},{{"id":"missing","expected_revision":1}}]}}"#),
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
                &format!(r#"{{"notes":[{{"id":"{first}","expected_revision":2}},{{"id":"{second}","expected_revision":2}},{{"id":"{first}","expected_revision":2}}]}}"#),
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
        assert!(hits.iter().all(|result| result["revision"]
            .as_i64()
            .is_some_and(|revision| revision > 0)));
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
