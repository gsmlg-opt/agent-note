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

pub fn system_router() -> Router<Arc<Context>> {
    Router::new()
        .route(
            "/api/system/config",
            get(get_config_handler).put(update_config_handler),
        )
        .route("/api/system/info", get(get_info_handler))
        .route("/api/system/backup", get(get_backup_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use flate2::read::GzDecoder;
    use http_body_util::BodyExt;
    use note_core::NoteAttachment;
    use note_embedding::StubEmbedder;
    use note_pipelines::{save_note, SaveNoteInput};
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use std::io::Read;
    use tower::ServiceExt;

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
            dir.path().join("attachments"),
        ));
        (system_router().with_state(ctx.clone()), ctx, dir)
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
        assert!(info["attachments_path"]
            .as_str()
            .unwrap()
            .ends_with("attachments"));
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
