use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_org::WorkspacePolicy;
use note_pipelines::{
    org::{OrgClock, OrgContext, OrgError, OrgTokenSource},
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

const NOW: i64 = 1_900_000_000;
const WORKSPACE: &str = "10000000-0000-4000-8000-000000000061";
const DOCUMENT: &str = "20000000-0000-4000-8000-000000000061";
const IMPORT_DOCUMENT: &str = "20000000-0000-4000-8000-000000000062";
const LIFECYCLE_DOCUMENT: &str = "20000000-0000-4000-8000-000000000063";
const LIFECYCLE_DOCUMENT_B: &str = "20000000-0000-4000-8000-000000000064";
const ITEM_A: &str = "30000000-0000-4000-8000-000000000061";
const ITEM_B: &str = "30000000-0000-4000-8000-000000000062";
const ITEM_C: &str = "30000000-0000-4000-8000-000000000063";

#[derive(Debug)]
struct AdjustableClock(AtomicI64);

impl AdjustableClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl OrgClock for AdjustableClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct DeterministicTokens(Mutex<VecDeque<String>>);

impl DeterministicTokens {
    fn new() -> Self {
        Self(Mutex::new(
            (1..=32)
                .map(|index| format!("transport-token-{index}"))
                .collect(),
        ))
    }
}

impl OrgTokenSource for DeterministicTokens {
    fn generate_token(&self) -> Result<String, OrgError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| OrgError::invalid_input("deterministic token sequence exhausted"))
    }
}

struct RejectingTokens;

impl OrgTokenSource for RejectingTokens {
    fn generate_token(&self) -> Result<String, OrgError> {
        Err(OrgError::invalid_input(
            "restart replay must not generate a replacement token",
        ))
    }
}

