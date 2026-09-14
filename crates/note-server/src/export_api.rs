use crate::app_state::AppState;
use crate::export::document::{build_export_document, ExportDocumentError, ExportDocumentLimits};
use crate::export::renderer::PdfRendererError;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use note_core::{export_filename, ExportFormat};
use note_pipelines::{freeze_note_export, NoteExportError, NoteExportLimits};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ExportCapabilities {
    markdown: bool,
    pdf: bool,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct PdfExportQuery {
    #[param(minimum = 1)]
    expected_revision: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ExportApiError {
    pub code: String,
    pub message: String,
    #[schema(example = json!({}))]
    pub details: serde_json::Value,
    pub retryable: bool,
    #[serde(skip)]
    #[schema(ignore)]
    status: StatusCode,
    #[serde(skip)]
    #[schema(ignore)]
    retry_after: Option<u64>,
}

impl ExportApiError {
    fn new(status: StatusCode, code: &str, message: &str, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: serde_json::json!({}),
            retryable,
            status,
            retry_after: None,
        }
    }

    fn disabled() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pdf_export_disabled",
            "PDF export is not configured",
            false,
        )
    }

    fn invalid_input() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "expected_revision must be a positive integer",
            false,
        )
    }

    fn busy() -> Self {
        let mut error = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "export_busy",
            "PDF export capacity is busy",
            true,
        );
        error.retry_after = Some(2);
        error
    }

    fn timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "pdf_export_timeout",
            "PDF export timed out",
            true,
        )
    }
}

