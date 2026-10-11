use super::SyncRuntimeStatus;
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
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(status))
        .routes(routes!(retry))
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
