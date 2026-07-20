use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::Response,
    Json,
};
use note_core::SystemConfig;
use note_pipelines::{
    create_backup_archive, export_data, get_system_config, get_system_info, update_system_config,
    Context, SystemInfo,
};
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

#[utoipa::path(
    get,
    path = "/api/system/config",
    tag = "system",
    responses(
        (status = 200, description = "System configuration", body = crate::openapi::SystemConfigSchema),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn get_config_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<SystemConfig>, (StatusCode, String)> {
    get_system_config(&ctx)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[utoipa::path(
    put,
    path = "/api/system/config",
    tag = "system",
    request_body = crate::openapi::SystemConfigSchema,
    responses(
        (status = 204, description = "System configuration updated"),
        (status = 400, description = "Invalid system configuration", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    get,
    path = "/api/system/info",
    tag = "system",
    responses(
        (status = 200, description = "System information", body = crate::openapi::SystemInfoSchema),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn get_info_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<SystemInfo>, (StatusCode, String)> {
    get_system_info(&ctx)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[utoipa::path(
    get,
    path = "/api/system/backup",
    tag = "system",
    responses(
        (status = 200, description = "System backup archive", body = inline(crate::openapi::Binary), content_type = "application/gzip"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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
    OpenApiRouter::new()
        .routes(routes!(get_config_handler, update_config_handler))
        .routes(routes!(get_info_handler))
        .routes(routes!(get_backup_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, Router};
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

    fn system_openapi_document() -> serde_json::Value {
        let (_, openapi) = system_router().split_for_parts();
        serde_json::to_value(openapi).unwrap()
    }

    #[test]
    fn openapi_contains_all_system_operations_and_contracts() {
        let document = system_openapi_document();

        for (path, method) in [
            ("/api/system/config", "get"),
            ("/api/system/config", "put"),
            ("/api/system/info", "get"),
            ("/api/system/backup", "get"),
        ] {
            assert!(
                document["paths"][path][method].is_object(),
                "missing {method} {path}"
            );
            assert_eq!(document["paths"][path][method]["tags"][0], "system");
        }

        let get_config = &document["paths"]["/api/system/config"]["get"];
        assert_eq!(
            get_config["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SystemConfigSchema"
        );
        assert_eq!(
            get_config["responses"]["500"]["content"]["text/plain"]["schema"]["type"],
            "string"
        );

        let put_config = &document["paths"]["/api/system/config"]["put"];
        assert_eq!(
            put_config["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SystemConfigSchema"
        );
        assert!(put_config["responses"]["204"].get("content").is_none());
        for status in ["400", "500"] {
            assert_eq!(
                put_config["responses"][status]["content"]["text/plain"]["schema"]["type"],
                "string"
            );
        }

        let get_info = &document["paths"]["/api/system/info"]["get"];
        assert_eq!(
            get_info["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SystemInfoSchema"
        );
        assert_eq!(
            get_info["responses"]["500"]["content"]["text/plain"]["schema"]["type"],
            "string"
        );

        let get_backup = &document["paths"]["/api/system/backup"]["get"];
        assert_eq!(
            get_backup["responses"]["200"]["content"]["application/gzip"]["schema"]["type"],
            "string"
        );
        assert_eq!(
            get_backup["responses"]["200"]["content"]["application/gzip"]["schema"]["format"],
            "binary"
        );
        assert_eq!(
            get_backup["responses"]["500"]["content"]["text/plain"]["schema"]["type"],
            "string"
        );
    }

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
