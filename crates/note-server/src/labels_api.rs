use axum::{
    extract::{FromRef, Path, State},
    Json,
};
use note_core::LabelKey;
use note_pipelines::{
    define_label_key_with_type, delete_label_key, list_label_keys, parse_label_value_type,
    update_label_key, update_label_key_with_type, Context,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DefineLabelKeyRequest {
    /// Label key. Selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, and `~`
    /// are not allowed.
    pub key: String,
    pub description: String,
    #[serde(default = "default_label_value_type")]
    #[schema(
        schema_with = label_value_type_with_text_default_schema,
        required = false
    )]
    pub value_type: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateLabelKeyRequest {
    pub description: String,
    #[serde(default)]
    #[schema(
        schema_with = crate::openapi::label_value_type_schema,
        required = false
    )]
    pub value_type: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LabelKeyDto {
    pub key: String,
    pub description: String,
    #[schema(schema_with = crate::openapi::label_value_type_schema)]
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

#[utoipa::path(
    post,
    path = "/api/labels",
    tag = "labels",
    request_body = DefineLabelKeyRequest,
    responses(
        (status = 200, description = "Label key defined"),
        (status = 400, description = "Invalid label key", body = String, content_type = "text/plain"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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
            // Invalid keys are the caller's fault (400); anything else (storage failure, or a
            // duplicate-key UNIQUE violation) falls through to 500. A duplicate arguably wants
            // 409, but mapping the backend-neutral StorageErrorKind::Constraint category is not
            // currently part of this handler's error handling.
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

#[utoipa::path(
    get,
    path = "/api/labels",
    tag = "labels",
    responses(
        (status = 200, description = "Label keys", body = Vec<LabelKeyDto>),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
async fn list_label_keys_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<LabelKeyDto>>, (axum::http::StatusCode, String)> {
    let keys = list_label_keys(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    put,
    path = "/api/labels/{key}",
    tag = "labels",
    params(("key" = String, Path, description = "Label key")),
    request_body = UpdateLabelKeyRequest,
    responses(
        (status = 200, description = "Label key updated"),
        (status = 400, description = "Invalid label key update", body = String, content_type = "text/plain")
    )
)]
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

#[utoipa::path(
    delete,
    path = "/api/labels/{key}",
    tag = "labels",
    params(("key" = String, Path, description = "Label key")),
    responses(
        (status = 200, description = "Label key deleted"),
        (status = 500, description = "Server error", body = String, content_type = "text/plain")
    )
)]
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

fn label_value_type_with_text_default_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::from(crate::openapi::label_value_type_schema())
        .default(Some(serde_json::json!("text")))
        .build()
}

pub fn labels_router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Arc<Context>: FromRef<S>,
{
    // GET has no body (unlike /api/notes/search), so a browser GET is safe here.
    OpenApiRouter::new()
        .routes(routes!(define_label_key_handler, list_label_keys_handler))
        .routes(routes!(update_label_key_handler, delete_label_key_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use serde_json::Value;
    use tower::ServiceExt;

    fn label_openapi_document() -> Value {
        let (_, openapi) = labels_router::<Arc<Context>>().split_for_parts();
        serde_json::to_value(openapi).unwrap()
    }

    #[test]
    fn openapi_contains_all_label_operations_and_contracts() {
        let document = label_openapi_document();

        for (path, methods) in [
            ("/api/labels", &["get", "post"][..]),
            ("/api/labels/{key}", &["put", "delete"][..]),
        ] {
            for method in methods {
                assert!(
                    document["paths"][path][method].is_object(),
                    "missing {method} {path}"
                );
                assert_eq!(document["paths"][path][method]["tags"][0], "labels");
            }
        }

        let post = &document["paths"]["/api/labels"]["post"];
        assert_eq!(
            post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/DefineLabelKeyRequest"
        );
        for status in ["400", "500"] {
            assert_eq!(
                post["responses"][status]["content"]["text/plain"]["schema"]["type"],
                "string"
            );
        }
        assert!(post["responses"]["200"].get("content").is_none());

        let get = &document["paths"]["/api/labels"]["get"];
        assert_eq!(
            get["responses"]["200"]["content"]["application/json"]["schema"]["type"],
            "array"
        );
        assert_eq!(
            get["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
            "#/components/schemas/LabelKeyDto"
        );
        assert_eq!(
            get["responses"]["500"]["content"]["text/plain"]["schema"]["type"],
            "string"
        );

        for (method, error_status) in [("put", "400"), ("delete", "500")] {
            let operation = &document["paths"]["/api/labels/{key}"][method];
            let key = operation["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == "key")
                .expect("key path parameter");
            assert_eq!(key["in"], "path");
            assert_eq!(key["required"], true);
            assert_eq!(key["schema"]["type"], "string");
            assert!(operation["responses"]["200"].get("content").is_none());
            assert_eq!(
                operation["responses"][error_status]["content"]["text/plain"]["schema"]["type"],
                "string"
            );
        }

        assert_eq!(
            document["paths"]["/api/labels/{key}"]["put"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/UpdateLabelKeyRequest"
        );

        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["DefineLabelKeyRequest"]["properties"]["value_type"]["default"],
            "text"
        );
        assert!(!schemas["DefineLabelKeyRequest"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("value_type")));
        assert!(!schemas["UpdateLabelKeyRequest"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("value_type")));
        for schema in [
            "DefineLabelKeyRequest",
            "UpdateLabelKeyRequest",
            "LabelKeyDto",
        ] {
            assert_eq!(
                schemas[schema]["properties"]["value_type"]["enum"],
                serde_json::json!(["text", "number", "version", "date", "datetime", "time"])
            );
        }
    }

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
        (labels_router::<Arc<Context>>().with_state(ctx).into(), dir)
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
    async fn selector_reserved_key_returns_400() {
        let (app, _dir) = test_app().await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/labels")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":"project$name","description":"d"}"#))
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