struct TransportSide {
    rest: Router,
    mcp: Router,
    clock: Arc<AdjustableClock>,
    database_path: std::path::PathBuf,
    attachments_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl TransportSide {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database_path = dir.path().join(format!("{name}.db"));
        let attachments_path = dir.path().join("attachments");
        let storage: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(&database_path).await.unwrap());
        let note = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments_path.clone())),
        ));
        let clock = Arc::new(AdjustableClock::new(NOW));
        let org = Arc::new(
            OrgContext::new(storage, clock.clone())
                .with_token_source(Arc::new(DeterministicTokens::new())),
        );
        let state = AppState::new(note.clone(), org.clone());
        assert!(
            Arc::ptr_eq(&state.org, &org),
            "REST extraction and mcp_router must receive the same Org Arc"
        );
        let (rest, _) = rest_router();
        let rest = rest.with_state(state);
        let mcp = note_mcp::mcp_router(note, org);
        Self {
            rest,
            mcp,
            clock,
            database_path,
            attachments_path,
            _dir: dir,
        }
    }

    async fn reopen(self) -> Self {
        let Self {
            rest,
            mcp,
            clock,
            database_path,
            attachments_path,
            _dir,
        } = self;
        drop(rest);
        drop(mcp);

        let storage: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(&database_path).await.unwrap());
        let note = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments_path.clone())),
        ));
        let org = Arc::new(
            OrgContext::new(storage, clock.clone()).with_token_source(Arc::new(RejectingTokens)),
        );
        let state = AppState::new(note.clone(), org.clone());
        assert!(Arc::ptr_eq(&state.org, &org));
        let (rest, _) = rest_router();
        let rest = rest.with_state(state);
        let mcp = note_mcp::mcp_router(note, org);
        Self {
            rest,
            mcp,
            clock,
            database_path,
            attachments_path,
            _dir,
        }
    }

    async fn rest_call(&self, method: Method, uri: &str, body: Option<Value>) -> Semantic {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = self
            .rest
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if status.is_success() {
            Semantic::Success(value)
        } else {
            Semantic::Error(normalize_error(&value))
        }
    }

    async fn mcp_call(&self, id: u64, name: &str, arguments: Value) -> Semantic {
        let response = self
            .mcp
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/org/mcp")
                    .header(header::HOST, "proxy.example.test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "method": "tools/call",
                            "params": {"name": name, "arguments": arguments}
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "MCP {name}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert!(response.headers().get("mcp-session-id").is_none());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if let Some(error) = value.get("error") {
            Semantic::Error(normalize_error(&error["data"]))
        } else {
            Semantic::Success(value["result"]["structuredContent"].clone())
        }
    }

    async fn mcp_org_tool_names(&self) -> BTreeSet<String> {
        let response = self
            .mcp
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/org/mcp")
                    .header(header::HOST, "proxy.example.test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        response["result"]["tools"]
            .as_array()
            .expect("MCP tools/list array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .filter(|name| name.starts_with("org_"))
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug, PartialEq)]
enum Semantic {
    Success(Value),
    Error(Value),
}

fn normalize_error(value: &Value) -> Value {
    json!({
        "code": value["code"],
        "message": value["message"],
        "details": value["details"],
        "retryable": value["retryable"]
    })
}

struct Pair {
    rest: TransportSide,
    mcp: TransportSide,
    next_id: u64,
    rest_to_mcp_ids: BTreeMap<String, String>,
}

impl Pair {
    async fn new() -> Self {
        Self {
            rest: TransportSide::new("rest-conformance").await,
            mcp: TransportSide::new("mcp-conformance").await,
            next_id: 1,
            rest_to_mcp_ids: BTreeMap::new(),
        }
    }

    async fn compare(
        &mut self,
        name: &str,
        arguments: Value,
        method: Method,
        uri: &str,
        rest_body: Option<Value>,
    ) -> Semantic {
        let rest = self.rest.rest_call(method, uri, rest_body).await;
        let mut mcp_arguments = arguments;
        translate_ids(&mut mcp_arguments, &self.rest_to_mcp_ids);
        let mut mcp = self.mcp.mcp_call(self.next_id, name, mcp_arguments).await;
        self.next_id += 1;
        align_generated_ids(&rest, &mut mcp, &mut self.rest_to_mcp_ids);
        assert_eq!(rest, mcp, "semantic transport mismatch for {name}");
        rest
    }

    async fn reopen(self) -> Self {
        Self {
            rest: self.rest.reopen().await,
            mcp: self.mcp.reopen().await,
            next_id: self.next_id,
            rest_to_mcp_ids: self.rest_to_mcp_ids,
        }
    }

    async fn call_success(
        &mut self,
        name: &str,
        arguments: Value,
        method: Method,
        uri: &str,
        rest_body: Option<Value>,
    ) -> Value {
        match self.compare(name, arguments, method, uri, rest_body).await {
            Semantic::Success(value) => value,
            Semantic::Error(error) => panic!("{name} unexpectedly failed: {error}"),
        }
    }

    async fn call_error(
        &mut self,
        expected_code: &str,
        name: &str,
        arguments: Value,
        method: Method,
        uri: &str,
        rest_body: Option<Value>,
    ) -> Value {
        match self.compare(name, arguments, method, uri, rest_body).await {
            Semantic::Error(error) => {
                assert_eq!(error["code"], expected_code, "{name} error code");
                error
            }
            Semantic::Success(value) => panic!("{name} unexpectedly succeeded: {value}"),
        }
    }

    fn set_time(&self, now: i64) {
        self.rest.clock.set(now);
        self.mcp.clock.set(now);
    }
}

fn translate_ids(value: &mut Value, ids: &BTreeMap<String, String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                translate_ids(value, ids);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                translate_ids(value, ids);
            }
        }
        Value::String(value) => {
            if let Some(replacement) = ids.get(value) {
                *value = replacement.clone();
            }
        }
        _ => {}
    }
}

fn align_generated_ids(rest: &Semantic, mcp: &mut Semantic, ids: &mut BTreeMap<String, String>) {
    let (rest, mcp) = match (rest, mcp) {
        (Semantic::Success(rest), Semantic::Success(mcp))
        | (Semantic::Error(rest), Semantic::Error(mcp)) => (rest, mcp),
        _ => return,
    };
    align_value_ids(rest, mcp, ids, None, false);
}

