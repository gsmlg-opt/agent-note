use std::sync::Arc;

use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_mcp::NoteMcpServer;
use note_pipelines::{
    org::{FixedOrgClock, OrgContext},
    Context,
};
use note_storage::{NewOrgEvent, OrgEventType, StorageBackend};
use note_storage_turso::TursoStorage;
use rmcp::{
    model::{CallToolRequestParams, ErrorData},
    service::ServiceError,
    ServiceExt,
};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;
const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000041";
const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000041";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000041";
const REVIEW_ID: &str = "30000000-0000-4000-8000-000000000042";

struct TestServer {
    server: NoteMcpServer,
    backend: Arc<dyn StorageBackend>,
    attachments: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("workflow.db"))
                .await
                .unwrap(),
        );
        let attachments = dir.path().join("attachments");
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments.clone())),
        ));
        let org = Arc::new(OrgContext::new(
            backend.clone(),
            Arc::new(FixedOrgClock::new(NOW)),
        ));
        Self {
            server: NoteMcpServer::new(note, org),
            backend,
            attachments,
            _dir: dir,
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<Value, ErrorData> {
        Self::call_server(self.server.clone(), name, arguments).await
    }

    async fn call_at(&self, now: i64, name: &str, arguments: Value) -> Result<Value, ErrorData> {
        let note = Arc::new(Context::new(
            self.backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(self.attachments.clone())),
        ));
        let org = Arc::new(OrgContext::new(
            self.backend.clone(),
            Arc::new(FixedOrgClock::new(now)),
        ));
        Self::call_server(NoteMcpServer::new(note, org), name, arguments).await
    }

    async fn call_server(
        service: NoteMcpServer,
        name: &str,
        arguments: Value,
    ) -> Result<Value, ErrorData> {
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            service
                .serve(server_transport)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ().serve(client_transport).await.unwrap();
        let result = client
            .call_tool(
                CallToolRequestParams::new(name.to_owned())
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await;
        client.cancel().await.unwrap();
        server_task.await.unwrap();
        match result {
            Ok(result) => Ok(result.structured_content.expect("structured output")),
            Err(ServiceError::McpError(error)) => Err(error),
            Err(error) => panic!("unexpected MCP service error: {error:?}"),
        }
    }
}

fn policy() -> Value {
    json!({
        "allow_cross_workspace_agenda": false,
        "allowed_types": ["project", "epic", "issue", "task", "subtask", "review", "approval", "incident", "milestone"],
        "states": ["BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED", "CANCELLED"],
        "transitions": [
            ["BACKLOG", "READY"], ["BACKLOG", "CANCELLED"], ["READY", "RUNNING"],
            ["READY", "CANCELLED"], ["RUNNING", "BLOCKED"], ["RUNNING", "READY"],
            ["RUNNING", "REVIEW"], ["RUNNING", "DONE"], ["RUNNING", "FAILED"],
            ["RUNNING", "CANCELLED"], ["BLOCKED", "READY"], ["BLOCKED", "CANCELLED"],
            ["REVIEW", "DONE"], ["REVIEW", "READY"], ["REVIEW", "CANCELLED"],
            ["FAILED", "READY"], ["FAILED", "RUNNING"], ["FAILED", "CANCELLED"]
        ],
        "initial_state": "BACKLOG", "running_state": "RUNNING", "executable_states": ["READY"],
        "review_state": "REVIEW", "failed_state": "FAILED", "cancelled_state": "CANCELLED",
        "successful_terminal_states": ["DONE"], "terminal_states": ["DONE", "CANCELLED"],
        "release_state": "READY", "review_rejection_state": "READY", "lease_expiry_recovery_state": "READY",
        "review_required_types": ["review"], "claim_policy": "open",
        "lease_duration_secs": 900, "retry_limit": 2, "concurrency_limit": 4, "tag_rules": {}
    })
}

async fn seeded() -> TestServer {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "create-workspace", "slug": "workflow", "display_name": "Workflow",
            "description": "test", "timezone": "UTC", "policy_schema_version": 1, "policy": policy()
        }),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "put-document", "document_id": DOCUMENT_ID, "path": "workflow.org",
            "source": "#+TITLE: Workflow\n", "expected_revision": null, "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "create-item", "document_id": DOCUMENT_ID, "parent_id": null,
            "item_id": ITEM_ID, "item_type": "task", "title": "Execute workflow",
            "initial_state": "READY", "priority": "A", "tags": ["workflow"],
            "assignee": "agent-one", "requires_review": false,
            "expected_revisions": { DOCUMENT_ID: 1 }
        }),
    )
    .await
    .unwrap();
    mcp
}

