use std::{collections::BTreeMap, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{OrgContext, SystemOrgClock},
    save_note, Context, SaveNoteInput,
};
use note_server::{openapi::rest_router, AppState, DashboardCacheInvalidator};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

struct TransportSide {
    router: Router,
    storage: Arc<dyn StorageBackend>,
    _dir: tempfile::TempDir,
}

struct SharedHttpSide {
    rest: Router,
    mcp: Router,
    _dir: tempfile::TempDir,
}

impl SharedHttpSide {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("shared.db"))
                .await
                .unwrap(),
        );
        let note = Arc::new(
            Context::new(
                storage.clone(),
                Arc::new(StubEmbedder),
                Arc::new(FilesystemAttachmentStore::new(
                    dir.path().join("attachments"),
                )),
            )
            .with_note_mutation_notifier(Arc::new(DashboardCacheInvalidator)),
        );
        save_note(
            &note,
            SaveNoteInput {
                title: "Shared dashboard note".into(),
                content: "Body".into(),
                attachments: vec![],
                labels: vec![("type".into(), "ietf-rfc".into())],
            },
        )
        .await
        .unwrap();
        let org = Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock)));
        let (rest, _) = rest_router();
        Self {
            rest: rest.with_state(AppState::new(note.clone(), org.clone())),
            mcp: note_mcp::mcp_router(note, org),
            _dir: dir,
        }
    }
}

impl TransportSide {
    async fn new_rest() -> Self {
        let (note, org, storage, dir) = seeded_contexts("rest").await;
        let (router, _) = rest_router();
        Self {
            router: router.with_state(AppState::new(note, org)),
            storage,
            _dir: dir,
        }
    }

    async fn new_mcp() -> Self {
        let (note, org, storage, dir) = seeded_contexts("mcp").await;
        Self {
            router: note_mcp::mcp_router(note, org),
            storage,
            _dir: dir,
        }
    }

    async fn final_labels(&self) -> BTreeMap<String, Vec<(String, String)>> {
        let session = self.storage.session().await.unwrap();
        session
            .list_all_notes()
            .await
            .unwrap()
            .into_iter()
            .map(|note| {
                let mut labels = note
                    .labels
                    .into_iter()
                    .map(|label| (label.key, label.value))
                    .collect::<Vec<_>>();
                labels.sort();
                (note.title, labels)
            })
            .collect()
    }
}

async fn seeded_contexts(
    name: &str,
) -> (
    Arc<Context>,
    Arc<OrgContext>,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join(format!("{name}.db")))
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

    for (title, labels) in [
        (
            "Add",
            vec![
                ("type".into(), "ietf-rfc".into()),
                ("owner".into(), "protocols".into()),
            ],
        ),
        (
            "Replace",
            vec![
                ("type".into(), "ietf-rfc".into()),
                ("owner".into(), "protocols".into()),
                ("project".into(), "old".into()),
            ],
        ),
        ("Unmatched", vec![("type".into(), "private-note".into())]),
    ] {
        save_note(
            &note,
            SaveNoteInput {
                title: title.into(),
                content: "Body".into(),
                attachments: vec![],
                labels,
            },
        )
        .await
        .unwrap();
    }

    (note, org, storage, dir)
}

#[tokio::test]
async fn bulk_note_label_update_has_rest_mcp_transport_parity() {
    let rest = TransportSide::new_rest().await;
    let mcp = TransportSide::new_mcp().await;
    let request = json!({
        "selector": "type=ietf-rfc",
        "set": [["project", "IETF-RFC"]]
    });

    let rest_response = rest
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/notes/bulk-labels")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rest_response.status(), StatusCode::OK);
    let rest_result: Value = serde_json::from_slice(
        &to_bytes(rest_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    let mcp_response = mcp
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "proxy.example.test")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {
                            "name": "bulk_update_note_labels",
                            "arguments": request
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mcp_response.status(), StatusCode::OK);
    let mcp_body: Value = serde_json::from_slice(
        &to_bytes(mcp_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(mcp_body.get("error").is_none(), "{mcp_body}");
    let mcp_result = &mcp_body["result"]["structuredContent"];

    let expected = json!({"matched": 2, "updated": 2, "unchanged": 0});
    assert_eq!(rest_result, expected);
    assert_eq!(mcp_result, &expected);

    let rest_labels = rest.final_labels().await;
    let mcp_labels = mcp.final_labels().await;
    assert_eq!(rest_labels, mcp_labels);
    assert_eq!(
        rest_labels["Add"],
        vec![
            ("owner".into(), "protocols".into()),
            ("project".into(), "IETF-RFC".into()),
            ("type".into(), "ietf-rfc".into()),
        ]
    );
    assert_eq!(rest_labels["Replace"], rest_labels["Add"]);
    assert_eq!(
        rest_labels["Unmatched"],
        vec![("type".into(), "private-note".into())]
    );
}

#[tokio::test]
async fn mcp_bulk_label_update_invalidates_the_shared_dashboard_cache() {
    let side = SharedHttpSide::new().await;

    let before = side
        .rest
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(before.status(), StatusCode::OK);
    let before: Value =
        serde_json::from_slice(&to_bytes(before.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(before["label_count"], 1);
    assert!(before["labels"]
        .as_array()
        .unwrap()
        .iter()
        .all(|label| label["key"] != "project"));
    let before_updated_at = before["recent_updates"][0]["updated_at"].as_i64().unwrap();

    let mcp_response = side
        .mcp
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(header::HOST, "proxy.example.test")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": {
                            "name": "bulk_update_note_labels",
                            "arguments": {
                                "selector": "type=ietf-rfc",
                                "set": [["project", "IETF-RFC"]]
                            }
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mcp_response.status(), StatusCode::OK);
    let mcp_body: Value = serde_json::from_slice(
        &to_bytes(mcp_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mcp_body["result"]["structuredContent"],
        json!({"matched": 1, "updated": 1, "unchanged": 0})
    );

    let after = side
        .rest
        .oneshot(
            Request::builder()
                .uri("/api/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::OK);
    let after: Value =
        serde_json::from_slice(&to_bytes(after.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(after["label_count"], 2);
    let project = after["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|label| label["key"] == "project")
        .expect("project label appears after MCP mutation");
    assert_eq!(project["count"], 1);
    assert!(after["recent_updates"][0]["updated_at"].as_i64().unwrap() > before_updated_at);
}
