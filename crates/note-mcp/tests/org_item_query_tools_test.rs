use std::sync::Arc;

use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_mcp::NoteMcpServer;
use note_pipelines::{
    org::{
        add_dependency, claim_item, link_note, report_progress, CommandEnvelope, DependencyRequest,
        FixedOrgClock, NoteLinkRequest, OrgClaimKind, OrgContext, ReportProgressRequest,
        StartClaimRequest,
    },
    Context,
};
use note_storage::{
    ConditionalUpdate, NewOrgAttempt, NewOrgEvent, NewOrgLease, OrgAttemptStatus, OrgEventType,
    OrgLeaseKind, StorageBackend, StoredOrgTimestamp,
};
use note_storage_turso::TursoStorage;
use rmcp::{
    model::{CallToolRequestParams, ErrorData},
    service::ServiceError,
    ServiceExt,
};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;
const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000031";
const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000031";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000031";
const FOLLOW_UP_ID: &str = "30000000-0000-4000-8000-000000000032";

struct TestServer {
    server: NoteMcpServer,
    backend: Arc<dyn StorageBackend>,
    org: Arc<OrgContext>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-item-mcp.db"))
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
        Self {
            server: NoteMcpServer::new(note, org.clone()),
            backend,
            org,
            _dir: dir,
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<Value, ErrorData> {
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let service = self.server.clone();
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
            Err(error) => panic!("unexpected MCP error: {error:?}"),
        }
    }
}

fn policy(allow_cross_workspace_agenda: bool) -> Value {
    json!({
        "allow_cross_workspace_agenda": allow_cross_workspace_agenda,
        "allowed_types": ["project", "epic", "issue", "task", "subtask", "review", "approval", "incident", "milestone"],
        "states": ["BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED", "CANCELLED"],
        "transitions": [
            ["BACKLOG", "READY"], ["BACKLOG", "CANCELLED"],
            ["READY", "RUNNING"], ["READY", "CANCELLED"],
            ["RUNNING", "BLOCKED"], ["RUNNING", "READY"], ["RUNNING", "REVIEW"],
            ["RUNNING", "DONE"], ["RUNNING", "FAILED"], ["RUNNING", "CANCELLED"],
            ["BLOCKED", "READY"], ["BLOCKED", "CANCELLED"],
            ["REVIEW", "DONE"], ["REVIEW", "READY"], ["REVIEW", "CANCELLED"],
            ["FAILED", "READY"], ["FAILED", "RUNNING"], ["FAILED", "CANCELLED"]
        ],
        "initial_state": "BACKLOG", "running_state": "RUNNING", "executable_states": ["READY"],
        "review_state": "REVIEW", "failed_state": "FAILED", "cancelled_state": "CANCELLED",
        "successful_terminal_states": ["DONE"], "terminal_states": ["DONE", "CANCELLED"],
        "release_state": "READY", "review_rejection_state": "READY", "lease_expiry_recovery_state": "READY",
        "review_required_types": [], "claim_policy": "assignment_restricted", "lease_duration_secs": 900,
        "retry_limit": 2, "concurrency_limit": 4, "tag_rules": {}
    })
}

async fn seeded() -> TestServer {
    let mcp = TestServer::new().await;
    mcp.call("org_create_workspace", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "create-workspace", "slug": "item-tools", "display_name": "Item tools",
        "description": "test", "timezone": "Asia/Shanghai", "policy_schema_version": 1, "policy": policy(false)
    })).await.unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "put-document", "document_id": DOCUMENT_ID, "path": "work.org",
            "source": "#+TITLE: Work\n", "expected_revision": null, "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    mcp
}

async fn create_workspace(mcp: &TestServer, workspace_id: &str, allow_cross_workspace: bool) {
    mcp.call(
        "org_create_workspace",
        json!({
            "schema_version": 1, "workspace_id": workspace_id, "actor_id": "agent-one",
            "operation_id": format!("create-{workspace_id}"), "slug": format!("ws-{workspace_id}"),
            "display_name": "Workspace", "description": "test", "timezone": "Asia/Shanghai",
            "policy_schema_version": 1, "policy": policy(allow_cross_workspace)
        }),
    )
    .await
    .unwrap();
}

