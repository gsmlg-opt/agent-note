use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt as _;
use note_attachments::{
    AttachmentStore, AttachmentStoreInfo, BoundedReadError, DeleteObjectOutcome,
    FilesystemAttachmentStore, PutObjectRequest, StoredObject,
};
use note_core::NoteAttachment;
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
    let store: Arc<dyn AttachmentStore> = Arc::new(FilesystemAttachmentStore::new(
        dir.path().join("attachments"),
    ));
    test_app_with_note(config, renderer, "Hello export".into(), vec![], store, dir).await
}

async fn test_app_with_note(
    config: PdfExportConfig,
    renderer: Arc<dyn PdfRenderer>,
    content: String,
    attachments: Vec<NoteAttachment>,
    store: Arc<dyn AttachmentStore>,
    dir: tempfile::TempDir,
) -> (axum::Router, String, i64, tempfile::TempDir) {
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let note_ctx = Arc::new(Context::new(storage.clone(), Arc::new(StubEmbedder), store));
    let note = save_note(
        &note_ctx,
        SaveNoteInput {
            title: "会議 \"notes\"\r\n".into(),
            content,
            attachments,
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

async fn assert_typed_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
    retryable: bool,
) -> serde_json::Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    if status == StatusCode::TOO_MANY_REQUESTS {
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
    }
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert_eq!(body["code"], code);
    assert!(body["message"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(body["details"].is_object());
    assert_eq!(body["retryable"], retryable);
    assert_eq!(body.as_object().unwrap().len(), 4);
    body
}

struct FailingReadStore;

#[async_trait]
impl AttachmentStore for FailingReadStore {
    async fn put_immutable(&self, request: PutObjectRequest) -> anyhow::Result<StoredObject> {
        Ok(StoredObject {
            object_key: request.object_key,
            size_bytes: request.bytes.len() as u64,
            checksum_sha256: request.checksum_sha256,
        })
    }

    async fn read_object_bounded(&self, _: &str, _: u64) -> anyhow::Result<Vec<u8>> {
        Err(BoundedReadError::StorageFailure.into())
    }

    async fn delete_object(&self, _: &str) -> anyhow::Result<DeleteObjectOutcome> {
        Ok(DeleteObjectOutcome::Deleted)
    }

    async fn read_legacy(&self, _: &str, _: &str) -> anyhow::Result<Vec<u8>> {
        Err(BoundedReadError::StorageFailure.into())
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "failing-test-store".into(),
            location: None,
        }
    }
}

struct GateRenderer {
    calls: AtomicUsize,
    release: tokio::sync::Semaphore,
    active: AtomicUsize,
    max_active: AtomicUsize,
    dropped: AtomicUsize,
}

struct ActiveRender<'a> {
    renderer: &'a GateRenderer,
}

impl Drop for ActiveRender<'_> {
    fn drop(&mut self) {
        self.renderer.active.fetch_sub(1, Ordering::SeqCst);
        self.renderer.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl PdfRenderer for GateRenderer {
    async fn render(
        &self,
        _: &ExportDocumentPackage,
        _: Duration,
    ) -> Result<Vec<u8>, PdfRendererError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _active = ActiveRender { renderer: self };
        self.release.acquire().await.unwrap().forget();
        Ok(b"%PDF-released".to_vec())
    }
}

#[tokio::test]
async fn capabilities_disclose_only_feature_availability() {
    for (config, pdf) in [
        (PdfExportConfig::default(), false),
        (
            PdfExportConfig {
                enabled: true,
                renderer_url: Some("http://renderer-secret.invalid:3000".into()),
                ..PdfExportConfig::default()
            },
            true,
        ),
    ] {
        let renderer = Arc::new(FixedRenderer {
            calls: AtomicUsize::new(0),
            result: Ok(b"%PDF-fixture".to_vec()),
        });
        let (app, _, _, _dir) = test_app(config, renderer).await;

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
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"markdown": true, "pdf": pdf})
        );
        assert!(!String::from_utf8_lossy(&body).contains("renderer-secret"));
    }
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

    assert_typed_error(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "pdf_export_disabled",
        false,
    )
    .await;
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
        assert_typed_error(response, expected_status, expected_code, false).await;
    }
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn route_maps_all_pre_render_failures_without_calling_renderer() {
    async fn request(app: axum::Router, id: &str, revision: i64) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .uri(format!(
                    "/api/notes/{id}/export/pdf?expected_revision={revision}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    }

    let enabled = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        ..PdfExportConfig::default()
    };

    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let (app, _id, _revision, _dir) = test_app(enabled.clone(), renderer.clone()).await;
    assert_typed_error(
        request(app, "does-not-exist", 1).await,
        StatusCode::NOT_FOUND,
        "note_not_found",
        false,
    )
    .await;
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);

    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let pipeline_limit = PdfExportConfig {
        max_markdown_bytes: 4,
        ..enabled.clone()
    };
    let (app, id, revision, _dir) = test_app(pipeline_limit, renderer.clone()).await;
    assert_typed_error(
        request(app, &id, revision).await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "export_limit_exceeded",
        false,
    )
    .await;
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);

    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn AttachmentStore> = Arc::new(FilesystemAttachmentStore::new(
        dir.path().join("attachments"),
    ));
    let invalid_data = "![bad](data:image/png;base64,not-valid-base64)".to_string();
    let (app, id, revision, _dir) = test_app_with_note(
        enabled.clone(),
        renderer.clone(),
        invalid_data,
        vec![],
        store,
        dir,
    )
    .await;
    assert_typed_error(
        request(app, &id, revision).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "export_asset_invalid",
        false,
    )
    .await;
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);

    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn AttachmentStore> = Arc::new(FilesystemAttachmentStore::new(
        dir.path().join("attachments"),
    ));
    let deeply_nested = format!("{}text{}", "<div>".repeat(300), "</div>".repeat(300));
    let (app, id, revision, _dir) = test_app_with_note(
        enabled.clone(),
        renderer.clone(),
        deeply_nested,
        vec![],
        store,
        dir,
    )
    .await;
    assert_typed_error(
        request(app, &id, revision).await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "export_limit_exceeded",
        false,
    )
    .await;
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 0);

    let renderer = Arc::new(FixedRenderer {
        calls: AtomicUsize::new(0),
        result: Ok(b"%PDF-fixture".to_vec()),
    });
    let dir = tempfile::tempdir().unwrap();
    let attachment = NoteAttachment {
        id: "image".into(),
        path: "image.png".into(),
        mime: "image/png".into(),
        description: String::new(),
        content: b"not important".to_vec(),
        storage: None,
    };
    let (app, id, revision, _dir) = test_app_with_note(
        enabled,
        renderer.clone(),
        "![image](image.png)".into(),
        vec![attachment],
        Arc::new(FailingReadStore),
        dir,
    )
    .await;
    assert_typed_error(
        request(app, &id, revision).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "export_storage_failure",
        true,
    )
    .await;
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

    assert!(started.elapsed() < Duration::from_secs(2));
    assert_typed_error(
        response,
        StatusCode::GATEWAY_TIMEOUT,
        "pdf_export_timeout",
        true,
    )
    .await;
}

