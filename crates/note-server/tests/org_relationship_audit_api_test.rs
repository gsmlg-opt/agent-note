use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{FixedOrgClock, OrgContext},
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::{
    NewNote, NewOrgAttempt, NewOrgEvent, NoteMutationResult, OrgAttemptStatus, OrgEventType,
    StorageBackend,
};
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

const NOW: i64 = 2_000_000_000;
const WORKSPACE: &str = "10000000-0000-4000-8000-000000000501";
const WORKSPACE_B: &str = "10000000-0000-4000-8000-000000000502";
const DOCUMENT: &str = "20000000-0000-4000-8000-000000000501";
const TARGET_DOCUMENT: &str = "20000000-0000-4000-8000-000000000502";
const ITEM: &str = "30000000-0000-4000-8000-000000000501";
const DEPENDENCY: &str = "30000000-0000-4000-8000-000000000502";

struct TestApp {
    app: Router,
    mcp: Router,
    backend: Arc<dyn StorageBackend>,
    _dir: tempfile::TempDir,
}

impl TestApp {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-relationship-audit-rest.db"))
                .await
                .unwrap(),
        );
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        let org = Arc::new(OrgContext::new(
            backend.clone(),
            Arc::new(FixedOrgClock::new(NOW)),
        ));
        let (app, _) = rest_router();
        let mcp = note_mcp::mcp_router(note.clone(), org.clone());
        Self {
            app: app.with_state(AppState::new(note, org)),
            mcp,
            backend,
            _dir: dir,
        }
    }

    async fn mcp_call(&self, name: &str, arguments: Value) -> Value {
        let response = self
            .mcp
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::HOST, "notes.example.test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "tools/call",
                            "params": {"name": name, "arguments": arguments}
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body.get("error").is_none(),
            "MCP call {name} failed: {body}"
        );
        body["result"]["structuredContent"].clone()
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        let request_body = if let Some(body) = body {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(request_body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn seed(&self) {
        let (status, body) = self
            .call(
                Method::POST,
                "/api/org/workspaces",
                Some(json!({
                    "schema_version": 1,
                    "actor_id": "seed",
                    "operation_id": "create-workspace",
                    "workspace_id": WORKSPACE,
                    "slug": "relationship-rest",
                    "display_name": "Relationship REST",
                    "description": "Task 5 integration fixture",
                    "timezone": "UTC",
                    "policy_schema_version": 1,
                    "policy": note_org::WorkspacePolicy::engineering_default()
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let source = format!(
            "* READY Dependent\n:PROPERTIES:\n:ID: {ITEM}\n:AGENT_NOTE_TYPE: task\n:END:\n* DONE Prerequisite\n:PROPERTIES:\n:ID: {DEPENDENCY}\n:AGENT_NOTE_TYPE: task\n:END:\n"
        );
        let (status, body) = self
            .call(
                Method::PUT,
                &format!("/api/org/documents/{DOCUMENT}"),
                Some(json!({
                    "schema_version": 1,
                    "actor_id": "seed",
                    "operation_id": "put-document",
                    "workspace_id": WORKSPACE,
                    "path": "relationships.org",
                    "source": source,
                    "expected_revision": null,
                    "lease_proofs": {}
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

fn mutation(operation_id: &str, revision: i64) -> Value {
    json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE,
        "actor_id": "agent-one",
        "operation_id": operation_id,
        "document_id": DOCUMENT,
        "expected_revisions": {DOCUMENT: revision},
        "lease": null
    })
}

fn merge(mut left: Value, right: Value) -> Value {
    left.as_object_mut()
        .unwrap()
        .extend(right.as_object().unwrap().clone());
    left
}

#[tokio::test]
async fn dependency_add_and_delete_body_use_the_shared_pipeline_contract() {
    let api = TestApp::new().await;
    api.seed().await;

    let add = merge(
        mutation("add-dependency", 1),
        json!({"dependency_id": DEPENDENCY}),
    );
    let (status, added) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/dependencies"),
            Some(add.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["operation_id"], "add-dependency");
    assert_eq!(added["data"]["operation"]["action"], "add");
    assert_eq!(added["data"]["operation"]["dependency_id"], DEPENDENCY);
    let mcp_added = api
        .mcp_call(
            "org_add_dependency",
            merge(add.clone(), json!({"item_id": ITEM})),
        )
        .await;
    assert_eq!(mcp_added, added);

    let (status, replay) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/dependencies"),
            Some(add),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, added);

    let (status, removed) = api
        .call(
            Method::DELETE,
            &format!("/api/org/items/{ITEM}/dependencies/{DEPENDENCY}"),
            Some(mutation("remove-dependency", 2)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["data"]["operation"]["action"], "remove");
    assert_eq!(removed["data"]["operation"]["dependency_id"], DEPENDENCY);
}

#[tokio::test]
async fn dependency_errors_and_delete_extractor_use_structured_contracts() {
    let api = TestApp::new().await;
    api.seed().await;

    let self_dependency = merge(
        mutation("self-dependency", 1),
        json!({"dependency_id": ITEM}),
    );
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/dependencies"),
            Some(self_dependency),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_input");

    let valid = merge(
        mutation("cycle-seed", 1),
        json!({"dependency_id": DEPENDENCY}),
    );
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/dependencies"),
            Some(valid),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cycle = merge(mutation("cycle-attempt", 2), json!({"dependency_id": ITEM}));
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{DEPENDENCY}/dependencies"),
            Some(cycle),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_input");

    let (status, body) = api
        .call(
            Method::DELETE,
            &format!("/api/org/items/{ITEM}/dependencies/{DEPENDENCY}"),
            Some(json!({"schema_version": 1})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_input");
    assert_eq!(body["retryable"], false);
    assert!(body["details"].is_object());

    let leased = TestApp::new().await;
    leased.seed().await;
    let (status, claim) = leased
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/claim"),
            Some(json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE,
                "actor_id": "owner",
                "operation_id": "claim-item",
                "document_id": DOCUMENT,
                "expected_document_revision": 1,
                "kind": "execution"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    let policy_guard = merge(
        mutation("dependency-without-lease", 2),
        json!({"dependency_id": DEPENDENCY}),
    );
    let (status, body) = leased
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/dependencies"),
            Some(policy_guard),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "stale_lease");
}

#[tokio::test]
async fn note_links_are_typed_idempotent_weak_and_reverse_queryable() {
    let api = TestApp::new().await;
    api.seed().await;
    let note_id = "40000000-0000-4000-8000-000000000501";
    api.backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Design evidence",
            content: "Evidence body",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();

    let link = merge(
        mutation("link-note", 1),
        json!({
            "purpose": "evidence",
            "note_id": note_id,
            "description": "Build evidence"
        }),
    );
    let (status, linked) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(link.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["data"]["operation"]["purpose"], "evidence");
    let (status, replay) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(link.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, linked);

    let divergent = merge(link, json!({"description": "Different evidence"}));
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(divergent),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "idempotency_conflict");

    let (status, reverse) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reverse}");
    assert_eq!(reverse["items"][0]["id"], ITEM);

    api.backend
        .session()
        .await
        .unwrap()
        .soft_delete_note(note_id, 1, NOW + 1)
        .await
        .unwrap();
    let (status, context) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{ITEM}/context?workspace_id={WORKSPACE}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert_eq!(context["note_links"][0]["available"], false);
    let (status, deleted_reverse) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted_reverse}");
    assert_eq!(deleted_reverse["items"][0]["id"], ITEM);
    assert_eq!(
        api.backend
            .session()
            .await
            .unwrap()
            .permanently_delete_note(note_id, 2)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
    let (status, missing_context) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{ITEM}/context?workspace_id={WORKSPACE}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{missing_context}");
    assert_eq!(missing_context["note_links"][0]["note_id"], note_id);
    assert_eq!(missing_context["note_links"][0]["available"], false);
    let (status, missing_reverse) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{missing_reverse}");
    assert_eq!(missing_reverse["items"][0]["id"], ITEM);

    let missing = merge(
        mutation("link-missing-note", 2),
        json!({
            "purpose": "reference",
            "note_id": "40000000-0000-4000-8000-000000000599",
            "description": "Unavailable target"
        }),
    );
    let (status, unavailable) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(missing),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unavailable}");
    assert_eq!(unavailable["code"], "note_unavailable");

    let unlink = merge(
        mutation("unlink-note", 2),
        json!({"purpose": "evidence", "note_id": note_id}),
    );
    let (status, unlinked) = api
        .call(
            Method::DELETE,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(unlink),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{unlinked}");
    assert_eq!(unlinked["data"]["operation"]["action"], "unlink");

    let (status, reverse) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reverse}");
    assert!(reverse["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn reverse_lookup_requires_explicit_archived_workspace_selection() {
    let api = TestApp::new().await;
    api.seed().await;
    let note_id = "40000000-0000-4000-8000-000000000502";
    api.backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Archived evidence",
            content: "Evidence body",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let link = merge(
        mutation("link-before-archive", 1),
        json!({
            "purpose": "reference",
            "note_id": note_id,
            "description": "Archived reference"
        }),
    );
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/note-links"),
            Some(link),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE}/archive"),
            Some(json!({
                "schema_version": 1,
                "actor_id": "agent-one",
                "operation_id": "archive-linked-workspace",
                "expected_revision": 1
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, active_only) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{active_only}");
    assert!(active_only["items"].as_array().unwrap().is_empty());
    let (status, archived) = api
        .call(
            Method::GET,
            &format!("/api/org/notes/{note_id}/work-items?limit=50&include_archived=true"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["items"][0]["id"], ITEM);
}

#[tokio::test]
async fn cross_workspace_move_history_preserves_source_to_target_lineage() {
    let api = TestApp::new().await;
    api.seed().await;
    let (status, body) = api
        .call(
            Method::POST,
            "/api/org/workspaces",
            Some(json!({
                "schema_version": 1,
                "actor_id": "seed",
                "operation_id": "create-target-workspace",
                "workspace_id": WORKSPACE_B,
                "slug": "relationship-target",
                "display_name": "Relationship Target",
                "description": "Cross-workspace audit fixture",
                "timezone": "UTC",
                "policy_schema_version": 1,
                "policy": note_org::WorkspacePolicy::engineering_default()
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, moved) = api
        .call(
            Method::POST,
            &format!("/api/org/documents/{DOCUMENT}/move"),
            Some(json!({
                "schema_version": 1,
                "actor_id": "agent-one",
                "operation_id": "move-workspace-document",
                "workspace_id": WORKSPACE,
                "target_workspace_id": WORKSPACE_B,
                "expected_document_revision": 1,
                "expected_source_workspace_revision": 1,
                "expected_target_workspace_revision": 1,
                "lease_proofs": {}
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");

    let (_, source_history) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE}/events?limit=200"),
            None,
        )
        .await;
    let (_, target_history) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_B}/events?limit=200"),
            None,
        )
        .await;
    let source_events = source_history["items"].as_array().unwrap();
    let target_events = target_history["items"].as_array().unwrap();
    assert!(source_events
        .windows(2)
        .all(|pair| pair[0]["sequence"].as_i64() < pair[1]["sequence"].as_i64()));
    assert!(target_events
        .windows(2)
        .all(|pair| pair[0]["sequence"].as_i64() < pair[1]["sequence"].as_i64()));
    let source_move = source_events
        .iter()
        .find(|event| event["event_type"] == "document_move")
        .unwrap();
    let target_move = target_events
        .iter()
        .find(|event| event["event_type"] == "document_move")
        .unwrap();
    assert_eq!(
        target_move["metadata"]["lineage_previous_event_id"],
        source_move["id"]
    );
}

#[tokio::test]
async fn event_history_is_sequence_ordered_filter_bound_paginated_and_token_safe() {
    let api = TestApp::new().await;
    api.seed().await;
    let session = api.backend.session().await.unwrap();
    let attempt_metadata = json!({});
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-7",
            workspace_id: WORKSPACE.parse().unwrap(),
            work_item_id: ITEM.parse().unwrap(),
            attempt_number: 7,
            actor_id: "agent-one",
            status: OrgAttemptStatus::Running,
            started_at: NOW,
            note_refs: &[],
            artifacts: &[],
            metadata: &attempt_metadata,
        })
        .await
        .unwrap();
    for (id, occurred_at, index) in [
        ("z-event", NOW + 30, 0),
        ("a-event", NOW + 10, 1),
        ("m-event", NOW + 20, 2),
    ] {
        let metadata = json!({
            "index": index,
            "fencing_token": format!("raw-token-{index}"),
            "nested": {"token_hash": format!("digest-{index}")},
            "ordinary": "preserved"
        });
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id: WORKSPACE.parse().unwrap(),
                subject_kind: "audit_probe",
                subject_id: ITEM,
                actor_id: "agent-one",
                attempt_id: Some("attempt-7"),
                event_type: OrgEventType::Progress,
                occurred_at,
                summary: "audit probe",
                metadata: &metadata,
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }

    let uri = format!(
        "/api/org/workspaces/{WORKSPACE}/events?subject_kind=audit_probe&subject_id={ITEM}&limit=2"
    );
    let (status, first) = api.call(Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let mcp_first = api
        .mcp_call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE,
                "subject_kind": "audit_probe",
                "subject_id": ITEM,
                "cursor": null,
                "limit": 2
            }),
        )
        .await;
    assert_eq!(mcp_first, first);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["items"][0]["id"], "z-event");
    assert_eq!(first["items"][1]["id"], "a-event");
    assert!(
        first["items"][0]["sequence"].as_i64().unwrap()
            < first["items"][1]["sequence"].as_i64().unwrap()
    );
    assert!(
        first["items"][0]["occurred_at"].as_i64().unwrap()
            > first["items"][1]["occurred_at"].as_i64().unwrap()
    );
    assert_eq!(first["items"][0]["attempt_id"], "attempt-7");
    assert_eq!(first["items"][0]["metadata"]["ordinary"], "preserved");
    assert_eq!(first["items"][0]["metadata"]["fencing_token"], "[REDACTED]");
    assert_eq!(
        first["items"][0]["metadata"]["nested"]["token_hash"],
        "[REDACTED]"
    );
    assert!(!first.to_string().contains("raw-token"));
    assert!(!first.to_string().contains("digest-"));

    let cursor = first["next_cursor"].as_str().unwrap();
    let (status, second) = api
        .call(Method::GET, &format!("{uri}&cursor={cursor}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["items"][0]["id"], "m-event");

    let (status, mismatch) = api
        .call(
            Method::GET,
            &format!(
                "/api/org/workspaces/{WORKSPACE}/events?subject_kind=audit_probe&subject_id={DEPENDENCY}&limit=2&cursor={cursor}"
            ),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{mismatch}");
    assert_eq!(mismatch["code"], "invalid_input");

    let (status, bounds) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE}/events?limit=201"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bounds}");
    assert_eq!(bounds["code"], "invalid_input");

    let (status, target) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{TARGET_DOCUMENT}"),
            Some(json!({
                "schema_version": 1,
                "actor_id": "agent-one",
                "operation_id": "put-target-document",
                "workspace_id": WORKSPACE,
                "path": "moved.org",
                "source": "",
                "expected_revision": null,
                "lease_proofs": {}
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{target}");
    let (status, moved) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM}/move"),
            Some(json!({
                "schema_version": 1,
                "actor_id": "agent-one",
                "operation_id": "move-item",
                "workspace_id": WORKSPACE,
                "source_document_id": DOCUMENT,
                "target_document_id": TARGET_DOCUMENT,
                "target_parent_id": null,
                "expected_revisions": {DOCUMENT: 1, TARGET_DOCUMENT: 1},
                "lease_proofs": {}
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");

    let (status, archived) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE}/archive"),
            Some(json!({
                "schema_version": 1,
                "actor_id": "agent-one",
                "operation_id": "archive-workspace",
                "expected_revision": 1
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let (status, history) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE}/events?limit=200"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let events = history["items"].as_array().unwrap();
    assert!(events
        .windows(2)
        .all(|pair| pair[0]["sequence"].as_i64() < pair[1]["sequence"].as_i64()));
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "item_move" && event["subject_id"] == ITEM));
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "workspace_archive"));
}