async fn seed_workspace_assigned_item(
    mcp: &TestServer,
    workspace_id: &str,
    document_id: &str,
    item_id: &str,
) {
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1, "workspace_id": workspace_id, "actor_id": "agent-one",
            "operation_id": format!("put-{document_id}"), "document_id": document_id,
            "path": format!("{document_id}.org"), "source": "#+TITLE: Multi workspace\n",
            "expected_revision": null, "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": workspace_id, "actor_id": "agent-one",
            "operation_id": format!("create-{item_id}"), "document_id": document_id,
            "parent_id": null, "item_id": item_id, "item_type": "task",
            "title": "Assigned cross-workspace item", "initial_state": "BACKLOG",
            "priority": "A", "tags": ["multi"], "assignee": "agent-one",
            "requires_review": false, "expected_revisions": { (document_id): 1 }
        }),
    )
    .await
    .unwrap();
}

async fn seed_all_operational_views(mcp: &TestServer) -> Vec<String> {
    create_item(mcp, ITEM_ID, "create-operational-base", 1).await;
    let session = mcp.backend.session().await.unwrap();
    let base = session
        .get_org_work_item(ITEM_ID.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let ids = (41..=50)
        .map(|suffix| format!("30000000-0000-4000-8000-{suffix:012}"))
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let mut row = base.clone();
        row.id = id.parse().unwrap();
        row.source_order = index as i64 + 1;
        row.title = format!("Operational {index}");
        row.assignee = None;
        row.scheduled = None;
        row.deadline = None;
        row.state = Some(
            match index {
                0 => "READY",
                1 => "BACKLOG",
                2 => "RUNNING",
                3 => "BLOCKED",
                4 => "REVIEW",
                5 | 6 => "BACKLOG",
                7 => "FAILED",
                8 => "DONE",
                _ => "READY",
            }
            .into(),
        );
        if index == 0 {
            row.assignee = Some("agent-one".into());
            row.scheduled = Some(StoredOrgTimestamp {
                raw: "<2027-01-15 Fri 09:00>".into(),
                local: "2027-01-15T09:00:00".into(),
                timezone: "Asia/Shanghai".into(),
                utc_timestamp: NOW,
            });
            row.deadline = row.scheduled.clone();
        }
        if index == 1 {
            row.assignee = Some("agent-one".into());
            row.priority = None;
        }
        if index == 5 {
            row.priority = None;
            row.scheduled = base.scheduled.clone().or_else(|| {
                Some(StoredOrgTimestamp {
                    raw: "<2027-01-15 Fri 09:00>".into(),
                    local: "2027-01-15T09:00:00".into(),
                    timezone: "Asia/Shanghai".into(),
                    utc_timestamp: NOW,
                })
            });
        }
        if index == 6 {
            row.deadline = Some(StoredOrgTimestamp {
                raw: "<2027-01-15 Fri 09:00>".into(),
                local: "2027-01-15T09:00:00".into(),
                timezone: "Asia/Shanghai".into(),
                utc_timestamp: NOW,
            });
        }
        rows.push(row);
    }
    session
        .replace_org_document_projection(DOCUMENT_ID.parse().unwrap(), &rows)
        .await
        .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "operational-running-attempt",
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            work_item_id: ids[2].parse().unwrap(),
            attempt_number: 1,
            actor_id: "agent-one",
            status: OrgAttemptStatus::Running,
            started_at: NOW - 100,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({"nested": {"lease_token": "raw", "secretary": "kept"}}),
        })
        .await
        .unwrap();
    for attempt_number in 1..=3 {
        session
            .insert_org_attempt(NewOrgAttempt {
                id: &format!("failed-attempt-{attempt_number}"),
                workspace_id: WORKSPACE_ID.parse().unwrap(),
                work_item_id: ids[7].parse().unwrap(),
                attempt_number,
                actor_id: "agent-one",
                status: OrgAttemptStatus::Failed,
                started_at: NOW - 200 + attempt_number,
                note_refs: &[],
                artifacts: &[],
                metadata: &json!({}),
            })
            .await
            .unwrap();
    }
    assert!(matches!(
        session
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "operational-expired-lease",
                    workspace_id: WORKSPACE_ID.parse().unwrap(),
                    work_item_id: ids[2].parse().unwrap(),
                    attempt_id: "operational-running-attempt",
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-one",
                    fencing_token_hash:
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    acquired_at: NOW - 100,
                    last_heartbeat_at: NOW - 100,
                    expires_at: NOW,
                },
                4,
                NOW - 100
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    session
        .append_org_event(NewOrgEvent {
            id: "operational-completion",
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            subject_kind: "work_item",
            subject_id: &ids[8],
            actor_id: "agent-one",
            attempt_id: None,
            event_type: OrgEventType::Completion,
            occurred_at: NOW,
            summary: "completed",
            metadata: &json!({}),
            previous_state: Some("RUNNING"),
            resulting_state: Some("DONE"),
        })
        .await
        .unwrap();
    ids
}