impl IntoResponse for ExportApiError {
    fn into_response(self) -> Response {
        let retry_after = self.retry_after;
        let mut response = (self.status, Json(self)).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response.headers_mut().insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        if let Some(seconds) = retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

#[utoipa::path(
    get,
    path = "/api/export/capabilities",
    tag = "export",
    responses((status = 200, description = "Configured export formats", body = ExportCapabilities,
        headers(("Cache-Control" = String, description = "Always no-store"))))
)]
async fn capabilities_handler(State(state): State<AppState>) -> Response {
    let mut response = Json(ExportCapabilities {
        markdown: true,
        pdf: state.pdf_export.is_some(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[utoipa::path(
    get,
    path = "/api/notes/{id}/export/pdf",
    tag = "export",
    params(("id" = String, Path, description = "Note ID"), PdfExportQuery),
    responses(
        (status = 200, description = "PDF download", body = crate::openapi::Binary, content_type = "application/pdf",
            headers(
                ("Cache-Control" = String, description = "Always no-store"),
                ("X-Content-Type-Options" = String, description = "Always nosniff"),
                ("X-Note-Revision" = i64, description = "Exported saved revision"),
                ("Content-Disposition" = String, description = "Safe attachment filename")
            )
        ),
        (status = 400, description = "Invalid revision", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 404, description = "Note not found", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 409, description = "Stale revision", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 413, description = "Export limit exceeded", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 422, description = "Export asset invalid", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 429, description = "Export capacity busy", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"), ("Retry-After" = u64, description = "Short manual retry delay in seconds"))),
        (status = 500, description = "Storage failure", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 502, description = "Renderer conversion failure", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 503, description = "PDF export unavailable or disabled", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff"))),
        (status = 504, description = "PDF export timeout", body = ExportApiError,
            headers(("Cache-Control" = String, description = "Always no-store"), ("X-Content-Type-Options" = String, description = "Always nosniff")))
    )
)]
async fn pdf_export_handler(
    State(state): State<AppState>,
    Path(note_id): Path<String>,
    query: Result<Query<PdfExportQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ExportApiError> {
    let runtime = state
        .pdf_export
        .clone()
        .ok_or_else(ExportApiError::disabled)?;
    let expected_revision = query
        .map_err(|_| ExportApiError::invalid_input())?
        .expected_revision;
    if expected_revision <= 0 || note_id.trim().is_empty() {
        return Err(ExportApiError::invalid_input());
    }
    let permit = runtime
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ExportApiError::busy())?;
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(runtime.config.total_deadline_secs);
    execute_export(state, runtime, permit, note_id, expected_revision, deadline).await
}

async fn execute_export(
    state: AppState,
    runtime: Arc<crate::app_state::PdfExportRuntime>,
    permit: tokio::sync::OwnedSemaphorePermit,
    note_id: String,
    expected_revision: i64,
    deadline: tokio::time::Instant,
) -> Result<Response, ExportApiError> {
    let pipeline_limits = NoteExportLimits {
        max_markdown_bytes: runtime.config.max_markdown_bytes,
        max_asset_count: runtime.config.max_asset_count,
        max_asset_bytes: runtime.config.max_asset_bytes,
        max_combined_asset_bytes: runtime.config.max_combined_asset_bytes,
    };
    let frozen = tokio::time::timeout_at(
        deadline,
        freeze_note_export(&state.note, &note_id, expected_revision, pipeline_limits),
    )
    .await
    .map_err(|_| ExportApiError::timeout())?
    .map_err(|error| {
        eprintln!("PDF export snapshot failed: {error:?}");
        map_pipeline_error(error)
    })?;
    let title = frozen.note.title.clone();
    let revision = frozen.note.revision;
    let document_limits = ExportDocumentLimits {
        max_pixels_per_image: runtime.config.max_pixels_per_image,
        max_combined_pixels: runtime.config.max_combined_pixels,
        max_packaged_asset_bytes: runtime.config.max_asset_bytes as usize,
        max_combined_packaged_asset_bytes: runtime.config.max_combined_asset_bytes as usize,
        ..ExportDocumentLimits::default()
    };
    // The blocking document task owns admission while it runs. If the handler is cancelled or
    // the total deadline elapses, dropping its join handle must not admit unbounded orphaned CPU
    // work; successful preparation returns the same permit for the remote conversion phase.
    let (package, permit) = tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || {
            build_export_document(&frozen, document_limits).map(|package| (package, permit))
        }),
    )
    .await
    .map_err(|_| ExportApiError::timeout())?
    .map_err(|_| storage_error())?
    .map_err(|error| {
        eprintln!("PDF export document preparation failed: {error:?}");
        map_document_error(error)
    })?;
    let remaining = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .ok_or_else(ExportApiError::timeout)?;
    let renderer_timeout = remaining.min(Duration::from_secs(runtime.config.renderer_timeout_secs));
    let renderer = runtime.renderer.clone();
    // Once remote conversion starts, the detached task owns both admission and the absolute
    // request deadline. Dropping the client handler cannot admit another remote conversion while
    // this one is still pending, and the task cannot outlive the configured total deadline.
    let render_task = tokio::spawn(async move {
        let _permit = permit;
        tokio::time::timeout_at(deadline, renderer.render(&package, renderer_timeout))
            .await
            .map_err(|_| PdfRendererError::Timeout)?
    });
    let pdf = render_task
        .await
        .map_err(|_| storage_error())?
        .map_err(|error| {
            eprintln!("PDF export renderer request failed: {error:?}");
            map_renderer_error(error)
        })?;
    pdf_response(pdf, &title, &note_id, revision)
}

fn map_pipeline_error(error: NoteExportError) -> ExportApiError {
    match error {
        NoteExportError::InvalidExpectedRevision | NoteExportError::InvalidInput => {
            ExportApiError::invalid_input()
        }
        NoteExportError::NoteNotFound => ExportApiError::new(
            StatusCode::NOT_FOUND,
            "note_not_found",
            "Note not found",
            false,
        ),
        NoteExportError::StaleRevision { .. } => ExportApiError::new(
            StatusCode::CONFLICT,
            "stale_revision",
            "The note changed after it was read",
            false,
        ),
        NoteExportError::MarkdownLimitExceeded
        | NoteExportError::AssetCountLimitExceeded
        | NoteExportError::AssetLimitExceeded { .. }
        | NoteExportError::CombinedAssetLimitExceeded => ExportApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_limit_exceeded",
            "Note export exceeds a configured limit",
            false,
        ),
        NoteExportError::AssetInvalid { destination }
        | NoteExportError::AssetMissing { destination } => ExportApiError {
            details: serde_json::json!({"path": destination}),
            ..ExportApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "export_asset_invalid",
                "A referenced export image is missing or invalid",
                false,
            )
        },
        NoteExportError::AssetChecksumMismatch { .. }
        | NoteExportError::AssetSizeMismatch { .. } => ExportApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "export_asset_invalid",
            "A referenced export image failed integrity validation",
            false,
        ),
        NoteExportError::MismatchedAssetPlan
        | NoteExportError::StorageFailure
        | NoteExportError::UnsupportedBoundedRead => storage_error(),
    }
}

fn map_document_error(error: ExportDocumentError) -> ExportApiError {
    match error {
        ExportDocumentError::InvalidImage { destination }
        | ExportDocumentError::UnsupportedImage { destination }
        | ExportDocumentError::MimeSignatureMismatch { destination }
        | ExportDocumentError::UnsafeSvg { destination }
        | ExportDocumentError::AssetNotInManifest { destination }
        | ExportDocumentError::AssetChecksumMismatch { destination }
        | ExportDocumentError::AssetSizeMismatch { destination } => ExportApiError {
            details: serde_json::json!({"path": destination}),
            ..ExportApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "export_asset_invalid",
                "A referenced export image is invalid",
                false,
            )
        },
        ExportDocumentError::InvalidDataImage => ExportApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "export_asset_invalid",
            "An embedded export image is invalid",
            false,
        ),
        ExportDocumentError::ImageDimensionsExceeded { .. }
        | ExportDocumentError::SvgComplexityExceeded { .. }
        | ExportDocumentError::PackagedAssetLimitExceeded { .. }
        | ExportDocumentError::CombinedImageDimensionsExceeded
        | ExportDocumentError::CombinedPackagedAssetLimitExceeded => ExportApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_limit_exceeded",
            "Export document exceeds a configured limit",
            false,
        ),
        ExportDocumentError::InvalidLimits => storage_error(),
        _ => ExportApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_limit_exceeded",
            "Export document exceeds a configured limit",
            false,
        ),
    }
}