#[tokio::test]
async fn client_cancellation_does_not_release_remote_render_admission() {
    let renderer = Arc::new(GateRenderer {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
        dropped: AtomicUsize::new(0),
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
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let busy = app.clone().oneshot(request()).await.unwrap();
    assert_typed_error(busy, StatusCode::TOO_MANY_REQUESTS, "export_busy", true).await;
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 1);

    first.abort();
    let _ = first.await;
    let still_busy = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(still_busy.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(renderer.calls.load(Ordering::SeqCst), 1);

    renderer.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let third = tokio::spawn(app.oneshot(request()));
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    renderer.release.add_permits(1);
    assert_eq!(third.await.unwrap().unwrap().status(), StatusCode::OK);
    assert_eq!(renderer.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(renderer.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelled_remote_render_releases_admission_at_its_absolute_deadline() {
    let renderer = Arc::new(GateRenderer {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
        dropped: AtomicUsize::new(0),
    });
    let config = PdfExportConfig {
        enabled: true,
        renderer_url: Some("http://renderer.invalid:3000".into()),
        total_deadline_secs: 1,
        renderer_timeout_secs: 1,
        max_in_flight: 1,
        ..PdfExportConfig::default()
    };
    let (app, id, revision, _dir) = test_app(config, renderer.clone()).await;
    let uri = format!("/api/notes/{id}/export/pdf?expected_revision={revision}");
    let request = || Request::builder().uri(&uri).body(Body::empty()).unwrap();

    let first = tokio::spawn(app.clone().oneshot(request()));
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    first.abort();
    let _ = first.await;

    let busy = app.clone().oneshot(request()).await.unwrap();
    assert_typed_error(busy, StatusCode::TOO_MANY_REQUESTS, "export_busy", true).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let second = tokio::spawn(app.oneshot(request()));
    tokio::time::timeout(Duration::from_secs(2), async {
        while renderer.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    renderer.release.add_permits(1);
    assert_eq!(second.await.unwrap().unwrap().status(), StatusCode::OK);
    assert_eq!(renderer.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(renderer.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn renderer_failures_use_safe_typed_transport_errors() {
    for (renderer_error, status, code, retryable) in [
        (
            PdfRendererError::ConversionFailed,
            StatusCode::BAD_GATEWAY,
            "pdf_renderer_failed",
            true,
        ),
        (
            PdfRendererError::InvalidResponse,
            StatusCode::BAD_GATEWAY,
            "pdf_renderer_failed",
            true,
        ),
        (
            PdfRendererError::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "pdf_renderer_unavailable",
            true,
        ),
        (
            PdfRendererError::Timeout,
            StatusCode::GATEWAY_TIMEOUT,
            "pdf_export_timeout",
            true,
        ),
        (
            PdfRendererError::ResponseLimitExceeded,
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_limit_exceeded",
            false,
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
        assert_typed_error(response, status, code, retryable).await;
    }
}

#[test]
fn openapi_registers_both_export_routes_and_response_headers() {
    let (_, openapi) = rest_router();
    let value = serde_json::to_value(openapi).unwrap();
    let paths = &value["paths"];
    assert!(paths.get("/api/export/capabilities").is_some());
    let pdf = &paths["/api/notes/{id}/export/pdf"]["get"];
    let expected_revision = pdf["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "expected_revision")
        .unwrap();
    assert_eq!(expected_revision["schema"]["minimum"], 1);
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
    for status in [
        "400", "404", "409", "413", "422", "429", "500", "502", "503", "504",
    ] {
        let response = &pdf["responses"][status];
        assert_eq!(
            response["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ExportApiError"
        );
        assert!(
            response["headers"].get("Cache-Control").is_some(),
            "missing no-store header for {status}"
        );
        assert!(
            response["headers"].get("X-Content-Type-Options").is_some(),
            "missing nosniff header for {status}"
        );
    }
    let capabilities = &paths["/api/export/capabilities"]["get"]["responses"]["200"];
    assert!(capabilities["headers"].get("Cache-Control").is_some());
}