fn claim_input(operation_id: &str, kind: &str, revision: i64) -> Value {
    json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": operation_id, "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": revision, "kind": kind
    })
}

fn lease_fields(claim: &Value, kind: &str) -> Value {
    json!({
        "lease_id": claim["lease_id"], "kind": kind,
        "fencing_token": claim["fencing_token"]
    })
}

fn stale_lease_data() -> Value {
    json!({
        "code": "stale_lease",
        "message": "Org lease proof is stale or invalid",
        "details": {},
        "retryable": true
    })
}

async fn create_review_item(mcp: &TestServer) {
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "create-review-item", "document_id": DOCUMENT_ID, "parent_id": null,
            "item_id": REVIEW_ID, "item_type": "review", "title": "Review workflow",
            "initial_state": "READY", "priority": "A", "tags": ["review"],
            "assignee": "agent-one", "requires_review": true,
            "expected_revisions": { DOCUMENT_ID: 2 }
        }),
    )
    .await
    .unwrap();
}

async fn review_ready(mcp: &TestServer, prefix: &str) -> Value {
    create_review_item(mcp).await;
    let execution = mcp
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": format!("{prefix}-execution-claim"), "work_item_id": REVIEW_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 3, "kind": "execution"
            }),
        )
        .await
        .unwrap();
    let review = mcp.call("org_request_review", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": format!("{prefix}-request-review"), "work_item_id": REVIEW_ID,
        "document_id": DOCUMENT_ID, "expected_document_revision": 4,
        "lease_id": execution["lease_id"], "fencing_token": execution["fencing_token"],
        "result_summary": "ready", "note_refs": [], "artifacts": [], "metadata": {"stage": "review"}
    })).await.unwrap();
    assert_eq!(review["data"]["context"]["item"]["state"], "REVIEW");
    mcp.call(
        "org_claim_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": format!("{prefix}-review-claim"), "work_item_id": REVIEW_ID,
            "document_id": DOCUMENT_ID, "expected_document_revision": 5, "kind": "review"
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn execution_claim_delegates_through_real_mcp_and_returns_the_only_raw_token() {
    let mcp = seeded().await;
    let claim = mcp
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "claim", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 2, "kind": "execution"
            }),
        )
        .await
        .expect("claim handler must be live");
    assert!(claim["fencing_token"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(claim["context"]["item"]["state"], "RUNNING");
}

#[tokio::test]
async fn claim_heartbeat_progress_and_release_preserve_exact_replay_and_revision_boundaries() {
    let mcp = seeded().await;
    let input = claim_input("claim-replay", "execution", 2);
    let claim = mcp.call("org_claim_item", input.clone()).await.unwrap();
    assert_eq!(mcp.call("org_claim_item", input).await.unwrap(), claim);
    let events_after_claim = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(
        events_after_claim,
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len()
    );
    assert!(mcp
        .call("org_claim_item", claim_input("claim-replay", "review", 2))
        .await
        .is_err());

    let revision_before = mcp
        .backend
        .session()
        .await
        .unwrap()
        .get_org_document(DOCUMENT_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .revision;
    let heartbeat_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "heartbeat", "work_item_id": ITEM_ID,
        "lease_id": claim["lease_id"], "kind": "execution", "fencing_token": claim["fencing_token"]
    });
    let heartbeat = mcp
        .call("org_heartbeat_claim", heartbeat_input.clone())
        .await
        .unwrap();
    assert_eq!(
        mcp.call("org_heartbeat_claim", heartbeat_input)
            .await
            .unwrap(),
        heartbeat
    );
    assert_eq!(heartbeat["data"]["lease"]["work_item_id"], ITEM_ID);
    assert_eq!(heartbeat["data"]["context"]["item"]["state"], "RUNNING");
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .get_org_document(DOCUMENT_ID.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .revision,
        revision_before
    );

    let events_before_rejected_payload = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let rejected_payload = mcp.call("org_report_progress", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "progress-reserved-key", "work_item_id": ITEM_ID,
        "lease_id": claim["lease_id"], "kind": "execution", "fencing_token": claim["fencing_token"],
        "summary": "halfway", "metadata": {"token_digest": "must-not-persist"}
    })).await.unwrap_err();
    assert_eq!(
        rejected_payload.data.as_ref().unwrap(),
        &json!({
            "code": "invalid_input",
            "message": "Org workflow public payload contains reserved fencing material",
            "details": {},
            "retryable": false
        })
    );
    let session = mcp.backend.session().await.unwrap();
    assert_eq!(
        session
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_rejected_payload
    );
    assert!(session
        .get_org_operation(WORKSPACE_ID.parse().unwrap(), "progress-reserved-key")
        .await
        .unwrap()
        .is_none());

    let progress = mcp.call("org_report_progress", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "progress", "work_item_id": ITEM_ID,
        "lease_id": claim["lease_id"], "kind": "execution", "fencing_token": claim["fencing_token"],
        "summary": "halfway", "metadata": {
            "phase": "build", "tokenized_count": 2, "hashmap_size": 3
        }
    })).await.unwrap();
    assert_eq!(progress["data"]["context"]["item"]["state"], "RUNNING");
    let progress_event = progress["data"]["context"]["history_segments"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|segment| segment["events"].as_array().unwrap())
        .find(|event| event["summary"] == "halfway")
        .unwrap();
    assert_eq!(progress_event["metadata"]["tokenized_count"], 2);
    assert_eq!(progress_event["metadata"]["hashmap_size"], 3);

    for input in [
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "wrong-token", "work_item_id": ITEM_ID,
            "lease_id": claim["lease_id"], "kind": "execution", "fencing_token": "wrong-secret",
            "summary": "wrong", "metadata": {}
        }),
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "missing-token", "work_item_id": ITEM_ID,
            "lease_id": claim["lease_id"], "kind": "execution",
            "summary": "missing", "metadata": {}
        }),
    ] {
        let error = mcp.call("org_report_progress", input).await.unwrap_err();
        assert_eq!(error.data.as_ref().unwrap(), &stale_lease_data());
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("wrong-secret"));
    }
    let incomplete_nested = mcp
        .call(
            "org_transition_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "missing-nested-token", "work_item_id": ITEM_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 3,
                "target_state": "BLOCKED", "lease": {
                    "lease_id": claim["lease_id"], "kind": "execution"
                }, "error": null, "metadata": {}
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        incomplete_nested.data.as_ref().unwrap(),
        &stale_lease_data()
    );

    let released = mcp
        .call(
            "org_release_claim",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "release", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 3, "lease_id": claim["lease_id"], "kind": "execution",
                "fencing_token": claim["fencing_token"], "target_state": "READY"
            }),
        )
        .await
        .unwrap();
    assert_eq!(released["data"]["context"]["item"]["state"], "READY");
    assert!(released["data"].get("released").is_none());

    let stale = mcp.call("org_report_progress", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "late-progress", "work_item_id": ITEM_ID,
        "lease_id": claim["lease_id"], "kind": "execution", "fencing_token": claim["fencing_token"],
        "summary": "late", "metadata": {}
    })).await.unwrap_err();
    assert_eq!(stale.data.as_ref().unwrap(), &stale_lease_data());
    let encoded = serde_json::to_string(&stale).unwrap();
    assert!(!encoded.contains(claim["fencing_token"].as_str().unwrap()));
}