async fn create_item(mcp: &TestServer, item_id: &str, operation_id: &str, revision: i64) -> Value {
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": operation_id, "document_id": DOCUMENT_ID, "parent_id": null,
            "item_id": item_id, "item_type": "task", "title": format!("Item {item_id}"),
            "initial_state": "READY", "priority": "A", "tags": ["mcp"],
            "assignee": "agent-one", "requires_review": false,
            "expected_revisions": { DOCUMENT_ID: revision }
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn create_get_context_and_operational_queries_delegate_through_mcp() {
    let mcp = seeded().await;
    let created = mcp.call("org_create_item", json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "create-item", "document_id": DOCUMENT_ID, "parent_id": null,
        "item_id": ITEM_ID, "item_type": "task", "title": "Ship MCP", "initial_state": "READY",
        "priority": "A", "tags": ["mcp"], "assignee": "agent-one", "requires_review": false,
        "expected_revisions": { DOCUMENT_ID: 1 }
    })).await.expect("create item handler must be live");
    assert_eq!(created["data"]["resulting_items"][0]["id"], ITEM_ID);

    let item = mcp
        .call(
            "org_get_item",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await
        .unwrap();
    assert_eq!(item["workspace_id"], WORKSPACE_ID);
    let context = mcp
        .call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await
        .unwrap();
    assert_eq!(context["workspace"]["policy"]["initial_state"], "BACKLOG");
    assert_eq!(context["workspace_revision"], 1);

    let queue = mcp.call("org_query_queue", json!({
        "workspace_ids": [WORKSPACE_ID], "view": "ready", "item_type": "task", "state": "READY",
        "priority": "A", "tags": ["mcp"], "assignee": "agent-one", "include_archived": false
    })).await.unwrap();
    assert_eq!(queue["evaluated_at"], NOW);
    assert_eq!(queue["items"][0]["item"]["id"], ITEM_ID);
}

#[tokio::test]
async fn queue_and_agenda_expose_only_their_typed_views() {
    let mcp = TestServer::new().await;
    assert!(mcp
        .call(
            "org_query_queue",
            json!({"workspace_ids": [WORKSPACE_ID], "view": "scheduled"})
        )
        .await
        .is_err());
    assert!(mcp
        .call(
            "org_query_agenda",
            json!({"workspace_ids": [WORKSPACE_ID], "view": "ready"})
        )
        .await
        .is_err());
}

#[tokio::test]
async fn typed_item_and_field_patch_inputs_reject_out_of_contract_values() {
    let mcp = TestServer::new().await;
    assert!(mcp
        .call(
            "org_create_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "bad-type", "document_id": DOCUMENT_ID, "parent_id": null,
                "item_id": ITEM_ID, "item_type": "unknown", "title": "bad",
                "initial_state": null, "priority": null, "tags": [], "assignee": null,
                "requires_review": false, "expected_revisions": {}
            }),
        )
        .await
        .is_err());
    assert!(mcp
        .call(
            "org_schedule_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "bad-patch", "item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "scheduled": {"action": "set", "value": "<2027-01-15 Fri>", "extra": true},
                "deadline": {"action": "unchanged"}, "expected_revisions": {}, "lease": null
            }),
        )
        .await
        .is_err());
}

