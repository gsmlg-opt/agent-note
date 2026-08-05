use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{FixedOrgClock, OrgContext},
    Context,
};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tower::ServiceExt as _;

const NOW: i64 = 1_800_000_000;
const STDIO_TIMEOUT: Duration = Duration::from_secs(5);
const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000051";
const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000051";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000051";

struct Bundle {
    note: Arc<Context>,
    org: Arc<OrgContext>,
    _dir: tempfile::TempDir,
}

impl Bundle {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join(format!("{name}.db")))
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
        let org = Arc::new(OrgContext::new(backend, Arc::new(FixedOrgClock::new(NOW))));
        Self {
            note,
            org,
            _dir: dir,
        }
    }
}

struct StdioClient {
    reader: BufReader<ReadHalf<tokio::io::DuplexStream>>,
    writer: Option<WriteHalf<tokio::io::DuplexStream>>,
    next_id: u64,
    server: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl StdioClient {
    async fn start(bundle: &Bundle) -> Self {
        let (server_transport, client_transport) = tokio::io::duplex(1024 * 1024);
        let note = bundle.note.clone();
        let org = bundle.org.clone();
        let server = tokio::spawn(async move {
            note_mcp::serve_stdio_transport(note, org, server_transport).await
        });
        let (reader, writer) = split(client_transport);
        let mut client = Self {
            reader: BufReader::new(reader),
            writer: Some(writer),
            next_id: 1,
            server: Some(server),
        };
        let initialized = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "raw-stdio-test", "version": "1"}
                }),
            )
            .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        client
            .send(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .await;
        client
    }

    async fn send(&mut self, message: Value) {
        let writer = self.writer.as_mut().expect("stdio client is open");
        writer
            .write_all(serde_json::to_string(&message).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .await;
        loop {
            let mut line = String::new();
            match tokio::time::timeout(STDIO_TIMEOUT, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => {
                    self.abort_server();
                    panic!("stdio server closed before response {id}");
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    self.abort_server();
                    panic!("failed to read stdio response {id}: {error}");
                }
                Err(_) => {
                    self.abort_server();
                    panic!("timed out waiting for stdio response {id}");
                }
            }
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == id {
                return response;
            }
        }
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    async fn close(mut self) {
        let shutdown = tokio::time::timeout(
            STDIO_TIMEOUT,
            self.writer
                .as_mut()
                .expect("stdio client is open")
                .shutdown(),
        )
        .await;
        if !matches!(shutdown, Ok(Ok(()))) {
            self.abort_server();
            panic!("failed to close stdio client writer: {shutdown:?}");
        }
        drop(self.writer.take());

        let mut server = self.server.take().expect("stdio server task is present");
        match tokio::time::timeout(STDIO_TIMEOUT, &mut server).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => panic!("stdio server returned an error after EOF: {error}"),
            Ok(Err(error)) => panic!("stdio server task failed after EOF: {error}"),
            Err(_) => {
                server.abort();
                panic!("stdio server did not stop after EOF");
            }
        }
    }

    fn abort_server(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        self.abort_server();
    }
}

#[derive(Clone)]
struct HttpClient {
    router: Router,
    next_id: u64,
}

impl HttpClient {
    async fn start(bundle: &Bundle) -> Self {
        let router = note_mcp::mcp_router(bundle.note.clone(), bundle.org.clone());
        let mut client = Self { router, next_id: 1 };
        let (status, headers, initialized) = client
            .post(json!({
                "jsonrpc": "2.0",
                "id": client.next_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "raw-http-test", "version": "1"}
                }
            }))
            .await;
        client.next_id += 1;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert!(headers.get("mcp-session-id").is_none());
        let (status, headers, body) = client
            .post(json!({
                "jsonrpc": "2.0", "method": "notifications/initialized"
            }))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(headers.get("mcp-session-id").is_none());
        assert!(body.is_null());
        client
    }

    async fn post(&self, message: Value) -> (StatusCode, header::HeaderMap, Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::HOST, "public.example.test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(serde_json::to_vec(&message).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()))
        };
        (status, headers, body)
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let (status, headers, response) = self
            .post(json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params
            }))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert!(headers.get("mcp-session-id").is_none());
        assert_eq!(response["id"], id);
        response
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }
}