#[tokio::test]
async fn failure_retry_and_result_flow_returns_context_and_retry_token_only() {
    let mcp = seeded().await;
    let claim = mcp
        .call(
            "org_claim_item",
            claim_input("claim-failure", "execution", 2),
        )
        .await
        .unwrap();
    let failed = mcp.call("org_transition_item", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "fail", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 3, "target_state": "FAILED",
        "lease": lease_fields(&claim, "execution"), "error": "boom", "metadata": {"phase": "run"}
    })).await.unwrap();
    assert_eq!(failed["data"]["context"]["item"]["state"], "FAILED");
    assert!(failed.get("fencing_token").is_none());

    let retry_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "retry", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 4
    });
    let retry = mcp
        .call("org_retry_item", retry_input.clone())
        .await
        .unwrap();
    assert_eq!(
        mcp.call("org_retry_item", retry_input).await.unwrap(),
        retry
    );
    assert!(retry["fencing_token"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(retry["context"]["item"]["state"], "RUNNING");

    let result = mcp
        .call(
            "org_submit_result",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "result", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 5, "lease_id": retry["lease_id"],
                "fencing_token": retry["fencing_token"], "result_summary": "done",
                "note_refs": [], "artifacts": [], "metadata": {"outcome": "ok"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["data"]["context"]["item"]["state"], "DONE");
    assert!(result["data"].get("review_outcome").is_none());
}

#[tokio::test]
async fn transition_tool_blocks_unblocks_and_cancels_with_policy_lifecycle_rules() {
    let mcp = seeded().await;
    let first_claim = mcp
        .call("org_claim_item", claim_input("block-claim", "execution", 2))
        .await
        .unwrap();
    let blocked = mcp.call("org_transition_item", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "block", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 3, "target_state": "BLOCKED",
        "lease": lease_fields(&first_claim, "execution"), "error": null, "metadata": {"reason": "wait"}
    })).await.unwrap();
    assert_eq!(blocked["data"]["context"]["item"]["state"], "BLOCKED");
    let unblocked = mcp
        .call(
            "org_transition_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "unblock", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 4, "target_state": "READY",
                "lease": null, "error": null, "metadata": {"reason": "ready"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(unblocked["data"]["context"]["item"]["state"], "READY");
    let second_claim = mcp
        .call(
            "org_claim_item",
            claim_input("cancel-claim", "execution", 5),
        )
        .await
        .unwrap();
    let cancelled = mcp.call("org_transition_item", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "cancel", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 6, "target_state": "CANCELLED",
        "lease": lease_fields(&second_claim, "execution"), "error": null, "metadata": {"reason": "stop"}
    })).await.unwrap();
    assert_eq!(cancelled["data"]["context"]["item"]["state"], "CANCELLED");
}