#[tokio::test]
async fn follow_up_assignment_and_schedule_return_strict_pipeline_results() {
    let mcp = seeded().await;
    let created = create_item(&mcp, ITEM_ID, "create-root", 1).await;
    let events_after_create = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(create_item(&mcp, ITEM_ID, "create-root", 1).await, created);
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_after_create
    );

    let follow_up_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "create-follow-up",
        "item": {
            "document_id": DOCUMENT_ID, "parent_id": null, "item_id": FOLLOW_UP_ID,
            "item_type": "task", "title": "Follow up", "initial_state": "BACKLOG",
            "priority": null, "tags": ["follow-up"], "assignee": null,
            "requires_review": false, "expected_revisions": { DOCUMENT_ID: 2 }
        },
        "origin": {"kind": "work_item", "id": ITEM_ID}
    });
    let follow_up = mcp
        .call("org_create_follow_up", follow_up_input.clone())
        .await
        .unwrap();
    assert_eq!(
        follow_up["data"]["operation"]["origin"]["work_item_id"],
        ITEM_ID
    );
    assert_eq!(follow_up["data"]["resulting_items"][0]["id"], FOLLOW_UP_ID);
    let events_after_follow_up = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(
        mcp.call("org_create_follow_up", follow_up_input)
            .await
            .unwrap(),
        follow_up
    );
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_after_follow_up
    );

    let assign_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "assign-root", "item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "assignee": "agent-two", "expected_revisions": { DOCUMENT_ID: 3 }, "lease": null
    });
    let assigned = mcp
        .call("org_assign_item", assign_input.clone())
        .await
        .unwrap();
    assert_eq!(
        assigned["data"]["operation"]["previous_assignee"],
        "agent-one"
    );
    assert_eq!(
        assigned["data"]["operation"]["resulting_assignee"],
        "agent-two"
    );
    let events_after_assign = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(
        mcp.call("org_assign_item", assign_input).await.unwrap(),
        assigned
    );
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_after_assign
    );

    let schedule_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "schedule-root", "item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "scheduled": {"action": "set", "value": "<2027-01-15 Fri 09:00>"},
        "deadline": {"action": "unchanged"},
        "expected_revisions": { DOCUMENT_ID: 4 }, "lease": null
    });
    let scheduled = mcp
        .call("org_schedule_item", schedule_input.clone())
        .await
        .unwrap();
    assert_eq!(
        scheduled["data"]["operation"]["resulting_scheduled"],
        "<2027-01-15 Fri 09:00>"
    );
    let events_before_replay = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let replay = mcp.call("org_schedule_item", schedule_input).await.unwrap();
    assert_eq!(replay, scheduled);
    let session = mcp.backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(DOCUMENT_ID.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .revision,
        5
    );
    assert_eq!(
        session
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_replay
    );

    let item = mcp
        .call(
            "org_get_item",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await
        .unwrap();
    assert_eq!(item["scheduled"]["raw"], "<2027-01-15 Fri 09:00>");
    assert_eq!(item["scheduled"]["local"], "2027-01-15 09:00:00");
    assert_eq!(item["scheduled"]["timezone"], "Asia/Shanghai");
    assert_eq!(item["scheduled"]["utc_timestamp"], 1_799_974_800_i64);
}

fn operational_args(workspace_ids: Vec<&str>, view: &str) -> Value {
    json!({
        "workspace_ids": workspace_ids, "view": view, "item_type": null, "state": null,
        "priority": null, "tags": [], "assignee": null, "scheduled_from": null,
        "scheduled_to": null, "deadline_from": null, "deadline_to": null,
        "completed_from": null, "completed_to": null, "from": null, "to": null,
        "include_archived": false, "cursor": null
    })
}