fn align_value_ids(
    rest: &Value,
    mcp: &mut Value,
    ids: &mut BTreeMap<String, String>,
    field: Option<&str>,
    generated_record: bool,
) {
    match (rest, mcp) {
        (Value::Array(rest), Value::Array(mcp)) if rest.len() == mcp.len() => {
            for (rest, mcp) in rest.iter().zip(mcp) {
                align_value_ids(rest, mcp, ids, field, false);
            }
        }
        (Value::Object(rest), Value::Object(mcp)) => {
            let event_record = rest.contains_key("sequence") && rest.contains_key("event_type");
            let attempt_record = rest.contains_key("attempt_number")
                && rest.contains_key("status")
                && rest.contains_key("work_item_id");
            let lease_record = rest.contains_key("expires_at")
                && rest.contains_key("kind")
                && rest.contains_key("status")
                && rest.contains_key("work_item_id");
            let generated_record = event_record || attempt_record || lease_record;
            for (key, rest) in rest {
                if let Some(mcp) = mcp.get_mut(key) {
                    align_value_ids(rest, mcp, ids, Some(key), generated_record);
                }
            }
        }
        (Value::String(rest), Value::String(mcp))
            if rest != mcp
                && uuid::Uuid::parse_str(rest).is_ok()
                && uuid::Uuid::parse_str(mcp).is_ok()
                && (matches!(
                    field,
                    Some("event_ids" | "event_id" | "lease_id" | "attempt_id")
                ) || (field == Some("id") && generated_record)) =>
        {
            if let Some(existing) = ids.get(rest) {
                assert_eq!(
                    existing, mcp,
                    "generated REST id {rest} changed its MCP correspondence"
                );
            }
            if let Some((other_rest, _)) = ids
                .iter()
                .find(|(other_rest, existing_mcp)| *other_rest != rest && *existing_mcp == mcp)
            {
                panic!("generated MCP id {mcp} is already paired with REST id {other_rest}");
            }
            ids.insert(rest.clone(), mcp.clone());
            *mcp = rest.clone();
        }
        _ => {}
    }
}

fn command(actor: &str, operation: &str) -> Value {
    json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE,
        "actor_id": actor,
        "operation_id": operation
    })
}

fn merge(mut left: Value, right: Value) -> Value {
    left.as_object_mut()
        .unwrap()
        .extend(right.as_object().unwrap().clone());
    left
}

fn without(mut value: Value, keys: &[&str]) -> Value {
    let object = value.as_object_mut().unwrap();
    for key in keys {
        object.remove(*key);
    }
    value
}

fn policy() -> Value {
    let mut policy = serde_json::to_value(WorkspacePolicy::engineering_default()).unwrap();
    policy["concurrency_limit"] = json!(1);
    policy
}

async fn document_revision(pair: &mut Pair) -> i64 {
    let value = pair
        .call_success(
            "org_get_document",
            json!({"workspace_id": WORKSPACE, "document_id": DOCUMENT}),
            Method::GET,
            &format!("/api/org/documents/{DOCUMENT}?workspace_id={WORKSPACE}"),
            None,
        )
        .await;
    value["revision"].as_i64().expect("document revision")
}

async fn create_item(pair: &mut Pair, item_id: &str, title: &str, review: bool) {
    let revision = document_revision(pair).await;
    let input = merge(
        command("seed", &format!("create-{item_id}")),
        json!({
            "document_id": DOCUMENT,
            "parent_id": null,
            "item_id": item_id,
            "item_type": "task",
            "title": title,
            "initial_state": "READY",
            "priority": "A",
            "tags": ["transport"],
            "assignee": null,
            "requires_review": review,
            "expected_revisions": {DOCUMENT: revision}
        }),
    );
    pair.call_success(
        "org_create_item",
        input.clone(),
        Method::POST,
        &format!("/api/org/workspaces/{WORKSPACE}/items"),
        Some(without(input, &["workspace_id"])),
    )
    .await;
}

