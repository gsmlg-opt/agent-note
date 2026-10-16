use super::{ConnectionInput, ConnectionSummary, SaveError, SyncRuntimeStatus};
use crate::AppState;
use axum::{extract::State, http::StatusCode, Json};
use utoipa_axum::{router::OpenApiRouter, routes};

#[utoipa::path(get,path="/api/system/sync",tag="system",responses(
    (status=200,description="Safe sync connection profiles and runtime status",body=SyncRuntimeStatus),
    (status=500,description="Sync state unavailable",body=String,content_type="text/plain")
))]
async fn status(
    State(state): State<AppState>,
) -> Result<Json<SyncRuntimeStatus>, (StatusCode, String)> {
    state.sync.status(&state.note).await.map(Json).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "sync state unavailable".into(),
        )
    })
}
#[utoipa::path(post,path="/api/system/sync/retry",tag="system",responses(
    (status=204,description="Sync worker notified; connection remains asynchronous")
))]
async fn retry(State(state): State<AppState>) -> StatusCode {
    state.sync.retry();
    StatusCode::NO_CONTENT
}
#[utoipa::path(get,path="/api/system/sync/connections",tag="system",responses(
    (status=200,description="Connection profiles without credential paths",body=Vec<ConnectionSummary>)
))]
async fn connections(State(state): State<AppState>) -> Json<Vec<ConnectionSummary>> {
    Json(state.sync.connections())
}
#[utoipa::path(put,path="/api/system/sync/connections",tag="system",request_body=ConnectionInput,responses(
    (status=200,description="Connection saved and runtime notified",body=ConnectionSummary),
    (status=400,description="Invalid connection or unavailable credentials",body=String),
    (status=500,description="Connection could not be saved",body=String)
))]
async fn save_connection(
    State(state): State<AppState>,
    input: Result<Json<ConnectionInput>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ConnectionSummary>, (StatusCode, String)> {
    let Json(input) = input.map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "invalid sync connection request".into(),
        )
    })?;
    state.sync.save_connection(input).await.map(Json).map_err(|error| match error {
        SaveError::Invalid(message) => (StatusCode::BAD_REQUEST, message),
        SaveError::Persistence => (StatusCode::INTERNAL_SERVER_ERROR, "sync connections could not be saved; verify the server profile storage directory is writable".into()),
    })
}
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(status))
        .routes(routes!(retry))
        .routes(routes!(connections, save_connection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_client::SyncRuntime;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_pipelines::{
        org::{OrgContext, SystemOrgClock},
        Context,
    };
    use note_storage_turso::TursoStorage;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn app_with_runtime(runtime: Arc<SyncRuntime>, dir: &std::path::Path) -> axum::Router {
        let storage = Arc::new(TursoStorage::open(dir.join("api-db")).await.unwrap());
        let ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(dir.join("attachments"))),
        ));
        let state = AppState::new(
            ctx,
            Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock))),
        )
        .with_sync(runtime);
        let (app, _) = router().split_for_parts();
        app.with_state(state)
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri("/api/system/sync/connections")
            .header("content-type", "application/json")
            .body(
                body.map(|body| Body::from(body.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!(String::from_utf8_lossy(&bytes)));
        (status, value)
    }

    fn input(profile: &super::super::SyncConnection) -> serde_json::Value {
        serde_json::json!({
            "id": profile.id,
            "url": profile.url,
            "client_cert": profile.client_cert,
            "client_key": profile.client_key,
            "ca_cert": profile.ca_cert,
            "max_blob_bytes": profile.max_blob_bytes,
        })
    }

    #[tokio::test]
    async fn connection_updates_persist_restart_and_keep_omitted_credentials_without_exposing_paths(
    ) {
        let certs = super::super::tests::certificates();
        let path = certs.dir.path().join("connections.json");
        let runtime = SyncRuntime::open(&[], path.clone(), certs.dir.path().into()).unwrap();
        let app = app_with_runtime(runtime.clone(), certs.dir.path()).await;
        assert_eq!(
            call(&app, "GET", None).await,
            (StatusCode::OK, serde_json::json!([]))
        );
        let (status, summary) = call(&app, "PUT", Some(input(&certs.profile))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(summary["has_client_cert"], true);
        assert_eq!(summary["has_client_key"], true);
        assert_eq!(summary["has_ca_cert"], true);
        assert_eq!(summary["mode"], "mtls");
        assert!(summary["ssh"].is_null());
        assert_eq!(summary.as_object().unwrap().len(), 8);
        let serialized = summary.to_string();
        assert!(!serialized.contains("client.pem") && !serialized.contains("client.key"));
        assert!(!serialized.contains("PRIVATE KEY") && !serialized.contains("CERTIFICATE"));
        let edit =
            serde_json::json!({"id":"local", "url":certs.profile.url, "max_blob_bytes":1024});
        let (status, summary) = call(&app, "PUT", Some(edit)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(summary["has_ca_cert"], true);
        let edit = serde_json::json!({"id":"local", "url":certs.profile.url, "max_blob_bytes":1024, "ca_cert":""});
        let (status, summary) = call(&app, "PUT", Some(edit)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(summary["max_blob_bytes"], 1024);
        assert_eq!(summary["has_client_key"], true);
        assert_eq!(summary["has_ca_cert"], false);
        let before = call(&app, "GET", None).await.1;
        let mut fallback = certs.profile.clone();
        fallback.id = "obsolete-toml".into();
        let restarted =
            SyncRuntime::open(&[fallback], path.clone(), certs.dir.path().into()).unwrap();
        let restarted_app = app_with_runtime(restarted, certs.dir.path()).await;
        assert_eq!(call(&restarted_app, "GET", None).await.1, before);
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("PRIVATE KEY") && !bytes.contains("CERTIFICATE"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn invalid_credentials_and_write_failures_leave_catalog_unchanged_and_errors_redacted() {
        let certs = super::super::tests::certificates();
        let path = certs.dir.path().join("connections.json");
        let runtime = SyncRuntime::open(&[], path.clone(), certs.dir.path().into()).unwrap();
        let app = app_with_runtime(runtime, certs.dir.path()).await;
        assert_eq!(
            call(&app, "PUT", Some(input(&certs.profile))).await.0,
            StatusCode::OK
        );
        let saved = std::fs::read(&path).unwrap();
        let catalog = call(&app, "GET", None).await.1;
        let other = super::super::tests::certificates();
        let cases = [
            serde_json::json!({"client_cert":""}),
            serde_json::json!({"client_key":"private-secret-missing.key"}),
            serde_json::json!({"client_key":other.profile.client_key}),
            serde_json::json!({"ca_cert":"private-secret-missing-ca.pem"}),
            serde_json::json!({"url":"ws://localhost/socket?vsn=2.0.0"}),
            serde_json::json!({"max_blob_bytes":0}),
            serde_json::json!({"max_blob_bytes":super::super::DEFAULT_MAX_BLOB_BYTES + 1}),
            serde_json::json!({"client_key":42}),
            serde_json::json!({"url":certs.profile.url.replacen("wss:", "WSS:", 1), "client_key":"private-secret-missing.key"}),
        ];
        for changes in cases {
            let mut body = input(&certs.profile);
            body.as_object_mut()
                .unwrap()
                .extend(changes.as_object().unwrap().clone());
            let (status, error) = call(&app, "PUT", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(!error.to_string().contains("private-secret"));
            assert!(!error
                .to_string()
                .contains(certs.dir.path().to_str().unwrap()));
            assert_eq!(call(&app, "GET", None).await.1, catalog);
            assert_eq!(std::fs::read(&path).unwrap(), saved);
        }
        let blocked_path = certs
            .dir
            .path()
            .join("absent-directory")
            .join("connections.json");
        let runtime = SyncRuntime::open(
            &[certs.profile.clone()],
            blocked_path.clone(),
            certs.dir.path().into(),
        )
        .unwrap();
        let app = app_with_runtime(runtime, certs.dir.path()).await;
        let mut changed = input(&certs.profile);
        changed["max_blob_bytes"] = serde_json::json!(1024);
        let (status, error) = call(&app, "PUT", Some(changed)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!error.to_string().contains("absent-directory"));
        assert_eq!(call(&app, "GET", None).await.1, catalog);
        assert!(!blocked_path.exists());
    }

    #[tokio::test]
    async fn status_routes_expose_only_public_profile_fields_and_default_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(TursoStorage::open(dir.path().join("db")).await.unwrap());
        let ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        let profile = super::super::SyncConnection {
            id: "gao_note".into(),
            mode: super::super::ConnectionMode::Mtls,
            ssh: None,
            url: "wss://admin.gsmlg.org/note-sync/websocket?vsn=2.0.0".into(),
            client_cert: "private-cert.pem".into(),
            client_key: "private-key.pem".into(),
            ca_cert: None,
            max_blob_bytes: super::super::DEFAULT_MAX_BLOB_BYTES,
        };
        let state = AppState::new(
            ctx,
            Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock))),
        )
        .with_sync(SyncRuntime::new(&[profile]));
        let (app, _) = router().split_for_parts();
        let app = app.with_state(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/system/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("private-key"));
        assert!(!text.contains("private-cert"));
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["state"], "disabled");
        assert_eq!(json["profiles"][0].as_object().unwrap().len(), 2);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/system/sync/connections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json[0]["id"], "gao_note");
        assert_eq!(json[0]["has_client_cert"], true);
        assert_eq!(json[0]["has_client_key"], true);
        assert_eq!(json[0]["has_ca_cert"], false);
        assert_eq!(json[0]["mode"], "mtls");
        assert!(json[0]["ssh"].is_null());
        assert_eq!(json[0].as_object().unwrap().len(), 8);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/system/sync/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