#[tokio::test]
async fn review_claim_approval_and_rejection_follow_the_review_lease() {
    let approved_server = seeded().await;
    let review_claim = review_ready(&approved_server, "approve").await;
    let approved = approved_server
        .call(
            "org_approve_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "approve", "work_item_id": REVIEW_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 5, "lease_id": review_claim["lease_id"],
                "fencing_token": review_claim["fencing_token"], "metadata": {"decision": "yes"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(approved["data"]["context"]["item"]["state"], "DONE");

    let rejected_server = seeded().await;
    let review_claim = review_ready(&rejected_server, "reject").await;
    let rejected = rejected_server
        .call(
            "org_reject_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "reject", "work_item_id": REVIEW_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 5, "lease_id": review_claim["lease_id"],
                "fencing_token": review_claim["fencing_token"], "reason": "changes requested",
                "metadata": {"decision": "no"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(rejected["data"]["context"]["item"]["state"], "READY");
}

#[tokio::test]
async fn dependency_and_note_relationship_tools_round_trip_with_reverse_lookup() {
    const DEPENDENCY_ID: &str = "30000000-0000-4000-8000-000000000043";
    let mcp = seeded().await;
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "create-dependency", "document_id": DOCUMENT_ID, "parent_id": null,
            "item_id": DEPENDENCY_ID, "item_type": "task", "title": "Dependency",
            "initial_state": "BACKLOG", "priority": null, "tags": [], "assignee": null,
            "requires_review": false, "expected_revisions": { DOCUMENT_ID: 2 }
        }),
    )
    .await
    .unwrap();
    let dependency_input = |operation_id: &str| {
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": operation_id, "item_id": ITEM_ID, "dependency_id": DEPENDENCY_ID,
            "document_id": DOCUMENT_ID, "expected_revisions": { DOCUMENT_ID: if operation_id == "add-dependency" { 3 } else { 4 } },
            "lease": null
        })
    };
    let added = mcp
        .call("org_add_dependency", dependency_input("add-dependency"))
        .await
        .unwrap();
    assert_eq!(added["data"]["operation"]["action"], "add");
    assert_eq!(added["data"]["operation"]["dependency_id"], DEPENDENCY_ID);
    let blocked_queue = mcp
        .call(
            "org_query_queue",
            json!({
                "workspace_ids": [WORKSPACE_ID], "view": "ready"
            }),
        )
        .await
        .unwrap();
    assert!(!blocked_queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["item"]["id"] == ITEM_ID));
    let removed = mcp
        .call(
            "org_remove_dependency",
            dependency_input("remove-dependency"),
        )
        .await
        .unwrap();
    assert_eq!(removed["data"]["operation"]["action"], "remove");
    let ready_queue = mcp
        .call(
            "org_query_queue",
            json!({
                "workspace_ids": [WORKSPACE_ID], "view": "ready"
            }),
        )
        .await
        .unwrap();
    assert!(ready_queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["item"]["id"] == ITEM_ID));

    let note = mcp
        .call(
            "save_note",
            json!({"title": "Linked", "content": "body", "labels": []}),
        )
        .await
        .unwrap();
    let note_id = note["id"].as_str().unwrap();
    let note_revision = note["revision"].as_i64().unwrap();
    let link = mcp
        .call(
            "org_link_note",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "link-note", "item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "purpose": "evidence", "note_id": note_id, "description": "build evidence",
                "expected_revisions": { DOCUMENT_ID: 5 }, "lease": null
            }),
        )
        .await
        .unwrap();
    assert_eq!(link["data"]["operation"]["purpose"], "evidence");
    let reverse = mcp
        .call(
            "org_list_note_work_items",
            json!({
                "note_id": note_id, "include_archived": false
            }),
        )
        .await
        .unwrap();
    assert_eq!(reverse["items"][0]["id"], ITEM_ID);
    let context = mcp
        .call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await
        .unwrap();
    assert_eq!(context["note_links"][0]["available"], true);
    mcp.call(
        "delete_note",
        json!({"id": note_id, "expected_revision": note_revision}),
    )
    .await
    .unwrap();
    let context = mcp
        .call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await
        .unwrap();
    assert_eq!(context["note_links"][0]["available"], false);
    let unlinked = mcp
        .call(
            "org_unlink_note",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "unlink-note", "item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "purpose": "evidence", "note_id": note_id,
                "expected_revisions": { DOCUMENT_ID: 6 }, "lease": null
            }),
        )
        .await
        .unwrap();
    assert_eq!(unlinked["data"]["operation"]["action"], "unlink");
    let reverse = mcp
        .call(
            "org_list_note_work_items",
            json!({
                "note_id": note_id, "include_archived": false
            }),
        )
        .await
        .unwrap();
    assert!(reverse["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn event_history_uses_items_sequence_cursor_limits_and_subject_pair_binding() {
    let mcp = seeded().await;
    let session = mcp.backend.session().await.unwrap();
    for index in 0..55 {
        session
            .append_org_event(NewOrgEvent {
                id: &format!("audit-{index:03}"),
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                subject_kind: "work_item",
                subject_id: ITEM_ID,
                actor_id: "agent-one",
                attempt_id: None,
                event_type: OrgEventType::Progress,
                occurred_at: NOW,
                summary: "audit progress",
                metadata: &json!({"index": index}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    let first = mcp
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": null, "subject_id": null
            }),
        )
        .await
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    assert!(first["next_cursor"].is_string());
    let sequences = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["sequence"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let second = mcp
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": null, "subject_id": null,
                "cursor": first["next_cursor"], "limit": 50
            }),
        )
        .await
        .unwrap();
    assert!(second["items"][0]["sequence"].as_i64().unwrap() > *sequences.last().unwrap());

    let subject = mcp
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item", "subject_id": ITEM_ID,
                "limit": 200
            }),
        )
        .await
        .unwrap();
    assert!(subject["items"].as_array().unwrap().len() >= 55);
    assert!(subject["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["subject_id"] == ITEM_ID));
    assert!(mcp
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item", "subject_id": null
            })
        )
        .await
        .is_err());
    assert!(mcp
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": null, "subject_id": null, "limit": 201
            })
        )
        .await
        .is_err());
}

