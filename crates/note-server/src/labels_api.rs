use axum::{
    extract::{Path, State},
    routing::{post, put},
    Json, Router,
};
use note_core::LabelKey;
use note_pipelines::{
    define_label_key_with_type, delete_label_key, list_label_keys, parse_label_value_type,
    update_label_key, update_label_key_with_type, Context,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct DefineLabelKeyRequest {
    pub key: String,
    pub description: String,
    #[serde(default = "default_label_value_type")]
    pub value_type: String,
}

#[derive(Deserialize)]
pub struct UpdateLabelKeyRequest {
    pub description: String,
    #[serde(default)]
    pub value_type: Option<String>,
}

#[derive(Serialize)]
pub struct LabelKeyDto {
    pub key: String,
    pub description: String,
    pub value_type: String,
}

impl From<LabelKey> for LabelKeyDto {
    fn from(lk: LabelKey) -> Self {
        Self {
            key: lk.key,
            description: lk.description,
            value_type: lk.value_type.as_str().to_string(),
        }
    }
}

async fn define_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<DefineLabelKeyRequest>,
) -> Result<(), (axum::http::StatusCode, String)> {
    let value_type = parse_label_value_type(&req.value_type).map_err(|e| {
        let status = if e
            .downcast_ref::<note_core::LabelKeyValidationError>()
            .is_some()
        {
            axum::http::StatusCode::BAD_REQUEST
        } else {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;
    define_label_key_with_type(&ctx, &req.key, &req.description, value_type)
        .await
        .map_err(|e| {
            // Empty keys are the caller's fault (400); anything else (storage failure, or a
            // duplicate-key UNIQUE violation) falls through to 500. A duplicate arguably wants 409,
            // but mapping the backend-neutral StorageErrorKind::Constraint category is not currently
            // part of this handler's error handling.
            let status = if e
                .downcast_ref::<note_core::LabelKeyValidationError>()
                .is_some()
            {
                axum::http::StatusCode::BAD_REQUEST
            } else {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e.to_string())
        })?;
    crate::notes_api::invalidate_dashboard_cache();
    Ok(())
}

async fn list_label_keys_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<LabelKeyDto>>, (axum::http::StatusCode, String)> {
    let keys = list_label_keys(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

async fn update_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Path(key): Path<String>,
    Json(req): Json<UpdateLabelKeyRequest>,
) -> Result<(), (axum::http::StatusCode, String)> {
    match req.value_type {
        Some(value_type) => {
            let value_type = parse_label_value_type(&value_type)
                .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
            update_label_key_with_type(&ctx, &key, &req.description, value_type)
                .await
                .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
            crate::notes_api::invalidate_dashboard_cache();
            Ok(())
        }
        None => {
            update_label_key(&ctx, &key, &req.description)
                .await
                .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
            crate::notes_api::invalidate_dashboard_cache();
            Ok(())
        }
    }
}

async fn delete_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Path(key): Path<String>,
) -> Result<(), (axum::http::StatusCode, String)> {
    delete_label_key(&ctx, &key)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::notes_api::invalidate_dashboard_cache();
    Ok(())
}

fn default_label_value_type() -> String {
    "text".to_string()
}

pub fn labels_router() -> Router<Arc<Context>> {
    // GET has no body (unlike /api/notes/search), so a browser GET is safe here.
    Router::new()
        .route(
            "/api/labels",
            post(define_label_key_handler).get(list_label_keys_handler),
        )
        .route(
            "/api/labels/{key}",
            put(update_label_key_handler).delete(delete_label_key_handler),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use tower::ServiceExt;

    async fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(dir.path().join("t.db")).await.unwrap());
        let ctx = Arc::new(Context::new(
            storage,
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        (labels_router().with_state(ctx), dir)
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

    #[tokio::test]
    async fn define_returns_200_and_list_returns_it() {
        let (app, _dir) = test_app().await;
        let define = Request::builder()
            .method("POST")
            .uri("/api/labels")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"key":"status","description":"Workflow status"}"#,
            ))
            .unwrap();
        let resp = app.clone().oneshot(define).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let list = Request::builder()
            .method("GET")
            .uri("/api/labels")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(list).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(arr
            .as_array()
            .expect("array")
            .iter()
            .any(|k| k.get("key").and_then(|v| v.as_str()) == Some("status")));
        let status = arr
            .as_array()
            .expect("array")
            .iter()
            .find(|k| k.get("key").and_then(|v| v.as_str()) == Some("status"))
            .expect("status label key");
        assert_eq!(
            status.get("value_type").and_then(|v| v.as_str()),
            Some("text")
        );
    }

    #[tokio::test]
    async fn define_with_value_type_roundtrips() {
        let (app, _dir) = test_app().await;
        let resp = app
            .clone()
            .oneshot(post(
                "/api/labels",
                r#"{"key":"version","description":"Release version","value_type":"version"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app.oneshot(get("/api/labels")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let version = arr
            .as_array()
            .expect("array")
            .iter()
            .find(|k| k.get("key").and_then(|v| v.as_str()) == Some("version"))
            .expect("version label key");
        assert_eq!(
            version.get("value_type").and_then(|v| v.as_str()),
            Some("version")
        );
    }

    #[tokio::test]
    async fn empty_key_returns_400() {
        let (app, _dir) = test_app().await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/labels")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":"","description":"d"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn empty_description_is_allowed() {
        let (app, _dir) = test_app().await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/labels")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":"status","description":""}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_label_key_changes_description() {
        let (app, _dir) = test_app().await;
        let resp = app
            .clone()
            .oneshot(post(
                "/api/labels",
                r#"{"key":"status","description":"old"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(put(
                "/api/labels/status",
                r#"{"description":"","value_type":"number"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app.oneshot(get("/api/labels")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let status = arr
            .as_array()
            .expect("array")
            .iter()
            .find(|k| k.get("key").and_then(|v| v.as_str()) == Some("status"))
            .expect("status label key");
        assert_eq!(status.get("description").and_then(|v| v.as_str()), Some(""));
        assert_eq!(
            status.get("value_type").and_then(|v| v.as_str()),
            Some("number")
        );
    }

    #[tokio::test]
    async fn delete_label_key_removes_it_from_list() {
        let (app, _dir) = test_app().await;
        let resp = app
            .clone()
            .oneshot(post(
                "/api/labels",
                r#"{"key":"status","description":"old"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(delete("/api/labels/status"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app.oneshot(get("/api/labels")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!arr
            .as_array()
            .expect("array")
            .iter()
            .any(|k| k.get("key").and_then(|v| v.as_str()) == Some("status")));
    }
}