enum TransportClient {
    Stdio(StdioClient),
    Http(HttpClient),
}

impl TransportClient {
    async fn start(bundle: &Bundle, kind: TransportKind) -> Self {
        match kind {
            TransportKind::Stdio => Self::Stdio(StdioClient::start(bundle).await),
            TransportKind::Http => Self::Http(HttpClient::start(bundle).await),
        }
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        match self {
            Self::Stdio(client) => client.call(name, arguments).await,
            Self::Http(client) => client.call(name, arguments).await,
        }
    }

    async fn close(self) {
        if let Self::Stdio(client) = self {
            client.close().await;
        }
    }
}

#[derive(Clone, Copy)]
enum TransportKind {
    Stdio,
    Http,
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
        "review_required_types": [], "claim_policy": "open", "lease_duration_secs": 900,
        "retry_limit": 2, "concurrency_limit": 4, "tag_rules": {}
    })
}

fn workspace_input(display_name: &str) -> Value {
    json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
        "operation_id": "create-workspace", "slug": "transport-parity",
        "display_name": display_name, "description": "transport parity",
        "timezone": "UTC", "policy_schema_version": 1, "policy": policy()
    })
}

fn tool_content(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn assert_success(response: &Value, operation: &str) {
    assert!(response.get("error").is_none(), "{operation}: {response}");
    assert!(
        response["result"]["structuredContent"].is_object(),
        "{operation}: {response}"
    );
}

fn assert_error(response: &Value, code: &str) {
    let data = &response["error"]["data"];
    assert_eq!(data["code"], code, "{response}");
    assert!(data["message"].is_string(), "{response}");
    assert!(data["details"].is_object(), "{response}");
    assert!(data["retryable"].is_boolean(), "{response}");
    assert_eq!(data.as_object().unwrap().len(), 4, "{response}");
}

async fn run_representative_scenario(kind: TransportKind) -> Vec<Value> {
    let bundle = Bundle::new(match kind {
        TransportKind::Stdio => "scenario-stdio",
        TransportKind::Http => "scenario-http",
    })
    .await;
    let mut client = TransportClient::start(&bundle, kind).await;
    let mut responses = Vec::new();

    let create_workspace = client
        .call("org_create_workspace", workspace_input("Transport Parity"))
        .await;
    assert_success(&create_workspace, "create workspace");
    responses.push(create_workspace);

    let idempotency_conflict = client
        .call("org_create_workspace", workspace_input("Divergent"))
        .await;
    assert_error(&idempotency_conflict, "idempotency_conflict");
    responses.push(idempotency_conflict);

    let stale_revision = client
        .call(
            "org_update_workspace",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "stale-workspace", "expected_revision": 9,
                "slug": "transport-parity", "display_name": "Stale",
                "description": "must fail", "timezone": "UTC",
                "policy_schema_version": 1, "policy": policy()
            }),
        )
        .await;
    assert_error(&stale_revision, "stale_revision");
    responses.push(stale_revision);

    let document = client
        .call(
            "org_put_document",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "put-document", "document_id": DOCUMENT_ID,
                "path": "transport.org", "source": "#+TITLE: Transport\n",
                "expected_revision": null, "lease_proofs": {}
            }),
        )
        .await;
    assert_success(&document, "put document");
    responses.push(document);

    let item = client
        .call(
            "org_create_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "create-item", "document_id": DOCUMENT_ID, "parent_id": null,
                "item_id": ITEM_ID, "item_type": "task", "title": "Transport item",
                "initial_state": "READY", "priority": "A", "tags": ["transport"],
                "assignee": "agent-one", "requires_review": true,
                "expected_revisions": { DOCUMENT_ID: 1 }
            }),
        )
        .await;
    assert_success(&item, "create item");
    responses.push(item);

    let queue = client
        .call(
            "org_query_queue",
            json!({"workspace_ids": [WORKSPACE_ID], "view": "ready"}),
        )
        .await;
    assert_success(&queue, "queue");
    assert_eq!(tool_content(&queue)["items"][0]["item"]["id"], ITEM_ID);
    responses.push(queue);

    let claim = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "claim", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 2, "kind": "execution"
            }),
        )
        .await;
    assert_success(&claim, "claim");
    let claim_data = tool_content(&claim).clone();
    responses.push(claim.clone());

    let active_lease = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-two",
                "operation_id": "second-claim", "work_item_id": ITEM_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 3,
                "kind": "execution"
            }),
        )
        .await;
    assert_error(&active_lease, "active_lease");
    responses.push(active_lease);

    let heartbeat = client
        .call(
            "org_heartbeat_claim",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "heartbeat", "work_item_id": ITEM_ID,
                "lease_id": claim_data["lease_id"], "kind": "execution",
                "fencing_token": claim_data["fencing_token"]
            }),
        )
        .await;
    assert_success(&heartbeat, "heartbeat");
    responses.push(heartbeat);

    let stale_lease = client
        .call(
            "org_report_progress",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "stale-progress", "work_item_id": ITEM_ID,
                "lease_id": claim_data["lease_id"], "kind": "execution",
                "fencing_token": "wrong-token", "summary": "must fail", "metadata": {}
            }),
        )
        .await;
    assert_error(&stale_lease, "stale_lease");
    responses.push(stale_lease);

    let blocked = client
        .call(
            "org_transition_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "block", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 3, "target_state": "BLOCKED",
                "lease": {"lease_id": claim_data["lease_id"], "kind": "execution",
                    "fencing_token": claim_data["fencing_token"]},
                "error": "waiting", "metadata": {"reason": "dependency"}
            }),
        )
        .await;
    assert_success(&blocked, "block");
    responses.push(blocked);

    let unblocked = client
        .call(
            "org_transition_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "unblock", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 4, "target_state": "READY",
                "lease": null, "error": null, "metadata": {}
            }),
        )
        .await;
    assert_success(&unblocked, "unblock");
    responses.push(unblocked);

    let execution = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "reclaim", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": 5, "kind": "execution"
            }),
        )
        .await;
    assert_success(&execution, "reclaim");
    let execution_data = tool_content(&execution).clone();
    responses.push(execution);

    let review_requested = client
        .call(
            "org_request_review",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-one",
                "operation_id": "request-review", "work_item_id": ITEM_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 6,
                "lease_id": execution_data["lease_id"],
                "fencing_token": execution_data["fencing_token"],
                "result_summary": "ready", "note_refs": [], "artifacts": [],
                "metadata": {"stage": "review"}
            }),
        )
        .await;
    assert_success(&review_requested, "request review");
    responses.push(review_requested);

    let review_claim = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "reviewer",
                "operation_id": "review-claim", "work_item_id": ITEM_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 7,
                "kind": "review"
            }),
        )
        .await;
    assert_success(&review_claim, "review claim");
    let review_data = tool_content(&review_claim).clone();
    responses.push(review_claim);

    let approved = client
        .call(
            "org_approve_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "reviewer",
                "operation_id": "approve", "work_item_id": ITEM_ID,
                "document_id": DOCUMENT_ID, "expected_document_revision": 7,
                "lease_id": review_data["lease_id"],
                "fencing_token": review_data["fencing_token"], "metadata": {"verdict": "ok"}
            }),
        )
        .await;
    assert_success(&approved, "approve");
    responses.push(approved);

    let events = client
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item",
                "subject_id": ITEM_ID, "limit": 200
            }),
        )
        .await;
    assert_success(&events, "events");
    assert!(tool_content(&events)["items"].as_array().unwrap().len() >= 8);
    responses.push(events);

    client.close().await;
    responses
}

