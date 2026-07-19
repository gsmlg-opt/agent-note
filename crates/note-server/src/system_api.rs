use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use note_core::SystemConfig;
use note_pipelines::{
    create_backup_archive, export_data, get_system_config, get_system_info, update_system_config,
    Context, SystemInfo,
};
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;

async fn get_config_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<SystemConfig>, (StatusCode, String)> {
    get_system_config(&ctx)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn update_config_handler(
    State(ctx): State<Arc<Context>>,
    Json(config): Json<SystemConfig>,
) -> Result<StatusCode, (StatusCode, String)> {
    update_system_config(&ctx, &config)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|error| {
            let status = if error
                .downcast_ref::<note_core::SystemConfigValidationError>()
                .is_some()
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, error.to_string())
        })
}

async fn get_info_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<SystemInfo>, (StatusCode, String)> {
    get_system_info(&ctx)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn get_backup_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Response, (StatusCode, String)> {
    let data = export_data(&ctx)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let archive = tokio::task::spawn_blocking(move || create_backup_archive(&data))
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"agent-note-backup-{timestamp}.tar.gz\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Body::from(archive))
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

pub fn system_router() -> OpenApiRouter<Arc<Context>> {
    Router::new()
        .route(
            "/api/system/config",
            get(get_config_handler).put(update_config_handler),
        )
        .route("/api/system/info", get(get_info_handler))
        .route("/api/system/backup", get(get_backup_handler))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use flate2::read::GzDecoder;
    use http_body_util::BodyExt;
    use note_attachments::{FilesystemAttachmentStore, S3AttachmentConfig, S3AttachmentStore};
    use note_core::NoteAttachment;
    use note_embedding::{
        EmbeddingBackendInfo, OpenAiCompatibleConfig, OpenAiCompatibleEmbedder, StubEmbedder,
    };
    use note_pipelines::{save_note, EmbeddingJobNotifier, SaveNoteInput};
    use note_storage::{
        BackendInfo, StorageBackend, StorageError, StorageErrorKind, StorageResult, StorageSession,
        StorageTransaction, TransactionMode,
    };
    use note_storage_turso::TursoStorage;
    use std::{io::Read, time::Duration};
    use tower::ServiceExt;

    struct InfoBackend {
        _connection_url: String,
    }

    struct NoopNotifier;

    impl EmbeddingJobNotifier for NoopNotifier {
        fn wake(&self) {}
    }

    #[async_trait::async_trait]
    impl StorageBackend for InfoBackend {
        async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
            Err(StorageError::new(
                StorageErrorKind::Operation,
                "session not used by system info",
            ))
        }

        async fn begin(
            &self,
            _mode: TransactionMode,
        ) -> StorageResult<Box<dyn StorageTransaction>> {
            Err(StorageError::new(
                StorageErrorKind::Operation,
                "transaction not used by system info",
            ))
        }

        async fn info(&self) -> StorageResult<BackendInfo> {
            Ok(BackendInfo {
                engine: "pg".into(),
                location: None,
                size_bytes: None,
            })
        }
    }

    async fn test_app() -> (Router, Arc<Context>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let ctx = Arc::new(Context::new(
            storage,
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        (system_router().with_state(ctx.clone()).into(), ctx, dir)
    }

    fn request(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn config_defaults_and_roundtrips() {
        let (app, _ctx, _dir) = test_app().await;
        let response = app
            .clone()
            .oneshot(request("GET", "/api/system/config", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(config["duplicate_check"]["enabled"], false);

        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/system/config",
                r#"{"duplicate_check":{"enabled":true,"rules":[{"terms":[{"key":"skill-name"},{"key":"version"}]}]}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(request("GET", "/api/system/config", ""))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(config["duplicate_check"]["enabled"], true);
        assert_eq!(
            config["duplicate_check"]["rules"][0]["terms"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn invalid_config_returns_bad_request() {
        let (app, _ctx, _dir) = test_app().await;
        let response = app
            .oneshot(request(
                "PUT",
                "/api/system/config",
                r#"{"duplicate_check":{"enabled":true,"rules":[{"terms":[]}]}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn info_reports_read_only_storage_fields() {
        let (app, _ctx, _dir) = test_app().await;
        let response = app
            .oneshot(request("GET", "/api/system/info", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(info["database_engine"], "embed");
        assert!(info["database_path"].as_str().unwrap().ends_with("test.db"));
        assert!(info["database_size_bytes"].as_u64().unwrap() > 0);
        assert_eq!(info["attachments_engine"], "filesystem");
        assert!(info["attachments_location"]
            .as_str()
            .unwrap()
            .ends_with("attachments"));
    }

    #[tokio::test]
    async fn info_reports_safe_s3_location_without_endpoint_or_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
            S3AttachmentStore::new(S3AttachmentConfig {
                bucket: "agent-note".into(),
                prefix: "attachments".into(),
                region: Some("us-east-1".into()),
                endpoint: Some("http://minio.internal:9000".into()),
                force_path_style: true,
            })
            .await
            .unwrap(),
        );
        let ctx = Arc::new(Context::new(storage, Arc::new(StubEmbedder), attachments));
        let app: Router = system_router().with_state(ctx).into();
        let response = app
            .oneshot(request("GET", "/api/system/info", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(info["attachments_engine"], "s3");
        assert_eq!(info["attachments_location"], "s3://agent-note/attachments");
        let rendered = serde_json::to_string(&info).unwrap();
        assert!(!rendered.contains("minio.internal"));
        assert!(!rendered.contains("AWS_ACCESS_KEY_ID"));
        assert!(!rendered.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!rendered.contains("minioadmin"));
    }

    #[tokio::test]
    async fn info_reports_pg_and_openai_without_local_fields_or_credentials() {
        const PASSWORD: &str = "do-not-render-this-secret";
        const BASE_URL: &str = "https://embedding.example/private";
        const TOKEN: &str = "secret-token";
        const API_KEY_ENV: &str = "EMBEDDING_API_KEY";
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(InfoBackend {
            _connection_url: format!("postgresql://agent:{PASSWORD}@database/notes"),
        });
        let embedder = OpenAiCompatibleEmbedder::new(OpenAiCompatibleConfig {
            base_url: BASE_URL.into(),
            model: "bge-m3".into(),
            bearer_token: Some(TOKEN.into()),
            timeout: Duration::from_secs(1),
            max_retries: 0,
        })
        .unwrap();
        let ctx = Arc::new(Context::with_embedding_job_notifier(
            storage,
            Arc::new(embedder),
            EmbeddingBackendInfo::openai("bge-m3"),
            Arc::new(NoopNotifier),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        let app: Router = system_router().with_state(ctx).into();

        let response = app
            .oneshot(request("GET", "/api/system/info", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(info["database_engine"], "pg");
        assert!(info["database_path"].is_null());
        assert!(info["database_size_bytes"].is_null());
        assert_eq!(info["embedding_engine"], "openai");
        assert_eq!(info["embedding_model"], "bge-m3");
        assert_eq!(info["embedding_fingerprint"], "bge-m3:1024");
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!body.contains(PASSWORD));
        assert!(!body.contains(BASE_URL));
        assert!(!body.contains(TOKEN));
        assert!(!body.contains(API_KEY_ENV));
        assert!(!body.contains("authorization"));
    }

    #[tokio::test]
    async fn backup_download_contains_all_note_data() {
        let (app, ctx, _dir) = test_app().await;
        save_note(
            &ctx,
            SaveNoteInput {
                title: "Backup me".into(),
                content: "Complete note content".into(),
                attachments: vec![NoteAttachment {
                    id: "details".into(),
                    path: "./details.txt".into(),
                    mime: "text/plain".into(),
                    description: "Backup details".into(),
                    content: b"attachment content".to_vec(),
                }],
                labels: vec![("status".into(), "ready".into())],
            },
        )
        .await
        .unwrap();

        let response = app
            .oneshot(request("GET", "/api/system/backup", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/gzip");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let disposition = response.headers()["content-disposition"].to_str().unwrap();
        assert!(disposition.starts_with("attachment; filename=\"agent-note-backup-"));
        assert!(disposition.ends_with(".tar.gz\""));

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_ref()));
        let mut entries = archive.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().to_str(), Some("notes.json"));
        let mut json = String::new();
        entry.read_to_string(&mut json).unwrap();
        assert!(entries.next().is_none());

        let export: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(export["version"], 2);
        assert_eq!(export["notes"].as_array().unwrap().len(), 1);
        assert_eq!(export["notes"][0]["title"], "Backup me");
        assert_eq!(export["notes"][0]["content"], "Complete note content");
        assert_eq!(
            export["notes"][0]["attachments"][0]["content"],
            "attachment content"
        );
        assert_eq!(
            export["notes"][0]["attachments"][0]["content_base64"],
            "YXR0YWNobWVudCBjb250ZW50"
        );
        assert_eq!(export["notes"][0]["labels"][0][0], "status");
        assert_eq!(export["notes"][0]["labels"][0][1], "ready");
    }
}
