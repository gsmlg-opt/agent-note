use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt as _;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::org::{OrgContext, SystemOrgClock};
use note_pipelines::{save_note, Context, SaveNoteInput};
use note_server::config::PdfExportConfig;
use note_server::export::document::ExportDocumentPackage;
use note_server::export::renderer::{PdfRenderer, PdfRendererError};
use note_server::{openapi::rest_router, AppState};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt as _;

struct FixedRenderer {
    calls: AtomicUsize,
    result: Result<Vec<u8>, PdfRendererError>,
}

#[async_trait]
impl PdfRenderer for FixedRenderer {
    async fn render(
        &self,
        _: &ExportDocumentPackage,
        _: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.clone()
    }
}

async fn test_app(
    config: PdfExportConfig,
    renderer: Arc<dyn PdfRenderer>,
) -> (axum::Router, String, i64, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let note_ctx = Arc::new(Context::new(
        storage.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    ));
    let note = save_note(
        &note_ctx,
        SaveNoteInput {
            title: "会議 \"notes\"\r\n".into(),
            content: "Hello export".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let org = Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock)));
    let state = AppState::new(note_ctx, org).with_pdf_export(config, renderer);
    let (router, _) = rest_router();
    (router.with_state(state), note.id, note.revision, dir)
}

struct GateRenderer {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
}

#[async_trait]
impl PdfRenderer for GateRenderer {
    async fn render(
        &self,
        _: &ExportDocumentPackage,
        _: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_waiters();
        std::future::pending().await
    }
}

#[tokio::test]
async fn capabilities_disclose_only_feature_availability() {
    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let (app, _, _, _dir) = test_app(PdfExportConfig::default(), renderer).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/export/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"markdown": true, "pdf": false})
    );
}

#[tokio::test]
async fn disabled_pdf_route_returns_typed_no_store_error_without_renderer_call() {
    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let (app, id, revision, _dir) = test_app(PdfExportConfig::default(), renderer.clone()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/notes/{id}/export/pdf?expected_revision={revision}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "pdf_export_disabled"
    );
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pdf_success_has_safe_download_headers_and_requested_revision() {
    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let config = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        ..PdfExportConfig::default()
    };
    let (app, id, revision, _dir) = test_app(config, renderer.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/notes/{id}/export/pdf?expected_revision={revision}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["x-note-revision"], revision.to_string());
    let disposition = response.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap();
    assert!(disposition.starts_with("attachment; filename=\"notes.pdf\"; filename*=UTF-8''"));
    assert!(!disposition.contains('\r'));
    assert!(!disposition.contains('\n'));
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"%PDF-fixture".as_slice()
    );
}

#[tokio::test]
async fn invalid_or_stale_revision_never_calls_renderer_and_errors_are_no_store() {
    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let config = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        ..PdfExportConfig::default()
    };
    let (app, id, revision, _dir) = test_app(config, renderer.clone()).await;

    for (query, expected_status, expected_code) in [
        (String::new(), StatusCode::BAD_REQUEST, "invalid_input"),
        (
            "expected_revision=0".to_string(),
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            format!("expected_revision={}", revision + 1),
            StatusCode::CONFLICT,
            "stale_revision",
        ),
        (
            format!("expected_revision={revision}&url=http%3A%2F%2Fexample.com"),
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/notes/{id}/export/pdf?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
            expected_code
        );
    }
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);
}

struct SlowRenderer;

#[async_trait]
impl PdfRenderer for SlowRenderer {
    async fn render(
        &self,
        _: &ExportDocumentPackage,
        _: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(b"%PDF-late".to_vec())
    }
}

#[tokio::test]
async fn total_deadline_bounds_the_complete_export_operation() {
    let config = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        total_deadline_secs: 1,
        renderer_timeout_secs: 1,
        ..PdfExportConfig::default()
    };
    let (app, id, revision, _dir) = test_app(config, Arc::new(SlowRenderer)).await;
    let started = std::time::Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/notes/{id}/export/pdf?expected_revision={revision}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(2));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "pdf_export_timeout"
    );
}

#[tokio::test]
async fn saturation_rejects_without_queueing_and_cancellation_releases_the_permit() {
    let renderer = Arc::new(GateRenderer {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
    });
    let config = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        max_in_flight: 1,
        ..PdfExportConfig::default()
    };
    let (app, id, revision, _dir) = test_app(config, renderer.clone()).await;
    let uri = format!("/api/notes/{id}/export/pdf?expected_revision={revision}");
    let request = || Request::builder().uri(&uri).body(Body::empty()).unwrap();

    let first = tokio::spawn(app.clone().oneshot(request()));
    tokio::time::timeout(Duration::from_secs(2), renderer.entered.notified())
        .await
        .unwrap();
    let busy = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(busy.headers()[header::RETRY_AFTER], "2");
    let body = busy.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "export_busy"
    );
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 1);

    first.abort();
    let _ = first.await;
    let third = tokio::spawn(app.oneshot(request()));
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    third.abort();
    let _ = third.await;
}

#[tokio::test]
async fn renderer_failures_use_safe_typed_transport_errors() {
    for (renderer_error, status, code) in [
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
    ] {
        let renderer = Arc::new(FixedRenderer {
            calls: AtomicUsize::new(0),
            result: Err(renderer_error),
        });
        let config = PdfExportConfig {
            enabled: true,
            renderer_url: Some("http://renderer.invalid:3000".into()),
            ..PdfExportConfig::default()
        };
        let (app, id, revision, _dir) = test_app(config, renderer).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/notes/{id}/export/pdf?expected_revision={revision}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        assert_eq!(body["code"], code);
        assert!(body.get("message").unwrap().as_str().is_some());
        assert_eq!(body.get("details").unwrap(), &serde_json::json!({}));
        assert!(body.get("retryable").unwrap().is_boolean());
    }
}

#[test]
fn openapi_registers_both_export_routes_and_response_headers() {
    let (_, openapi) = rest_router();
    let value = serde_json::to_value(openapi).unwrap();
    let paths = &value["paths"];
    assert!(paths.get("/api/export/capabilities").is_some());
    let pdf = &paths["/api/notes/{id}/export/pdf"]["get"];
    assert!(pdf["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|parameter| parameter["name"] == "expected_revision"));
    let headers = &pdf["responses"]["200"]["headers"];
    for name in [
        "Cache-Control",
        "X-Content-Type-Options",
        "X-Note-Revision",
        "Content-Disposition",
    ] {
        assert!(headers.get(name).is_some(), "missing OpenAPI header {name}");
    }
    assert!(pdf["responses"]["429"]["headers"]
        .get("Retry-After")
        .is_some());
}
