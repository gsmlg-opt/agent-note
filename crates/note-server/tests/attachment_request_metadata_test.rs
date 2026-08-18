use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{OrgContext, SystemOrgClock},
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::json;
use tower::ServiceExt;

async fn test_app() -> (Router, Arc<dyn StorageBackend>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let note = Arc::new(Context::new(
        storage.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    ));
    let org = Arc::new(OrgContext::new(storage.clone(), Arc::new(SystemOrgClock)));
    let (router, _) = rest_router();

    (router.with_state(AppState::new(note, org)), storage, dir)
}

#[tokio::test]
async fn save_rejects_internal_attachment_storage_fields_before_writing() {
    let (app, storage, _dir) = test_app().await;

    for (field, value) in [
        (
            "storage",
            json!({
                "object_key": "notes/note-1/file.txt/generation-1",
                "storage_generation": "generation-1",
                "size_bytes": 7,
                "checksum_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }),
        ),
        ("object_key", json!("notes/note-1/file.txt/generation-1")),
        ("storage_generation", json!("generation-1")),
        ("size_bytes", json!(7)),
        (
            "checksum_sha256",
            json!("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
        ),
    ] {
        let mut attachment = json!({
            "id": "file",
            "path": "file.txt",
            "mime": "text/plain",
            "content": "payload"
        });
        attachment[field] = value;
        let body = json!({
            "title": format!("Reject {field}"),
            "content": "Body",
            "attachments": [attachment]
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/notes")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "field: {field}");
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8",
            "field: {field}"
        );
        let response_body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            &response_body[..],
            b"invalid note request",
            "field: {field}"
        );
    }

    let session = storage.session().await.unwrap();
    let notes = session
        .list_note_summaries(&[], Some(0), Some(100))
        .await
        .unwrap();
    assert!(notes.is_empty());
}

#[test]
fn openapi_closes_attachment_requests_to_unknown_fields() {
    let (_, openapi) = rest_router();
    let document = serde_json::to_value(openapi).unwrap();
    let request = &document["components"]["schemas"]["AttachmentRequest"];
    let object = if request["properties"].is_object() {
        request
    } else {
        &request["allOf"][0]
    };

    assert_eq!(object["additionalProperties"], false);
    assert_eq!(
        document["paths"]["/api/notes"]["post"]["responses"]["400"]["content"]["text/plain"]
            ["schema"]["type"],
        "string"
    );
}