struct StableNormalizer {
    replacements: BTreeMap<String, String>,
    next_uuid: usize,
    next_token: usize,
}

impl StableNormalizer {
    fn new() -> Self {
        Self {
            replacements: BTreeMap::new(),
            next_uuid: 1,
            next_token: 1,
        }
    }

    fn normalize(&mut self, value: &mut Value) {
        self.normalize_field(None, value);
    }

    fn normalize_field(&mut self, key: Option<&str>, value: &mut Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.normalize_field(key, value);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    self.normalize_field(Some(key), value);
                }
            }
            Value::String(text) => {
                if key == Some("text") {
                    if let Ok(mut embedded) = serde_json::from_str::<Value>(text) {
                        self.normalize(&mut embedded);
                        *text = serde_json::to_string(&embedded).unwrap();
                        return;
                    }
                }
                let replacement = if key == Some("fencing_token") {
                    let next = self.next_token;
                    self.next_token += usize::from(!self.replacements.contains_key(text));
                    Some(format!("<fencing-token-{next}>"))
                } else if is_dynamic_uuid(text) {
                    let next = self.next_uuid;
                    self.next_uuid += usize::from(!self.replacements.contains_key(text));
                    Some(format!("<uuid-{next}>"))
                } else {
                    None
                };
                if let Some(replacement) = replacement {
                    *text = self
                        .replacements
                        .entry(text.clone())
                        .or_insert(replacement)
                        .clone();
                }
            }
            _ => {}
        }
    }
}