fn map_renderer_error(error: PdfRendererError) -> ExportApiError {
    match error {
        PdfRendererError::Timeout => ExportApiError::timeout(),
        PdfRendererError::Unavailable => ExportApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pdf_renderer_unavailable",
            "PDF renderer is temporarily unavailable",
            true,
        ),
        PdfRendererError::PackageLimitExceeded | PdfRendererError::ResponseLimitExceeded => {
            ExportApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "export_limit_exceeded",
                "PDF export exceeds a configured limit",
                false,
            )
        }
        PdfRendererError::InvalidConfiguration => storage_error(),
        PdfRendererError::InvalidPackage
        | PdfRendererError::ConversionFailed
        | PdfRendererError::InvalidResponse => ExportApiError::new(
            StatusCode::BAD_GATEWAY,
            "pdf_renderer_failed",
            "PDF renderer failed to convert the note",
            true,
        ),
    }
}

fn storage_error() -> ExportApiError {
    ExportApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "export_storage_failure",
        "Note export storage operation failed",
        true,
    )
}

fn pdf_response(
    pdf: Vec<u8>,
    title: &str,
    note_id: &str,
    revision: i64,
) -> Result<Response, ExportApiError> {
    let filename = export_filename(title, note_id, ExportFormat::Pdf);
    let ascii = ascii_filename_fallback(&filename, note_id);
    let encoded = utf8_percent_encode(&filename, NON_ALPHANUMERIC).to_string();
    let disposition = HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .map_err(|_| storage_error())?;
    let mut response = pdf.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    response.headers_mut().insert(
        "x-note-revision",
        HeaderValue::from_str(&revision.to_string()).map_err(|_| storage_error())?,
    );
    Ok(response)
}

fn ascii_filename_fallback(filename: &str, note_id: &str) -> String {
    let ascii = filename
        .chars()
        .filter(|ch| ch.is_ascii() && !ch.is_ascii_control() && !matches!(ch, '"' | '\\'))
        .collect::<String>();
    let ascii = ascii.trim();
    if ascii.is_empty() || ascii == ".pdf" {
        format!("note-{note_id}.pdf")
    } else {
        ascii.to_owned()
    }
}

pub fn export_router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    AppState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(capabilities_handler))
        .routes(routes!(pdf_export_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_errors_map_to_the_required_statuses_and_codes() {
        let cases = [
            (
                NoteExportError::NoteNotFound,
                StatusCode::NOT_FOUND,
                "note_not_found",
            ),
            (
                NoteExportError::StaleRevision {
                    expected: 1,
                    current: 2,
                },
                StatusCode::CONFLICT,
                "stale_revision",
            ),
            (
                NoteExportError::MarkdownLimitExceeded,
                StatusCode::PAYLOAD_TOO_LARGE,
                "export_limit_exceeded",
            ),
            (
                NoteExportError::AssetMissing {
                    destination: "image.png".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "export_asset_invalid",
            ),
            (
                NoteExportError::StorageFailure,
                StatusCode::INTERNAL_SERVER_ERROR,
                "export_storage_failure",
            ),
        ];
        for (input, status, code) in cases {
            let actual = map_pipeline_error(input);
            assert_eq!(actual.status, status);
            assert_eq!(actual.code, code);
        }
    }

    #[test]
    fn renderer_errors_map_without_exposing_diagnostics() {
        let cases = [
            (
                PdfRendererError::ConversionFailed,
                StatusCode::BAD_GATEWAY,
                "pdf_renderer_failed",
            ),
            (
                PdfRendererError::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "pdf_renderer_unavailable",
            ),
            (
                PdfRendererError::Timeout,
                StatusCode::GATEWAY_TIMEOUT,
                "pdf_export_timeout",
            ),
            (
                PdfRendererError::ResponseLimitExceeded,
                StatusCode::PAYLOAD_TOO_LARGE,
                "export_limit_exceeded",
            ),
        ];
        for (input, status, code) in cases {
            let actual = map_renderer_error(input);
            assert_eq!(actual.status, status);
            assert_eq!(actual.code, code);
            assert!(!actual.message.contains("renderer body"));
        }
    }

    #[test]
    fn header_fallback_is_ascii_and_cannot_inject_fields() {
        assert_eq!(ascii_filename_fallback("会議 notes.pdf", "n1"), "notes.pdf");
        assert_eq!(ascii_filename_fallback("\r\n\"\\.pdf", "n1"), "note-n1.pdf");
    }
}
