use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use note_core::SystemConfig;
use note_pipelines::{
    get_system_config, get_system_info, update_system_config, Context, SystemInfo,
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

pub fn system_router() -> Router<Arc<Context>> {
    Router::new()
        .route(
            "/api/system/config",
            get(get_config_handler).put(update_config_handler),
        )
        .route("/api/system/info", get(get_info_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use note_embedding::StubEmbedder;
    use note_storage::Storage;
    use tower::ServiceExt;

    async fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Arc::new(Context::with_attachment_dir(
            Arc::new(storage),
            Arc::new(StubEmbedder),
            dir.path().join("attachments"),
        ));
        (system_router().with_state(ctx), dir)
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
        let (app, _dir) = test_app().await;
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
        let (app, _dir) = test_app().await;
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
        let (app, _dir) = test_app().await;
        let response = app
            .oneshot(request("GET", "/api/system/info", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(info["database_path"].as_str().unwrap().ends_with("test.db"));
        assert!(info["database_size_bytes"].as_u64().unwrap() > 0);
        assert!(info["attachments_path"]
            .as_str()
            .unwrap()
            .ends_with("attachments"));
    }
}