#[tokio::test]
async fn only_claim_and_retry_output_schemas_advertise_raw_tokens() {
    let mcp = TestServer::new().await;
    for tool in mcp.server.registered_tools().into_iter().filter(|tool| {
        matches!(
            tool.name.as_ref(),
            "org_claim_item"
                | "org_heartbeat_claim"
                | "org_release_claim"
                | "org_report_progress"
                | "org_submit_result"
                | "org_transition_item"
                | "org_retry_item"
                | "org_request_review"
                | "org_approve_item"
                | "org_reject_item"
                | "org_add_dependency"
                | "org_remove_dependency"
                | "org_link_note"
                | "org_unlink_note"
                | "org_list_note_work_items"
                | "org_list_events"
        )
    }) {
        let schema = serde_json::to_string(tool.output_schema.as_ref().unwrap()).unwrap();
        let advertises_token = schema.contains("fencing_token");
        assert_eq!(
            advertises_token,
            matches!(tool.name.as_ref(), "org_claim_item" | "org_retry_item"),
            "{}",
            tool.name
        );
        assert!(!schema.contains("fencing_token_hash"), "{}", tool.name);
    }
}

#[tokio::test]
async fn workflow_input_schemas_close_claim_kinds_and_require_object_metadata() {
    fn resolve<'a>(root: &'a Value, mut node: &'a Value) -> &'a Value {
        while let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            node = root
                .pointer(reference.strip_prefix('#').unwrap())
                .expect("local schema reference");
        }
        node
    }

    let mcp = TestServer::new().await;
    let tools = mcp.server.registered_tools();
    for name in [
        "org_claim_item",
        "org_heartbeat_claim",
        "org_release_claim",
        "org_report_progress",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .unwrap();
        let schema = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
        let kind = resolve(&schema, &schema["properties"]["kind"]);
        assert_eq!(kind["enum"], json!(["execution", "review"]), "{name}");
    }
    for name in [
        "org_report_progress",
        "org_submit_result",
        "org_transition_item",
        "org_request_review",
        "org_approve_item",
        "org_reject_item",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .unwrap();
        let schema = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
        let metadata = resolve(&schema, &schema["properties"]["metadata"]);
        assert_eq!(metadata["type"], "object", "{name}");
    }
}