#[tokio::test]
async fn live_mcp_org_names_equal_generated_rest_operation_ids() {
    let side = TransportSide::new("inventory-conformance").await;
    let mcp_names = side.mcp_org_tool_names().await;
    let (_, openapi) = rest_router();
    let document = serde_json::to_value(openapi).unwrap();
    let rest_names = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(path, _)| path.starts_with("/api/org"))
        .flat_map(|(_, item)| {
            ["get", "put", "post", "delete", "patch"]
                .into_iter()
                .filter_map(|method| item[method]["operationId"].as_str().map(str::to_owned))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(mcp_names.len(), 40);
    assert_eq!(mcp_names, rest_names);
}

#[tokio::test]
async fn document_lifecycle_successes_and_errors_match_across_transports() {
    let mut pair = Pair::new().await;
    let workspace = merge(
        command("seed", "lifecycle-workspace"),
        json!({
            "slug": "lifecycle-conformance",
            "display_name": "Lifecycle Conformance",
            "description": "REST and MCP lifecycle parity",
            "timezone": "UTC",
            "policy_schema_version": 1,
            "policy": policy()
        }),
    );
    pair.call_success(
        "org_create_workspace",
        workspace.clone(),
        Method::POST,
        "/api/org/workspaces",
        Some(workspace),
    )
    .await;

    for (document_id, path) in [
        (LIFECYCLE_DOCUMENT, "lifecycle/first.org"),
        (LIFECYCLE_DOCUMENT_B, "lifecycle/second.org"),
    ] {
        let create = merge(
            command("seed", &format!("create-{document_id}")),
            json!({"document_id": document_id, "path": path}),
        );
        let result = pair
            .call_success(
                "org_create_document",
                create.clone(),
                Method::POST,
                &format!("/api/org/workspaces/{WORKSPACE}/documents"),
                Some(without(create, &["workspace_id"])),
            )
            .await;
        assert_eq!(result["document_revisions"][document_id], 1);
        assert_eq!(result["data"]["document_id"], document_id);
        assert_eq!(result["data"]["archived_at"], Value::Null);
    }

    let rename = merge(
        command("seed", "rename-lifecycle"),
        json!({
            "document_id": LIFECYCLE_DOCUMENT,
            "new_path": "lifecycle/renamed.org",
            "expected_revision": 1
        }),
    );
    let renamed = pair
        .call_success(
            "org_rename_document",
            rename.clone(),
            Method::PATCH,
            &format!("/api/org/documents/{LIFECYCLE_DOCUMENT}/path"),
            Some(without(rename, &["document_id"])),
        )
        .await;
    assert_eq!(renamed["document_revisions"][LIFECYCLE_DOCUMENT], 2);
    assert_eq!(renamed["data"]["path"], "lifecycle/renamed.org");

    let conflict = merge(
        command("seed", "rename-conflict"),
        json!({
            "document_id": LIFECYCLE_DOCUMENT_B,
            "new_path": "lifecycle/renamed.org",
            "expected_revision": 1
        }),
    );
    pair.call_error(
        "document_path_conflict",
        "org_rename_document",
        conflict.clone(),
        Method::PATCH,
        &format!("/api/org/documents/{LIFECYCLE_DOCUMENT_B}/path"),
        Some(without(conflict, &["document_id"])),
    )
    .await;

    let stale = merge(
        command("seed", "archive-stale"),
        json!({
            "document_id": LIFECYCLE_DOCUMENT,
            "expected_revision": 99
        }),
    );
    pair.call_error(
        "stale_revision",
        "org_archive_document",
        stale.clone(),
        Method::POST,
        &format!("/api/org/documents/{LIFECYCLE_DOCUMENT}/archive"),
        Some(without(stale, &["document_id"])),
    )
    .await;

    let archive = merge(
        command("seed", "archive-lifecycle"),
        json!({
            "document_id": LIFECYCLE_DOCUMENT,
            "expected_revision": 2
        }),
    );
    let archived = pair
        .call_success(
            "org_archive_document",
            archive.clone(),
            Method::POST,
            &format!("/api/org/documents/{LIFECYCLE_DOCUMENT}/archive"),
            Some(without(archive, &["document_id"])),
        )
        .await;
    assert_eq!(archived["document_revisions"][LIFECYCLE_DOCUMENT], 3);
    assert_eq!(archived["data"]["archived_at"], NOW);

    let restore = merge(
        command("seed", "restore-lifecycle"),
        json!({
            "document_id": LIFECYCLE_DOCUMENT,
            "expected_revision": 3
        }),
    );
    let restored = pair
        .call_success(
            "org_restore_document",
            restore.clone(),
            Method::POST,
            &format!("/api/org/documents/{LIFECYCLE_DOCUMENT}/restore"),
            Some(without(restore, &["document_id"])),
        )
        .await;
    assert_eq!(restored["document_revisions"][LIFECYCLE_DOCUMENT], 4);
    assert_eq!(restored["data"]["archived_at"], Value::Null);
}

#[tokio::test]
async fn streamable_http_mcp_and_rest_preserve_org_semantics() {
    let mut pair = Pair::new().await;

    let create = merge(
        command("seed", "create-workspace"),
        json!({
            "slug": "transport-conformance",
            "display_name": "Transport Conformance",
            "description": "separate deterministic Turso stores",
            "timezone": "UTC",
            "policy_schema_version": 1,
            "policy": policy()
        }),
    );
    pair.call_success(
        "org_create_workspace",
        create.clone(),
        Method::POST,
        "/api/org/workspaces",
        Some(create.clone()),
    )
    .await;

    let mut divergent = create;
    divergent["display_name"] = json!("Divergent replay");
    let error = pair
        .call_error(
            "idempotency_conflict",
            "org_create_workspace",
            divergent.clone(),
            Method::POST,
            "/api/org/workspaces",
            Some(divergent),
        )
        .await;
    assert_eq!(error["code"], "idempotency_conflict");

    let stale = merge(
        command("seed", "stale-workspace"),
        json!({
            "expected_revision": 99,
            "slug": "transport-conformance",
            "display_name": "Stale",
            "description": "must fail",
            "timezone": "UTC",
            "policy_schema_version": 1,
            "policy": policy()
        }),
    );
    let error = pair
        .call_error(
            "stale_revision",
            "org_update_workspace",
            stale.clone(),
            Method::PATCH,
            &format!("/api/org/workspaces/{WORKSPACE}"),
            Some(without(stale, &["workspace_id"])),
        )
        .await;
    assert_eq!(error["code"], "stale_revision");

    let imported = merge(
        command("seed", "import-workspace"),
        json!({
            "documents": [{
                "document_id": IMPORT_DOCUMENT,
                "path": "imported.org",
                "source": "#+TITLE: Imported\n"
            }],
            "expected_revisions": {},
            "lease_proofs": {}
        }),
    );
    pair.call_success(
        "org_import_workspace",
        imported.clone(),
        Method::POST,
        &format!("/api/org/workspaces/{WORKSPACE}/import"),
        Some(without(imported, &["workspace_id"])),
    )
    .await;

    let put = merge(
        command("seed", "put-document"),
        json!({
            "document_id": DOCUMENT,
            "path": "work.org",
            "source": "#+TITLE: Work\n",
            "expected_revision": null,
            "lease_proofs": {}
        }),
    );
    pair.call_success(
        "org_put_document",
        put.clone(),
        Method::PUT,
        &format!("/api/org/documents/{DOCUMENT}"),
        Some(without(put, &["document_id"])),
    )
    .await;

    create_item(&mut pair, ITEM_A, "Primary", true).await;
    create_item(&mut pair, ITEM_B, "Dependency", false).await;
    create_item(&mut pair, ITEM_C, "Concurrency probe", false).await;

    pair.call_success(
        "org_get_item",
        json!({"workspace_id": WORKSPACE, "item_id": ITEM_A}),
        Method::GET,
        &format!("/api/org/items/{ITEM_A}?workspace_id={WORKSPACE}"),
        None,
    )
    .await;
    pair.call_success(
        "org_get_item_context",
        json!({"workspace_id": WORKSPACE, "item_id": ITEM_A}),
        Method::GET,
        &format!("/api/org/items/{ITEM_A}/context?workspace_id={WORKSPACE}"),
        None,
    )
    .await;
    pair.call_success(
        "org_query_queue",
        json!({"workspace_ids": [WORKSPACE], "view": "ready", "limit": 50}),
        Method::GET,
        &format!("/api/org/queue?workspace_ids={WORKSPACE}&view=ready&limit=50"),
        None,
    )
    .await;

    let dependency_revision = document_revision(&mut pair).await;
    let dependency = merge(
        command("seed", "add-dependency"),
        json!({
            "item_id": ITEM_A,
            "dependency_id": ITEM_B,
            "document_id": DOCUMENT,
            "expected_revisions": {DOCUMENT: dependency_revision},
            "lease": null
        }),
    );
    pair.call_success(
        "org_add_dependency",
        dependency.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_A}/dependencies"),
        Some(without(dependency, &["item_id"])),
    )
    .await;

    let revision = document_revision(&mut pair).await;
    let blocked_claim = merge(
        command("agent-a", "claim-blocked"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": revision,
            "kind": "execution"
        }),
    );
    let error = pair
        .call_error(
            "dependency_blocked",
            "org_claim_item",
            blocked_claim.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(blocked_claim, &["work_item_id"])),
        )
        .await;
    assert_eq!(error["code"], "dependency_blocked");

    let revision = document_revision(&mut pair).await;
    let claim_dependency = merge(
        command("agent-b", "claim-dependency"),
        json!({
            "work_item_id": ITEM_B,
            "document_id": DOCUMENT,
            "expected_document_revision": revision,
            "kind": "execution"
        }),
    );
    let claim = pair
        .call_success(
            "org_claim_item",
            claim_dependency.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_B}/claim"),
            Some(without(claim_dependency, &["work_item_id"])),
        )
        .await;

    let active = merge(
        command("agent-c", "active-lease"),
        json!({
            "work_item_id": ITEM_B,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "execution"
        }),
    );
    let error = pair
        .call_error(
            "active_lease",
            "org_claim_item",
            active.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_B}/claim"),
            Some(without(active, &["work_item_id"])),
        )
        .await;
    assert_eq!(error["code"], "active_lease");

    let heartbeat = merge(
        command("agent-b", "heartbeat"),
        json!({
            "work_item_id": ITEM_B,
            "lease_id": claim["lease_id"],
            "kind": "execution",
            "fencing_token": claim["fencing_token"]
        }),
    );
    pair.call_success(
        "org_heartbeat_claim",
        heartbeat.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_B}/claim/heartbeat"),
        Some(without(heartbeat.clone(), &["work_item_id"])),
    )
    .await;
    let mut stale_heartbeat = heartbeat;
    stale_heartbeat["operation_id"] = json!("stale-heartbeat");
    stale_heartbeat["fencing_token"] = json!("wrong-token");
    let error = pair
        .call_error(
            "stale_lease",
            "org_heartbeat_claim",
            stale_heartbeat.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_B}/claim/heartbeat"),
            Some(without(stale_heartbeat, &["work_item_id"])),
        )
        .await;
    assert_eq!(error["code"], "stale_lease");

    let concurrency = merge(
        command("agent-c", "concurrency-limit"),
        json!({
            "work_item_id": ITEM_C,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "execution"
        }),
    );
    let error = pair
        .call_error(
            "concurrency_limit",
            "org_claim_item",
            concurrency.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_C}/claim"),
            Some(without(concurrency, &["work_item_id"])),
        )
        .await;
    assert_eq!(error["code"], "concurrency_limit");

    let transition = merge(
        command("agent-b", "finish-dependency"),
        json!({
            "work_item_id": ITEM_B,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "target_state": "DONE",
            "lease": {
                "lease_id": claim["lease_id"],
                "kind": "execution",
                "fencing_token": claim["fencing_token"]
            },
            "error": null,
            "metadata": {"transport": "parity"}
        }),
    );
    pair.call_success(
        "org_transition_item",
        transition.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_B}/transition"),
        Some(without(transition, &["work_item_id"])),
    )
    .await;

    let claim_primary = merge(
        command("agent-a", "claim-primary"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "execution"
        }),
    );
    let first_primary = pair
        .call_success(
            "org_claim_item",
            claim_primary.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(claim_primary, &["work_item_id"])),
        )
        .await;

    pair.set_time(first_primary["expires_at"].as_i64().unwrap());
    let reclaim = merge(
        command("agent-c", "reclaim-primary"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "execution"
        }),
    );
    let reclaimed = pair
        .call_success(
            "org_claim_item",
            reclaim.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(reclaim, &["work_item_id"])),
        )
        .await;

    let review_request = merge(
        command("agent-c", "request-review"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "lease_id": reclaimed["lease_id"],
            "fencing_token": reclaimed["fencing_token"],
            "result_summary": "ready for review",
            "note_refs": [],
            "artifacts": [],
            "metadata": {"transport": "parity"}
        }),
    );
    pair.call_success(
        "org_request_review",
        review_request.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_A}/review/request"),
        Some(without(review_request, &["work_item_id"])),
    )
    .await;

    let review_claim = merge(
        command("reviewer", "claim-review"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "review"
        }),
    );
    let review = pair
        .call_success(
            "org_claim_item",
            review_claim.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(review_claim, &["work_item_id"])),
        )
        .await;
    let approve = merge(
        command("reviewer", "approve"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "lease_id": review["lease_id"],
            "fencing_token": review["fencing_token"],
            "metadata": {"verdict": "approved"}
        }),
    );
    pair.call_success(
        "org_approve_item",
        approve.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_A}/review/approve"),
        Some(without(approve, &["work_item_id"])),
    )
    .await;

    pair.call_success(
        "org_list_events",
        json!({
            "workspace_id": WORKSPACE,
            "subject_kind": "work_item",
            "subject_id": ITEM_A,
            "limit": 200
        }),
        Method::GET,
        &format!(
            "/api/org/workspaces/{WORKSPACE}/events?subject_kind=work_item&subject_id={ITEM_A}&limit=200"
        ),
        None,
    )
    .await;

    let archive = merge(command("seed", "archive"), json!({"expected_revision": 1}));
    pair.call_success(
        "org_archive_workspace",
        archive.clone(),
        Method::POST,
        &format!("/api/org/workspaces/{WORKSPACE}/archive"),
        Some(without(archive, &["workspace_id"])),
    )
    .await;
    pair.call_success(
        "org_get_workspace",
        json!({"workspace_id": WORKSPACE}),
        Method::GET,
        &format!("/api/org/workspaces/{WORKSPACE}"),
        None,
    )
    .await;
    pair.call_success(
        "org_export_workspace",
        json!({"workspace_id": WORKSPACE}),
        Method::GET,
        &format!("/api/org/workspaces/{WORKSPACE}/export"),
        None,
    )
    .await;
}