#[tokio::test]
async fn all_ten_operational_views_return_the_seeded_workspace_items() {
    let mcp = seeded().await;
    let ids = seed_all_operational_views(&mcp).await;
    let expected = [
        ("org_query_queue", "ready", ids[2].as_str()),
        ("org_query_queue", "assigned", ids[0].as_str()),
        ("org_query_queue", "running", ids[2].as_str()),
        ("org_query_queue", "blocked", ids[3].as_str()),
        ("org_query_queue", "review", ids[4].as_str()),
        ("org_query_agenda", "scheduled", ids[5].as_str()),
        ("org_query_agenda", "upcoming_deadline", ids[6].as_str()),
        ("org_query_queue", "failed", ids[7].as_str()),
        ("org_query_queue", "expired_lease", ids[2].as_str()),
        ("org_query_queue", "completed", ids[8].as_str()),
    ];
    for (tool, view, expected_id) in expected {
        let page = mcp
            .call(tool, operational_args(vec![WORKSPACE_ID], view))
            .await
            .unwrap();
        assert_eq!(page["evaluated_at"], NOW, "{view}");
        let actual = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["item"]["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(actual.contains(&expected_id), "{view}: {actual:?}");
        assert!(page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["item"]["workspace_id"] == WORKSPACE_ID));
    }
}