fn is_dynamic_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
        && !matches!(value, WORKSPACE_ID | DOCUMENT_ID | ITEM_ID)
}

fn normalize_scenario(mut responses: Vec<Value>) -> Vec<Value> {
    let mut normalizer = StableNormalizer::new();
    for response in &mut responses {
        normalizer.normalize(response);
    }
    responses
}

fn normalized_inventory(response: &Value) -> Vec<Value> {
    let mut tools = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            json!({
                "name": tool["name"],
                "description": tool["description"],
                "inputSchema": tool["inputSchema"],
                "outputSchema": tool["outputSchema"]
            })
        })
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    tools
}

#[tokio::test]
async fn production_transport_factories_share_the_same_47_tool_registry() {
    let bundle = Bundle::new("inventory").await;
    let mut stdio = StdioClient::start(&bundle).await;
    let mut http = HttpClient::start(&bundle).await;
    let stdio_tools = stdio.request("tools/list", json!({})).await;
    let http_tools = http.request("tools/list", json!({})).await;

    assert_eq!(stdio_tools["result"]["tools"].as_array().unwrap().len(), 47);
    assert_eq!(http_tools["result"]["tools"].as_array().unwrap().len(), 47);
    let stdio_inventory = normalized_inventory(&stdio_tools);
    let http_inventory = normalized_inventory(&http_tools);
    assert_eq!(
        stdio_inventory
            .iter()
            .filter(|tool| tool["name"].as_str().unwrap().starts_with("org_"))
            .count(),
        36
    );
    assert_eq!(stdio_inventory, http_inventory);

    stdio.close().await;
}

#[tokio::test]
async fn real_transports_match_for_reads_mutations_workflow_and_structured_errors() {
    let stdio = normalize_scenario(run_representative_scenario(TransportKind::Stdio).await);
    let http = normalize_scenario(run_representative_scenario(TransportKind::Http).await);
    assert!(serde_json::to_string(&stdio)
        .unwrap()
        .contains("<fencing-token-"));
    assert_eq!(stdio, http);
}

#[tokio::test]
async fn streamable_http_is_stateless_json_post_only_without_auth_or_session_headers() {
    let bundle = Bundle::new("http-boundary").await;
    let router = note_mcp::mcp_router(bundle.note, bundle.org);
    for method in [Method::GET, Method::DELETE] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/mcp")
                    .header(header::HOST, "public.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "POST");
        assert!(response.headers().get("mcp-session-id").is_none());
        assert!(response.headers().get("www-authenticate").is_none());
    }
}

#[tokio::test]
async fn production_transport_factories_serve_note_tools_alongside_org_tools() {
    let stdio_bundle = Bundle::new("note-tools-stdio").await;
    let http_bundle = Bundle::new("note-tools-http").await;
    let mut stdio = StdioClient::start(&stdio_bundle).await;
    let mut http = HttpClient::start(&http_bundle).await;

    let stdio_notes = stdio.call("list_notes", json!({})).await;
    let http_notes = http.call("list_notes", json!({})).await;
    assert_success(&stdio_notes, "list notes over stdio");
    assert_success(&http_notes, "list notes over HTTP");
    assert_eq!(tool_content(&stdio_notes), tool_content(&http_notes));

    stdio.close().await;
}