#[tokio::test]
async fn heartbeat_output_schema_requires_a_non_nullable_lease() {
    fn resolve<'a>(root: &'a Value, mut node: &'a Value) -> &'a Value {
        while let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            node = root
                .pointer(reference.strip_prefix('#').unwrap())
                .expect("local schema reference");
        }
        node
    }

    let mcp = TestServer::new().await;
    let tool = mcp
        .server
        .registered_tools()
        .into_iter()
        .find(|tool| tool.name.as_ref() == "org_heartbeat_claim")
        .unwrap();
    let schema = serde_json::to_value(tool.output_schema.as_ref().unwrap()).unwrap();
    let data = resolve(&schema, &schema["properties"]["data"]);
    assert!(data["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field == "lease"));
    let lease = resolve(&schema, &data["properties"]["lease"]);
    assert_eq!(lease["type"], "object");
    assert!(lease.get("anyOf").is_none());
}

#[tokio::test]
async fn expired_owner_cannot_progress_transition_or_complete_after_reclaim() {
    let mcp = seeded().await;
    let agent_a = mcp
        .call(
            "org_claim_item",
            claim_input("agent-a-claim", "execution", 2),
        )
        .await
        .unwrap();
    let events_before_expired = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let expired = mcp
        .call_at(
            NOW + 900,
            "org_report_progress",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "expired-a-progress", "work_item_id": ITEM_ID,
                "lease_id": agent_a["lease_id"], "kind": "execution",
                "fencing_token": agent_a["fencing_token"], "summary": "expired", "metadata": {}
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(expired.data.as_ref().unwrap(), &stale_lease_data());
    let session = mcp.backend.session().await.unwrap();
    assert_eq!(
        session
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_expired
    );
    assert!(session
        .get_org_operation(WORKSPACE_ID.parse().unwrap(), "expired-a-progress")
        .await
        .unwrap()
        .is_none());
    let agent_b = mcp.call_at(NOW + 900, "org_claim_item", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-two",
        "operation_id": "agent-b-reclaim", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 3, "kind": "execution"
    })).await.unwrap();
    assert_ne!(agent_a["fencing_token"], agent_b["fencing_token"]);
    assert_eq!(agent_b["context"]["attempts"].as_array().unwrap().len(), 2);
    let before = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let delayed = [
        (
            "org_report_progress",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "delayed-a-progress", "work_item_id": ITEM_ID,
                "lease_id": agent_a["lease_id"], "kind": "execution", "fencing_token": agent_a["fencing_token"],
                "summary": "late", "metadata": {}
            }),
        ),
        (
            "org_transition_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "delayed-a-transition", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 3, "target_state": "FAILED",
                "lease": lease_fields(&agent_a, "execution"), "error": "late", "metadata": {}
            }),
        ),
        (
            "org_submit_result",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "delayed-a-result", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 3, "lease_id": agent_a["lease_id"],
                "fencing_token": agent_a["fencing_token"], "result_summary": "late",
                "note_refs": [], "artifacts": [], "metadata": {}
            }),
        ),
    ];
    for (tool, input) in delayed {
        let error = mcp.call_at(NOW + 900, tool, input).await.unwrap_err();
        assert_eq!(error.data.as_ref().unwrap(), &stale_lease_data(), "{tool}");
    }
    let after = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before);
}