#[tokio::test]
async fn operational_filters_timestamps_limits_and_cursors_are_pipeline_owned() {
    let mcp = seeded().await;
    let ids = seed_all_operational_views(&mcp).await;
    let mut filtered = operational_args(vec![WORKSPACE_ID], "assigned");
    let object = filtered.as_object_mut().unwrap();
    object.insert("item_type".into(), json!("task"));
    object.insert("state".into(), json!("READY"));
    object.insert("priority".into(), json!("A"));
    object.insert("tags".into(), json!(["mcp"]));
    object.insert("assignee".into(), json!("agent-one"));
    object.insert("scheduled_from".into(), json!(NOW));
    object.insert("scheduled_to".into(), json!(NOW));
    object.insert("deadline_from".into(), json!(NOW));
    object.insert("deadline_to".into(), json!(NOW));
    let page = mcp.call("org_query_queue", filtered).await.unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["item"]["id"], ids[0]);
    let stamp = &page["items"][0]["item"]["scheduled"];
    assert_eq!(stamp["raw"], "<2027-01-15 Fri 09:00>");
    assert_eq!(stamp["local"], "2027-01-15T09:00:00");
    assert_eq!(stamp["timezone"], "Asia/Shanghai");
    assert_eq!(stamp["utc_timestamp"], NOW);

    for (tool, view) in [
        ("org_query_queue", "assigned"),
        ("org_query_agenda", "scheduled"),
    ] {
        let mut args = operational_args(vec![WORKSPACE_ID], view);
        args.as_object_mut()
            .unwrap()
            .insert("priority".into(), json!("none"));
        let unprioritized = mcp.call(tool, args).await.unwrap();
        assert!(!unprioritized["items"].as_array().unwrap().is_empty());
        assert!(unprioritized["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["item"]["priority"].is_null()));
    }

    let mut agenda = operational_args(vec![WORKSPACE_ID], "scheduled");
    agenda
        .as_object_mut()
        .unwrap()
        .insert("from".into(), json!(NOW));
    agenda
        .as_object_mut()
        .unwrap()
        .insert("to".into(), json!(NOW));
    assert!(
        !mcp.call("org_query_agenda", agenda).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let mut first_args = operational_args(vec![WORKSPACE_ID], "assigned");
    first_args
        .as_object_mut()
        .unwrap()
        .insert("limit".into(), json!(1));
    let first = mcp
        .call("org_query_queue", first_args.clone())
        .await
        .unwrap();
    assert_eq!(
        mcp.call("org_query_queue", first_args.clone())
            .await
            .unwrap()["next_cursor"],
        first["next_cursor"]
    );
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let mut wrong_view = operational_args(vec![WORKSPACE_ID], "running");
    wrong_view
        .as_object_mut()
        .unwrap()
        .insert("cursor".into(), json!(cursor.clone()));
    assert!(mcp.call("org_query_queue", wrong_view).await.is_err());
    let mut wrong_family = operational_args(vec![WORKSPACE_ID], "scheduled");
    wrong_family
        .as_object_mut()
        .unwrap()
        .insert("cursor".into(), json!(cursor.clone()));
    assert!(mcp.call("org_query_agenda", wrong_family).await.is_err());
    first_args
        .as_object_mut()
        .unwrap()
        .insert("cursor".into(), json!(cursor));
    let second = mcp
        .call("org_query_queue", first_args.clone())
        .await
        .unwrap();
    assert_eq!(second["evaluated_at"], first["evaluated_at"]);
    assert_ne!(
        second["items"][0]["item"]["id"],
        first["items"][0]["item"]["id"]
    );

    first_args
        .as_object_mut()
        .unwrap()
        .insert("tags".into(), json!(["changed"]));
    assert!(mcp.call("org_query_queue", first_args).await.is_err());
    let mut malformed = operational_args(vec![WORKSPACE_ID], "ready");
    malformed
        .as_object_mut()
        .unwrap()
        .insert("cursor".into(), json!("not-a-cursor"));
    assert!(mcp.call("org_query_queue", malformed).await.is_err());
    for invalid in ["None", "AA", "a"] {
        let mut args = operational_args(vec![WORKSPACE_ID], "ready");
        args.as_object_mut()
            .unwrap()
            .insert("priority".into(), json!(invalid));
        assert!(mcp.call("org_query_queue", args).await.is_err());
    }
    let mut maximum = operational_args(vec![WORKSPACE_ID], "ready");
    maximum
        .as_object_mut()
        .unwrap()
        .insert("limit".into(), json!(200));
    mcp.call("org_query_queue", maximum).await.unwrap();
    let mut too_large = operational_args(vec![WORKSPACE_ID], "ready");
    too_large
        .as_object_mut()
        .unwrap()
        .insert("limit".into(), json!(201));
    assert!(mcp.call("org_query_queue", too_large).await.is_err());

    let mut completed = operational_args(vec![WORKSPACE_ID], "completed");
    completed
        .as_object_mut()
        .unwrap()
        .insert("completed_from".into(), json!(NOW));
    completed
        .as_object_mut()
        .unwrap()
        .insert("completed_to".into(), json!(NOW));
    let completed_page = mcp
        .call("org_query_queue", completed.clone())
        .await
        .unwrap();
    assert_eq!(completed_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(completed_page["items"][0]["item"]["id"], ids[8]);
    completed
        .as_object_mut()
        .unwrap()
        .insert("completed_from".into(), json!(NOW + 1));
    completed
        .as_object_mut()
        .unwrap()
        .insert("completed_to".into(), json!(NOW + 1));
    assert!(
        mcp.call("org_query_queue", completed).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let session = mcp.backend.session().await.unwrap();
    let template = session
        .get_org_work_item(ids[0].parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut assigned_rows = Vec::new();
    for suffix in 100..151 {
        let mut row = template.clone();
        row.id = format!("30000000-0000-4000-8000-{suffix:012}")
            .parse()
            .unwrap();
        row.source_order = i64::from(suffix);
        row.state = Some("BACKLOG".into());
        row.assignee = Some("agent-one".into());
        row.scheduled = None;
        row.deadline = None;
        assigned_rows.push(row);
    }
    session
        .replace_org_document_projection(DOCUMENT_ID.parse().unwrap(), &assigned_rows)
        .await
        .unwrap();
    let default_page = mcp
        .call(
            "org_query_queue",
            operational_args(vec![WORKSPACE_ID], "assigned"),
        )
        .await
        .unwrap();
    assert_eq!(default_page["items"].as_array().unwrap().len(), 50);
    assert!(default_page["next_cursor"].is_string());
    let mut max_page = operational_args(vec![WORKSPACE_ID], "assigned");
    max_page
        .as_object_mut()
        .unwrap()
        .insert("limit".into(), json!(200));
    let max_len = mcp.call("org_query_queue", max_page).await.unwrap()["items"]
        .as_array()
        .unwrap()
        .len();
    assert!(max_len > 50 && max_len <= 200);
}

#[tokio::test]
async fn multi_workspace_queries_require_every_workspace_to_opt_in() {
    const SECOND_WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000032";
    let rejected = seeded().await;
    create_workspace(&rejected, SECOND_WORKSPACE_ID, true).await;
    assert!(rejected
        .call(
            "org_query_queue",
            operational_args(vec![WORKSPACE_ID, SECOND_WORKSPACE_ID], "ready")
        )
        .await
        .is_err());

    let allowed = TestServer::new().await;
    create_workspace(&allowed, WORKSPACE_ID, true).await;
    create_workspace(&allowed, SECOND_WORKSPACE_ID, true).await;
    seed_workspace_assigned_item(
        &allowed,
        WORKSPACE_ID,
        "20000000-0000-4000-8000-000000000033",
        "30000000-0000-4000-8000-000000000033",
    )
    .await;
    seed_workspace_assigned_item(
        &allowed,
        SECOND_WORKSPACE_ID,
        "20000000-0000-4000-8000-000000000034",
        "30000000-0000-4000-8000-000000000034",
    )
    .await;
    let page = allowed
        .call(
            "org_query_queue",
            operational_args(vec![WORKSPACE_ID, SECOND_WORKSPACE_ID], "assigned"),
        )
        .await
        .unwrap();
    assert_eq!(page["evaluated_at"], NOW);
    let workspace_ids = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["item"]["workspace_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        workspace_ids,
        [WORKSPACE_ID, SECOND_WORKSPACE_ID].into_iter().collect()
    );

    let alias = format!(" {WORKSPACE_ID} ");
    assert!(allowed
        .call(
            "org_query_queue",
            operational_args(vec![WORKSPACE_ID, alias.as_str()], "ready")
        )
        .await
        .is_err());
}

#[tokio::test]
async fn item_context_is_complete_ordered_and_redacts_nested_sensitive_metadata() {
    let mcp = seeded().await;
    create_item(&mcp, ITEM_ID, "create-parent", 1).await;
    mcp.call(
        "org_create_item",
        json!({
            "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
            "operation_id": "create-child", "document_id": DOCUMENT_ID, "parent_id": ITEM_ID,
            "item_id": FOLLOW_UP_ID, "item_type": "task", "title": "Context child",
            "initial_state": "READY", "priority": "B", "tags": ["context"],
            "assignee": "agent-one", "requires_review": false,
            "expected_revisions": { DOCUMENT_ID: 2 }
        }),
    )
    .await
    .unwrap();

    let envelope = |operation_id: &str| CommandEnvelope {
        schema_version: 1,
        workspace_id: WORKSPACE_ID.parse().unwrap(),
        actor_id: "agent-one".into(),
        operation_id: operation_id.into(),
    };
    add_dependency(
        &mcp.org,
        &envelope("add-dependency"),
        &DependencyRequest {
            item_id: FOLLOW_UP_ID.parse().unwrap(),
            dependency_id: ITEM_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            expected_revisions: [(DOCUMENT_ID.parse().unwrap(), 3)].into_iter().collect(),
            lease: None,
        },
    )
    .await
    .unwrap();

    let note = mcp
        .call(
            "save_note",
            json!({
                "title": "Available context", "content": "linked", "labels": []
            }),
        )
        .await
        .unwrap();
    let note_id = note["id"].as_str().unwrap().to_owned();
    let note_revision = note["revision"].as_i64().unwrap();
    link_note(
        &mcp.org,
        &envelope("link-note"),
        &NoteLinkRequest {
            item_id: FOLLOW_UP_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            purpose: "context".into(),
            note_id: note_id.parse().unwrap(),
            description: "available note".into(),
            expected_revisions: [(DOCUMENT_ID.parse().unwrap(), 4)].into_iter().collect(),
            lease: None,
        },
    )
    .await
    .unwrap();

    let session = mcp.backend.session().await.unwrap();
    let mut projection = session
        .list_org_workspace_projection(WORKSPACE_ID.parse().unwrap())
        .await
        .unwrap();
    projection
        .iter_mut()
        .find(|item| item.id.to_string() == ITEM_ID)
        .unwrap()
        .state = Some("DONE".into());
    session
        .replace_org_document_projection(DOCUMENT_ID.parse().unwrap(), &projection)
        .await
        .unwrap();
    let claim = claim_item(
        &mcp.org,
        &envelope("claim-child"),
        &StartClaimRequest {
            work_item_id: FOLLOW_UP_ID.parse().unwrap(),
            document_id: DOCUMENT_ID.parse().unwrap(),
            expected_document_revision: 5,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let attempt_id = claim.context.attempts.last().unwrap().id.clone();
    report_progress(
        &mcp.org,
        &envelope("progress-child"),
        &ReportProgressRequest {
            schema_version: 1,
            work_item_id: FOLLOW_UP_ID.parse().unwrap(),
            lease_id: claim.lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token,
            summary: "working".into(),
            metadata: json!({"phase": "build"}),
        },
    )
    .await
    .unwrap();
    session
        .append_org_event(NewOrgEvent {
            id: "legacy-sensitive-progress",
            workspace_id: WORKSPACE_ID.parse().unwrap(),
            subject_kind: "work_item",
            subject_id: FOLLOW_UP_ID,
            actor_id: "legacy-agent",
            attempt_id: Some(&attempt_id),
            event_type: OrgEventType::Progress,
            occurred_at: NOW,
            summary: "legacy sensitive metadata",
            metadata: &json!({
                "nested": {"lease_token": "raw-lease-token", "lease_hash": "raw-hash"},
                "fencingToken": "raw-fencing-token", "secretary": "kept", "passwordless": "kept-too"
            }),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();

    let context = mcp
        .call(
            "org_get_item_context",
            json!({
                "workspace_id": WORKSPACE_ID, "item_id": FOLLOW_UP_ID
            }),
        )
        .await
        .unwrap();
    assert_eq!(context["workspace"]["policy"]["initial_state"], "BACKLOG");
    assert_eq!(context["workspace_revision"], 1);
    assert_eq!(context["document"]["revision"], 6);
    assert_eq!(context["parent"]["id"], ITEM_ID);
    let parent_context = mcp
        .call(
            "org_get_item_context",
            json!({
                "workspace_id": WORKSPACE_ID, "item_id": ITEM_ID
            }),
        )
        .await
        .unwrap();
    assert!(parent_context["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|child| child["id"] == FOLLOW_UP_ID));
    assert_eq!(context["dependencies"][0]["item"]["id"], ITEM_ID);
    assert_eq!(context["note_links"][0]["note_id"], note_id);
    assert_eq!(context["note_links"][0]["available"], true);
    assert_eq!(context["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(context["lease"]["work_item_id"], FOLLOW_UP_ID);
    assert!(context["operational"]["recovery"]["eligible"].is_boolean());
    let events = context["history_segments"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|segment| segment["events"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(events
        .windows(2)
        .all(|pair| pair[0]["sequence"].as_i64() < pair[1]["sequence"].as_i64()));
    let progress = events
        .iter()
        .find(|event| event["summary"] == "legacy sensitive metadata")
        .unwrap();
    assert_eq!(progress["metadata"]["nested"]["lease_token"], "[REDACTED]");
    assert_eq!(progress["metadata"]["nested"]["lease_hash"], "[REDACTED]");
    assert_eq!(progress["metadata"]["fencingToken"], "[REDACTED]");
    assert_eq!(progress["metadata"]["secretary"], "kept");
    assert_eq!(progress["metadata"]["passwordless"], "kept-too");
    let encoded = serde_json::to_string(&context).unwrap();
    assert!(!encoded.contains("raw-lease-token"));
    assert!(!encoded.contains("raw-fencing-token"));
    assert!(!encoded.contains("fencing_token_hash"));
    mcp.call(
        "delete_note",
        json!({"id": note_id, "expected_revision": note_revision}),
    )
    .await
    .unwrap();
    let unavailable = mcp
        .call(
            "org_get_item_context",
            json!({
                "workspace_id": WORKSPACE_ID, "item_id": FOLLOW_UP_ID
            }),
        )
        .await
        .unwrap();
    assert_eq!(unavailable["note_links"][0]["available"], false);
}
