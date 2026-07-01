use axum::{extract::State, routing::post, Json, Router};
use note_core::LabelKey;
use note_pipelines::{define_label_key, list_label_keys, Context};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct DefineLabelKeyRequest {
    pub key: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct LabelKeyDto {
    pub key: String,
    pub description: String,
}

impl From<LabelKey> for LabelKeyDto {
    fn from(lk: LabelKey) -> Self {
        Self {
            key: lk.key,
            description: lk.description,
        }
    }
}

async fn define_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<DefineLabelKeyRequest>,
) -> Result<(), (axum::http::StatusCode, String)> {
    define_label_key(&ctx, &req.key, &req.description)
        .await
        .map_err(|e| {
            // Empty key/description are the caller's fault (400); anything else (storage failure,
            // or a duplicate-key UNIQUE violation) falls through to 500. A duplicate arguably wants
            // 409, but distinguishing it from a genuine storage error needs libsql-error inspection
            // we don't do here — noted as a minor known limitation.
            let status = if e.downcast_ref::<note_core::LabelKeyValidationError>().is_some() {
                axum::http::StatusCode::BAD_REQUEST
            } else {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e.to_string())
        })
}

async fn list_label_keys_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<LabelKeyDto>>, (axum::http::StatusCode, String)> {
    let keys = list_label_keys(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub fn labels_router() -> Router<Arc<Context>> {
    // GET has no body (unlike /api/notes/search), so a browser GET is safe here.
    Router::new().route(
        "/api/labels",
        post(define_label_key_handler).get(list_label_keys_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use note_embedding::StubEmbedder;
    use note_storage::Storage;
    use tower::ServiceExt;

    async fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_local(dir.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(StubEmbedder)));
        (labels_router().with_state(ctx), dir)
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
    async fn empty_description_returns_400() {
        let (app, _dir) = test_app().await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/labels")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":"status","description":""}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