#[tokio::test]
async fn restart_preserves_claim_idempotency_across_mcp_and_rest() {
    let mut pair = Pair::new().await;

    let create = merge(
        command("seed", "restart-create-workspace"),
        json!({
            "slug": "restart-conformance",
            "display_name": "Restart Conformance",
            "description": "durable transport idempotency",
            "timezone": "UTC",
            "policy_schema_version": 1,
            "policy": policy()
        }),
    );
    pair.call_success(
        "org_create_workspace",
        create.clone(),
        Method::POST,
        "/api/org/workspaces",
        Some(create),
    )
    .await;

    let put = merge(
        command("seed", "restart-put-document"),
        json!({
            "document_id": DOCUMENT,
            "path": "restart.org",
            "source": "#+TITLE: Restart\n",
            "expected_revision": null,
            "lease_proofs": {}
        }),
    );
    pair.call_success(
        "org_put_document",
        put.clone(),
        Method::PUT,
        &format!("/api/org/documents/{DOCUMENT}"),
        Some(without(put, &["document_id"])),
    )
    .await;
    create_item(&mut pair, ITEM_A, "Restart claim", false).await;

    let claim_input = merge(
        command("restart-agent", "restart-claim"),
        json!({
            "work_item_id": ITEM_A,
            "document_id": DOCUMENT,
            "expected_document_revision": document_revision(&mut pair).await,
            "kind": "execution"
        }),
    );
    let claimed = pair
        .call_success(
            "org_claim_item",
            claim_input.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(claim_input.clone(), &["work_item_id"])),
        )
        .await;
    let context_before = pair
        .call_success(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE, "item_id": ITEM_A}),
            Method::GET,
            &format!("/api/org/items/{ITEM_A}/context?workspace_id={WORKSPACE}"),
            None,
        )
        .await;

    let mut pair = pair.reopen().await;
    let replayed = pair
        .call_success(
            "org_claim_item",
            claim_input.clone(),
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            Some(without(claim_input.clone(), &["work_item_id"])),
        )
        .await;
    assert_eq!(
        replayed, claimed,
        "restart changed the durable claim result"
    );

    let mut divergent = claim_input;
    divergent["actor_id"] = json!("different-restart-agent");
    pair.call_error(
        "idempotency_conflict",
        "org_claim_item",
        divergent.clone(),
        Method::POST,
        &format!("/api/org/items/{ITEM_A}/claim"),
        Some(without(divergent, &["work_item_id"])),
    )
    .await;

    let context_after = pair
        .call_success(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE, "item_id": ITEM_A}),
            Method::GET,
            &format!("/api/org/items/{ITEM_A}/context?workspace_id={WORKSPACE}"),
            None,
        )
        .await;
    assert_eq!(
        context_after, context_before,
        "restart replay duplicated attempts, leases, events, or revisions"
    );
}
