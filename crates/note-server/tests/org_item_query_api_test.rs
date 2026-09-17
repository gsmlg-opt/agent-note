use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId};
use note_pipelines::{
    org::{FixedOrgClock, OrgContext},
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::{
    ConditionalUpdate, NewOrgAttempt, NewOrgDocument, NewOrgEvent, NewOrgLease, OrgAttemptStatus,
    OrgEventType, OrgLeaseKind, OrgProjectedWorkItem, StorageBackend, StoredOrgTimestamp,
};
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

const NOW: i64 = 1_800_000_000;
const WORKSPACE_A: &str = "10000000-0000-4000-8000-000000000101";
const WORKSPACE_B: &str = "10000000-0000-4000-8000-000000000102";
const WORKSPACE_C: &str = "10000000-0000-4000-8000-000000000103";
const DOCUMENT_A: &str = "20000000-0000-4000-8000-000000000101";
const ITEM_A: &str = "30000000-0000-4000-8000-000000000101";
const ITEM_B: &str = "30000000-0000-4000-8000-000000000102";
const ITEM_C: &str = "30000000-0000-4000-8000-000000000103";

struct TestApp {
    app: Router,
    mcp: Router,
    backend: Arc<dyn StorageBackend>,
    org: Arc<OrgContext>,
    _dir: tempfile::TempDir,
}

impl TestApp {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-item-rest.db"))
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
            app: app.with_state(AppState::new(note, org.clone())),
            mcp,
            backend,
            org,
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
                    .uri("/org/mcp")
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
        let body = if let Some(body) = body {
            request = request.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "application/json");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn create_workspace(&self, workspace_id: &str, operation_id: &str, cross: bool) {
        let mut policy =
            serde_json::to_value(note_org::WorkspacePolicy::engineering_default()).unwrap();
        policy["allow_cross_workspace_agenda"] = json!(cross);
        let (status, _) = self
            .call(
                Method::POST,
                "/api/org/workspaces",
                Some(json!({
                    "schema_version": 1,
                    "actor_id": "agent-one",
                    "operation_id": operation_id,
                    "workspace_id": workspace_id,
                    "slug": format!("workspace-{operation_id}"),
                    "display_name": "Engineering",
                    "description": "Task 3 REST test",
                    "timezone": "Asia/Shanghai",
                    "policy_schema_version": 1,
                    "policy": policy
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn put_empty_document(&self, workspace_id: &str, document_id: &str, operation_id: &str) {
        let (status, _) = self
            .call(
                Method::PUT,
                &format!("/api/org/documents/{document_id}"),
                Some(json!({
                    "schema_version": 1,
                    "actor_id": "agent-one",
                    "operation_id": operation_id,
                    "workspace_id": workspace_id,
                    "path": "tasks.org",
                    "source": "",
                    "expected_revision": null,
                    "lease_proofs": {}
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}

fn item_payload(item_id: &str, title: &str, expected_revision: i64) -> Value {
    json!({
        "document_id": DOCUMENT_A,
        "parent_id": null,
        "item_id": item_id,
        "item_type": "task",
        "title": title,
        "initial_state": "READY",
        "priority": "A",
        "tags": ["rest", "ops"],
        "assignee": "agent-one",
        "requires_review": false,
        "expected_revisions": {DOCUMENT_A: expected_revision}
    })
}

fn command(operation_id: &str) -> Value {
    json!({
        "schema_version": 1,
        "actor_id": "agent-one",
        "operation_id": operation_id,
        "workspace_id": WORKSPACE_A
    })
}

fn path_command(operation_id: &str) -> Value {
    json!({
        "schema_version": 1,
        "actor_id": "agent-one",
        "operation_id": operation_id
    })
}

fn extend(mut base: Value, fields: &[(&str, Value)]) -> Value {
    let object = base.as_object_mut().unwrap();
    for (key, value) in fields {
        object.insert((*key).to_owned(), value.clone());
    }
    base
}

fn merge(mut base: Value, fields: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    base
}

#[tokio::test]
async fn item_routes_create_read_follow_up_assign_schedule_and_preserve_org_times() {
    let api = TestApp::new().await;
    api.create_workspace(WORKSPACE_A, "create-workspace", false)
        .await;
    api.put_empty_document(WORKSPACE_A, DOCUMENT_A, "put-document")
        .await;

    let create = merge(path_command("create-item"), item_payload(ITEM_A, "Root", 1));
    let (status, created) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_A}/items"),
            Some(create.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["resulting_items"][0]["id"], ITEM_A);
    assert_eq!(
        created,
        api.mcp_call(
            "org_create_item",
            merge(command("create-item"), item_payload(ITEM_A, "Root", 1)),
        )
        .await,
    );
    let (_, replay) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_A}/items"),
            Some(create),
        )
        .await;
    assert_eq!(replay, created);

    let (status, item) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{ITEM_A}?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["workspace_id"], WORKSPACE_A);
    assert_eq!(item["title"], "Root");
    assert_eq!(
        item,
        api.mcp_call(
            "org_get_item",
            json!({"workspace_id": WORKSPACE_A, "item_id": ITEM_A}),
        )
        .await,
    );
    assert_eq!(
        item,
        serde_json::to_value(
            note_pipelines::org::get_item(
                &api.org,
                WORKSPACE_A.parse().unwrap(),
                ITEM_A.parse().unwrap(),
            )
            .await
            .unwrap()
        )
        .unwrap(),
        "REST item and MCP's shared pipeline source must serialize identically",
    );

    let follow_up = extend(
        command("create-follow-up"),
        &[
            ("item", item_payload(ITEM_B, "Follow up", 2)),
            ("origin", json!({"kind": "work_item", "id": ITEM_A})),
        ],
    );
    let (status, followed) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_B}/follow-ups"),
            Some(follow_up.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        followed["data"]["operation"]["origin"]["work_item_id"],
        ITEM_A
    );
    assert_eq!(followed["data"]["resulting_items"][0]["id"], ITEM_B);
    assert_eq!(
        followed,
        api.mcp_call("org_create_follow_up", follow_up).await,
    );

    let event_item = item_payload(ITEM_C, "Event follow up", 3);
    let event_follow_up = extend(
        command("create-event-follow-up"),
        &[
            ("item", event_item),
            (
                "origin",
                json!({"kind": "event", "id": created["event_ids"][0]}),
            ),
        ],
    );
    let (status, error) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/follow-ups"),
            Some(event_follow_up.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["code"], "invalid_input");
    let (status, event_followed) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_C}/follow-ups"),
            Some(event_follow_up.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        event_followed,
        api.mcp_call("org_create_follow_up", event_follow_up).await,
    );

    let assign = extend(
        command("assign-item"),
        &[
            ("document_id", json!(DOCUMENT_A)),
            ("assignee", json!("agent-two")),
            ("expected_revisions", json!({DOCUMENT_A: 4})),
            ("lease", Value::Null),
        ],
    );
    let (status, assigned) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/assignment"),
            Some(assign.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        assigned["data"]["operation"]["resulting_assignee"],
        "agent-two"
    );
    assert_eq!(
        assigned,
        api.mcp_call(
            "org_assign_item",
            extend(assign, &[("item_id", json!(ITEM_A))]),
        )
        .await,
    );

    let schedule = extend(
        command("schedule-item"),
        &[
            ("document_id", json!(DOCUMENT_A)),
            (
                "scheduled",
                json!({"action": "set", "value": "<2027-01-15 Fri 09:00>"}),
            ),
            (
                "deadline",
                json!({"action": "set", "value": "<2027-01-16 Sat 17:30>"}),
            ),
            ("expected_revisions", json!({DOCUMENT_A: 5})),
            ("lease", Value::Null),
        ],
    );
    let (status, schedule_result) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/schedule"),
            Some(schedule.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        schedule_result,
        api.mcp_call(
            "org_schedule_item",
            extend(schedule, &[("item_id", json!(ITEM_A))]),
        )
        .await,
    );
    let (_, scheduled) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{ITEM_A}?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(scheduled["scheduled"]["raw"], "<2027-01-15 Fri 09:00>");
    assert_eq!(scheduled["scheduled"]["timezone"], "Asia/Shanghai");
    assert!(scheduled["scheduled"]["utc_timestamp"].is_i64());
    assert_eq!(
        scheduled,
        api.mcp_call(
            "org_get_item",
            json!({"workspace_id": WORKSPACE_A, "item_id": ITEM_A}),
        )
        .await,
    );
    let (_, context) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{ITEM_A}/context?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(
        context,
        api.mcp_call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_A, "item_id": ITEM_A}),
        )
        .await,
    );
}

fn timestamp(raw: &str) -> StoredOrgTimestamp {
    StoredOrgTimestamp {
        raw: raw.to_owned(),
        local: "2027-01-15T09:00:00".to_owned(),
        timezone: "Asia/Shanghai".to_owned(),
        utc_timestamp: NOW,
    }
}

fn row(
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    item_id: WorkItemId,
    state: &str,
    source_order: i64,
) -> OrgProjectedWorkItem {
    OrgProjectedWorkItem {
        id: item_id,
        workspace_id,
        document_id,
        parent_id: None,
        source_order,
        item_type: WorkItemType::Task,
        title: format!("Item {source_order}"),
        state: Some(state.to_owned()),
        priority: Some('A'),
        scheduled: None,
        deadline: None,
        assignee: None,
        requires_review: false,
        created_at: 100,
        tags: vec!["ops".to_owned()],
        dependencies: Vec::new(),
        note_links: Vec::new(),
    }
}

fn workspace(value: &str) -> WorkspaceId {
    value.parse().unwrap()
}

fn document(value: u32) -> DocumentId {
    format!("20000000-0000-4000-8000-{value:012}")
        .parse()
        .unwrap()
}

fn item(value: u32) -> WorkItemId {
    format!("30000000-0000-4000-8000-{value:012}")
        .parse()
        .unwrap()
}

async fn seed_views(api: &TestApp, workspace_id: &str, document_id: DocumentId, base: u32) {
    let workspace_id = workspace(workspace_id);
    let session = api.backend.session().await.unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "operational.org",
            source: "",
            content_hash: "seed",
            now: 1,
        })
        .await
        .unwrap();
    let mut rows = vec![
        row(workspace_id, document_id, item(base), "READY", 1),
        row(workspace_id, document_id, item(base + 1), "BACKLOG", 2),
        row(workspace_id, document_id, item(base + 2), "RUNNING", 3),
        row(workspace_id, document_id, item(base + 3), "BLOCKED", 4),
        row(workspace_id, document_id, item(base + 4), "REVIEW", 5),
        row(workspace_id, document_id, item(base + 5), "BACKLOG", 6),
        row(workspace_id, document_id, item(base + 6), "BACKLOG", 7),
        row(workspace_id, document_id, item(base + 7), "FAILED", 8),
        row(workspace_id, document_id, item(base + 8), "DONE", 9),
    ];
    rows[0].assignee = Some("agent-one".to_owned());
    rows[0].scheduled = Some(timestamp("<2027-01-15 Fri 09:00>"));
    rows[0].deadline = Some(timestamp("<2027-01-15 Fri 09:00>"));
    rows[1].assignee = Some("agent-one".to_owned());
    rows[1].priority = None;
    rows[5].scheduled = Some(timestamp("<2027-01-15 Fri 09:00>"));
    rows[5].priority = None;
    rows[6].deadline = Some(timestamp("<2027-01-15 Fri 09:00>"));
    session
        .replace_org_document_projection(document_id, &rows)
        .await
        .unwrap();
    let running_attempt = format!("running-attempt-{base}");
    session
        .insert_org_attempt(NewOrgAttempt {
            id: &running_attempt,
            workspace_id,
            work_item_id: item(base + 2),
            attempt_number: 1,
            actor_id: "agent-one",
            status: OrgAttemptStatus::Running,
            started_at: NOW - 100,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    for attempt_number in 1..=3 {
        session
            .insert_org_attempt(NewOrgAttempt {
                id: &format!("failed-attempt-{base}-{attempt_number}"),
                workspace_id,
                work_item_id: item(base + 7),
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
                    id: &format!("expired-lease-{base}"),
                    workspace_id,
                    work_item_id: item(base + 2),
                    attempt_id: &running_attempt,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-one",
                    fencing_token_hash:
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    acquired_at: NOW - 100,
                    last_heartbeat_at: NOW - 100,
                    expires_at: NOW,
                },
                4,
                NOW - 100,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    session
        .append_org_event(NewOrgEvent {
            id: &format!("completion-{base}"),
            workspace_id,
            subject_kind: "work_item",
            subject_id: &item(base + 8).to_string(),
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
}

fn operational_uri(route: &str, workspace_ids: &str, view: &str, extra: &str) -> String {
    format!(
        "/api/org/{route}?workspace_ids={workspace_ids}&view={view}&include_archived=false{extra}"
    )
}

fn mcp_operational_args(workspace_ids: Vec<&str>, view: &str) -> Value {
    json!({
        "workspace_ids": workspace_ids,
        "view": view,
        "item_type": null,
        "state": null,
        "priority": null,
        "tags": [],
        "assignee": null,
        "scheduled_from": null,
        "scheduled_to": null,
        "deadline_from": null,
        "deadline_to": null,
        "completed_from": null,
        "completed_to": null,
        "from": null,
        "to": null,
        "include_archived": false,
        "cursor": null
    })
}

#[tokio::test]
async fn queue_and_agenda_cover_all_views_filters_multi_workspace_cursors_and_limits() {
    let api = TestApp::new().await;
    api.create_workspace(WORKSPACE_A, "workspace-a", true).await;
    api.create_workspace(WORKSPACE_B, "workspace-b", true).await;
    api.create_workspace(WORKSPACE_C, "workspace-c", false)
        .await;
    seed_views(&api, WORKSPACE_A, document(201), 201).await;
    seed_views(&api, WORKSPACE_B, document(301), 301).await;

    for (route, view) in [
        ("queue", "ready"),
        ("queue", "assigned"),
        ("queue", "running"),
        ("queue", "blocked"),
        ("queue", "review"),
        ("agenda", "scheduled"),
        ("agenda", "upcoming_deadline"),
        ("queue", "failed"),
        ("queue", "expired_lease"),
        ("queue", "completed"),
    ] {
        let (status, page) = api
            .call(
                Method::GET,
                &operational_uri(route, WORKSPACE_A, view, ""),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{route}/{view}: {page}");
        assert_eq!(page["evaluated_at"], NOW);
        assert!(!page["items"].as_array().unwrap().is_empty(), "{view}");
        assert!(page["items"].as_array().unwrap().iter().all(|row| {
            row["item"]["workspace_id"] == WORKSPACE_A
                && row.get("fencing_token").is_none()
                && row.get("token_hash").is_none()
        }));
        assert_eq!(
            page,
            api.mcp_call(
                if route == "queue" {
                    "org_query_queue"
                } else {
                    "org_query_agenda"
                },
                mcp_operational_args(vec![WORKSPACE_A], view),
            )
            .await,
            "REST/MCP operational parity for {view}",
        );
    }

    let filter = "&item_type=task&state=READY&priority=A&tags=ops&assignee=agent-one&scheduled_from=1800000000&scheduled_to=1800000000&deadline_from=1800000000&deadline_to=1800000000";
    let (_, filtered) = api
        .call(
            Method::GET,
            &operational_uri("queue", WORKSPACE_A, "assigned", filter),
            None,
        )
        .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        filtered["items"][0]["item"]["scheduled"]["raw"],
        "<2027-01-15 Fri 09:00>"
    );

    for (route, view) in [("queue", "assigned"), ("agenda", "scheduled")] {
        let (_, rest) = api
            .call(
                Method::GET,
                &operational_uri(route, WORKSPACE_A, view, "&priority=none"),
                None,
            )
            .await;
        let mut args = mcp_operational_args(vec![WORKSPACE_A], view);
        args.as_object_mut()
            .unwrap()
            .insert("priority".into(), json!("none"));
        let mcp = api
            .mcp_call(
                if route == "queue" {
                    "org_query_queue"
                } else {
                    "org_query_agenda"
                },
                args,
            )
            .await;
        assert_eq!(rest, mcp, "REST/MCP priority=none parity");
        assert!(!rest["items"].as_array().unwrap().is_empty());
        assert!(rest["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["item"]["priority"].is_null()));
    }

    let (_, agenda) = api
        .call(
            Method::GET,
            &operational_uri(
                "agenda",
                WORKSPACE_A,
                "scheduled",
                "&from=1800000000&to=1800000000",
            ),
            None,
        )
        .await;
    assert!(!agenda["items"].as_array().unwrap().is_empty());
    let (_, completed) = api
        .call(
            Method::GET,
            &operational_uri(
                "queue",
                WORKSPACE_A,
                "completed",
                "&completed_from=1800000000&completed_to=1800000000",
            ),
            None,
        )
        .await;
    assert_eq!(completed["items"].as_array().unwrap().len(), 1);

    let multi = format!("{WORKSPACE_A},{WORKSPACE_B}");
    let (_, combined) = api
        .call(
            Method::GET,
            &operational_uri("queue", &multi, "assigned", ""),
            None,
        )
        .await;
    let identities = combined["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["item"]["workspace_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(identities, [WORKSPACE_A, WORKSPACE_B].into_iter().collect());

    let forbidden_multi = format!("{WORKSPACE_A},{WORKSPACE_C}");
    let (status, error) = api
        .call(
            Method::GET,
            &operational_uri("queue", &forbidden_multi, "ready", ""),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["code"], "invalid_input");

    let (_, first) = api
        .call(
            Method::GET,
            &operational_uri("queue", WORKSPACE_A, "assigned", "&limit=1"),
            None,
        )
        .await;
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, second) = api
        .call(
            Method::GET,
            &operational_uri(
                "queue",
                WORKSPACE_A,
                "assigned",
                &format!("&limit=1&cursor={cursor}"),
            ),
            None,
        )
        .await;
    assert_ne!(
        first["items"][0]["item"]["id"],
        second["items"][0]["item"]["id"]
    );
    assert_eq!(first["evaluated_at"], second["evaluated_at"]);

    for uri in [
        operational_uri("queue", WORKSPACE_A, "assigned", "&limit=201"),
        operational_uri(
            "queue",
            WORKSPACE_A,
            "running",
            &format!("&limit=1&cursor={cursor}"),
        ),
        operational_uri("agenda", WORKSPACE_A, "ready", ""),
        operational_uri("queue", WORKSPACE_A, "ready", "&cursor=not-a-cursor"),
        operational_uri("queue", WORKSPACE_A, "ready", "&priority=None"),
        operational_uri("queue", WORKSPACE_A, "ready", "&priority=AA"),
        operational_uri("queue", WORKSPACE_A, "ready", "&priority=a"),
    ] {
        let (status, error) = api.call(Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "invalid_input");
    }

    let tie_document = document(501);
    let session = api.backend.session().await.unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: tie_document,
            workspace_id: workspace(WORKSPACE_A),
            path: "stable-ties.org",
            source: "",
            content_hash: "stable-ties",
            now: 1,
        })
        .await
        .unwrap();
    let tied = (600..655)
        .enumerate()
        .map(|(index, suffix)| {
            let mut row = row(
                workspace(WORKSPACE_A),
                tie_document,
                item(suffix),
                "BACKLOG",
                index as i64 + 1,
            );
            row.assignee = Some("agent-one".to_owned());
            row.scheduled = Some(timestamp("<2027-01-15 Fri 09:00>"));
            row
        })
        .collect::<Vec<_>>();
    session
        .replace_org_document_projection(tie_document, &tied)
        .await
        .unwrap();
    let (_, default_page) = api
        .call(
            Method::GET,
            &operational_uri("queue", WORKSPACE_A, "assigned", ""),
            None,
        )
        .await;
    assert_eq!(default_page["items"].as_array().unwrap().len(), 50);
    let default_cursor = default_page["next_cursor"].as_str().unwrap();
    let (_, tail) = api
        .call(
            Method::GET,
            &operational_uri(
                "queue",
                WORKSPACE_A,
                "assigned",
                &format!("&cursor={default_cursor}"),
            ),
            None,
        )
        .await;
    assert_eq!(tail["items"].as_array().unwrap().len(), 7);
    let unique = default_page["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(tail["items"].as_array().unwrap())
        .map(|row| row["item"]["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        57,
        "stable ties must not duplicate or skip rows"
    );
    let (_, maximum) = api
        .call(
            Method::GET,
            &operational_uri("queue", WORKSPACE_A, "assigned", "&limit=200"),
            None,
        )
        .await;
    assert_eq!(maximum["items"].as_array().unwrap().len(), 57);

    let (_, agenda_default) = api
        .call(
            Method::GET,
            &operational_uri("agenda", WORKSPACE_A, "scheduled", ""),
            None,
        )
        .await;
    assert_eq!(agenda_default["items"].as_array().unwrap().len(), 50);
    assert_eq!(
        agenda_default,
        api.mcp_call(
            "org_query_agenda",
            mcp_operational_args(vec![WORKSPACE_A], "scheduled"),
        )
        .await,
    );
    let agenda_cursor = agenda_default["next_cursor"].as_str().unwrap();
    let (_, agenda_tail) = api
        .call(
            Method::GET,
            &operational_uri(
                "agenda",
                WORKSPACE_A,
                "scheduled",
                &format!("&cursor={agenda_cursor}"),
            ),
            None,
        )
        .await;
    assert_eq!(agenda_tail["items"].as_array().unwrap().len(), 7);
    let (status, mismatch) = api
        .call(
            Method::GET,
            &operational_uri(
                "agenda",
                WORKSPACE_A,
                "upcoming_deadline",
                &format!("&cursor={agenda_cursor}"),
            ),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(mismatch["code"], "invalid_input");
    let (_, agenda_maximum) = api
        .call(
            Method::GET,
            &operational_uri("agenda", WORKSPACE_A, "scheduled", "&limit=200"),
            None,
        )
        .await;
    assert_eq!(agenda_maximum["items"].as_array().unwrap().len(), 57);

    let (status, _) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_B}/archive"),
            Some(extend(
                path_command("archive-workspace-b"),
                &[("expected_revision", json!(1))],
            )),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, hidden) = api
        .call(
            Method::GET,
            &operational_uri("queue", WORKSPACE_B, "assigned", ""),
            None,
        )
        .await;
    assert!(hidden["items"].as_array().unwrap().is_empty());
    let (_, visible) = api
        .call(
            Method::GET,
            &format!(
                "/api/org/queue?workspace_ids={WORKSPACE_B}&view=assigned&include_archived=true"
            ),
            None,
        )
        .await;
    assert!(!visible["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn rich_context_exposes_recovery_lineage_and_safe_metadata_without_tokens() {
    let api = TestApp::new().await;
    api.create_workspace(WORKSPACE_A, "context-workspace", false)
        .await;
    let workspace_id = workspace(WORKSPACE_A);
    let document_id = document(401);
    let parent_id = item(401);
    let target_id = item(402);
    let dependency_id = item(403);
    let session = api.backend.session().await.unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "context.org",
            source: "",
            content_hash: "context",
            now: 1,
        })
        .await
        .unwrap();
    let parent = row(workspace_id, document_id, parent_id, "READY", 1);
    let mut target = row(workspace_id, document_id, target_id, "RUNNING", 2);
    target.parent_id = Some(parent_id);
    target.dependencies = vec![dependency_id];
    target.note_links = vec![note_org::NoteLink {
        purpose: "context".to_owned(),
        note_id: "40000000-0000-4000-8000-000000000401".parse().unwrap(),
        description: "missing note".to_owned(),
    }];
    let dependency = row(workspace_id, document_id, dependency_id, "DONE", 3);
    let mut child = row(workspace_id, document_id, item(404), "READY", 4);
    child.parent_id = Some(target_id);
    session
        .replace_org_document_projection(document_id, &[parent, target, dependency, child])
        .await
        .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "context-attempt",
            workspace_id,
            work_item_id: target_id,
            attempt_number: 1,
            actor_id: "agent-one",
            status: OrgAttemptStatus::Running,
            started_at: NOW - 60,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({"phase": "build", "fencing_token": "raw-secret-token"}),
        })
        .await
        .unwrap();
    assert!(matches!(
        session
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "context-lease",
                    workspace_id,
                    work_item_id: target_id,
                    attempt_id: "context-attempt",
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-one",
                    fencing_token_hash:
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    acquired_at: NOW - 60,
                    last_heartbeat_at: NOW - 60,
                    expires_at: NOW,
                },
                4,
                NOW - 60,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    for (id, occurred_at, summary) in [
        ("context-event-1", NOW + 100, "first by lineage"),
        ("context-event-2", NOW - 100, "second by lineage"),
    ] {
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id,
                subject_kind: "work_item",
                subject_id: &target_id.to_string(),
                actor_id: "agent-one",
                attempt_id: Some("context-attempt"),
                event_type: OrgEventType::Progress,
                occurred_at,
                summary,
                metadata: &json!({"token_hash": "raw-secret-hash", "safe": true}),
                previous_state: Some("RUNNING"),
                resulting_state: Some("RUNNING"),
            })
            .await
            .unwrap();
    }

    let (status, context) = api
        .call(
            Method::GET,
            &format!("/api/org/items/{target_id}/context?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["parent"]["id"], parent_id.to_string());
    assert_eq!(context["children"][0]["id"], item(404).to_string());
    assert_eq!(context["workspace_revision"], 1);
    assert_eq!(context["document"]["revision"], 1);
    assert_eq!(
        context["dependencies"][0]["item"]["id"],
        dependency_id.to_string()
    );
    assert_eq!(context["dependencies"][0]["satisfied"], true);
    assert_eq!(context["note_links"][0]["available"], false);
    assert_eq!(context["attempts"][0]["metadata"]["phase"], "build");
    assert_eq!(
        context["attempts"][0]["metadata"]["fencing_token"],
        "[REDACTED]"
    );
    assert_eq!(context["lease"]["status"], "expired");
    assert!(context["lease"].get("fencing_token").is_none());
    assert!(context["lease"].get("fencing_token_hash").is_none());
    assert!(context["operational"]["recovery"]["candidate"].is_boolean());
    let events = context["history_segments"][0]["events"].as_array().unwrap();
    assert_eq!(events[0]["summary"], "first by lineage");
    assert_eq!(events[1]["summary"], "second by lineage");
    assert_eq!(events[0]["metadata"]["token_hash"], "[REDACTED]");
    assert!(!context.to_string().contains("raw-secret"));
    assert_eq!(
        context,
        api.mcp_call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_A, "item_id": target_id.to_string()}),
        )
        .await,
    );
}

#[test]
fn task_three_handlers_are_thin_and_never_read_storage_or_time() {
    let items = include_str!("../src/org_api/items.rs");
    let operational = include_str!("../src/org_api/operational.rs");
    for source in [items, operational] {
        for forbidden in [".storage(", ".clock(", "SystemTime", "Utc::now", "SELECT "] {
            assert!(!source.contains(forbidden), "handler contains {forbidden}");
        }
    }
}
