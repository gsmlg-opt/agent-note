use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{OrgClock, OrgContext},
    Context,
};
use note_storage::{
    BackendInfo, StorageBackend, StorageResult, StorageSession, StorageTransaction, TransactionMode,
};
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
    backend: Arc<dyn StorageBackend>,
    clock: Arc<AdjustableClock>,
    database_path: PathBuf,
    attachments_path: PathBuf,
    _dir: tempfile::TempDir,
}

#[derive(Debug)]
struct AdjustableClock(AtomicI64);

struct BeginBarrierStorageBackend {
    inner: Arc<dyn StorageBackend>,
    gate: Mutex<Option<BeginBarrierGate>>,
}

struct BeginBarrierGate {
    barrier: Arc<tokio::sync::Barrier>,
    remaining: usize,
}

impl BeginBarrierStorageBackend {
    fn new(inner: Arc<dyn StorageBackend>) -> Self {
        Self {
            inner,
            gate: Mutex::new(None),
        }
    }

    fn arm_immediate_pair(&self) {
        let mut gate = self.gate.lock().unwrap();
        assert!(gate.is_none(), "begin barrier is already armed");
        *gate = Some(BeginBarrierGate {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            remaining: 2,
        });
    }
}

impl StorageBackend for BeginBarrierStorageBackend {
    fn session<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = StorageResult<Box<dyn StorageSession>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.session().await })
    }

    fn begin<'life0, 'async_trait>(
        &'life0 self,
        mode: TransactionMode,
    ) -> Pin<
        Box<dyn Future<Output = StorageResult<Box<dyn StorageTransaction>>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let barrier = if mode == TransactionMode::Immediate {
                let mut gate = self.gate.lock().unwrap();
                let barrier = gate.as_ref().map(|state| state.barrier.clone());
                let disarm = gate.as_mut().is_some_and(|state| {
                    state.remaining -= 1;
                    state.remaining == 0
                });
                if disarm {
                    *gate = None;
                }
                barrier
            } else {
                None
            };
            if let Some(barrier) = barrier {
                barrier.wait().await;
            }
            self.inner.begin(mode).await
        })
    }

    fn info<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<Box<dyn Future<Output = StorageResult<BackendInfo>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.info().await })
    }
}

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

impl Bundle {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database_path = dir.path().join(format!("{name}.db"));
        let attachments_path = dir.path().join("attachments");
        let backend: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(&database_path).await.unwrap());
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments_path.clone())),
        ));
        let clock = Arc::new(AdjustableClock::new(NOW));
        let org = Arc::new(OrgContext::new(backend.clone(), clock.clone()));
        Self {
            note,
            org,
            backend,
            clock,
            database_path,
            attachments_path,
            _dir: dir,
        }
    }

    async fn new_with_begin_barrier(name: &str) -> (Self, Arc<BeginBarrierStorageBackend>) {
        let dir = tempfile::tempdir().unwrap();
        let database_path = dir.path().join(format!("{name}.db"));
        let attachments_path = dir.path().join("attachments");
        let inner: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(&database_path).await.unwrap());
        let gated = Arc::new(BeginBarrierStorageBackend::new(inner));
        let backend: Arc<dyn StorageBackend> = gated.clone();
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments_path.clone())),
        ));
        let clock = Arc::new(AdjustableClock::new(NOW));
        let org = Arc::new(OrgContext::new(backend.clone(), clock.clone()));
        (
            Self {
                note,
                org,
                backend,
                clock,
                database_path,
                attachments_path,
                _dir: dir,
            },
            gated,
        )
    }

    async fn reopen(self) -> Self {
        let Self {
            note,
            org,
            backend,
            clock,
            database_path,
            attachments_path,
            _dir,
        } = self;
        drop(note);
        drop(org);
        drop(backend);
        let backend: Arc<dyn StorageBackend> =
            Arc::new(TursoStorage::open(&database_path).await.unwrap());
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(attachments_path.clone())),
        ));
        let org = Arc::new(OrgContext::new(backend.clone(), clock.clone()));
        Self {
            note,
            org,
            backend,
            clock,
            database_path,
            attachments_path,
            _dir,
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

fn acceptance_source() -> String {
    format!(
        "#+TITLE: Acceptance\r\n#+CUSTOM: opaque syntax\r\n* READY [#A] Imported task :acceptance:\r\n:PROPERTIES:\r\n:ID: {ITEM_ID}\r\n:AGENT_NOTE_TYPE: task\r\n:REQUIRES_REVIEW: true\r\n:END:\r\nBody  \r\n"
    )
}

fn tool_content(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn assert_success(response: &Value, operation: &str) {
    assert!(response.get("error").is_none(), "{operation} failed");
    assert!(
        response["result"]["structuredContent"].is_object(),
        "{operation} returned no structured content"
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

#[tokio::test]
async fn turso_mcp_workflow_survives_race_expiry_review_export_restart_and_replay() {
    let (bundle, race_gate) = Bundle::new_with_begin_barrier("task7-acceptance").await;
    let mut client = HttpClient::start(&bundle).await;

    let saved_note = client
        .call(
            "save_note",
            json!({
                "title": "Legacy Markdown",
                "content": "# Existing\nbody",
                "labels": [["kind", "legacy"]]
            }),
        )
        .await;
    assert_success(&saved_note, "save legacy Markdown note");
    let note_id = tool_content(&saved_note)["id"].as_str().unwrap().to_owned();

    let created = client
        .call("org_create_workspace", workspace_input("Task 7 acceptance"))
        .await;
    assert_success(&created, "create acceptance workspace");

    let import_input = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-a",
        "operation_id": "acceptance-import",
        "documents": [{
            "document_id": DOCUMENT_ID,
            "path": "acceptance.org",
            "source": acceptance_source()
        }],
        "expected_revisions": {},
        "lease_proofs": {}
    });
    let imported = client
        .call("org_import_workspace", import_input.clone())
        .await;
    assert_success(&imported, "import acceptance workspace");

    let ready = client
        .call(
            "org_query_queue",
            json!({"workspace_ids": [WORKSPACE_ID], "view": "ready"}),
        )
        .await;
    assert_success(&ready, "query ready queue");
    assert_eq!(tool_content(&ready)["items"][0]["item"]["id"], ITEM_ID);

    let mut agent_a = HttpClient::start(&bundle).await;
    let mut agent_b = HttpClient::start(&bundle).await;
    let claim_a_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-a",
        "operation_id": "claim-a", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 1, "kind": "execution"
    });
    let claim_b_input = json!({
        "schema_version": 1, "workspace_id": WORKSPACE_ID, "actor_id": "agent-b",
        "operation_id": "claim-b", "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
        "expected_document_revision": 1, "kind": "execution"
    });
    race_gate.arm_immediate_pair();
    let (claim_a, claim_b) = tokio::join!(
        agent_a.call("org_claim_item", claim_a_input.clone()),
        agent_b.call("org_claim_item", claim_b_input.clone())
    );
    assert_ne!(
        claim_a.get("error").is_none(),
        claim_b.get("error").is_none()
    );
    let (winner_actor, loser_actor, winner_claim, loser_claim, winner_claim_input) =
        if claim_a.get("error").is_none() {
            ("agent-a", "agent-b", claim_a, claim_b, claim_a_input)
        } else {
            ("agent-b", "agent-a", claim_b, claim_a, claim_b_input)
        };
    assert_success(&winner_claim, "winning claim");
    assert_error(&loser_claim, "active_lease");
    let winner = tool_content(&winner_claim).clone();
    assert_eq!(winner["context"]["attempts"].as_array().unwrap().len(), 1);

    bundle.clock.set(winner["expires_at"].as_i64().unwrap() - 1);
    let heartbeat = client
        .call(
            "org_heartbeat_claim",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": winner_actor, "operation_id": "heartbeat-before-expiry",
                "work_item_id": ITEM_ID, "lease_id": winner["lease_id"],
                "kind": "execution", "fencing_token": winner["fencing_token"]
            }),
        )
        .await;
    assert_success(&heartbeat, "heartbeat before expiry");
    let heartbeat_data = tool_content(&heartbeat);
    assert_eq!(
        heartbeat_data["data"]["context"]["document"]["revision"],
        winner["context"]["document"]["revision"]
    );
    let renewed_expiry = heartbeat_data["data"]["lease"]["expires_at"]
        .as_i64()
        .unwrap();
    bundle.clock.set(renewed_expiry);

    let expired_heartbeat = client
        .call(
            "org_heartbeat_claim",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": winner_actor, "operation_id": "heartbeat-at-expiry",
                "work_item_id": ITEM_ID, "lease_id": winner["lease_id"],
                "kind": "execution", "fencing_token": winner["fencing_token"]
            }),
        )
        .await;
    assert_error(&expired_heartbeat, "stale_lease");

    let reclaimed = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": loser_actor, "operation_id": "reclaim-expired",
                "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": winner["context"]["document"]["revision"],
                "kind": "execution"
            }),
        )
        .await;
    assert_success(&reclaimed, "reclaim expired execution");
    let reclaimed_data = tool_content(&reclaimed).clone();
    assert!(
        reclaimed_data["fencing_token"] != winner["fencing_token"],
        "reclaim did not rotate the fencing token"
    );
    assert_eq!(
        reclaimed_data["context"]["attempts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let events_before_stale = client
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item",
                "subject_id": ITEM_ID, "limit": 200
            }),
        )
        .await;
    let workspace_id = WORKSPACE_ID.parse().unwrap();
    let document_id = DOCUMENT_ID.parse().unwrap();
    let item_id = ITEM_ID.parse().unwrap();
    let session = bundle.backend.session().await.unwrap();
    let document_before_stale = session
        .get_org_document(document_id)
        .await
        .unwrap()
        .unwrap();
    let projection_before_stale = session
        .list_org_workspace_projection(workspace_id)
        .await
        .unwrap();
    let attempts_before_stale = session.list_org_attempts(item_id).await.unwrap();
    let leases_before_stale = session.list_org_lease_history(item_id).await.unwrap();
    let stored_events_before_stale = session
        .list_org_events(workspace_id, None, 200)
        .await
        .unwrap();
    assert!(session
        .get_org_operation(workspace_id, "delayed-owner-progress")
        .await
        .unwrap()
        .is_none());
    drop(session);
    let delayed = client
        .call(
            "org_report_progress",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": winner_actor, "operation_id": "delayed-owner-progress",
                "work_item_id": ITEM_ID, "lease_id": winner["lease_id"],
                "kind": "execution", "fencing_token": winner["fencing_token"],
                "summary": "late", "metadata": {}
            }),
        )
        .await;
    assert_error(&delayed, "stale_lease");
    let events_after_stale = client
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item",
                "subject_id": ITEM_ID, "limit": 200
            }),
        )
        .await;
    assert_eq!(
        tool_content(&events_before_stale),
        tool_content(&events_after_stale)
    );
    let session = bundle.backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document_id)
            .await
            .unwrap()
            .unwrap(),
        document_before_stale
    );
    assert_eq!(
        session
            .list_org_workspace_projection(workspace_id)
            .await
            .unwrap(),
        projection_before_stale
    );
    assert_eq!(
        session.list_org_attempts(item_id).await.unwrap(),
        attempts_before_stale
    );
    assert_eq!(
        session.list_org_lease_history(item_id).await.unwrap(),
        leases_before_stale
    );
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
        stored_events_before_stale
    );
    assert!(session
        .get_org_operation(workspace_id, "delayed-owner-progress")
        .await
        .unwrap()
        .is_none());
    drop(session);

    let review_requested = client
        .call(
            "org_request_review",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": loser_actor, "operation_id": "acceptance-review-request",
                "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": reclaimed_data["context"]["document"]["revision"],
                "lease_id": reclaimed_data["lease_id"],
                "fencing_token": reclaimed_data["fencing_token"],
                "result_summary": "ready for review", "note_refs": [], "artifacts": [],
                "metadata": {"stage": "acceptance"}
            }),
        )
        .await;
    assert_success(&review_requested, "request review");
    let review_request_data = tool_content(&review_requested);

    let review_claim = client
        .call(
            "org_claim_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": "reviewer", "operation_id": "acceptance-review-claim",
                "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": review_request_data["data"]["context"]["document"]["revision"],
                "kind": "review"
            }),
        )
        .await;
    assert_success(&review_claim, "claim review");
    let review_claim_data = tool_content(&review_claim).clone();
    let approved = client
        .call(
            "org_approve_item",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": "reviewer", "operation_id": "acceptance-approve",
                "work_item_id": ITEM_ID, "document_id": DOCUMENT_ID,
                "expected_document_revision": review_claim_data["context"]["document"]["revision"],
                "lease_id": review_claim_data["lease_id"],
                "fencing_token": review_claim_data["fencing_token"],
                "metadata": {"verdict": "approved"}
            }),
        )
        .await;
    assert_success(&approved, "approve review");
    assert_eq!(
        tool_content(&approved)["data"]["context"]["item"]["state"],
        "DONE"
    );

    let document = client
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await;
    let linked = client
        .call(
            "org_link_note",
            json!({
                "schema_version": 1, "workspace_id": WORKSPACE_ID,
                "actor_id": "operator", "operation_id": "link-legacy-note",
                "item_id": ITEM_ID, "document_id": DOCUMENT_ID, "purpose": "evidence",
                "note_id": note_id, "description": "legacy compatibility",
                "expected_revisions": {DOCUMENT_ID: tool_content(&document)["revision"]},
                "lease": null
            }),
        )
        .await;
    assert_success(&linked, "link legacy Markdown note");

    let context_before = client
        .call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await;
    let events_before = client
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item",
                "subject_id": ITEM_ID, "limit": 200
            }),
        )
        .await;
    let sequences = tool_content(&events_before)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["sequence"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let event_types = tool_content(&events_before)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        [
            "claim",
            "start",
            "heartbeat",
            "lease_expiry",
            "claim",
            "start",
            "result_submission",
            "review_request",
            "claim",
            "approval",
            "completion",
            "note_link_change",
        ]
    );
    let export_before = client
        .call(
            "org_export_workspace",
            json!({"workspace_id": WORKSPACE_ID}),
        )
        .await;
    assert_success(&export_before, "export before restart");
    assert!(tool_content(&export_before)["documents"][0]["source"]
        .as_str()
        .unwrap()
        .contains("#+CUSTOM: opaque syntax\r\n"));

    let replay_before = client
        .call("org_import_workspace", import_input.clone())
        .await;
    assert_eq!(tool_content(&replay_before), tool_content(&imported));

    drop(client);
    drop(agent_a);
    drop(agent_b);
    drop(race_gate);
    let bundle = bundle.reopen().await;
    let mut reopened = HttpClient::start(&bundle).await;

    let note_after = reopened.call("get_note", json!({"id": note_id})).await;
    assert_success(&note_after, "get legacy Markdown note after restart");
    assert_eq!(tool_content(&note_after)["title"], "Legacy Markdown");
    assert_eq!(tool_content(&note_after)["content"], "# Existing\nbody");

    let context_after = reopened
        .call(
            "org_get_item_context",
            json!({"workspace_id": WORKSPACE_ID, "item_id": ITEM_ID}),
        )
        .await;
    let events_after = reopened
        .call(
            "org_list_events",
            json!({
                "workspace_id": WORKSPACE_ID, "subject_kind": "work_item",
                "subject_id": ITEM_ID, "limit": 200
            }),
        )
        .await;
    let export_after = reopened
        .call(
            "org_export_workspace",
            json!({"workspace_id": WORKSPACE_ID}),
        )
        .await;
    assert_eq!(tool_content(&context_after), tool_content(&context_before));
    assert_eq!(tool_content(&events_after), tool_content(&events_before));
    assert_eq!(tool_content(&export_after), tool_content(&export_before));
    assert_eq!(
        tool_content(&context_after)["attempts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        tool_content(&context_after)["note_links"][0]["note_id"],
        note_id
    );

    let import_replay_after = reopened.call("org_import_workspace", import_input).await;
    assert_eq!(tool_content(&import_replay_after), tool_content(&imported));
    let claim_replay_after = reopened.call("org_claim_item", winner_claim_input).await;
    assert!(
        tool_content(&claim_replay_after)["fencing_token"] == winner["fencing_token"],
        "claim replay returned a different fencing token"
    );
    let mut redacted_claim_replay = tool_content(&claim_replay_after).clone();
    redacted_claim_replay["fencing_token"] = json!("<redacted>");
    let mut redacted_winner_claim = tool_content(&winner_claim).clone();
    redacted_winner_claim["fencing_token"] = json!("<redacted>");
    assert_eq!(
        redacted_claim_replay, redacted_winner_claim,
        "claim replay changed its non-sensitive result"
    );
    assert!(bundle
        .backend
        .session()
        .await
        .unwrap()
        .get_org_operation(WORKSPACE_ID.parse().unwrap(), "acceptance-import")
        .await
        .unwrap()
        .is_some());
}
