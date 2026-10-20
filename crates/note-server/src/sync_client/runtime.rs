//! Phoenix socket runtime. Durability and convergence remain in pipelines.
use super::{
    connections::Connections,
    diagnostics::{Failure, SyncDiagnostic},
    phoenix::{Frame, FRAME_BYTES},
    progress::{SyncActiveTask, SyncDirectionProgress, SyncManagerProgress, SyncProgress},
    pump::Io,
    scan::{self, ScanSpool, SourceProgress, PAGE_SIZE},
    tasks::{self, Admission, Ledger, Status as TaskStatus, Submission},
    tls,
    transfer::{encode_metadata, StagedBlob},
    ConnectionInput, ConnectionMode, ConnectionSummary, SaveError, SyncConnection, SyncProfile,
};
use anyhow::{bail, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::FutureExt;
#[cfg(test)]
use futures::{SinkExt, StreamExt};
use note_core::sync::{
    SyncAck, SyncAssociation, SyncEvent, SyncEventKind, SyncStateQuery, SyncStateResponse,
};
use note_pipelines::{sync, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{oneshot, watch, Notify, RwLock},
    time::{timeout, Instant},
};
use tokio_tungstenite::{
    tungstenite::{protocol::WebSocketConfig, Message},
    MaybeTlsStream, WebSocketStream,
};
use utoipa::ToSchema;

const TOPIC: &str = "note_sync:v1";
const CHUNK: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
const HEARTBEAT: Duration = Duration::from_secs(30);

// Match GaoNote's bounded apply budget, allowing another 30 seconds for its reply.
fn commit_wait(total_bytes: u64, attachments: usize) -> Duration {
    let seconds = 60_u64
        .saturating_add(total_bytes.div_ceil(1024 * 1024).saturating_mul(2))
        .saturating_add((attachments as u64).saturating_mul(5))
        .min(900);
    Duration::from_secs(seconds + 30)
}
// Older GaoNote senders read the next attachment before announcing it. Keep
// that preparation gap separate from chunk inactivity, with 10s for delivery.
fn attachment_prepare_wait(size: u64) -> Duration {
    Duration::from_secs(
        30 + size
            .min(super::DEFAULT_MAX_BLOB_BYTES)
            .div_ceil(1024 * 1024),
    ) + DEADLINE
}
pub trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Transport for T {}
pub(super) type Socket = WebSocketStream<MaybeTlsStream<Box<dyn Transport>>>;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncBlockedNote {
    pub note_id: String,
    pub event_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncRuntimeStatus {
    pub profiles: Vec<SyncProfile>,
    pub state: String,
    pub last_error: Option<String>,
    pub last_success_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub next_retry_at: Option<i64>,
    pub diagnostics: Vec<SyncDiagnostic>,
    pub pending_notes: usize,
    pub initializing: bool,
    pub node_id: Option<String>,
    pub peer_node_id: Option<String>,
    pub blocked_notes: Vec<SyncBlockedNote>,
    pub blocked_notes_total: usize,
    pub progress: Option<SyncProgress>,
}
impl Default for SyncRuntimeStatus {
    fn default() -> Self {
        Self {
            profiles: vec![],
            state: "disabled".into(),
            last_error: None,
            last_success_at: None,
            last_attempt_at: None,
            next_retry_at: None,
            diagnostics: vec![],
            pending_notes: 0,
            initializing: false,
            node_id: None,
            peer_node_id: None,
            blocked_notes: vec![],
            blocked_notes_total: 0,
            progress: None,
        }
    }
}
#[derive(Default)]
pub struct SyncRuntime {
    connections: Arc<Connections>,
    status: Arc<RwLock<SyncRuntimeStatus>>,
    workers: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    wake: Notify,
    blocked: std::sync::Mutex<BTreeMap<String, SyncBlockedNote>>,
    retry_epoch: std::sync::atomic::AtomicU64,
    rejection_directory: std::sync::Mutex<Option<Arc<tempfile::TempDir>>>,
    rejected_total: std::sync::atomic::AtomicUsize,
}
impl SyncRuntime {
    pub fn new(profiles: &[SyncConnection]) -> Arc<Self> {
        Self::with_connections(Connections::memory(profiles))
    }
    pub fn open(
        profiles: &[SyncConnection],
        path: PathBuf,
        config_base: PathBuf,
    ) -> Result<Arc<Self>> {
        Ok(Self::with_connections(Connections::open(
            profiles,
            path,
            config_base,
        )?))
    }
    fn with_connections(connections: Connections) -> Arc<Self> {
        Arc::new(Self {
            connections: Arc::new(connections),
            status: Arc::new(RwLock::new(SyncRuntimeStatus::default())),
            workers: std::sync::Mutex::new(Vec::new()),
            wake: Notify::new(),
            blocked: std::sync::Mutex::new(BTreeMap::new()),
            retry_epoch: std::sync::atomic::AtomicU64::new(0),
            rejection_directory: std::sync::Mutex::new(None),
            rejected_total: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn worker<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = T> + Send + 'static,
    ) -> (oneshot::Receiver<T>, tokio::task::AbortHandle) {
        let (send, receive) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = send.send(future.await);
        });
        let mut workers = self.workers.lock().expect("sync worker registry poisoned");
        workers.retain(|worker| !worker.is_finished());
        let abort = handle.abort_handle();
        workers.push(handle);
        (receive, abort)
    }
    async fn drain_workers(&self) {
        let workers =
            std::mem::take(&mut *self.workers.lock().expect("sync worker registry poisoned"));
        for worker in workers {
            let _ = worker.await;
        }
    }
    pub fn connections(&self) -> Vec<ConnectionSummary> {
        self.connections
            .snapshot()
            .iter()
            .map(ConnectionSummary::from)
            .collect()
    }
    pub async fn save_connection(
        &self,
        input: ConnectionInput,
    ) -> Result<ConnectionSummary, SaveError> {
        self.connections.update(input).await
    }
    pub async fn status(&self, ctx: &Context) -> Result<SyncRuntimeStatus> {
        let durable = sync::sync_status(ctx).await?;
        let mut state = self.status.read().await.clone();
        state.profiles = self
            .connections
            .snapshot()
            .iter()
            .map(SyncConnection::public_profile)
            .collect();
        state.blocked_notes = self
            .blocked
            .lock()
            .expect("sync rejection mutex poisoned")
            .values()
            .cloned()
            .collect();
        state.blocked_notes_total = self
            .rejected_total
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(state.blocked_notes.len());
        if state.state == "connected"
            && (!state.blocked_notes.is_empty()
                || state
                    .last_error
                    .as_ref()
                    .is_some_and(|error| error.starts_with("Note ")))
        {
            state.last_error = state
                .blocked_notes
                .first()
                .map(|note| format!("Note {} cannot sync: {}", note.note_id, note.reason));
        }
        state.pending_notes = durable.pending_notes;
        state.initializing = durable.enabled && !durable.initialized;
        state.node_id = (!durable.node_id.is_empty()).then_some(durable.node_id);
        if !durable.enabled {
            state.state = "disabled".into();
            state.next_retry_at = None;
            if let Some(progress) = &mut state.progress {
                progress.upload.suspend("paused");
                progress.download.suspend("paused");
            }
        }
        Ok(state)
    }
    pub fn retry(&self) {
        self.retry_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.rejected_total
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.blocked
            .lock()
            .expect("sync rejection mutex poisoned")
            .clear();
        self.wake.notify_one();
    }
    fn is_descriptor_blocked(&self, event: &SyncAck) -> bool {
        let mut blocked = self.blocked.lock().expect("sync rejection mutex poisoned");
        if let Some(old) = blocked.get(&event.note_id) {
            if old.event_id == event.event_id {
                return true;
            }
            blocked.remove(&event.note_id);
        }
        false
    }
    fn blocked_sample(&self, note: SyncBlockedNote) {
        let mut samples = self.blocked.lock().expect("sync rejection mutex poisoned");
        samples.insert(note.note_id.clone(), note);
        while samples.len() > 64 {
            samples.pop_first();
        }
    }
    async fn block(&self, event: &SyncEvent, reason: &str) {
        self.blocked_sample(SyncBlockedNote {
            note_id: event.note_id.clone(),
            event_id: event.event_id.clone(),
            reason: reason.into(),
        });
        self.status.write().await.last_error =
            Some(format!("Note {} cannot sync: {}", event.note_id, reason));
    }
    fn record(status: &mut SyncRuntimeStatus, diagnostic: SyncDiagnostic) {
        eprintln!(
            "{}",
            json!({"event": "note_sync", "diagnostic": &diagnostic})
        );
        status.diagnostics.push(diagnostic);
        if status.diagnostics.len() > 20 {
            status.diagnostics.remove(0);
        }
    }
    async fn begin_attempt(&self) {
        let mut status = self.status.write().await;
        status.state = "connecting".into();
        status.last_attempt_at = Some(chrono::Utc::now().timestamp_millis());
        status.next_retry_at = None;
        if let Some(progress) = &mut status.progress {
            progress.upload.suspend("reconnecting");
            progress.download.suspend("reconnecting");
            progress.touch();
        }
    }
    async fn connected(&self, peer_node_id: String) {
        let mut status = self.status.write().await;
        status.state = "connected".into();
        status.next_retry_at = None;
        status.last_error = None;
        status.peer_node_id = Some(peer_node_id);
        Self::record(
            &mut status,
            Failure::new(
                "sync",
                "connected",
                "Connected to GaoNote; Phoenix channel identity and selectors verified.",
            )
            .event("info"),
        );
    }
    async fn error(&self, error: &anyhow::Error) {
        let mut status = self.status.write().await;
        status.state = "error".into();
        let failure = Failure::safe(error);
        status.last_error = Some(failure.message.clone());
        status.next_retry_at = None;
        Self::record(&mut status, failure.event("error"));
        if let Some(progress) = &mut status.progress {
            progress.upload.suspend("reconnecting");
            progress.download.suspend("reconnecting");
            progress.touch();
        }
    }
    async fn progress(&self, update: impl FnOnce(&mut SyncProgress)) {
        let mut status = self.status.write().await;
        let progress = status
            .progress
            .get_or_insert_with(|| SyncProgress::new("local_queue"));
        update(progress);
        progress.touch();
    }
}

pub async fn connect_socket(profile: &SyncConnection) -> Result<Socket> {
    profile.validate().map_err(|_| Failure::new("config", "invalid_configuration", "Invalid sync connection configuration; verify mode, endpoint and required credentials."))?;
    let url = reqwest::Url::parse(&profile.url).context("invalid sync URL")?;
    let tls = if url.scheme() == "wss" {
        Some(tls::client_config(profile).map_err(|error| Failure::safe(&error))?)
    } else {
        None
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(FRAME_BYTES))
        .max_frame_size(Some(FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(FRAME_BYTES * 2);
    let connect = async {
        let stream: Box<dyn Transport> = if profile.mode == ConnectionMode::Ssh {
            Box::new(
                super::ssh::SshStream::connect(profile.ssh.as_ref().unwrap(), &url)
                    .map_err(|error| Failure::safe(&error))?,
            )
        } else {
            let stream = TcpStream::connect((
                url.host_str().unwrap().trim_matches(['[', ']']),
                url.port_or_known_default().unwrap(),
            ))
            .await
            .map_err(|error| Failure::io(&error, "tcp"))?;
            stream
                .set_nodelay(true)
                .context("sync TCP connection setup failed")?;
            Box::new(stream)
        };
        tokio_tungstenite::client_async_tls_with_config(
            url.as_str(),
            stream,
            Some(config),
            tls.map(tokio_tungstenite::Connector::Rustls),
        )
        .await
        .map(|(socket, _)| socket)
        .map_err(|error| Failure::websocket(&error).into())
    };
    timeout(DEADLINE, connect)
        .await
        .context("sync connection startup timed out")?
}

/// The worker polls durable state; process termination never acknowledges pending work.
pub async fn run(
    ctx: Arc<Context>,
    runtime: Arc<SyncRuntime>,
    mut shutdown: watch::Receiver<bool>,
) {
    if let Err(error) = sync::release_abandoned_sync_events(&ctx).await {
        runtime.error(&error).await;
        return;
    }
    let mut failures = 0u32;
    // Subscribe before taking a snapshot. A replacement between snapshot and
    // select stays pending on this receiver and cancels the obsolete session.
    let mut profiles = runtime.connections.subscribe();
    loop {
        if *shutdown.borrow() {
            break;
        }
        let snapshot = profiles.borrow_and_update().clone();
        let config = match note_pipelines::get_system_config(&ctx).await {
            Ok(config) => config.sync,
            Err(error) => {
                runtime.error(&error).await;
                if pause(
                    &runtime,
                    &mut shutdown,
                    &mut profiles,
                    Duration::from_secs(2),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };
        if !config.enabled {
            {
                let mut status = runtime.status.write().await;
                status.state = "disabled".into();
                status.next_retry_at = None;
            }
            if pause(
                &runtime,
                &mut shutdown,
                &mut profiles,
                Duration::from_secs(1),
            )
            .await
            {
                break;
            }
            continue;
        }
        let profile = snapshot
            .iter()
            .find(|p| Some(&p.id) == config.connection_profile.as_ref());
        runtime.begin_attempt().await;
        let result = if let Some(profile) = profile {
            tokio::select! {
                biased;
                _=shutdown.changed()=>break,
                _=profiles.changed()=>Ok(()),
                result=watch_config(&ctx,&config)=>result,

                result=session(&ctx,profile,&config,&runtime)=>result,
            }
        } else {
            Err(anyhow::anyhow!(
                "selected sync connection profile is unavailable; add it in System settings"
            ))
        };
        runtime.drain_workers().await;
        // The sole worker has dropped the entire old session before reclaiming
        // all its leases, covering cancellation between DB commit and tracking.
        if let Err(error) = sync::release_abandoned_sync_events(&ctx).await {
            runtime.error(&error).await;
            return;
        }
        match result {
            Ok(()) => {
                failures = 0;
                continue;
            }
            Err(error) => {
                runtime.error(&error).await;
                failures = failures.saturating_add(1);
            }
        }
        let base = 1u64 << failures.min(5);
        let jitter = (uuid::Uuid::new_v4().as_u128() % 1000) as u64;
        let delay = Duration::from_millis((base * 1000 + jitter).min(60_000));
        runtime.status.write().await.next_retry_at =
            Some(chrono::Utc::now().timestamp_millis() + delay.as_millis() as i64);
        if pause(&runtime, &mut shutdown, &mut profiles, delay).await {
            break;
        }
    }
    runtime.drain_workers().await;
    if let Err(error) = sync::release_abandoned_sync_events(&ctx).await {
        runtime.error(&error).await;
    }
}
/// A separate future watches configuration while join, inventory, preparation
/// or chunk transmission is awaiting I/O. Dropping the session stops all traffic.
async fn watch_config(ctx: &Context, initial: &note_core::sync::SyncConfig) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(125));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if note_pipelines::get_system_config(ctx).await?.sync != *initial {
            return Ok(());
        }
    }
}

async fn pause(
    runtime: &SyncRuntime,
    shutdown: &mut watch::Receiver<bool>,
    profiles: &mut watch::Receiver<Vec<SyncConnection>>,
    delay: Duration,
) -> bool {
    tokio::select! { _=shutdown.changed()=>true,_=profiles.changed()=>false,_=runtime.wake.notified()=>false,_=tokio::time::sleep(delay)=>false }
}

#[derive(Serialize, Deserialize)]
struct Hello {
    protocol_version: u32,
    node_id: String,
    selectors: Vec<String>,
    max_blob_bytes: u64,
}
#[derive(Deserialize)]
struct PeerHello {
    #[serde(flatten)]
    hello: Hello,
    #[serde(default)]
    capabilities: Vec<String>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Negotiation {
    capabilities: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryComplete {
    inventory_id: String,
    total_notes: usize,
}
struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct InboundQuery {
    reference: String,
    task: AbortTask<Result<SyncStateResponse>>,
    started: Instant,
}
struct Incoming {
    id: String,
    metadata: StagedBlob,
    event: Option<SyncEvent>,
    blobs: HashMap<String, StagedBlob>,
    idle_deadline: Instant,
}
struct Session<'a> {
    ctx: &'a Context,
    runtime: &'a SyncRuntime,
    socket: Io,
    join: String,
    counter: u64,
    limit: u64,
    incoming: Option<Incoming>,
    heartbeat: Option<(String, Instant)>,
    next_heartbeat: Instant,
    outbound: Option<(String, String)>,
    outbound_transfer: Option<String>,
    state_query: bool,
    inbound_query: Option<InboundQuery>,
    remote_inventory_complete: bool,
    task_sync: bool,
    task_origin: String,
    task_sequence: u64,
    received_tasks: Ledger,
    outgoing_tasks: HashMap<String, OutgoingTask>,
    task_jobs: HashMap<String, TaskJob>,
    manager_failed: bool,
    scan_progress: Option<watch::Receiver<SourceProgress>>,
    manager_sequence: u64,
    remote_manager: Option<(String, u64, Value)>,
    resolved: Arc<tempfile::TempDir>,
    deferred_abort: Option<(String, String, Value)>,
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        // Disposable work must settle before the runtime drains durable apply and releases leases.
        for job in self.task_jobs.values() {
            if !job.durable {
                job.abort.abort();
            }
        }
    }
}
async fn session(
    ctx: &Context,
    profile: &SyncConnection,
    config: &note_core::sync::SyncConfig,
    runtime: &SyncRuntime,
) -> Result<()> {
    sync::initialize_sync(ctx).await?;
    let status = sync::sync_status(ctx).await?;
    let mut selectors: Vec<_> = config
        .rules
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.selector.clone())
        .collect();
    selectors.sort();
    selectors.dedup();
    let hello = Hello {
        protocol_version: 1,
        node_id: status.node_id.clone(),
        selectors,
        max_blob_bytes: profile.max_blob_bytes,
    };
    let mut client = Session {
        ctx,
        runtime,
        socket: Io::start(connect_socket(profile).await?),
        join: uuid::Uuid::new_v4().to_string(),
        counter: 0,
        limit: profile.max_blob_bytes,
        incoming: None,
        heartbeat: None,
        next_heartbeat: Instant::now() + HEARTBEAT,
        outbound: None,
        outbound_transfer: None,
        state_query: false,
        inbound_query: None,
        remote_inventory_complete: false,
        task_sync: false,
        task_origin: status.node_id.clone(),
        task_sequence: 0,
        received_tasks: Ledger::new(String::new()),
        outgoing_tasks: HashMap::new(),
        task_jobs: HashMap::new(),
        manager_failed: false,
        scan_progress: None,
        manager_sequence: 0,
        remote_manager: None,
        resolved: Arc::new(tempfile::tempdir()?),
        deferred_abort: None,
    };
    let peer: PeerHello = serde_json::from_value(
        client
            .request("phx_join", serde_json::to_value(&hello)?)
            .await?,
    )
    .context("sync peer returned invalid hello")?;
    let mut unique = HashSet::new();
    anyhow::ensure!(
        peer.capabilities.len() <= 32
            && peer
                .capabilities
                .iter()
                .all(|c| c.len() <= 64 && unique.insert(c)),
        "invalid sync capabilities"
    );
    let supports_query = peer.capabilities.iter().any(|c| c == "state_query_v1");
    let supports_tasks = peer.capabilities.iter().any(|c| c == "task_sync_v1");
    let peer = peer.hello;
    if peer.protocol_version != 1
        || uuid::Uuid::parse_str(&peer.node_id).is_err()
        || peer.node_id == hello.node_id
        || peer.selectors != hello.selectors
        || peer.max_blob_bytes == 0
    {
        bail!("sync peer hello has incompatible protocol, node identity or authorized selectors");
    }
    client.limit = profile.max_blob_bytes.min(peer.max_blob_bytes);
    sync::pin_sync_peer(ctx, &profile.id, &peer.node_id).await?;
    client.received_tasks = Ledger::new(peer.node_id.clone());
    runtime.connected(peer.node_id).await;
    if supports_query {
        let selection = Negotiation {
            capabilities: if supports_tasks {
                vec!["state_query_v1".into(), "task_sync_v1".into()]
            } else {
                vec!["state_query_v1".into()]
            },
        };
        let accepted: Negotiation = serde_json::from_value(
            client
                .request("sync_negotiate", serde_json::to_value(&selection)?)
                .await?,
        )
        .context("invalid sync capability negotiation")?;
        let accepted_set: HashSet<_> = accepted.capabilities.iter().collect();
        let selected_set: HashSet<_> = selection.capabilities.iter().collect();
        anyhow::ensure!(
            accepted.capabilities.len() == accepted_set.len() && accepted_set == selected_set,
            "sync capabilities were not accepted"
        );
        client.state_query = true;
        client.task_sync = supports_tasks;
    }
    if !client.task_sync {
        runtime.status.write().await.last_error =
            Some("GaoNote upgrade required: task_sync_v1 is unavailable".into());
        loop {
            let message = client.socket.next().await;
            client.handle_message(message).await?;
        }
    }
    let basis = if client.state_query {
        "remote_verified"
    } else {
        "legacy_replay"
    };
    runtime.progress(|p| *p = SyncProgress::new(basis)).await;
    // The query handler is active before GaoNote may begin its outbound pass.
    if let Err(error) = client.start_inventory().await {
        if error.downcast_ref::<RemoteTaskFailure>().is_none() {
            return Err(error);
        }
        runtime.status.write().await.last_error =
            Some("Remote inventory task failed; connection remains available".into());
    }
    client.reconcile_round(true).await?;
    loop {
        let retry = runtime.wake.notified().now_or_never().is_some();
        match client.scheduled_work(config, retry).await? {
            None => return Ok(()),
            Some(_) if retry && !client.manager_failed => client.reconcile_round(true).await?,
            Some(true) => client.reconcile_round(false).await?,
            Some(false) => {
                if let Ok(message) = timeout(Duration::from_millis(250), client.socket.next()).await
                {
                    client.handle_message(message).await?;
                }
            }
        }
        client.check_liveness().await?;
    }
}

impl Session<'_> {
    fn transfer_worker(&mut self) -> TransferWorker {
        TransferWorker {
            ctx: self.ctx.clone(),
            runtime: WorkerProgress {
                status: self.runtime.status.clone(),
                task_mode: self.task_sync,
                resolved: self.resolved.clone(),
            },
            incoming: self.incoming.take(),
            limit: self.limit,
            remote_inventory_complete: self.remote_inventory_complete,
        }
    }
    async fn show_task(&mut self, status: &TaskStatus, operation: &str, upload: bool) {
        self.runtime
            .progress(|progress| {
                let direction = if upload {
                    &mut progress.upload
                } else {
                    &mut progress.download
                };
                if direction.active_task.as_ref().is_some_and(|old| {
                    old.id == status.task_id
                        && old.state == status.state
                        && old.phase == status.phase
                        && old.error
                            == status
                                .error
                                .as_ref()
                                .map(|error| format!("Synchronization task: {}", error.code))
                }) {
                    return;
                }
                direction.active_task = Some(SyncActiveTask {
                    id: status.task_id.clone(),
                    operation: operation.into(),
                    state: status.state.clone(),
                    phase: status.phase.clone(),
                    updated_at: chrono::Utc::now().timestamp_millis(),
                    error: status
                        .error
                        .as_ref()
                        .map(|error| format!("Synchronization task: {}", error.code)),
                });
            })
            .await;
    }
    async fn publish_task(&mut self, status: TaskStatus, operation: &str) -> Result<()> {
        if status.state == "failed" {
            let mut state = self.runtime.status.write().await;
            SyncRuntime::record(&mut state, Failure::new("task", "sync_task_failed", "An incoming synchronization task failed; connection remains available and pending notes are preserved.").event("error"));
        }
        self.show_task(&status, operation, false).await;
        self.send("task_status", serde_json::to_value(status)?, None)
            .await
    }
    async fn task_error(&mut self, id: &str, code: &str) -> Result<()> {
        self.send("task_error", json!({"task_id":id,"code":code}), None)
            .await
    }
    async fn record_remote_task_failure(&mut self, id: &str, operation: &str, code: &str) {
        // Identity and code have passed wire validation; operation is locally owned.
        let message = format!(
            "Remote synchronization task {id} ({operation}) failed ({code}); connection remains available and pending notes are preserved."
        );
        let mut state = self.runtime.status.write().await;
        SyncRuntime::record(
            &mut state,
            Failure::new("task", "sync_task_failed", &message).event("error"),
        );
    }
    async fn submit_task(
        &mut self,
        operation: &str,
        payload: Value,
        detached: bool,
    ) -> Result<String> {
        anyhow::ensure!(
            self.outgoing_tasks.len() < 4,
            "outgoing task capacity exceeded"
        );
        self.task_sequence = self
            .task_sequence
            .checked_add(1)
            .context("task sequence exhausted")?;
        let id = format!("{}:{}", self.task_origin, self.task_sequence);
        let submit = Submission {
            task_id: id.clone(),
            operation: operation.into(),
            payload,
        };
        self.outgoing_tasks.insert(
            id.clone(),
            OutgoingTask {
                operation: operation.into(),
                status: None,
                error: None,
                detached,
            },
        );
        self.send("task_submit", serde_json::to_value(submit)?, None)
            .await?;
        Ok(id)
    }
    async fn start_inventory(&mut self) -> Result<()> {
        let id = self.submit_task("inventory_start", json!({}), true).await?;
        // Admission permits our own duplex checking; the remote scan remains a live task.
        self.wait_remote_task(&id, true).await?;
        Ok(())
    }
    async fn task_request(&mut self, operation: &str, payload: Value) -> Result<Value> {
        let id = self.submit_task(operation, payload, false).await?;
        let result = self.wait_remote_task(&id, false).await;
        self.outgoing_tasks.remove(&id);
        result
    }
    async fn wait_remote_task(&mut self, id: &str, admission: bool) -> Result<Value> {
        let mut probe = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(task) = self.outgoing_tasks.get(id) {
                if let Some(error) = &task.error {
                    return Err(RemoteTaskFailure(error.clone()).into());
                }
                if let Some(status) = &task.status {
                    if status.state == "succeeded" {
                        return Ok(status.result.clone().unwrap());
                    }
                    if status.terminal() {
                        return Err(RemoteTaskFailure(
                            status
                                .error
                                .as_ref()
                                .map_or("cancelled", |error| error.code.as_str())
                                .into(),
                        )
                        .into());
                    }
                    if admission {
                        return Ok(json!({}));
                    }
                }
            } else if admission {
                return Ok(json!({}));
            }
            self.check_liveness().await?;
            if Instant::now() >= probe {
                self.send("task_get", json!({"task_id":id}), None).await?;
                probe = Instant::now() + Duration::from_secs(5);
            }
            let wake = self.next_wake().min(probe);
            tokio::select! {
                message = self.socket.next() => self.handle_message(message).await?,
                _ = tokio::time::sleep_until(wake) => {},
            }
        }
    }
    async fn handle_task_frame(&mut self, frame: Frame) -> Result<()> {
        match frame.event.as_str() {
            "sync_manager_status" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ManagerWire {
                    round_id: String,
                    status_seq: u64,
                    progress: ManagerData,
                }
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ManagerData {
                    #[serde(default)]
                    state: Option<String>,
                    #[serde(default)]
                    error: Option<tasks::TaskError>,
                    mode: String,
                    page_size: u64,
                    pages_scanned: u64,
                    pages_checked: u64,
                    notes_scanned: u64,
                    scan_complete: bool,
                    source_cursor: Option<String>,
                    last_progress_at: i64,
                    checked_notes: usize,
                    total_notes: Option<usize>,
                    #[serde(default)]
                    planned_notes: Option<usize>,
                    #[serde(default)]
                    equal_notes: Option<usize>,
                    #[serde(default)]
                    superseded_notes: Option<usize>,
                    #[serde(default)]
                    blocked_notes: Option<usize>,
                }
                let wire: ManagerWire = serde_json::from_value(frame.payload.clone())
                    .context("invalid sync manager status")?;
                anyhow::ensure!(
                    uuid::Uuid::parse_str(&wire.round_id).is_ok()
                        && wire.status_seq > 0
                        && matches!(wire.progress.mode.as_str(), "initial_scan" | "changes")
                        && wire.progress.page_size > 0
                        && wire.progress.page_size <= 32
                        && wire.progress.pages_checked <= wire.progress.pages_scanned
                        && wire.progress.checked_notes as u64 <= wire.progress.notes_scanned
                        && wire.progress.last_progress_at >= 0
                        && wire
                            .progress
                            .source_cursor
                            .as_ref()
                            .is_none_or(|cursor| uuid::Uuid::parse_str(cursor).is_ok()),
                    "invalid sync manager counters"
                );
                if let Some((_, sequence, old)) = &self.remote_manager {
                    if wire.status_seq < *sequence {
                        return Ok(());
                    }
                    if wire.status_seq == *sequence {
                        anyhow::ensure!(old == &frame.payload, "conflicting sync manager snapshot");
                        return Ok(());
                    }
                }
                anyhow::ensure!(
                    wire.progress.state.as_ref().is_none_or(|state| matches!(
                        state.as_str(),
                        "running" | "idle" | "failed"
                    )) && wire
                        .progress
                        .error
                        .as_ref()
                        .is_none_or(|error| !error.code.is_empty()
                            && error.code.len() <= 64
                            && error.code.bytes().all(|byte| byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || byte == b'_')),
                    "invalid sync manager outcome"
                );
                let new_round = self
                    .remote_manager
                    .as_ref()
                    .is_none_or(|(round, _, _)| round != &wire.round_id);
                let data = wire.progress;
                self.runtime
                    .progress(|p| {
                        if new_round {
                            p.download = SyncDirectionProgress::new("remote_verified");
                            p.download.run_id = wire.round_id.clone();
                        }
                        p.download.manager = Some(SyncManagerProgress {
                            state: data.state,
                            error: data
                                .error
                                .map(|error| format!("Synchronization manager: {}", error.code)),
                            mode: data.mode,
                            page_size: data.page_size,
                            pages_scanned: data.pages_scanned,
                            pages_checked: data.pages_checked,
                            notes_scanned: data.notes_scanned,
                            scan_complete: data.scan_complete,
                            source_cursor: data.source_cursor,
                            last_progress_at: data.last_progress_at,
                        });
                        p.download.notes_checked = data.checked_notes;
                        p.download.notes_total = data.total_notes;
                        if let Some(planned) = data.planned_notes {
                            p.download.notes_planned = planned;
                            p.download.notes_remaining =
                                Some(planned.saturating_sub(p.download.notes_completed));
                        }
                        if let Some(equal) = data.equal_notes {
                            p.download.notes_equal = equal;
                        }
                        if let Some(superseded) = data.superseded_notes {
                            p.download.notes_superseded = superseded;
                        }
                        if let Some(blocked) = data.blocked_notes {
                            p.download.blocked_notes = p.download.blocked_notes.max(blocked);
                        }
                    })
                    .await;
                if new_round {
                    self.remote_inventory_complete = false;
                }
                self.remote_manager = Some((wire.round_id, wire.status_seq, frame.payload));
            }
            "task_status" => {
                let status: TaskStatus =
                    serde_json::from_value(frame.payload.clone()).context("invalid task status")?;
                anyhow::ensure!(
                    serde_json::to_value(&status)? == frame.payload
                        && tasks::sequence(&status.task_id, &self.task_origin).is_some(),
                    "invalid task status identity"
                );
                status.validate()?;
                let Some(task) = self.outgoing_tasks.get_mut(&status.task_id) else {
                    anyhow::ensure!(
                        tasks::sequence(&status.task_id, &self.task_origin)
                            .is_some_and(|seq| seq <= self.task_sequence),
                        "unsolicited task status"
                    );
                    return Ok(());
                };
                if let Some(old) = &task.status {
                    if !old.accept_update(&status)? {
                        return Ok(());
                    }
                }
                let operation = task.operation.clone();
                let detach = task.detached && status.terminal();
                task.status = Some(status.clone());
                self.show_task(&status, &operation, true).await;
                if status.state == "failed" {
                    self.record_remote_task_failure(
                        &status.task_id,
                        &operation,
                        &status.error.as_ref().unwrap().code,
                    )
                    .await;
                }
                if detach {
                    if status.state != "succeeded" {
                        self.runtime.status.write().await.last_error = Some(
                            "Remote inventory task failed; queued notes remain pending".into(),
                        );
                    }
                    self.outgoing_tasks.remove(&status.task_id);
                }
            }
            "task_error" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ErrorReply {
                    task_id: String,
                    code: String,
                }
                let error: ErrorReply =
                    serde_json::from_value(frame.payload).context("invalid task error")?;
                anyhow::ensure!(
                    !error.code.is_empty()
                        && error.code.len() <= 64
                        && error
                            .code
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c == b'_')
                        && tasks::sequence(&error.task_id, &self.task_origin)
                            .is_some_and(|seq| seq <= self.task_sequence),
                    "invalid task error identity"
                );
                if let Some(task) = self.outgoing_tasks.get_mut(&error.task_id) {
                    let operation = task.operation.clone();
                    task.error = Some(error.code.clone());
                    self.record_remote_task_failure(&error.task_id, &operation, &error.code)
                        .await;
                }
            }
            "task_submit" => {
                let submit: Submission = match serde_json::from_value(frame.payload.clone()) {
                    Ok(submit) => submit,
                    Err(_) => {
                        let id = frame.payload["task_id"]
                            .as_str()
                            .filter(|id| id.len() <= 64)
                            .unwrap_or("");
                        return self.task_error(id, "invalid_submission").await;
                    }
                };
                let admission = match self.received_tasks.admit(&submit) {
                    Ok(admission) => admission,
                    Err(code) => return self.task_error(&submit.task_id, code).await,
                };
                let status = match admission {
                    Admission::Replay(status) => {
                        return self.publish_task(status, &submit.operation).await
                    }
                    Admission::Start(status) => status,
                };
                self.publish_task(status, &submit.operation).await?;
                let status =
                    self.received_tasks
                        .update(&submit.task_id, &submit.operation, json!({}));
                self.publish_task(status, &submit.operation).await?;
                if submit.operation == "transfer_abort" {
                    // The ledger also retains quarantined commits after their worker exits.
                    if let Some(transfer_task) =
                        self.received_tasks.active_transfer().map(str::to_owned)
                    {
                        let status = self.received_tasks.update(
                            &submit.task_id,
                            "cancellation_pending",
                            json!({}),
                        );
                        self.publish_task(status, &submit.operation).await?;
                        self.deferred_abort = Some((submit.task_id, transfer_task, submit.payload));
                        return Ok(());
                    }
                }
                let ctx = self.ctx.clone();
                let operation = submit.operation.clone();
                let payload = submit.payload;
                let durable = operation == "transfer_commit";
                let event = if durable {
                    self.incoming
                        .as_ref()
                        .and_then(|incoming| incoming.event.clone())
                } else {
                    None
                };
                let (receive, abort) = match operation.as_str() {
                    "state_query" => self.runtime.worker(async move {
                        let result = async {
                            let query: SyncStateQuery = serde_json::from_value(payload.clone())?;
                            anyhow::ensure!(
                                serde_json::to_value(&query)? == payload,
                                "invalid sync state query fields"
                            );
                            sync::validate_sync_state_query(&query)?;
                            Ok(serde_json::to_value(
                                sync::query_sync_state(&ctx, &query).await?,
                            )?)
                        }
                        .await;
                        TaskCompletion {
                            worker: None,
                            result,
                        }
                    }),
                    "inventory_start" => self.runtime.worker(async move {
                        let result = async {
                            let mut scan = sync::start_sync_candidate_scan(&ctx, false).await?;
                            while !scan.next_page(PAGE_SIZE).await?.is_empty() {}
                            Ok(json!({}))
                        }
                        .await;
                        TaskCompletion {
                            worker: None,
                            result,
                        }
                    }),
                    "inventory_complete" => {
                        let result = self.receive("inventory_complete", payload).await;
                        self.runtime.worker(async move {
                            TaskCompletion {
                                worker: None,
                                result,
                            }
                        })
                    }
                    _ => {
                        let mut worker = self.transfer_worker();
                        let operation = operation.clone();
                        self.runtime.worker(async move {
                            let mut result = worker.receive(&operation, payload.clone()).await;
                            if result.is_ok() && operation == "blob_chunk" {
                                result = Ok(json!({"transfer_id":payload["transfer_id"],"blob_id":payload["blob_id"],"received_bytes":payload["offset"].as_u64().unwrap()+BASE64.decode(payload["data"].as_str().unwrap()).unwrap().len() as u64}));
                            }
                            TaskCompletion { worker:Some(worker), result }
                        })
                    }
                };
                self.task_jobs.insert(
                    submit.task_id,
                    TaskJob {
                        operation,
                        receive,
                        abort,
                        cancelled: false,
                        durable,
                        event,
                    },
                );
            }
            "task_get" | "task_cancel" => {
                anyhow::ensure!(
                    frame
                        .payload
                        .as_object()
                        .is_some_and(|payload| payload.len() == 1),
                    "invalid task lookup"
                );
                let id = field(&frame.payload, "task_id")?;
                if frame.event == "task_cancel" {
                    if let Some(job) = self.task_jobs.get_mut(id) {
                        job.cancelled = true;
                        if !job.durable {
                            job.abort.abort();
                        }
                        let operation = job.operation.clone();
                        let status =
                            self.received_tasks
                                .update(id, "cancellation_pending", json!({}));
                        return self.publish_task(status, &operation).await;
                    }
                }
                match self.received_tasks.get(id) {
                    Ok(status) => {
                        self.send("task_status", serde_json::to_value(status)?, None)
                            .await?
                    }
                    Err(code) => self.task_error(id, code).await?,
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    async fn poll_task_jobs(&mut self) -> Result<()> {
        let mut completed = Vec::new();
        for (id, job) in &mut self.task_jobs {
            match job.receive.try_recv() {
                Ok(result) => completed.push((id.clone(), Some(result))),
                Err(oneshot::error::TryRecvError::Closed) => completed.push((id.clone(), None)),
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        for (id, completion) in completed {
            let job = self.task_jobs.remove(&id).unwrap();
            let Some(completion) = completion else {
                if job.durable {
                    let status = self.received_tasks.unknown(&id);
                    self.publish_task(status, &job.operation).await?;
                    if let Some(event) = job.event {
                        let ctx = self.ctx.clone();
                        let progress = WorkerProgress {
                            status: self.runtime.status.clone(),
                            task_mode: self.task_sync,
                            resolved: self.resolved.clone(),
                        };
                        let remote_complete = self.remote_inventory_complete;
                        let (receive, abort) = self.runtime.worker(async move {
                            let result = match sync::sync_received_event(&ctx, &event).await {
                                Ok(true) => {
                                    progress.complete_applied(&event, remote_complete).await;
                                    Ok(serde_json::to_value(event.acknowledgement()).unwrap())
                                }
                                _ => Err(UnknownCommit.into()),
                            };
                            TaskCompletion {
                                worker: None,
                                result,
                            }
                        });
                        self.task_jobs.insert(
                            id,
                            TaskJob {
                                operation: job.operation,
                                receive,
                                abort,
                                cancelled: job.cancelled,
                                durable: true,
                                event: None,
                            },
                        );
                    }
                } else {
                    let status = self.received_tasks.finish(
                        &id,
                        Err(if job.cancelled {
                            "cancelled"
                        } else {
                            "worker_failed"
                        }),
                    );
                    self.publish_task(status, &job.operation).await?;
                }
                continue;
            };
            if completion
                .result
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<UnknownCommit>().is_some())
            {
                let status = self.received_tasks.unknown(&id);
                self.publish_task(status, &job.operation).await?;
                continue;
            }
            if job.durable
                && completion
                    .result
                    .as_ref()
                    .is_err_and(|error| error.downcast_ref::<ApplyAttemptError>().is_some())
                && job.event.is_some()
            {
                let event = job.event.unwrap();
                let error = completion
                    .result
                    .unwrap_err()
                    .downcast::<ApplyAttemptError>()
                    .unwrap()
                    .source;
                let commit_uncertain = error
                    .downcast_ref::<sync::SyncCommitOutcomeUnknown>()
                    .is_some();
                let status = self.received_tasks.unknown(&id);
                self.publish_task(status, &job.operation).await?;
                let ctx = self.ctx.clone();
                let progress = WorkerProgress {
                    status: self.runtime.status.clone(),
                    task_mode: self.task_sync,
                    resolved: self.resolved.clone(),
                };
                let remote_complete = self.remote_inventory_complete;
                let (receive, abort) = self.runtime.worker(async move {
                    let result = match sync::sync_received_event(&ctx, &event).await {
                        Ok(true) => {
                            progress.complete_applied(&event, remote_complete).await;
                            Ok(serde_json::to_value(event.acknowledgement()).unwrap())
                        }
                        Ok(false) if !commit_uncertain => Err(error),
                        _ => Err(UnknownCommit.into()),
                    };
                    TaskCompletion {
                        worker: None,
                        result,
                    }
                });
                self.task_jobs.insert(
                    id,
                    TaskJob {
                        operation: job.operation,
                        receive,
                        abort,
                        cancelled: job.cancelled,
                        durable: true,
                        event: None,
                    },
                );
                continue;
            }
            let mut result = if job.cancelled && !job.durable {
                Err("cancelled")
            } else {
                completion
                    .result
                    .as_ref()
                    .map(|value| value.clone())
                    .map_err(|error| {
                        sync::sync_note_rejection_reason(error).unwrap_or("operation_failed")
                    })
            };
            if let Some(worker) = completion.worker {
                if result.is_ok() {
                    self.incoming = worker.incoming;
                    self.remote_inventory_complete = worker.remote_inventory_complete;
                }
                // Failed/cancelled task owns its staging until its worker has settled.
            }
            if result.as_ref().is_ok_and(|value| Frame::new(Some(&self.join), None, TOPIC, "task_status", json!({"task_id":id,"status_seq":u64::MAX,"state":"succeeded","phase":job.operation,"progress":{},"result":value,"error":null})).encode().is_err()) {
                result = Err("result_too_large");
            }
            let status = self.received_tasks.finish(&id, result);
            self.publish_task(status, &job.operation).await?;
        }
        if let Some((id, commit_id, payload)) = self.deferred_abort.take() {
            if self
                .received_tasks
                .get(&commit_id)
                .is_ok_and(|status| status.terminal())
            {
                let mut worker = self.transfer_worker();
                let result = worker.receive("transfer_abort", payload).await;
                self.incoming = worker.incoming;
                let status = self
                    .received_tasks
                    .finish(&id, result.map_err(|_| "operation_failed"));
                self.publish_task(status, "transfer_abort").await?;
            } else {
                self.deferred_abort = Some((id, commit_id, payload));
            }
        }
        Ok(())
    }
    fn rejection_path(&self, descriptor: &SyncAck) -> Result<PathBuf> {
        uuid::Uuid::parse_str(&descriptor.note_id)?;
        uuid::Uuid::parse_str(&descriptor.event_id)?;
        let mut directory = self
            .runtime
            .rejection_directory
            .lock()
            .expect("sync rejection directory mutex poisoned");
        if directory.is_none() {
            *directory = Some(Arc::new(
                tempfile::tempdir().map_err(|_| LocalManagerFailure)?,
            ));
        }
        let epoch = self
            .runtime
            .retry_epoch
            .load(std::sync::atomic::Ordering::Relaxed);
        Ok(directory
            .as_ref()
            .unwrap()
            .path()
            .join(format!("{epoch}-{}", descriptor.note_id)))
    }
    async fn descriptor_blocked(&mut self, descriptor: &SyncAck) -> Result<bool> {
        if !self.task_sync {
            return Ok(self.runtime.is_descriptor_blocked(descriptor));
        }
        let path = self.rejection_path(descriptor)?;
        match tokio::fs::read(&path).await {
            Ok(event) if event == descriptor.event_id.as_bytes() => Ok(true),
            Ok(_) => {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|_| LocalManagerFailure)?;
                self.runtime
                    .rejected_total
                    .fetch_update(
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                        |total| Some(total.saturating_sub(1)),
                    )
                    .ok();
                self.runtime
                    .blocked
                    .lock()
                    .expect("sync rejection mutex poisoned")
                    .remove(&descriptor.note_id);
                Ok(false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(LocalManagerFailure.into()),
        }
    }
    async fn reject_descriptor(&mut self, descriptor: &SyncAck) -> Result<()> {
        if self.task_sync {
            let path = self.rejection_path(descriptor)?;
            let existed = tokio::fs::try_exists(&path)
                .await
                .map_err(|_| LocalManagerFailure)?;
            tokio::fs::write(path, descriptor.event_id.as_bytes())
                .await
                .map_err(|_| LocalManagerFailure)?;
            if !existed {
                self.runtime
                    .rejected_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(())
    }
    // None means configuration changed; a failed manager still services its socket
    // at the idle poll interval, without repeatedly scanning the pending queue.
    async fn scheduled_work(
        &mut self,
        config: &note_core::sync::SyncConfig,
        retry: bool,
    ) -> Result<Option<bool>> {
        let current = match note_pipelines::get_system_config(self.ctx).await {
            Ok(current) => current.sync,
            Err(error) if Self::local_manager_error(&error) => {
                self.fail_local_manager().await?;
                return Ok(Some(false));
            }
            Err(error) => return Err(error),
        };
        if &current != config {
            return Ok(None);
        }
        if retry {
            self.manager_failed = false;
        }
        if self.manager_failed {
            return Ok(Some(false));
        }
        match self.has_pending_work().await {
            Ok(pending) => Ok(Some(pending)),
            Err(error) if Self::local_manager_error(&error) => {
                self.fail_local_manager().await?;
                Ok(Some(false))
            }
            Err(error) => Err(error),
        }
    }

    async fn has_pending_work(&mut self) -> Result<bool> {
        let mut after = None;
        loop {
            let page = sync::sync_pending_candidates(self.ctx, after.as_deref(), 32).await?;
            if page.is_empty() {
                return Ok(false);
            }
            for candidate in &page {
                if !self.descriptor_blocked(&candidate.descriptor).await? {
                    return Ok(true);
                }
            }
            after = page.last().map(|c| c.descriptor.note_id.clone());
        }
    }

    async fn query_state(&mut self, items: Vec<SyncAck>) -> Result<SyncStateResponse> {
        anyhow::ensure!(self.state_query, "sync state query was not negotiated");
        let query = SyncStateQuery {
            query_id: uuid::Uuid::new_v4().to_string(),
            items,
        };
        sync::validate_sync_state_query(&query)?;
        let value = self
            .request_with_budget(
                "state_query",
                serde_json::to_value(&query)?,
                Duration::from_secs(930),
                None,
            )
            .await?;
        let response: SyncStateResponse =
            serde_json::from_value(value.clone()).context("invalid sync state response")?;
        anyhow::ensure!(
            serde_json::to_value(&response)? == value && response.matches_query(&query),
            "sync state response does not match query"
        );
        for item in &response.items {
            if let Some(member) = &item.member {
                sync::validate_sync_state_query(&SyncStateQuery {
                    query_id: query.query_id.clone(),
                    items: vec![member.clone()],
                })?;
            }
        }
        Ok(response)
    }

    async fn resolve_upload(&mut self, note: &str, kind: &str) -> Result<()> {
        if !self.task_sync {
            self.runtime
                .progress(|p| match kind {
                    "equal" => p.upload.note_equal(note),
                    "superseded" => p.upload.supersede_note(note),
                    "blocked" => p.upload.block_note(note),
                    _ => p.upload.complete_note(note),
                })
                .await;
            return Ok(());
        }
        let run = self
            .runtime
            .status
            .read()
            .await
            .progress
            .as_ref()
            .unwrap()
            .upload
            .run_id
            .clone();
        let resolved_path = self.resolved.path().join(format!("{run}-{note}-resolved"));
        if kind == "blocked" {
            if tokio::fs::try_exists(resolved_path)
                .await
                .map_err(|_| LocalManagerFailure)?
            {
                return Ok(());
            }
            if scan::mark(self.resolved.path(), &run, note, "blocked")
                .await
                .map_err(|_| LocalManagerFailure)?
            {
                self.runtime
                    .progress(|p| {
                        p.upload.blocked_notes += 1;
                        p.upload.current_note = None;
                    })
                    .await;
            }
        } else if scan::mark(self.resolved.path(), &run, note, "resolved")
            .await
            .map_err(|_| LocalManagerFailure)?
        {
            let was_blocked = scan::unmark(self.resolved.path(), &run, note, "blocked")
                .await
                .map_err(|_| LocalManagerFailure)?;
            self.runtime
                .progress(|p| {
                    match kind {
                        "equal" => p.upload.notes_equal += 1,
                        "superseded" => p.upload.notes_superseded += 1,
                        _ => p.upload.notes_completed += 1,
                    }
                    if let Some(remaining) = &mut p.upload.notes_remaining {
                        *remaining = remaining.saturating_sub(1);
                    }
                    if was_blocked {
                        p.upload.blocked_notes = p.upload.blocked_notes.saturating_sub(1);
                    }
                    if p.upload
                        .current_note
                        .as_ref()
                        .is_some_and(|current| current.id == note)
                    {
                        p.upload.current_note = None;
                    }
                })
                .await;
        }
        self.manager_progress().await?;
        Ok(())
    }
    async fn manager_progress(&mut self) -> Result<()> {
        if !self.task_sync {
            return Ok(());
        }
        self.runtime
            .progress(|p| {
                if let Some(manager) = &mut p.upload.manager {
                    manager.last_progress_at = chrono::Utc::now().timestamp_millis();
                }
            })
            .await;
        self.publish_manager().await
    }
    async fn publish_manager(&mut self) -> Result<()> {
        if !self.task_sync {
            return Ok(());
        }
        let status = self.runtime.status.read().await;
        let Some(progress) = &status.progress else {
            return Ok(());
        };
        let upload = &progress.upload;
        let Some(manager) = &upload.manager else {
            return Ok(());
        };
        let mut data = serde_json::to_value(manager)?;
        let object = data.as_object_mut().unwrap();
        object.insert(
            "error".into(),
            if manager.error.is_some() {
                json!({"code":"source_failed","retryable":false})
            } else {
                Value::Null
            },
        );
        object.insert("checked_notes".into(), json!(upload.notes_checked));
        object.insert("total_notes".into(), json!(upload.notes_total));
        object.insert("planned_notes".into(), json!(upload.notes_planned));
        object.insert("equal_notes".into(), json!(upload.notes_equal));
        object.insert("superseded_notes".into(), json!(upload.notes_superseded));
        object.insert("blocked_notes".into(), json!(upload.blocked_notes));
        self.manager_sequence += 1;
        let payload =
            json!({"round_id":upload.run_id,"status_seq":self.manager_sequence,"progress":data});
        drop(status);
        self.send("sync_manager_status", payload, None).await
    }
    async fn refresh_scan(&mut self) -> Result<()> {
        let Some(source) = self
            .scan_progress
            .as_ref()
            .map(|source| source.borrow().clone())
        else {
            return Ok(());
        };
        let old = self
            .runtime
            .status
            .read()
            .await
            .progress
            .as_ref()
            .and_then(|p| p.upload.manager.as_ref())
            .map(|manager| (manager.pages_scanned, manager.scan_complete));
        if old == Some((source.pages_scanned, source.scan_complete)) {
            return Ok(());
        }
        self.runtime
            .progress(|p| {
                if let Some(manager) = &mut p.upload.manager {
                    manager.pages_scanned = source.pages_scanned;
                    manager.notes_scanned = source.notes_scanned as u64;
                    manager.scan_complete = source.scan_complete;
                    manager.source_cursor = source.source_cursor;
                    manager.last_progress_at = source.last_progress_at;
                }
                if source.scan_complete {
                    p.upload.notes_total = Some(source.notes_scanned);
                }
            })
            .await;
        self.publish_manager().await
    }
    async fn reconcile_pages(&mut self, full: bool) -> Result<()> {
        self.runtime
            .progress(|p| {
                p.upload = SyncDirectionProgress::new(if full {
                    "remote_verified"
                } else {
                    "local_queue"
                });
                p.upload.manager = Some(SyncManagerProgress {
                    state: Some("running".into()),
                    error: None,
                    mode: if full { "initial_scan" } else { "changes" }.into(),
                    page_size: PAGE_SIZE as u64,
                    pages_scanned: 0,
                    pages_checked: 0,
                    notes_scanned: 0,
                    scan_complete: false,
                    source_cursor: None,
                    last_progress_at: chrono::Utc::now().timestamp_millis(),
                });
            })
            .await;
        self.local_task("reconcile", "running", "checking").await;
        let mut spool =
            ScanSpool::start(self.ctx.clone(), !full).map_err(|_| LocalManagerFailure)?;
        self.scan_progress = Some(spool.subscribe());
        self.publish_manager().await?;
        loop {
            let (returned, result) = self
                .wait_task(tokio::spawn(async move {
                    let result = spool.next().await;
                    (spool, result)
                }))
                .await?;
            spool = returned;
            self.refresh_scan().await?;
            let Some(page) = result.map_err(|_| LocalManagerFailure)? else {
                break;
            };
            let response = if full {
                match self
                    .query_state(
                        page.iter()
                            .map(|candidate| candidate.descriptor.clone())
                            .collect(),
                    )
                    .await
                {
                    Ok(response) => Some(response),
                    Err(error) if error.downcast_ref::<RemoteTaskFailure>().is_some() => {
                        for candidate in &page {
                            let descriptor = &candidate.descriptor;
                            self.runtime.blocked_sample(SyncBlockedNote {
                                note_id: descriptor.note_id.clone(),
                                event_id: descriptor.event_id.clone(),
                                reason: "remote_state_query_failed".into(),
                            });
                            self.reject_descriptor(descriptor).await?;
                            self.resolve_upload(&descriptor.note_id, "blocked").await?;
                        }
                        self.runtime
                            .progress(|p| {
                                p.upload.notes_checked += page.len();
                                if let Some(manager) = &mut p.upload.manager {
                                    manager.pages_checked += 1;
                                    manager.last_progress_at =
                                        chrono::Utc::now().timestamp_millis();
                                }
                            })
                            .await;
                        self.publish_manager().await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            let mut work = Vec::with_capacity(page.len());
            for (index, mut candidate) in page.iter().cloned().enumerate() {
                let descriptor = &candidate.descriptor;
                if !sync::sync_candidate_current(self.ctx, descriptor, false).await? {
                    self.resolve_upload(&descriptor.note_id, "superseded")
                        .await?;
                    continue;
                }
                if let Some(item) = response.as_ref().map(|response| &response.items[index]) {
                    if !candidate.pending
                        && item.matches_snapshot(descriptor)
                        && sync::sync_candidate_current(self.ctx, descriptor, true).await?
                    {
                        self.resolve_upload(&descriptor.note_id, "equal").await?;
                        continue;
                    }
                    if item.association == SyncAssociation::Missing
                        && descriptor.kind == SyncEventKind::Snapshot
                    {
                        if sync::recover_missing_sync_association(self.ctx, descriptor).await? {
                            let current =
                                sync::sync_current_candidate(self.ctx, &descriptor.note_id).await?;
                            if let Some(current) = current.filter(|current| {
                                current.descriptor.kind == SyncEventKind::Attach
                                    && current.descriptor.generation == descriptor.generation
                                    && current.descriptor.control == descriptor.control
                                    && current.descriptor.version == descriptor.version
                                    && current.descriptor.snapshot_hash == descriptor.snapshot_hash
                            }) {
                                candidate = current;
                            } else {
                                self.resolve_upload(&descriptor.note_id, "superseded")
                                    .await?;
                                continue;
                            }
                        } else {
                            self.resolve_upload(&descriptor.note_id, "superseded")
                                .await?;
                            continue;
                        }
                    }
                }
                work.push(candidate);
            }
            self.runtime
                .progress(|p| {
                    p.upload.notes_checked += page.len();
                    p.upload.notes_planned += work.len();
                    p.upload.notes_remaining =
                        Some(p.upload.notes_remaining.unwrap_or(0) + work.len());
                    if let Some(manager) = &mut p.upload.manager {
                        manager.pages_checked += 1;
                        manager.last_progress_at = chrono::Utc::now().timestamp_millis();
                    }
                })
                .await;
            self.publish_manager().await?;
            // Source pages already carry global 8:1 pending/audit fairness.
            for candidate in work {
                self.deliver_candidate(candidate).await?;
            }
            self.service_ready().await?;
        }
        let total = spool.progress().notes_scanned;
        self.scan_progress = None;
        self.runtime
            .progress(|p| {
                p.upload.finish_check(total);
                p.upload.finish_round();
                if let Some(manager) = &mut p.upload.manager {
                    manager.state = Some("idle".into());
                    manager.error = None;
                }
                if full {
                    p.tracked_notes = Some(total);
                }
            })
            .await;
        self.publish_manager().await?;
        if full {
            let run_id = self
                .runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .upload
                .run_id
                .clone();
            if let Err(error) = self
                .task_request(
                    "inventory_complete",
                    json!({"inventory_id":run_id,"total_notes":total}),
                )
                .await
            {
                if error.downcast_ref::<RemoteTaskFailure>().is_none() {
                    return Err(error);
                }
            }
        }
        self.local_task("reconcile", "succeeded", "complete").await;
        Ok(())
    }
    async fn local_task(&mut self, operation: &str, state: &str, phase: &str) {
        self.runtime
            .progress(|p| {
                p.upload.active_task = Some(SyncActiveTask {
                    id: format!("local:{}", operation),
                    operation: operation.into(),
                    state: state.into(),
                    phase: phase.into(),
                    updated_at: chrono::Utc::now().timestamp_millis(),
                    error: None,
                })
            })
            .await;
    }
    fn local_manager_error(error: &anyhow::Error) -> bool {
        error.downcast_ref::<LocalManagerFailure>().is_some()
            || error.downcast_ref::<note_storage::StorageError>().is_some()
    }

    async fn fail_local_manager(&mut self) -> Result<()> {
        if self.manager_failed {
            return Ok(());
        }
        self.manager_failed = true;
        self.scan_progress = None;
        self.runtime
            .progress(|p| {
                p.upload.phase = "blocked".into();
                if let Some(manager) = &mut p.upload.manager {
                    manager.state = Some("failed".into());
                    manager.error = Some(
                        "Local synchronization scan failed; pending notes are preserved.".into(),
                    );
                    manager.last_progress_at = chrono::Utc::now().timestamp_millis();
                }
                p.upload.active_task = Some(SyncActiveTask {
                    id: "local:reconcile".into(),
                    operation: "reconcile".into(),
                    state: "failed".into(),
                    phase: "source_failed".into(),
                    updated_at: chrono::Utc::now().timestamp_millis(),
                    error: Some(
                        "Local synchronization scan failed; pending notes are preserved. Retry to resume.".into(),
                    ),
                });
            })
            .await;
        // A transport failure publishing the diagnostic still ends the session.
        self.publish_manager().await
    }

    async fn reconcile_round(&mut self, full: bool) -> Result<()> {
        if self.task_sync {
            let result = self.reconcile_pages(full).await;
            if result.as_ref().is_err_and(Self::local_manager_error) {
                self.fail_local_manager().await?;
                return Ok(());
            }
            return result;
        }
        let basis = if !full {
            "local_queue"
        } else if self.state_query {
            "remote_verified"
        } else {
            "legacy_replay"
        };
        self.runtime
            .progress(|p| p.upload = SyncDirectionProgress::new(basis))
            .await;
        self.local_task("reconcile", "running", "checking").await;
        let ctx = self.ctx.clone();
        let candidates = self
            .wait_task(tokio::spawn(async move {
                sync::sync_round_candidates(&ctx, !full).await
            }))
            .await??;
        self.local_task("reconcile", "succeeded", "checked").await;
        let total = candidates.len();
        self.runtime
            .progress(|p| {
                p.upload.notes_total = Some(total);
                if full {
                    p.tracked_notes = Some(total);
                }
            })
            .await;
        let mut work = Vec::new();
        let mut checked = 0;
        for page in candidates.chunks(32) {
            let response = if self.state_query && full {
                match self
                    .query_state(page.iter().map(|c| c.descriptor.clone()).collect())
                    .await
                {
                    Ok(response) => Some(response),
                    Err(error) if error.downcast_ref::<RemoteTaskFailure>().is_some() => {
                        for candidate in page {
                            let descriptor = &candidate.descriptor;
                            self.runtime.blocked_sample(SyncBlockedNote {
                                note_id: descriptor.note_id.clone(),
                                event_id: descriptor.event_id.clone(),
                                reason: "remote_state_query_failed".into(),
                            });
                            self.runtime
                                .progress(|p| p.upload.block_note(&descriptor.note_id))
                                .await;
                        }
                        checked += page.len();
                        self.runtime
                            .progress(|p| p.upload.notes_checked = checked)
                            .await;
                        self.service_ready().await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            for (index, mut candidate) in page.iter().cloned().enumerate() {
                let descriptor = &candidate.descriptor;
                if !sync::sync_candidate_current(self.ctx, descriptor, false).await? {
                    self.runtime
                        .progress(|p| p.upload.supersede_note(&descriptor.note_id))
                        .await;
                    continue;
                }
                if let Some(item) = response.as_ref().map(|r| &r.items[index]) {
                    if !candidate.pending
                        && item.matches_snapshot(descriptor)
                        && sync::sync_candidate_current(self.ctx, descriptor, true).await?
                    {
                        self.runtime
                            .progress(|p| p.upload.note_equal(&descriptor.note_id))
                            .await;
                        continue;
                    }
                    if item.association == SyncAssociation::Missing
                        && descriptor.kind == SyncEventKind::Snapshot
                    {
                        if sync::recover_missing_sync_association(self.ctx, descriptor).await? {
                            let current =
                                sync::sync_current_candidate(self.ctx, &descriptor.note_id).await?;
                            if let Some(current) = current.filter(|c| {
                                c.descriptor.kind == SyncEventKind::Attach
                                    && c.descriptor.generation == descriptor.generation
                                    && c.descriptor.control == descriptor.control
                                    && c.descriptor.version == descriptor.version
                                    && c.descriptor.snapshot_hash == descriptor.snapshot_hash
                            }) {
                                candidate = current;
                            } else {
                                self.runtime
                                    .progress(|p| p.upload.supersede_note(&descriptor.note_id))
                                    .await;
                                continue;
                            }
                        } else {
                            self.runtime
                                .progress(|p| p.upload.supersede_note(&descriptor.note_id))
                                .await;
                            continue;
                        }
                    }
                }
                work.push(candidate);
            }
            checked += page.len();
            self.runtime
                .progress(|p| p.upload.notes_checked = checked)
                .await;
            self.service_ready().await?;
        }
        self.runtime
            .progress(|p| p.upload.finish_check(total))
            .await;
        let (pending, audit): (Vec<_>, Vec<_>) = work.into_iter().partition(|c| c.pending);
        let mut pending: VecDeque<_> = pending.into();
        let mut audit: VecDeque<_> = audit.into();
        while !pending.is_empty() || !audit.is_empty() {
            // Bounded fairness: actual pending work wins, with one audit slot
            // after each eight pending selections so neither cohort starves.
            for _ in 0..8 {
                let Some(candidate) = pending.pop_front() else {
                    break;
                };
                self.deliver_candidate(candidate).await?;
            }
            if let Some(candidate) = audit.pop_front() {
                self.deliver_candidate(candidate).await?;
            }
        }
        self.runtime.progress(|p| p.upload.finish_round()).await;
        if full {
            let payload = if self.state_query {
                let run_id = self
                    .runtime
                    .status
                    .read()
                    .await
                    .progress
                    .as_ref()
                    .unwrap()
                    .upload
                    .run_id
                    .clone();
                serde_json::to_value(InventoryComplete {
                    inventory_id: run_id,
                    total_notes: total,
                })?
            } else {
                json!({})
            };
            if let Err(error) = self.request("inventory_complete", payload).await {
                if error.downcast_ref::<RemoteTaskFailure>().is_none() {
                    return Err(error);
                }
                self.runtime.status.write().await.last_error =
                    Some("Inventory completion task failed; connection remains available".into());
            }
        }
        Ok(())
    }

    async fn deliver_candidate(&mut self, candidate: sync::SyncCandidate) -> Result<()> {
        let descriptor = &candidate.descriptor;
        if self.descriptor_blocked(descriptor).await? {
            self.resolve_upload(&descriptor.note_id, "blocked").await?;
            return Ok(());
        }
        let event = sync::sync_candidate_event(self.ctx, descriptor).await?;
        let Some(event) = event else {
            self.resolve_upload(&descriptor.note_id, "superseded")
                .await?;
            return Ok(());
        };
        if !sync::reserve_sync_event(self.ctx, &event).await? {
            self.resolve_upload(&event.note_id, "superseded").await?;
            return Ok(());
        }
        let result = self.send_event(&event).await;
        sync::release_sync_event(self.ctx, &event.event_id).await?;
        match result {
            Ok(()) => {
                self.resolve_upload(&event.note_id, "completed").await?;
                self.runtime.status.write().await.last_success_at =
                    Some(chrono::Utc::now().timestamp_millis());
            }
            Err(error) if blocked_reason(&error).is_some() => {
                if self.task_sync {
                    if let Some(transfer) = self.outbound_transfer.take() {
                        let _ = self
                            .task_request("transfer_abort", json!({"transfer_id":transfer}))
                            .await;
                    }
                }
                self.runtime
                    .block(&event, blocked_reason(&error).unwrap())
                    .await;
                self.reject_descriptor(&event.acknowledgement()).await?;
                self.resolve_upload(&event.note_id, "blocked").await?;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn service_ready(&mut self) -> Result<()> {
        for _ in 0..32 {
            let Some(message) = self.socket.next().now_or_never() else {
                break;
            };
            self.handle_message(message).await?;
        }
        self.check_liveness().await
    }

    fn next_wake(&self) -> Instant {
        let mut wake = if self.socket.pumped() {
            Instant::now() + Duration::from_millis(250)
        } else {
            self.next_heartbeat
        };
        if let Some((_, at)) = &self.heartbeat {
            wake = (*at + DEADLINE).min(wake);
        }
        if !self.socket.pumped() {
            if let Some(incoming) = &self.incoming {
                wake = wake.min(incoming.idle_deadline);
            }
        }
        if self.inbound_query.is_some() || !self.task_jobs.is_empty() {
            wake = wake.min(Instant::now() + Duration::from_millis(50));
        }
        wake
    }

    async fn wait_task<T>(&mut self, task: tokio::task::JoinHandle<T>) -> Result<T> {
        let mut task = AbortTask(task);
        loop {
            self.check_liveness().await?;
            let wake = self.next_wake();
            tokio::select! {
                result=&mut task.0=>return result.context("sync background task failed"),
                message=self.socket.next()=>self.handle_message(message).await?,
                _=tokio::time::sleep_until(wake)=>{},
            }
        }
    }

    fn reference(&mut self) -> String {
        self.counter += 1;
        self.counter.to_string()
    }
    async fn send(&mut self, event: &str, payload: Value, reference: Option<&str>) -> Result<()> {
        let frame = Frame::new(Some(&self.join), reference, TOPIC, event, payload);
        if self.socket.pumped() {
            self.socket
                .send(Message::Text(frame.encode()?.into()))
                .await
                .context("sync socket write failed")?;
        } else {
            timeout(
                DEADLINE,
                self.socket.send(Message::Text(frame.encode()?.into())),
            )
            .await
            .context("sync socket write timed out")?
            .context("sync socket write failed")?;
        }
        Ok(())
    }
    async fn request(&mut self, event: &str, payload: Value) -> Result<Value> {
        if self.task_sync && tasks::OPERATIONS.contains(&event) {
            return self.task_request(event, payload).await;
        }
        let budget = if event == "transfer_commit" {
            commit_wait(0, 0)
        } else {
            DEADLINE
        };
        self.request_with_budget(event, payload, budget, None).await
    }
    async fn request_with_budget(
        &mut self,
        event: &str,
        payload: Value,
        budget: Duration,
        expected_ack: Option<&SyncAck>,
    ) -> Result<Value> {
        if self.task_sync && tasks::OPERATIONS.contains(&event) {
            return self.task_request(event, payload).await;
        }
        let reference = self.reference();
        self.send(event, payload, Some(&reference)).await?;
        self.await_reply(event, &reference, expected_ack, Instant::now() + budget)
            .await
    }
    async fn await_reply(
        &mut self,
        event: &str,
        reference: &str,
        expected_ack: Option<&SyncAck>,
        deadline: Instant,
    ) -> Result<Value> {
        loop {
            let control = self.socket.pumped() && matches!(event, "phx_join" | "sync_negotiate");
            anyhow::ensure!(
                control || Instant::now() < deadline,
                "sync Phoenix reply timed out"
            );
            self.check_liveness().await?;
            let wake = self.next_wake();
            let message = tokio::select! {
                message = self.socket.next() => message,
                _ = tokio::time::sleep_until(if control { wake } else { wake.min(deadline) }) => continue,
            };
            if let Some(Ok(Message::Text(text))) = &message {
                let frame = Frame::decode(text).context("invalid sync Phoenix frame")?;
                if frame.is_current(&self.join, reference)
                    && frame.event == "phx_reply"
                    && frame.topic == TOPIC
                {
                    if frame.payload["status"] != "ok" {
                        if event == "transfer_commit" && frame.payload["status"] == "error" {
                            if let Ok(rejection) = serde_json::from_value::<PeerNoteRejection>(
                                frame.payload["response"].clone(),
                            ) {
                                if rejection.code == "note_rejected"
                                    && matches!(
                                        rejection.reason.as_str(),
                                        "validation_failed" | "catalog_conflict"
                                    )
                                {
                                    return Err(rejection.into());
                                }
                            }
                        }
                        if event == "phx_join" {
                            return Err(Failure::new("phoenix", "phoenix_join_rejected", "GaoNote rejected the Phoenix channel join; verify the registered Agent Note node, trusted transport identity and authorized label selectors.").into());
                        }
                        bail!("sync peer rejected channel request");
                    }
                    return Ok(frame.payload["response"].clone());
                }
                if frame.topic == TOPIC
                    && frame.join_ref.as_deref() == Some(&self.join)
                    && frame.event == "ack"
                    && expected_ack.is_some_and(|expected| {
                        serde_json::from_value::<SyncAck>(frame.payload.clone())
                            .is_ok_and(|actual| &actual == expected)
                    })
                {
                    return Ok(frame.payload.clone());
                }
            }
            // Finish an incoming durable commit and its ACK before observing expiry.
            // Cancelling this handler could leave a published attachment without its note.
            self.handle_message(message).await?;
        }
    }
    async fn check_liveness(&mut self) -> Result<()> {
        self.poll_task_jobs().await?;
        self.refresh_scan().await?;
        if self.socket.pumped() {
            return Ok(());
        }
        let now = Instant::now();
        if self
            .inbound_query
            .as_ref()
            .is_some_and(|q| now.duration_since(q.started) >= Duration::from_secs(900))
        {
            bail!("sync state read timed out");
        }
        if self
            .inbound_query
            .as_ref()
            .is_some_and(|q| q.task.0.is_finished())
        {
            let mut query = self.inbound_query.take().unwrap();
            let response = (&mut query.task.0)
                .await
                .context("sync state read task failed")??;
            let reply = Frame::new(
                Some(&self.join),
                Some(&query.reference),
                TOPIC,
                "phx_reply",
                json!({"status":"ok","response":response}),
            );
            timeout(
                DEADLINE,
                self.socket.send(Message::Text(reply.encode()?.into())),
            )
            .await
            .context("sync state reply write timed out")??;
        }
        if self
            .heartbeat
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= DEADLINE)
        {
            bail!("sync heartbeat reply timed out");
        }
        if self
            .incoming
            .as_ref()
            .is_some_and(|i| now >= i.idle_deadline)
        {
            bail!("sync inbound transfer stalled");
        }
        if now >= self.next_heartbeat && self.heartbeat.is_none() {
            let reference = self.reference();
            let frame = Frame::new(None, Some(&reference), "phoenix", "heartbeat", json!({}));
            timeout(
                DEADLINE,
                self.socket.send(Message::Text(frame.encode()?.into())),
            )
            .await
            .context("sync heartbeat write timed out")??;
            self.heartbeat = Some((reference, now));
            self.next_heartbeat = now + HEARTBEAT;
        }
        Ok(())
    }
    async fn handle_message(
        &mut self,
        message: Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) -> Result<()> {
        let message = message
            .context("sync socket closed")?
            .context("sync socket read failed")?;
        let text = match message {
            Message::Text(text) => text,
            Message::Ping(bytes) => {
                self.socket.send(Message::Pong(bytes)).await?;
                return Ok(());
            }
            Message::Pong(_) => return Ok(()),
            Message::Close(_) => bail!("sync socket closed"),
            _ => bail!("sync requires Phoenix V2 JSON text frames"),
        };
        let frame = Frame::decode(&text).context("invalid sync Phoenix frame")?;
        if frame.topic == "phoenix" && frame.event == "phx_reply" {
            if self
                .heartbeat
                .as_ref()
                .is_some_and(|(r, _)| frame.reference.as_ref() == Some(r))
                && frame.payload["status"] == "ok"
            {
                self.heartbeat = None;
            }
            return Ok(());
        }
        if frame.topic != TOPIC || frame.join_ref.as_deref() != Some(&self.join) {
            return Ok(());
        }
        if matches!(frame.event.as_str(), "phx_error" | "phx_close") {
            bail!("sync Phoenix channel closed");
        }
        if self.task_sync
            && matches!(
                frame.event.as_str(),
                "task_submit"
                    | "task_get"
                    | "task_cancel"
                    | "task_status"
                    | "task_error"
                    | "sync_manager_status"
            )
        {
            return self.handle_task_frame(frame).await;
        }
        if frame.event == "phx_reply" {
            return Ok(());
        }
        if frame.event == "state_query" {
            anyhow::ensure!(
                self.state_query && self.inbound_query.is_none(),
                "sync state query was not negotiated or overlaps another query"
            );
            let reference = frame
                .reference
                .context("sync state query requires a request reference")?;
            let query: SyncStateQuery = serde_json::from_value(frame.payload.clone())
                .context("invalid sync state query")?;
            anyhow::ensure!(
                serde_json::to_value(&query)? == frame.payload,
                "invalid sync state query fields"
            );
            sync::validate_sync_state_query(&query)?;
            if self.remote_inventory_complete {
                self.runtime
                    .progress(|p| {
                        let basis = p.download.basis.clone();
                        p.download = SyncDirectionProgress::new(&basis);
                    })
                    .await;
                self.remote_inventory_complete = false;
            }
            let ctx = self.ctx.clone();
            self.inbound_query = Some(InboundQuery {
                reference,
                started: Instant::now(),
                task: AbortTask(tokio::spawn(async move {
                    sync::query_sync_state(&ctx, &query).await
                })),
            });
            return Ok(());
        }
        let is_commit = frame.event == "transfer_commit";
        let committing = if is_commit {
            self.incoming
                .as_ref()
                .and_then(|i| i.event.as_ref())
                .map(|event| (event.event_id.clone(), event.note_id.clone()))
        } else {
            None
        };
        let response = match self.receive(&frame.event, frame.payload).await {
            Ok(response) => response,
            Err(error) => {
                if let (Some((event_id, note_id)), Some(reason)) = (
                    committing,
                    sync::sync_note_rejection_reason(
                        error
                            .downcast_ref::<ApplyAttemptError>()
                            .map_or(&error, |attempt| &attempt.source),
                    ),
                ) {
                    self.runtime
                        .progress(|p| p.download.block_note(&note_id))
                        .await;
                    // receive consumed staging and the pipeline rolled back;
                    // expected note failures cannot poison the next transfer.
                    let rejection = json!({"code":"note_rejected","event_id":event_id,"note_id":note_id,"reason":reason});
                    if frame.reference.is_some() {
                        let reply = Frame::new(
                            Some(&self.join),
                            frame.reference.as_deref(),
                            TOPIC,
                            "phx_reply",
                            json!({"status":"error","response":rejection}),
                        );
                        timeout(
                            DEADLINE,
                            self.socket.send(Message::Text(reply.encode()?.into())),
                        )
                        .await
                        .context("sync rejection write timed out")??;
                    } else {
                        self.send("note_rejected", rejection, None).await?;
                    }
                    return Ok(());
                }
                return Err(error);
            }
        };
        if is_commit {
            self.send("ack", response.clone(), None).await?;
        }
        if frame.reference.is_some() {
            let reply = Frame::new(
                Some(&self.join),
                frame.reference.as_deref(),
                TOPIC,
                "phx_reply",
                json!({"status":"ok","response":response}),
            );
            timeout(
                DEADLINE,
                self.socket.send(Message::Text(reply.encode()?.into())),
            )
            .await
            .context("sync reply write timed out")??;
        }
        Ok(())
    }
    async fn receive(&mut self, event: &str, payload: Value) -> Result<Value> {
        match event {
            "transfer_begin" | "blob_chunk" | "blob_end" | "attachment_begin"
            | "transfer_commit" | "transfer_abort" => {
                let mut worker = self.transfer_worker();
                let result = worker.receive(event, payload).await;
                self.incoming = worker.incoming;
                self.remote_inventory_complete = worker.remote_inventory_complete;
                return result;
            }
            "note_rejected" => {
                let rejection: PeerNoteRejection =
                    serde_json::from_value(payload).context("invalid sync rejection push")?;
                anyhow::ensure!(
                    rejection.code == "note_rejected"
                        && matches!(
                            rejection.reason.as_str(),
                            "validation_failed" | "catalog_conflict"
                        ),
                    "invalid sync rejection code"
                );
                if self.outbound.as_ref().is_some_and(|(event_id, note_id)| {
                    event_id == &rejection.event_id && note_id == &rejection.note_id
                }) {
                    self.outbound = None;
                    return Err(rejection.into());
                }
            }
            "ack" => {
                let ack: SyncAck =
                    serde_json::from_value(payload).context("invalid sync acknowledgement push")?;
                sync::acknowledge_sync_event(self.ctx, &ack).await?;
                self.runtime.status.write().await.last_success_at =
                    Some(chrono::Utc::now().timestamp_millis());
            }
            "inventory_complete" => {
                let complete = if self.state_query {
                    let complete: InventoryComplete = serde_json::from_value(payload.clone())
                        .context("invalid remote inventory completion")?;
                    anyhow::ensure!(
                        uuid::Uuid::parse_str(&complete.inventory_id).is_ok()
                            && serde_json::to_value(&complete)? == payload,
                        "invalid remote inventory completion fields"
                    );
                    anyhow::ensure!(
                        self.incoming.is_none(),
                        "remote inventory completed during a transfer"
                    );
                    if let Some(progress) = &self.runtime.status.read().await.progress {
                        anyhow::ensure!(
                            complete.total_notes
                                >= progress
                                    .download
                                    .notes_completed
                                    .saturating_add(progress.download.blocked_notes),
                            "remote inventory total is smaller than its observed cohort"
                        );
                    }
                    Some(complete)
                } else {
                    anyhow::ensure!(payload == json!({}), "invalid legacy inventory completion");
                    None
                };
                self.remote_inventory_complete = true;
                self.runtime
                    .progress(|p| {
                        if let Some(complete) = complete {
                            p.download.run_id = complete.inventory_id;
                            p.download.notes_total = Some(complete.total_notes);
                        }
                        // Enumeration does not reveal the sender's unresolved work.
                        if p.download.manager.is_none() {
                            p.download.notes_remaining = None;
                        }
                        p.download.phase = if p.download.blocked_notes > 0 {
                            "blocked"
                        } else {
                            "idle"
                        }
                        .into();
                    })
                    .await;
            }
            _ => bail!("unknown sync Phoenix event"),
        }
        Ok(json!({}))
    }
    async fn send_blob(&mut self, transfer: &str, id: &str, bytes: &[u8]) -> Result<()> {
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            let payload = json!({"transfer_id":transfer,"blob_id":id,"offset":index*CHUNK,"data":BASE64.encode(chunk)});
            if self.task_sync {
                let result = self.task_request("blob_chunk", payload).await?;
                anyhow::ensure!(
                    result
                        == json!({"transfer_id":transfer,"blob_id":id,"received_bytes":index*CHUNK+chunk.len()}),
                    "invalid sync chunk receipt"
                );
            } else {
                self.send("blob_chunk", payload, None).await?;
            }
            if id != "event" {
                self.runtime
                    .progress(|p| {
                        if let Some(note) = &mut p.upload.current_note {
                            note.attachment_bytes_done = note
                                .attachment_bytes_done
                                .saturating_add(chunk.len() as u64)
                                .min(note.attachment_bytes_total);
                        }
                    })
                    .await;
            }
            self.manager_progress().await?;
            // Read while writing so simultaneous inventories and heartbeats do
            // not deadlock behind a large outgoing blob.
            while let Some(message) = self.socket.next().now_or_never() {
                self.handle_message(message).await?;
            }
            self.check_liveness().await?;
        }
        self.request("blob_end", json!({"transfer_id":transfer,"blob_id":id}))
            .await?;
        Ok(())
    }
    async fn send_event(&mut self, event: &SyncEvent) -> Result<()> {
        self.runtime.progress(|p| p.upload.begin_note(event)).await;
        let metadata = encode_metadata(event, self.limit)
            .map_err(|_| PreparationError("metadata exceeds negotiated limit".into()))?;
        let total = event
            .snapshot
            .as_ref()
            .map_or(Some(metadata.len() as u64), |s| {
                s.attachments
                    .iter()
                    .try_fold(metadata.len() as u64, |sum, a| {
                        sum.checked_add(a.size_bytes)
                    })
            })
            .context("sync attachment sizes overflow")?;
        if total > self.limit || metadata.len() as u64 > self.limit {
            return Err(PreparationError(
                "event exceeds negotiated blob limit; adjust profile/server limit".into(),
            )
            .into());
        }
        let ctx = self.ctx.clone();
        let source = event.clone();
        let limit = self.limit;
        self.local_task("preparation", "running", "preparing").await;
        let prepared = self
            .wait_task(tokio::spawn(async move {
                let event = source;

                let mut prepared = HashMap::new();
                if let Some(snapshot) = &event.snapshot {
                    for attachment in &snapshot.attachments {
                        let bytes = sync::read_sync_attachment(&ctx, &event, &attachment.id, limit)
                            .await
                            .map_err(|_| {
                                anyhow::anyhow!("source attachment cannot be read or verified")
                            })?;
                        anyhow::ensure!(
                            bytes.len() as u64 == attachment.size_bytes
                                && format!("{:x}", Sha256::digest(&bytes))
                                    == attachment.checksum_sha256,
                            "source attachment changed"
                        );
                        prepared.insert(attachment.id.clone(), bytes);
                    }
                }
                sync::validate_sync_event(&event, &prepared).map_err(|_| {
                    anyhow::anyhow!("local note contains invalid synchronization metadata")
                })?;
                if let Some(snapshot) = &event.snapshot {
                    for attachment in &snapshot.attachments {
                        let encoded_id = attachment
                            .id
                            .chars()
                            .try_fold(13usize, |len, ch| {
                                len.checked_add(match ch {
                                    '\"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                                    '\u{0}'..='\u{1f}' => 6,
                                    _ => ch.len_utf8(),
                                })
                            })
                            .context("attachment identifier length overflow")?;
                        anyhow::ensure!(
                            encoded_id <= FRAME_BYTES - 4 * CHUNK.div_ceil(3) - 2048,
                            "attachment identifier exceeds sync frame limit"
                        );
                    }
                }
                Ok::<_, anyhow::Error>(prepared)
            }))
            .await?
            .map_err(|error| PreparationError(error.to_string()))?;
        self.local_task("preparation", "succeeded", "prepared")
            .await;
        self.runtime
            .progress(|p| {
                if let Some(note) = &mut p.upload.current_note {
                    note.phase = "metadata".into();
                }
            })
            .await;
        self.outbound = Some((event.event_id.clone(), event.note_id.clone()));
        let transfer = uuid::Uuid::new_v4().to_string();
        self.outbound_transfer = Some(transfer.clone());
        self.request("transfer_begin",json!({"transfer_id":transfer,"size_bytes":metadata.len(),"checksum_sha256":format!("{:x}",Sha256::digest(&metadata))})).await?;
        self.send_blob(&transfer, "event", &metadata).await?;
        if let Some(snapshot) = &event.snapshot {
            for attachment in &snapshot.attachments {
                self.runtime
                    .progress(|p| {
                        if let Some(note) = &mut p.upload.current_note {
                            note.phase = "attachments".into();
                        }
                    })
                    .await;
                let bytes = prepared
                    .get(&attachment.id)
                    .context("prepared sync attachment missing")?;
                self.request(
                    "attachment_begin",
                    json!({"transfer_id":transfer,"blob_id":format!("attachment:{}",attachment.id)}),
                )
                .await?;
                self.send_blob(&transfer, &format!("attachment:{}", attachment.id), &bytes)
                    .await?;
                self.runtime
                    .progress(|p| {
                        if let Some(note) = &mut p.upload.current_note {
                            note.attachments_completed += 1;
                        }
                    })
                    .await;
            }
        }
        self.runtime
            .progress(|p| {
                if let Some(note) = &mut p.upload.current_note {
                    note.phase = "awaiting_commit".into();
                }
            })
            .await;
        let expected_ack = event.acknowledgement();
        let attachment_count = event.snapshot.as_ref().map_or(0, |s| s.attachments.len());
        let committed = self
            .request_with_budget(
                "transfer_commit",
                json!({"transfer_id":transfer}),
                commit_wait(total, attachment_count),
                Some(&expected_ack),
            )
            .await;
        let committed = match committed {
            Err(error) if error.downcast_ref::<PeerNoteRejection>().is_some() => {
                let rejection = error
                    .downcast_ref::<PeerNoteRejection>()
                    .expect("guarded rejection");
                anyhow::ensure!(
                    rejection.event_id == event.event_id && rejection.note_id == event.note_id,
                    "sync rejection does not match sent version"
                );
                self.outbound = None;
                return Err(error);
            }
            other => other?,
        };
        let ack: SyncAck = serde_json::from_value(committed)
            .context("sync peer returned invalid acknowledgement")?;
        anyhow::ensure!(
            ack == event.acknowledgement(),
            "sync acknowledgement does not match sent version"
        );
        sync::acknowledge_sync_event(self.ctx, &ack).await?;
        self.outbound = None;
        self.outbound_transfer = None;
        Ok(())
    }
}

struct WorkerProgress {
    status: Arc<RwLock<SyncRuntimeStatus>>,
    task_mode: bool,
    resolved: Arc<tempfile::TempDir>,
}
impl WorkerProgress {
    async fn progress(&self, update: impl FnOnce(&mut SyncProgress)) {
        let mut state = self.status.write().await;
        let progress = state
            .progress
            .get_or_insert_with(|| SyncProgress::new("remote_verified"));
        update(progress);
        progress.touch();
    }
}
impl WorkerProgress {
    async fn complete_applied(&self, event: &SyncEvent, remote_complete: bool) {
        let fresh = if self.task_mode {
            let run = self
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .download
                .run_id
                .clone();
            scan::mark(self.resolved.path(), &run, &event.note_id, "resolved")
                .await
                .unwrap_or(false)
        } else {
            true
        };
        self.progress(|p| {
            if fresh {
                if self.task_mode {
                    p.download.notes_completed += 1;
                    if let Some(remaining) = &mut p.download.notes_remaining {
                        *remaining = remaining.saturating_sub(1);
                    }
                } else {
                    p.download.complete_note(&event.note_id);
                }
            }
            if self.task_mode {
                p.download.release_page_ids();
            }
            p.download.current_note = None;
            if remote_complete {
                p.download.phase = "idle".into();
            }
        })
        .await;
        self.status.write().await.last_success_at = Some(chrono::Utc::now().timestamp_millis());
    }
}
struct TransferWorker {
    ctx: Context,
    runtime: WorkerProgress,
    incoming: Option<Incoming>,
    limit: u64,
    remote_inventory_complete: bool,
}
impl TransferWorker {
    async fn receive(&mut self, event: &str, payload: Value) -> Result<Value> {
        match event {
            "transfer_begin" => {
                if self.incoming.is_some() {
                    bail!("overlapping sync transfers are not allowed");
                }
                let id = field(&payload, "transfer_id")?.to_owned();
                anyhow::ensure!(
                    uuid::Uuid::parse_str(&id).is_ok(),
                    "invalid sync transfer ID"
                );
                let size = number(&payload, "size_bytes")?;
                let metadata =
                    StagedBlob::new(size, field(&payload, "checksum_sha256")?, self.limit)?;
                self.incoming = Some(Incoming {
                    id,
                    metadata,
                    event: None,
                    blobs: HashMap::new(),
                    idle_deadline: Instant::now() + DEADLINE,
                });
            }
            "blob_chunk" => {
                let incoming = self
                    .incoming
                    .as_mut()
                    .context("sync transfer has not begun")?;
                anyhow::ensure!(
                    field(&payload, "transfer_id")? == incoming.id,
                    "sync transfer ID mismatch"
                );
                let id = field(&payload, "blob_id")?;
                let encoded = field(&payload, "data")?;
                anyhow::ensure!(
                    encoded.len() <= 4 * CHUNK.div_ceil(3),
                    "sync chunk exceeds 64KiB"
                );
                let bytes = BASE64
                    .decode(encoded)
                    .context("invalid sync chunk base64")?;
                anyhow::ensure!(bytes.len() <= CHUNK, "sync chunk exceeds 64KiB");
                let blob = if id == "event" {
                    &mut incoming.metadata
                } else {
                    incoming
                        .blobs
                        .get_mut(id)
                        .context("sync attachment has not begun")?
                };
                blob.append(number(&payload, "offset")?, &bytes).await?;
                incoming.idle_deadline = Instant::now() + DEADLINE;
                if id != "event" {
                    self.runtime
                        .progress(|p| {
                            if let Some(note) = &mut p.download.current_note {
                                note.attachment_bytes_done = note
                                    .attachment_bytes_done
                                    .saturating_add(bytes.len() as u64)
                                    .min(note.attachment_bytes_total);
                            }
                        })
                        .await;
                }
            }
            "blob_end" => {
                let incoming = self
                    .incoming
                    .as_mut()
                    .context("sync transfer has not begun")?;
                anyhow::ensure!(
                    field(&payload, "transfer_id")? == incoming.id,
                    "sync transfer ID mismatch"
                );
                let id = field(&payload, "blob_id")?;
                if id == "event" {
                    incoming.metadata.finish().await?;
                    let event: SyncEvent = serde_json::from_slice(&incoming.metadata.read().await?)
                        .context("invalid sync event JSON")?;
                    let total = event
                        .snapshot
                        .as_ref()
                        .map_or(Some(0), |s| {
                            s.attachments
                                .iter()
                                .try_fold(0u64, |acc, a| acc.checked_add(a.size_bytes))
                        })
                        .context("sync attachment sizes overflow")?;
                    anyhow::ensure!(
                        total
                            .checked_add(incoming.metadata.size)
                            .is_some_and(|n| n <= self.limit),
                        "sync event exceeds negotiated blob limit"
                    );
                    if self.remote_inventory_complete {
                        self.runtime
                            .progress(|p| {
                                if p.download.manager.is_none() {
                                    let basis = p.download.basis.clone();
                                    p.download = SyncDirectionProgress::new(&basis);
                                }
                            })
                            .await;
                        self.remote_inventory_complete = false;
                    }
                    self.runtime
                        .progress(|p| {
                            p.download.begin_note(&event);
                            if let Some(note) = &mut p.download.current_note {
                                note.phase = if note.attachments_total == 0 {
                                    "awaiting_commit"
                                } else {
                                    "attachments"
                                }
                                .into();
                            }
                        })
                        .await;
                    incoming.event = Some(event);
                } else {
                    incoming
                        .blobs
                        .get_mut(id)
                        .context("sync attachment has not begun")?
                        .finish()
                        .await?;
                    self.runtime
                        .progress(|p| {
                            if let Some(note) = &mut p.download.current_note {
                                note.attachments_completed = note
                                    .attachments_completed
                                    .saturating_add(1)
                                    .min(note.attachments_total);
                                if note.attachments_completed == note.attachments_total {
                                    note.phase = "awaiting_commit".into();
                                }
                            }
                        })
                        .await;
                }
                // V1 permits any attachment order; allow the largest remaining
                // read budget only after all begun attachments are verified.
                // Start after local verification and flushing.
                let next_size = incoming
                    .event
                    .as_ref()
                    .filter(|_| incoming.blobs.values().all(StagedBlob::is_verified))
                    .and_then(|event| event.snapshot.as_ref())
                    .into_iter()
                    .flat_map(|snapshot| &snapshot.attachments)
                    .filter(|attachment| {
                        !incoming
                            .blobs
                            .contains_key(&format!("attachment:{}", attachment.id))
                    })
                    .map(|attachment| attachment.size_bytes)
                    .max();
                incoming.idle_deadline =
                    Instant::now() + next_size.map_or(DEADLINE, attachment_prepare_wait);
            }
            "attachment_begin" => {
                let incoming = self
                    .incoming
                    .as_mut()
                    .context("sync transfer has not begun")?;
                anyhow::ensure!(
                    field(&payload, "transfer_id")? == incoming.id,
                    "sync transfer ID mismatch"
                );
                let id = field(&payload, "blob_id")?;
                anyhow::ensure!(
                    !incoming.blobs.contains_key(id),
                    "duplicate sync attachment"
                );
                let descriptor = incoming
                    .event
                    .as_ref()
                    .and_then(|e| e.snapshot.as_ref())
                    .and_then(|s| {
                        s.attachments
                            .iter()
                            .find(|a| Some(a.id.as_str()) == id.strip_prefix("attachment:"))
                    })
                    .context("sync attachment missing from manifest")?;
                incoming.blobs.insert(
                    id.into(),
                    StagedBlob::new(
                        descriptor.size_bytes,
                        &descriptor.checksum_sha256,
                        self.limit,
                    )?,
                );
                incoming.idle_deadline = Instant::now() + DEADLINE;
            }
            "transfer_commit" => {
                let incoming = self
                    .incoming
                    .take()
                    .context("sync transfer has not begun")?;
                anyhow::ensure!(
                    field(&payload, "transfer_id")? == incoming.id,
                    "sync transfer ID mismatch"
                );
                let event = incoming.event.context("sync event metadata incomplete")?;
                self.runtime
                    .progress(|p| {
                        if let Some(note) = &mut p.download.current_note {
                            note.phase = "applying".into();
                        }
                    })
                    .await;
                let mut attachments = HashMap::new();
                if let Some(snapshot) = &event.snapshot {
                    for descriptor in &snapshot.attachments {
                        attachments.insert(
                            descriptor.id.clone(),
                            incoming
                                .blobs
                                .get(&format!("attachment:{}", descriptor.id))
                                .context("sync attachment incomplete")?
                                .read()
                                .await?,
                        );
                    }
                }
                // ACK is constructed only after the business transaction commits successfully.
                sync::apply_sync_event(&self.ctx, &event, &attachments)
                    .await
                    .map_err(|source| {
                        if sync::sync_note_rejection_reason(&source).is_some() {
                            source
                        } else {
                            ApplyAttemptError { source }.into()
                        }
                    })?;
                self.runtime
                    .complete_applied(&event, self.remote_inventory_complete)
                    .await;
                return Ok(serde_json::to_value(event.acknowledgement())?);
            }
            "transfer_abort" => {
                let id = field(&payload, "transfer_id")?;
                anyhow::ensure!(
                    self.incoming
                        .as_ref()
                        .is_none_or(|incoming| incoming.id == id),
                    "sync transfer ID mismatch"
                );
                self.incoming = None;
            }
            _ => bail!("invalid transfer operation"),
        }
        Ok(json!({}))
    }
}
#[derive(Debug)]
struct ApplyAttemptError {
    source: anyhow::Error,
}
impl std::fmt::Display for ApplyAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("sync durable apply attempt failed")
    }
}
impl std::error::Error for ApplyAttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
struct TaskCompletion {
    worker: Option<TransferWorker>,
    result: Result<Value>,
}
struct TaskJob {
    operation: String,
    receive: oneshot::Receiver<TaskCompletion>,
    abort: tokio::task::AbortHandle,
    cancelled: bool,
    durable: bool,
    event: Option<SyncEvent>,
}
struct OutgoingTask {
    operation: String,
    error: Option<String>,
    detached: bool,
    status: Option<TaskStatus>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerNoteRejection {
    code: String,
    event_id: String,
    note_id: String,
    reason: String,
}
impl std::fmt::Display for PeerNoteRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "receiver rejected note ({})", self.reason)
    }
}
impl std::error::Error for PeerNoteRejection {}
fn blocked_reason(error: &anyhow::Error) -> Option<&str> {
    if let Some(failure) = error.downcast_ref::<RemoteTaskFailure>() {
        Some(&failure.0)
    } else if error.downcast_ref::<PreparationError>().is_some() {
        Some("local_preparation_failed")
    } else {
        error
            .downcast_ref::<PeerNoteRejection>()
            .map(|r| r.reason.as_str())
    }
}

#[derive(Debug)]
struct LocalManagerFailure;
impl std::fmt::Display for LocalManagerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Local synchronization scan failed; pending notes remain queued")
    }
}
impl std::error::Error for LocalManagerFailure {}

#[derive(Debug)]
struct UnknownCommit;
impl std::fmt::Display for UnknownCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Durable outcome requires reconciliation")
    }
}
impl std::error::Error for UnknownCommit {}

#[derive(Debug)]
struct RemoteTaskFailure(String);
impl std::fmt::Display for RemoteTaskFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Remote synchronization task failed ({}); note remains pending",
            self.0
        )
    }
}
impl std::error::Error for RemoteTaskFailure {}

#[derive(Debug)]
struct PreparationError(String);
impl std::fmt::Display for PreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for PreparationError {}

fn field<'a>(payload: &'a Value, name: &str) -> Result<&'a str> {
    payload[name].as_str().context("invalid sync string field")
}
fn number(payload: &Value, name: &str) -> Result<u64> {
    payload[name].as_u64().context("invalid sync numeric field")
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../note-pipelines/tests/support/events.rs"]
mod commit_test_events;

#[cfg(test)]
#[path = "incremental_interop_test.rs"]
mod interop_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_client::{tests::certificates, DEFAULT_MAX_BLOB_BYTES};
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage_turso::TursoStorage;
    use tokio_rustls::TlsAcceptor;

    #[tokio::test]
    async fn scheduler_storage_failure_keeps_socket_pending_and_recovers_on_retry() {
        for failed_call in ["get_system_config", "list_sync_records"] {
            let dir = tempfile::tempdir().unwrap();
            let storage = Arc::new(TursoStorage::open(dir.path().join("db")).await.unwrap());
            let setup = Context::new(
                storage.clone(),
                Arc::new(StubEmbedder),
                Arc::new(FilesystemAttachmentStore::new(
                    dir.path().join("attachments"),
                )),
            );
            enable_test_notes(&setup).await;
            let config = note_pipelines::get_system_config(&setup)
                .await
                .unwrap()
                .sync;
            let backend = Arc::new(commit_test_events::EventStorageBackend::new(
                storage,
                commit_test_events::event_log(),
            ));
            let ctx = Context::new(
                backend.clone(),
                Arc::new(StubEmbedder),
                Arc::new(FilesystemAttachmentStore::new(
                    dir.path().join("attachments"),
                )),
            );
            let runtime = SyncRuntime::default();
            runtime.connected(uuid::Uuid::new_v4().to_string()).await;
            runtime
                .progress(|p| {
                    *p = SyncProgress::new("remote_verified");
                    p.upload.manager = Some(SyncManagerProgress {
                        state: Some("idle".into()),
                        error: None,
                        mode: "changes".into(),
                        page_size: 32,
                        pages_scanned: 0,
                        pages_checked: 0,
                        notes_scanned: 0,
                        scan_complete: true,
                        source_cursor: None,
                        last_progress_at: 0,
                    });
                })
                .await;
            let (socket, mut peer) = local_sockets().await;
            let mut client = local_session(&ctx, &runtime, socket);
            client.task_sync = true;
            client.socket.start_pump();
            backend.fail_next_repository_call(failed_call);
            assert_eq!(
                client.scheduled_work(&config, false).await.unwrap(),
                Some(false)
            );
            assert!(client.manager_failed);
            let status = runtime.status.read().await;
            assert_eq!(status.state, "connected");
            let upload = &status.progress.as_ref().unwrap().upload;
            assert_eq!(
                upload.manager.as_ref().unwrap().state.as_deref(),
                Some("failed")
            );
            assert_eq!(upload.active_task.as_ref().unwrap().state, "failed");
            drop(status);
            let failure = peer_frame(&mut peer).await;
            assert_eq!(failure.event, "sync_manager_status");
            assert_eq!(failure.payload["progress"]["state"], "failed");
            let pending_calls = backend.repository_call_count("list_sync_records");
            assert_eq!(
                client.scheduled_work(&config, false).await.unwrap(),
                Some(false)
            );
            assert_eq!(
                backend.repository_call_count("list_sync_records"),
                pending_calls
            );
            // A real pump heartbeat still completes on this exact socket.
            tokio::time::pause();
            let heartbeat = peer_frame(&mut peer).await;
            assert_eq!(heartbeat.event, "heartbeat");
            peer_reply(&mut peer, &heartbeat).await;
            tokio::task::yield_now().await;
            client.check_liveness().await.unwrap();
            tokio::time::resume();
            runtime.retry();
            let retry = runtime.wake.notified().now_or_never().is_some();
            assert!(retry);
            assert_eq!(
                client.scheduled_work(&config, retry).await.unwrap(),
                Some(true)
            );
            assert!(!client.manager_failed);
            assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 2);
            let worker = tokio::spawn(async move {
                let request = peer_frame(&mut peer).await;
                assert_eq!(request.join_ref.as_deref(), Some("local-session"));
                task_status_peer(&mut peer, &request, "succeeded", Some(json!({})), None).await;
                peer
            });
            client
                .task_request("inventory_complete", json!({}))
                .await
                .unwrap();
            let _peer = worker.await.unwrap();
            // A pause/config change still terminates scheduling while a manager is failed.
            client.manager_failed = true;
            let mut changed = config;
            changed.enabled = false;
            assert_eq!(client.scheduled_work(&changed, false).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn remote_manager_failure_is_visible_without_reconnecting_or_trusting_stale_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        runtime
            .progress(|p| *p = SyncProgress::new("remote_verified"))
            .await;
        client.remote_inventory_complete = true;
        let round = uuid::Uuid::new_v4().to_string();
        let payload = json!({"round_id":round,"status_seq":2,"progress":{"mode":"initial_scan","state":"failed","error":{"code":"source_failed","retryable":false},"page_size":32,"pages_scanned":1,"pages_checked":1,"notes_scanned":3,"scan_complete":false,"source_cursor":null,"last_progress_at":100,"checked_notes":3,"total_notes":null}});
        client
            .handle_task_frame(Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "sync_manager_status",
                payload.clone(),
            ))
            .await
            .unwrap();
        assert!(!client.remote_inventory_complete);
        let status = runtime.status.read().await;
        let manager = status
            .progress
            .as_ref()
            .unwrap()
            .download
            .manager
            .as_ref()
            .unwrap();
        assert_eq!(manager.state.as_deref(), Some("failed"));
        assert_eq!(
            manager.error.as_deref(),
            Some("Synchronization manager: source_failed")
        );
        drop(status);
        client
            .handle_task_frame(Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "sync_manager_status",
                payload.clone(),
            ))
            .await
            .unwrap();
        let mut stale = payload.clone();
        stale["status_seq"] = json!(1);
        stale["round_id"] = json!(uuid::Uuid::new_v4().to_string());
        client
            .handle_task_frame(Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "sync_manager_status",
                stale,
            ))
            .await
            .unwrap();
        assert_eq!(
            runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .download
                .run_id,
            round
        );
        let mut conflict = payload;
        conflict["progress"]["checked_notes"] = json!(2);
        assert!(client
            .handle_task_frame(Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "sync_manager_status",
                conflict
            ))
            .await
            .is_err());
    }
    #[tokio::test]
    async fn session_drop_cancels_disposable_work_before_runtime_drain() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let (receive, abort) =
            runtime.worker(async { std::future::pending::<TaskCompletion>().await });
        client.task_jobs.insert(
            "task".into(),
            TaskJob {
                operation: "state_query".into(),
                receive,
                abort,
                cancelled: false,
                durable: false,
                event: None,
            },
        );
        drop(client);
        timeout(Duration::from_secs(1), runtime.drain_workers())
            .await
            .expect("disposable worker cannot hold reconnect or shutdown open");
    }
    #[tokio::test(start_paused = true)]
    async fn missing_task_capability_keeps_one_healthy_socket_and_reports_upgrade_required() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        enable_test_notes(&ctx).await;
        let config = note_pipelines::get_system_config(&ctx).await.unwrap().sync;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut profile = certificates().profile;
        profile.mode = super::super::ConnectionMode::Direct;
        profile.id = "test".into();
        profile.url = format!(
            "ws://{}/note-sync/websocket?vsn=2.0.0",
            listener.local_addr().unwrap()
        );
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let join =
                Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(join.event, "phx_join");
            let hello = json!({"protocol_version":1,"node_id":uuid::Uuid::new_v4().to_string(),"selectors":join.payload["selectors"],"max_blob_bytes":DEFAULT_MAX_BLOB_BYTES,"capabilities":["state_query_v1"]});
            socket
                .send(Message::Text(
                    Frame::new(
                        join.join_ref.as_deref(),
                        join.reference.as_deref(),
                        TOPIC,
                        "phx_reply",
                        json!({"status":"ok","response":hello}),
                    )
                    .encode()
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            let negotiate =
                Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(negotiate.event, "sync_negotiate");
            socket
                .send(Message::Text(
                    Frame::new(
                        negotiate.join_ref.as_deref(),
                        negotiate.reference.as_deref(),
                        TOPIC,
                        "phx_reply",
                        json!({"status":"ok","response":negotiate.payload}),
                    )
                    .encode()
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            for _ in 0..2 {
                let heartbeat =
                    Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap())
                        .unwrap();
                assert_eq!(
                    heartbeat.event, "heartbeat",
                    "no inventory or business task may start without task capability"
                );
                socket
                    .send(Message::Text(
                        Frame::new(
                            None,
                            heartbeat.reference.as_deref(),
                            "phoenix",
                            "phx_reply",
                            json!({"status":"ok","response":{}}),
                        )
                        .encode()
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
        });
        let runtime = SyncRuntime::default();
        let session = session(&ctx, &profile, &config, &runtime);
        tokio::pin!(session);
        let socket = tokio::select! {result=&mut session=>panic!("healthy session ended: {result:?}"),result=peer=>result.unwrap()};
        let status = runtime.status(&ctx).await.unwrap();
        assert_eq!(status.state, "connected");
        assert!(status.last_error.unwrap().contains("upgrade required"));
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 2);
        drop(socket);
    }
    #[tokio::test]
    async fn rejection_cache_is_exact_bounded_and_survives_reconnection_until_retry() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let mut rejected = Vec::new();
        for _ in 0..80 {
            let mut event = incoming_event(&[]);
            event.note_id = uuid::Uuid::new_v4().to_string();
            event.event_id = uuid::Uuid::new_v4().to_string();
            let descriptor = event.acknowledgement();
            client.reject_descriptor(&descriptor).await.unwrap();
            runtime.block(&event, "operation_failed").await;
            rejected.push(descriptor);
        }
        assert_eq!(runtime.blocked.lock().unwrap().len(), 64);
        assert_eq!(
            runtime
                .rejected_total
                .load(std::sync::atomic::Ordering::Relaxed),
            80
        );
        drop(client);
        let (socket, _peer) = local_sockets().await;
        let mut replacement = local_session(&ctx, &runtime, socket);
        replacement.task_sync = true;
        for descriptor in &rejected {
            assert!(replacement.descriptor_blocked(descriptor).await.unwrap());
        }
        let mut changed = rejected[0].clone();
        changed.event_id = uuid::Uuid::new_v4().to_string();
        assert!(!replacement.descriptor_blocked(&changed).await.unwrap());
        assert_eq!(
            runtime
                .rejected_total
                .load(std::sync::atomic::Ordering::Relaxed),
            79
        );
        runtime.retry();
        for descriptor in rejected {
            assert!(!replacement.descriptor_blocked(&descriptor).await.unwrap());
        }
        assert_eq!(
            runtime
                .rejected_total
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
    #[tokio::test]
    async fn disk_progress_deduplicates_more_than_two_pages_and_a_fresh_round_counts_again() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        runtime
            .progress(|p| *p = SyncProgress::new("remote_verified"))
            .await;
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let ids: Vec<_> = (0..80).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        for id in &ids {
            client.resolve_upload(id, "completed").await.unwrap();
        }
        for id in &ids {
            client.resolve_upload(id, "completed").await.unwrap();
        }
        assert_eq!(
            runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .upload
                .notes_completed,
            80
        );
        runtime
            .progress(|p| p.upload = SyncDirectionProgress::new("remote_verified"))
            .await;
        client.resolve_upload(&ids[0], "completed").await.unwrap();
        assert_eq!(
            runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .upload
                .notes_completed,
            1
        );
    }
    async fn enable_test_notes(ctx: &Context) -> Vec<SyncEvent> {
        use note_core::{
            sync::{SyncConfig, SyncRule},
            SystemConfig,
        };
        note_pipelines::update_system_config(
            ctx,
            &SystemConfig {
                sync: SyncConfig {
                    enabled: true,
                    connection_profile: Some("test".into()),
                    rules: vec![SyncRule {
                        id: "test".into(),
                        enabled: true,
                        selector: "project=bear_note".into(),
                    }],
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sync::initialize_sync(ctx).await.unwrap();
        for title in ["First", "Second"] {
            note_pipelines::save_note(
                ctx,
                note_pipelines::SaveNoteInput {
                    title: title.into(),
                    content: format!("{title} note"),
                    labels: vec![("project".into(), "bear_note".into())],
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
        }
        sync::sync_inventory(ctx, None, 32).await.unwrap().events
    }

    #[tokio::test]
    async fn legacy_round_delivers_pending_before_acknowledged_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let events = enable_test_notes(&ctx).await;
        sync::acknowledge_sync_event(&ctx, &events[0].acknowledgement())
            .await
            .unwrap();
        let pending_id = events[1].note_id.clone();
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let worker = tokio::spawn(async move {
            let mut delivered = Vec::new();
            let mut metadata = Vec::new();
            loop {
                let frame = peer_frame(&mut peer).await;
                match frame.event.as_str() {
                    "transfer_begin" => {
                        metadata.clear();
                        peer_reply(&mut peer, &frame).await;
                    }
                    "blob_chunk" => metadata.extend(
                        BASE64
                            .decode(frame.payload["data"].as_str().unwrap())
                            .unwrap(),
                    ),
                    "blob_end" => peer_reply(&mut peer, &frame).await,
                    "transfer_commit" => {
                        let event: SyncEvent = serde_json::from_slice(&metadata).unwrap();
                        delivered.push(event.note_id.clone());
                        let reply = Frame::new(
                            frame.join_ref.as_deref(),
                            frame.reference.as_deref(),
                            TOPIC,
                            "phx_reply",
                            json!({"status":"ok","response":event.acknowledgement()}),
                        );
                        peer.send(Message::Text(reply.encode().unwrap().into()))
                            .await
                            .unwrap();
                    }
                    "inventory_complete" => {
                        assert_eq!(frame.payload, json!({}));
                        peer_reply(&mut peer, &frame).await;
                        break;
                    }
                    event => panic!("unexpected legacy event {event}"),
                }
            }
            assert_eq!(delivered.len(), 2);
            assert_eq!(delivered[0], pending_id);
        });
        client.reconcile_round(true).await.unwrap();
        worker.await.unwrap();
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 0);
        let status = runtime.status.read().await;
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_completed, 2);
        assert_eq!(upload.notes_remaining, Some(0));
        assert_eq!(status.progress.as_ref().unwrap().tracked_notes, Some(2));
    }

    #[tokio::test]
    async fn remote_task_rejection_details_survive_the_next_note_on_the_same_socket() {
        for (code, task_error) in [("catalog_conflict", false), ("validation_failed", true)] {
            let dir = tempfile::tempdir().unwrap();
            let ctx = local_context(dir.path()).await;
            enable_test_notes(&ctx).await;
            let runtime = SyncRuntime::default();
            runtime.connected(uuid::Uuid::new_v4().to_string()).await;
            let (socket, mut peer) = local_sockets().await;
            let mut client = local_session(&ctx, &runtime, socket);
            client.task_sync = true;
            client.state_query = true;
            let worker = tokio::spawn(async move {
                let mut metadata = Vec::new();
                let mut event = None;
                let mut rejected = None;
                let mut completed = 0;
                loop {
                    let request = peer_frame(&mut peer).await;
                    assert_eq!(request.join_ref.as_deref(), Some("local-session"));
                    if request.event == "sync_manager_status" {
                        continue;
                    }
                    assert_eq!(request.event, "task_submit");
                    let payload = &request.payload["payload"];
                    let operation = request.payload["operation"].as_str().unwrap();
                    let result = match operation {
                        "state_query" => json!({
                            "query_id":payload["query_id"],
                            "items":payload["items"].as_array().unwrap().iter().map(|item|json!({
                                "requested":item,"association":"attached","member":item,
                                "head":{"version":item["version"],"snapshot_hash":item["snapshot_hash"],"attachments_available":true}
                            })).collect::<Vec<_>>()
                        }),
                        "transfer_begin" => {
                            metadata.clear();
                            json!({})
                        }
                        "blob_chunk" => {
                            let bytes = BASE64.decode(payload["data"].as_str().unwrap()).unwrap();
                            metadata.extend_from_slice(&bytes);
                            json!({"transfer_id":payload["transfer_id"],"blob_id":payload["blob_id"],
                                "received_bytes":payload["offset"].as_u64().unwrap()+bytes.len() as u64})
                        }
                        "blob_end" => {
                            event = Some(serde_json::from_slice::<SyncEvent>(&metadata).unwrap());
                            json!({})
                        }
                        "transfer_commit" if rejected.is_none() => {
                            let failed = event.as_ref().unwrap();
                            rejected = Some((
                                failed.note_id.clone(),
                                failed.event_id.clone(),
                                request.payload["task_id"].as_str().unwrap().to_owned(),
                            ));
                            if task_error {
                                let frame = Frame::new(
                                    request.join_ref.as_deref(),
                                    None,
                                    TOPIC,
                                    "task_error",
                                    json!({"task_id":request.payload["task_id"],"code":code}),
                                );
                                peer.send(Message::Text(frame.encode().unwrap().into()))
                                    .await
                                    .unwrap();
                            } else {
                                task_status_peer(&mut peer, &request, "failed", None, Some(code))
                                    .await;
                            }
                            continue;
                        }
                        "transfer_commit" => {
                            completed += 1;
                            serde_json::to_value(event.as_ref().unwrap().acknowledgement()).unwrap()
                        }
                        "transfer_abort" => json!({}),
                        "inventory_complete" => {
                            assert_eq!(completed, 1);
                            task_status_peer(
                                &mut peer,
                                &request,
                                "succeeded",
                                Some(json!({})),
                                None,
                            )
                            .await;
                            return rejected.unwrap();
                        }
                        _ => panic!("unexpected operation {operation}"),
                    };
                    task_status_peer(&mut peer, &request, "succeeded", Some(result), None).await;
                }
            });
            client.reconcile_round(true).await.unwrap();
            let (note_id, event_id, task_id) = worker.await.unwrap();
            let status = runtime.status(&ctx).await.unwrap();
            assert_eq!(status.state, "connected");
            assert_eq!(status.pending_notes, 1);
            assert_eq!(status.blocked_notes.len(), 1);
            assert_eq!(status.blocked_notes[0].note_id, note_id);
            assert_eq!(status.blocked_notes[0].event_id, event_id);
            assert_eq!(status.blocked_notes[0].reason, code);
            assert!(status.last_error.as_ref().unwrap().contains(code));
            assert!(
                status.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "sync_task_failed"
                        && diagnostic.message.contains(code)
                        && diagnostic.message.contains("transfer_commit")
                        && diagnostic.message.contains(&task_id)
                }),
                "the original failure must remain diagnostic after cleanup and the next note"
            );
            let upload = &status.progress.as_ref().unwrap().upload;
            assert_eq!(upload.active_task.as_ref().unwrap().state, "succeeded");
            assert_eq!(upload.notes_completed, 1);
            assert_eq!(upload.blocked_notes, 1);
            assert!(!client.has_pending_work().await.unwrap());
            runtime.retry();
            assert!(client.has_pending_work().await.unwrap());
        }
    }

    #[tokio::test]
    async fn a_rejected_note_keeps_pending_and_yields_to_the_next_note_on_the_same_socket() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        enable_test_notes(&ctx).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let worker = tokio::spawn(async move {
            let mut rejected = None;
            for index in 0..2 {
                let begin = peer_frame(&mut peer).await;
                assert_eq!(begin.event, "transfer_begin");
                peer_reply(&mut peer, &begin).await;
                let mut metadata = Vec::new();
                loop {
                    let frame = peer_frame(&mut peer).await;
                    if frame.event == "blob_end" {
                        peer_reply(&mut peer, &frame).await;
                        break;
                    }
                    assert_eq!(frame.event, "blob_chunk");
                    metadata.extend(
                        BASE64
                            .decode(frame.payload["data"].as_str().unwrap())
                            .unwrap(),
                    );
                }
                let event: SyncEvent = serde_json::from_slice(&metadata).unwrap();
                let commit = peer_frame(&mut peer).await;
                assert_eq!(commit.event, "transfer_commit");
                if index == 0 {
                    rejected = Some(event.note_id.clone());
                    let reply = Frame::new(
                        commit.join_ref.as_deref(),
                        commit.reference.as_deref(),
                        TOPIC,
                        "phx_reply",
                        json!({"status":"error","response":{"code":"note_rejected","event_id":event.event_id,"note_id":event.note_id,"reason":"catalog_conflict"}}),
                    );
                    peer.send(Message::Text(reply.encode().unwrap().into()))
                        .await
                        .unwrap();
                } else {
                    peer_result(
                        &mut peer,
                        &commit,
                        serde_json::to_value(event.acknowledgement()).unwrap(),
                    )
                    .await;
                }
            }
            let complete = peer_frame(&mut peer).await;
            assert_eq!(complete.event, "inventory_complete");
            peer_reply(&mut peer, &complete).await;
            rejected.unwrap()
        });
        client.reconcile_round(true).await.unwrap();
        let rejected = worker.await.unwrap();
        let status = runtime.status(&ctx).await.unwrap();
        assert_eq!(status.pending_notes, 1);
        assert_eq!(status.blocked_notes[0].note_id, rejected);
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_completed, 1);
        assert_eq!(upload.notes_remaining, Some(1));
        assert_eq!(upload.blocked_notes, 1);
        assert_eq!(upload.phase, "blocked");
        assert!(!client.has_pending_work().await.unwrap());
        runtime.retry();
        assert!(client.has_pending_work().await.unwrap());
    }

    #[tokio::test]
    async fn negotiated_round_skips_unchanged_snapshots_without_blob_transfers() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        for event in enable_test_notes(&ctx).await {
            sync::acknowledge_sync_event(&ctx, &event.acknowledgement())
                .await
                .unwrap();
        }
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        let worker = tokio::spawn(async move {
            let query = peer_frame(&mut peer).await;
            assert_eq!(query.event, "state_query");
            let response = json!({"query_id":query.payload["query_id"],"items":query.payload["items"].as_array().unwrap().iter().map(|item|json!({"requested":item,"association":"attached","member":item,"head":{"version":item["version"],"snapshot_hash":item["snapshot_hash"],"attachments_available":true}})).collect::<Vec<_>>()});
            let reply = Frame::new(
                query.join_ref.as_deref(),
                query.reference.as_deref(),
                TOPIC,
                "phx_reply",
                json!({"status":"ok","response":response}),
            );
            peer.send(Message::Text(reply.encode().unwrap().into()))
                .await
                .unwrap();
            let complete = peer_frame(&mut peer).await;
            assert_eq!(complete.event, "inventory_complete");
            assert_eq!(complete.payload["total_notes"], 2);
            peer_reply(&mut peer, &complete).await;
        });
        client.reconcile_round(true).await.unwrap();
        worker.await.unwrap();
        let status = runtime.status.read().await;
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_equal, 2);
        assert_eq!(upload.notes_completed, 0);
        assert_eq!(upload.notes_remaining, Some(0));
    }

    fn equal_response(query: &Frame) -> Value {
        json!({"query_id":query.payload["query_id"],"items":query.payload["items"].as_array().unwrap().iter().map(|item|json!({"requested":item,"association":"attached","member":item,"head":{"version":item["version"],"snapshot_hash":item["snapshot_hash"],"attachments_available":true}})).collect::<Vec<_>>()})
    }

    async fn peer_result(
        peer: &mut WebSocketStream<tokio::io::DuplexStream>,
        request: &Frame,
        response: Value,
    ) {
        let reply = Frame::new(
            request.join_ref.as_deref(),
            request.reference.as_deref(),
            TOPIC,
            "phx_reply",
            json!({"status":"ok","response":response}),
        );
        peer.send(Message::Text(reply.encode().unwrap().into()))
            .await
            .unwrap();
    }

    async fn task_status_peer(
        peer: &mut WebSocketStream<tokio::io::DuplexStream>,
        request: &Frame,
        state: &str,
        result: Option<Value>,
        error: Option<&str>,
    ) {
        let status = TaskStatus {
            task_id: request.payload["task_id"].as_str().unwrap().into(),
            status_seq: if state == "queued" { 1 } else { 2 },
            state: state.into(),
            phase: request.payload["operation"]
                .as_str()
                .unwrap_or("working")
                .into(),
            progress: json!({}),
            result,
            error: error.map(|code| tasks::TaskError {
                code: code.into(),
                retryable: false,
            }),
        };
        let frame = Frame::new(
            request.join_ref.as_deref(),
            None,
            TOPIC,
            "task_status",
            serde_json::to_value(status).unwrap(),
        );
        peer.send(Message::Text(frame.encode().unwrap().into()))
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn negotiated_tasks_have_no_business_deadline_while_heartbeat_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        client.socket.start_pump();
        tokio::time::pause();
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            assert_eq!(request.event, "task_submit");
            task_status_peer(&mut peer, &request, "queued", None, None).await;
            let mut beats = 0;
            while beats < 33 {
                let frame = peer_frame(&mut peer).await;
                if frame.event == "heartbeat" {
                    beats += 1;
                    peer_reply(&mut peer, &frame).await;
                } else {
                    assert_eq!(frame.event, "task_get");
                }
            }
            task_status_peer(
                &mut peer,
                &request,
                "succeeded",
                Some(json!({"finished":true})),
                None,
            )
            .await;
            // Keep the transport alive until the result has reached its consumer.
            peer
        });
        let started = Instant::now();
        assert_eq!(
            client.task_request("state_query", json!({})).await.unwrap(),
            json!({"finished":true})
        );
        assert!(Instant::now() - started > Duration::from_secs(930));
        let _peer = worker.await.unwrap();
        tokio::time::resume();
    }
    #[tokio::test]
    async fn join_control_waits_for_reply_without_business_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.socket.start_pump();
        tokio::time::pause();
        let worker = tokio::spawn(async move {
            for operation in ["phx_join", "sync_negotiate"] {
                let request = peer_frame(&mut peer).await;
                assert_eq!(request.event, operation);
                let beat = peer_frame(&mut peer).await;
                assert_eq!(beat.event, "heartbeat");
                peer_reply(&mut peer, &beat).await;
                peer_reply(&mut peer, &request).await;
            }
            peer
        });
        client.request("phx_join", json!({})).await.unwrap();
        client.request("sync_negotiate", json!({})).await.unwrap();
        let _peer = worker.await.unwrap();
        tokio::time::resume();
    }
    #[tokio::test]
    async fn failed_task_does_not_reconnect_and_the_next_task_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let worker = tokio::spawn(async move {
            let first = peer_frame(&mut peer).await;
            task_status_peer(&mut peer, &first, "failed", None, Some("validation_failed")).await;
            let second = peer_frame(&mut peer).await;
            assert_eq!(first.join_ref, second.join_ref);
            task_status_peer(&mut peer, &second, "succeeded", Some(json!({})), None).await;
            peer
        });
        let error = client
            .task_request("transfer_begin", json!({}))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<RemoteTaskFailure>().is_some());
        assert_eq!(
            client
                .task_request("inventory_complete", json!({}))
                .await
                .unwrap(),
            json!({})
        );
        let _peer = worker.await.unwrap();
    }
    #[tokio::test]
    async fn chunk_progress_advances_only_after_exact_receiver_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = Arc::new(SyncRuntime::default());
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let event = incoming_event(&[(CHUNK + 17) as u64]);
        runtime.progress(|p| p.upload.begin_note(&event)).await;
        let observed = runtime.clone();
        let worker = tokio::spawn(async move {
            for expected in [0, CHUNK] {
                let request = peer_frame(&mut peer).await;
                assert_eq!(request.payload["operation"], "blob_chunk");
                assert_eq!(request.payload["payload"]["offset"], expected);
                let state = observed.status.read().await;
                assert_eq!(
                    state
                        .progress
                        .as_ref()
                        .unwrap()
                        .upload
                        .current_note
                        .as_ref()
                        .unwrap()
                        .attachment_bytes_done,
                    expected as u64
                );
                drop(state);
                task_status_peer(&mut peer, &request, "queued", None, None).await;
                // Stop-and-wait means no second chunk can exist before our receipt.
                assert!(timeout(Duration::from_millis(10), peer.next())
                    .await
                    .is_err());
                let payload = &request.payload["payload"];
                let length = BASE64
                    .decode(payload["data"].as_str().unwrap())
                    .unwrap()
                    .len();
                task_status_peer(&mut peer, &request, "succeeded", Some(json!({"transfer_id":payload["transfer_id"],"blob_id":payload["blob_id"],"received_bytes":expected+length})), None).await;
            }
            let end = peer_frame(&mut peer).await;
            assert_eq!(end.payload["operation"], "blob_end");
            task_status_peer(&mut peer, &end, "succeeded", Some(json!({})), None).await;
            peer
        });
        client
            .send_blob("test-transfer", "attachment:file", &vec![42; CHUNK + 17])
            .await
            .unwrap();
        assert_eq!(
            runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .upload
                .current_note
                .as_ref()
                .unwrap()
                .attachment_bytes_done,
            (CHUNK + 17) as u64
        );
        let _peer = worker.await.unwrap();
    }
    #[tokio::test]
    async fn durable_commit_errors_reconcile_exact_receipts_and_quarantine_unknown_outcomes() {
        use note_storage::{
            BackendInfo, StorageBackend, StorageResult, StorageSession, StorageTransaction,
            TransactionMode,
        };
        struct FailApplyReply {
            inner: Arc<commit_test_events::EventStorageBackend>,
            armed: std::sync::atomic::AtomicBool,
            receipt_persisted: bool,
        }
        #[async_trait::async_trait]
        impl StorageBackend for FailApplyReply {
            async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
                self.inner.session().await
            }
            async fn begin(
                &self,
                mode: TransactionMode,
            ) -> StorageResult<Box<dyn StorageTransaction>> {
                if mode == TransactionMode::Immediate
                    && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    if self.receipt_persisted {
                        self.inner.fail_next_commit_acknowledgement();
                    } else {
                        self.inner.fail_next_commit();
                    }
                }
                self.inner.begin(mode).await
            }
            async fn info(&self) -> StorageResult<BackendInfo> {
                self.inner.info().await
            }
        }
        for kind in [SyncEventKind::Attach, SyncEventKind::Fence] {
            for receipt_persisted in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let log = commit_test_events::event_log();
                let storage = Arc::new(TursoStorage::open(dir.path().join("db")).await.unwrap());
                let attachments = Arc::new(FilesystemAttachmentStore::new(
                    dir.path().join("attachments"),
                ));
                let setup =
                    Context::new(storage.clone(), Arc::new(StubEmbedder), attachments.clone());
                let mut event = enable_test_notes(&setup).await.remove(0);
                let backend = Arc::new(FailApplyReply {
                    inner: Arc::new(commit_test_events::EventStorageBackend::new(
                        storage,
                        log.clone(),
                    )),
                    armed: std::sync::atomic::AtomicBool::new(false),
                    receipt_persisted,
                });
                let ctx = Context::new(backend.clone(), Arc::new(StubEmbedder), attachments);
                event.note_id = uuid::Uuid::new_v4().to_string();
                event.event_id = uuid::Uuid::new_v4().to_string();
                let mut stamp = event.version.clone();
                stamp.millis += 1;
                stamp.node_id = uuid::Uuid::new_v4().to_string();
                event.generation = stamp.clone();
                event.control = stamp.clone();
                event.version = stamp;
                event.kind = kind;
                if kind == SyncEventKind::Fence {
                    event.snapshot = None;
                    event.control.counter += 1;
                }
                sync::validate_sync_event(&event, &HashMap::new()).unwrap();
                // Both apply paths must identify COMMIT uncertainty, even when a later
                // exact-receipt query cannot establish whether the commit persisted.
                let mut probe = event.clone();
                probe.note_id = uuid::Uuid::new_v4().to_string();
                probe.event_id = uuid::Uuid::new_v4().to_string();
                backend
                    .armed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let error = sync::apply_sync_event(&ctx, &probe, &HashMap::new())
                    .await
                    .unwrap_err();
                assert!(
                    error
                        .downcast_ref::<sync::SyncCommitOutcomeUnknown>()
                        .is_some(),
                    "{kind:?}: COMMIT error must preserve its uncertain-outcome marker: {error:?}"
                );
                assert_eq!(
                    sync::sync_received_event(&ctx, &probe).await.unwrap(),
                    receipt_persisted
                );
                let runtime = SyncRuntime::default();
                let (socket, _peer) = local_sockets().await;
                let mut client = local_session(&ctx, &runtime, socket);
                client.task_sync = true;
                let transfer = uuid::Uuid::new_v4().to_string();
                client.incoming = Some(Incoming {
                    id: transfer.clone(),
                    metadata: StagedBlob::new(0, &format!("{:x}", Sha256::digest([])), 100)
                        .unwrap(),
                    event: Some(event.clone()),
                    blobs: HashMap::new(),
                    idle_deadline: Instant::now() + DEADLINE,
                });
                backend
                    .armed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let id = format!("{}:1", uuid::Uuid::nil());
                client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":id,"operation":"transfer_commit","payload":{"transfer_id":transfer}}))).await.unwrap();
                timeout(Duration::from_secs(10), async {
                    while !client.task_jobs.is_empty() {
                        tokio::task::yield_now().await;
                        client.poll_task_jobs().await.unwrap();
                    }
                })
                .await
                .unwrap();
                let failure = if receipt_persisted {
                    "commit_ack_failed"
                } else {
                    "commit_failed"
                };
                assert_eq!(
                    log.lock()
                        .unwrap()
                        .iter()
                        .filter(|entry| entry.as_str() == failure)
                        .count(),
                    2,
                    "{kind:?}: fixture must exercise both the direct and runtime COMMIT failures"
                );
                let status = client.received_tasks.get(&id).unwrap();
                if receipt_persisted {
                    assert_eq!(status.state, "succeeded", "{kind:?}");
                    assert_eq!(
                        status.result,
                        Some(serde_json::to_value(event.acknowledgement()).unwrap())
                    );
                    assert!(status.error.is_none());
                } else {
                    assert!(
                        !status.terminal(),
                        "{kind:?}: missing receipt cannot settle COMMIT uncertainty"
                    );
                    assert_eq!(status.phase, "outcome_unknown");
                    assert!(status.result.is_none());
                    assert!(client.received_tasks.active_transfer().is_some());
                    assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 2);
                }
                status.validate().unwrap();
                assert_eq!(
                    sync::sync_received_event(&ctx, &event).await.unwrap(),
                    receipt_persisted
                );
                assert_eq!(
                    note_pipelines::get_note(&ctx, &event.note_id)
                        .await
                        .unwrap()
                        .is_some(),
                    receipt_persisted && kind == SyncEventKind::Attach
                );
                runtime.drain_workers().await;
            }
        }
    }

    #[tokio::test]
    async fn preapply_event_validation_rejection_fails_without_uncertain_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let mut event = enable_test_notes(&ctx).await.remove(0);
        event.snapshot_hash = "0".repeat(64);
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let transfer = uuid::Uuid::new_v4().to_string();
        client.incoming = Some(Incoming {
            id: transfer.clone(),
            metadata: StagedBlob::new(0, &format!("{:x}", Sha256::digest([])), 100).unwrap(),
            event: Some(event),
            blobs: HashMap::new(),
            idle_deadline: Instant::now() + DEADLINE,
        });
        let id = format!("{}:1", uuid::Uuid::nil());
        client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":id,"operation":"transfer_commit","payload":{"transfer_id":transfer}}))).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        let status = client.received_tasks.get(&id).unwrap();
        assert_eq!(status.state, "failed");
        assert_eq!(status.error.unwrap().code, "validation_failed");
        assert!(status.result.is_none());
        runtime.drain_workers().await;
    }

    #[tokio::test]
    async fn preapply_wrong_transfer_id_fails_even_when_the_staged_event_has_a_durable_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let mut event = enable_test_notes(&ctx).await.remove(0);
        event.note_id = uuid::Uuid::new_v4().to_string();
        event.event_id = uuid::Uuid::new_v4().to_string();
        let mut stamp = event.version.clone();
        stamp.millis += 1;
        stamp.node_id = uuid::Uuid::new_v4().to_string();
        event.generation = stamp.clone();
        event.control = stamp.clone();
        event.version = stamp;
        sync::apply_sync_event(&ctx, &event, &HashMap::new())
            .await
            .unwrap();
        assert!(sync::sync_received_event(&ctx, &event).await.unwrap());
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        client.incoming = Some(Incoming {
            id: uuid::Uuid::new_v4().to_string(),
            metadata: StagedBlob::new(0, &format!("{:x}", Sha256::digest([])), 100).unwrap(),
            event: Some(event),
            blobs: HashMap::new(),
            idle_deadline: Instant::now() + DEADLINE,
        });
        let id = format!("{}:1", uuid::Uuid::nil());
        client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":id,"operation":"transfer_commit","payload":{"transfer_id":uuid::Uuid::new_v4().to_string()}}))).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        let status = client.received_tasks.get(&id).unwrap();
        assert_eq!(status.state, "failed");
        assert!(status.result.is_none());
        assert_eq!(status.error.unwrap().code, "operation_failed");
        runtime.drain_workers().await;
    }

    #[tokio::test]
    async fn confirmed_rollback_without_receipt_fails_and_releases_transfer_slot() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let events = enable_test_notes(&ctx).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let id = format!("{}:1", uuid::Uuid::nil());
        let submit = Submission {
            task_id: id.clone(),
            operation: "transfer_commit".into(),
            payload: json!({"transfer_id":uuid::Uuid::new_v4().to_string()}),
        };
        assert!(matches!(
            client.received_tasks.admit(&submit),
            Ok(Admission::Start(_))
        ));
        let (receive, abort) = runtime.worker(async {
            TaskCompletion {
                worker: None,
                result: Err(ApplyAttemptError {
                    source: note_storage::StorageError::new(
                        note_storage::StorageErrorKind::Unavailable,
                        "operation failed before commit; transaction rolled back",
                    )
                    .into(),
                }
                .into()),
            }
        });
        client.task_jobs.insert(
            id.clone(),
            TaskJob {
                operation: "transfer_commit".into(),
                receive,
                abort,
                cancelled: false,
                durable: true,
                event: Some(events[0].clone()),
            },
        );
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        let status = client.received_tasks.get(&id).unwrap();
        assert_eq!(status.state, "failed");
        assert_eq!(status.error.as_ref().unwrap().code, "operation_failed");
        assert!(status.result.is_none());
        assert!(client.received_tasks.active_transfer().is_none());
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 2);
        client
            .receive(
                "transfer_begin",
                json!({
                    "transfer_id":uuid::Uuid::new_v4().to_string(), "size_bytes":0,
                    "checksum_sha256":format!("{:x}",Sha256::digest([])),
                }),
            )
            .await
            .unwrap();
        runtime.drain_workers().await;
    }

    #[tokio::test]
    async fn uncertain_commit_without_receipt_and_its_abort_remain_nonterminal() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let events = enable_test_notes(&ctx).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let id = format!("{}:1", uuid::Uuid::nil());
        let transfer = uuid::Uuid::new_v4().to_string();
        let submit = Submission {
            task_id: id.clone(),
            operation: "transfer_commit".into(),
            payload: json!({"transfer_id":transfer}),
        };
        assert!(matches!(
            client.received_tasks.admit(&submit),
            Ok(Admission::Start(_))
        ));
        let (receive, abort) = runtime.worker(async {
            TaskCompletion {
                worker: None,
                result: Err(ApplyAttemptError {
                    source: sync::SyncCommitOutcomeUnknown(note_storage::StorageError::new(
                        note_storage::StorageErrorKind::Unavailable,
                        "uncertain commit acknowledgement",
                    ))
                    .into(),
                }
                .into()),
            }
        });
        client.task_jobs.insert(
            id.clone(),
            TaskJob {
                operation: "transfer_commit".into(),
                receive,
                abort,
                cancelled: false,
                durable: true,
                event: Some(events[0].clone()),
            },
        );
        timeout(Duration::from_secs(3), async {
            while !client.task_jobs.is_empty() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        let status = client.received_tasks.get(&id).unwrap();
        assert!(!status.terminal());
        assert_eq!(status.phase, "outcome_unknown");
        assert!(status.result.is_none());
        let abort_id = format!("{}:2", uuid::Uuid::nil());
        client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":abort_id,"operation":"transfer_abort","payload":{"transfer_id":transfer}}))).await.unwrap();
        client.poll_task_jobs().await.unwrap();
        assert!(
            !client.received_tasks.get(&abort_id).unwrap().terminal(),
            "cleanup cannot settle an unresolved durable commit"
        );
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 2);
        runtime.drain_workers().await;
    }
    #[tokio::test]
    async fn transfer_abort_waits_for_delayed_chunk_then_cleans_staging_before_next_note() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let transfer = uuid::Uuid::new_v4().to_string();
        client.receive("transfer_begin",json!({"transfer_id":transfer,"size_bytes":1,"checksum_sha256":format!("{:x}",Sha256::digest(b"a"))})).await.unwrap();
        let submission = Submission {
            task_id: format!("{}:1", uuid::Uuid::nil()),
            operation: "blob_chunk".into(),
            payload: json!({"transfer_id":transfer,"blob_id":"event","offset":0,"data":"YQ=="}),
        };
        assert!(matches!(
            client.received_tasks.admit(&submission),
            Ok(Admission::Start(_))
        ));
        let mut staged = client.transfer_worker();
        let release = Arc::new(Notify::new());
        let delayed = release.clone();
        let payload = submission.payload;
        let (receive, abort) = runtime.worker(async move {
            delayed.notified().await;
            let result = staged.receive("blob_chunk", payload).await;
            TaskCompletion {
                worker: Some(staged),
                result,
            }
        });
        client.task_jobs.insert(
            submission.task_id.clone(),
            TaskJob {
                operation: "blob_chunk".into(),
                receive,
                abort,
                cancelled: false,
                durable: false,
                event: None,
            },
        );
        let abort_id = format!("{}:2", uuid::Uuid::nil());
        client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":abort_id,"operation":"transfer_abort","payload":{"transfer_id":transfer}}))).await.unwrap();
        for _ in 0..3 {
            tokio::task::yield_now().await;
            client.poll_task_jobs().await.unwrap();
        }
        assert!(
            !client.received_tasks.get(&abort_id).unwrap().terminal(),
            "abort cannot claim cleanup while the chunk worker owns staging"
        );
        release.notify_one();
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&abort_id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.received_tasks.get(&abort_id).unwrap().state,
            "succeeded"
        );
        assert!(
            client.incoming.is_none(),
            "the completed old chunk cannot restore staging after abort"
        );
        let next_id = format!("{}:3", uuid::Uuid::nil());
        client.handle_task_frame(Frame::new(Some("local-session"),None,TOPIC,"task_submit",json!({"task_id":next_id,"operation":"transfer_begin","payload":{"transfer_id":uuid::Uuid::new_v4().to_string(),"size_bytes":0,"checksum_sha256":format!("{:x}",Sha256::digest([]))}}))).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&next_id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.received_tasks.get(&next_id).unwrap().state,
            "succeeded"
        );
        runtime.drain_workers().await;
    }
    #[tokio::test]
    async fn cancelling_disposable_task_emits_canonical_cancelled_wire_status() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.task_sync = true;
        let id = format!("{}:1", uuid::Uuid::nil());
        let submit = Submission {
            task_id: id.clone(),
            operation: "state_query".into(),
            payload: json!({}),
        };
        assert!(matches!(
            client.received_tasks.admit(&submit),
            Ok(Admission::Start(_))
        ));
        let (receive, abort) =
            runtime.worker(async { std::future::pending::<TaskCompletion>().await });
        client.task_jobs.insert(
            id.clone(),
            TaskJob {
                operation: "state_query".into(),
                receive,
                abort,
                cancelled: false,
                durable: false,
                event: None,
            },
        );
        client
            .handle_task_frame(Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "task_cancel",
                json!({"task_id":id}),
            ))
            .await
            .unwrap();
        timeout(Duration::from_secs(3), async {
            while !client.received_tasks.get(&id).unwrap().terminal() {
                tokio::task::yield_now().await;
                client.poll_task_jobs().await.unwrap();
            }
        })
        .await
        .unwrap();
        let pending = peer_frame(&mut peer).await;
        assert_eq!(pending.payload["phase"], "cancellation_pending");
        let terminal = peer_frame(&mut peer).await;
        assert_eq!(terminal.payload["state"], "cancelled");
        assert_eq!(terminal.payload["result"], Value::Null);
        assert_eq!(terminal.payload["error"], Value::Null);
        let status: TaskStatus = serde_json::from_value(terminal.payload).unwrap();
        status.validate().unwrap();
        runtime.drain_workers().await;
    }

    #[tokio::test]
    async fn task_submission_is_admitted_and_chunk_failure_is_local() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        client.task_sync = true;
        let submit = Frame::new(
            Some("local-session"),
            None,
            TOPIC,
            "task_submit",
            json!({
                "task_id":format!("{}:1", uuid::Uuid::nil()),
                "operation":"blob_chunk",
                "payload":{"transfer_id":uuid::Uuid::new_v4().to_string(),"blob_id":"event","offset":0,"data":"YQ=="}
            }),
        );
        let result = client
            .handle_message(Some(Ok(Message::Text(submit.encode().unwrap().into()))))
            .await;
        assert!(
            result.is_ok(),
            "task failure must preserve the socket: {result:?}"
        );
    }

    #[tokio::test]
    async fn mutual_queries_are_serviced_before_inventory_and_query_replies() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let descriptor = enable_test_notes(&ctx).await[0].acknowledgement();
        let expected = descriptor.clone();
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        let worker = tokio::spawn(async move {
            for event in ["inventory_start", "state_query"] {
                let request = peer_frame(&mut peer).await;
                assert_eq!(request.event, event);
                let query = SyncStateQuery {
                    query_id: uuid::Uuid::new_v4().to_string(),
                    items: vec![expected.clone()],
                };
                let incoming = Frame::new(
                    request.join_ref.as_deref(),
                    Some("peer-query"),
                    TOPIC,
                    "state_query",
                    serde_json::to_value(&query).unwrap(),
                );
                peer.send(Message::Text(incoming.encode().unwrap().into()))
                    .await
                    .unwrap();
                // Withhold our reply until the opposite-direction query succeeds.
                let answered = peer_frame(&mut peer).await;
                assert_eq!(answered.event, "phx_reply");
                assert_eq!(answered.reference.as_deref(), Some("peer-query"));
                let response: SyncStateResponse =
                    serde_json::from_value(answered.payload["response"].clone()).unwrap();
                assert!(response.matches_query(&query));
                assert_eq!(response.items[0].member.as_ref(), Some(&expected));
                let response = if event == "state_query" {
                    equal_response(&request)
                } else {
                    json!({})
                };
                peer_result(&mut peer, &request, response).await;
            }
        });
        timeout(Duration::from_secs(3), async {
            client.request("inventory_start", json!({})).await.unwrap();
            client.query_state(vec![descriptor]).await.unwrap();
            worker.await.unwrap();
        })
        .await
        .expect("duplex state queries deadlocked");
    }

    #[tokio::test]
    async fn state_queries_reject_unnegotiated_malformed_and_overlapping_requests() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let descriptor = enable_test_notes(&ctx).await[0].acknowledgement();
        let query = json!({"query_id":uuid::Uuid::new_v4().to_string(),"items":[descriptor]});
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let message = |payload: Value| {
            Some(Ok(Message::Text(
                Frame::new(
                    Some("local-session"),
                    Some("query"),
                    TOPIC,
                    "state_query",
                    payload,
                )
                .encode()
                .unwrap()
                .into(),
            )))
        };
        assert!(client.handle_message(message(query.clone())).await.is_err());
        client.state_query = true;
        for payload in [
            json!({"query_id":"invalid","items":query["items"]}),
            json!({"query_id":query["query_id"],"items":[]}),
            json!({"query_id":query["query_id"],"items":query["items"],"extra":true}),
            json!({"query_id":query["query_id"],"items":vec![descriptor;33]}),
        ] {
            assert!(client.handle_message(message(payload)).await.is_err());
            assert!(client.inbound_query.is_none());
        }
        client.handle_message(message(query.clone())).await.unwrap();
        assert!(client.handle_message(message(query)).await.is_err());
    }

    #[tokio::test]
    async fn pending_equal_notes_send_metadata_then_attachments_and_wait_for_exact_ack() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Arc::new(local_context(dir.path()).await);
        let events = enable_test_notes(&ctx).await;
        let note = note_pipelines::get_note(&ctx, &events[0].note_id)
            .await
            .unwrap()
            .unwrap();
        note_pipelines::update_note(
            &ctx,
            &note.id,
            note.revision,
            note_pipelines::SaveNoteInput {
                title: note.title,
                content: note.content,
                labels: vec![("project".into(), "bear_note".into())],
                attachments: vec![note_core::NoteAttachment {
                    id: "event".into(),
                    path: "test.bin".into(),
                    mime: "application/octet-stream".into(),
                    description: String::new(),
                    content: vec![42; CHUNK + 11],
                    storage: None,
                }],
            },
        )
        .await
        .unwrap();
        let runtime = Arc::new(SyncRuntime::default());
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        let observed = runtime.clone();
        let worker = tokio::spawn(async move {
            let query = peer_frame(&mut peer).await;
            assert_eq!(query.event, "state_query");
            peer_result(&mut peer, &query, equal_response(&query)).await;
            for confirmed in 0..2 {
                let begin = peer_frame(&mut peer).await;
                assert_eq!(begin.event, "transfer_begin");
                peer_reply(&mut peer, &begin).await;
                let mut metadata = Vec::new();
                loop {
                    let frame = peer_frame(&mut peer).await;
                    assert_eq!(frame.payload["blob_id"], "event");
                    if frame.event == "blob_end" {
                        peer_reply(&mut peer, &frame).await;
                        break;
                    }
                    assert_eq!(frame.event, "blob_chunk");
                    metadata.extend(
                        BASE64
                            .decode(frame.payload["data"].as_str().unwrap())
                            .unwrap(),
                    );
                }
                let event: SyncEvent = serde_json::from_slice(&metadata).unwrap();
                for attachment in &event.snapshot.as_ref().unwrap().attachments {
                    let begin = peer_frame(&mut peer).await;
                    assert_eq!(begin.event, "attachment_begin");
                    assert_eq!(
                        begin.payload["blob_id"],
                        format!("attachment:{}", attachment.id)
                    );
                    peer_reply(&mut peer, &begin).await;
                    let mut bytes = Vec::new();
                    loop {
                        let frame = peer_frame(&mut peer).await;
                        if frame.event == "blob_end" {
                            peer_reply(&mut peer, &frame).await;
                            break;
                        }
                        assert_eq!(frame.event, "blob_chunk");
                        bytes.extend(
                            BASE64
                                .decode(frame.payload["data"].as_str().unwrap())
                                .unwrap(),
                        );
                    }
                    assert_eq!(bytes.len() as u64, attachment.size_bytes);
                    assert_eq!(
                        format!("{:x}", Sha256::digest(&bytes)),
                        attachment.checksum_sha256
                    );
                }
                let commit = peer_frame(&mut peer).await;
                assert_eq!(commit.event, "transfer_commit");
                {
                    let status = observed.status.read().await;
                    let upload = &status.progress.as_ref().unwrap().upload;
                    assert_eq!(upload.notes_completed, confirmed);
                    let current = upload.current_note.as_ref().unwrap();
                    assert_eq!(current.id, event.note_id);
                    assert_eq!(current.phase, "awaiting_commit");
                    assert_eq!(
                        current.attachment_bytes_done,
                        current.attachment_bytes_total
                    );
                    assert_eq!(current.attachments_completed, current.attachments_total);
                }
                assert!(timeout(Duration::from_millis(25), peer.next())
                    .await
                    .is_err());
                peer_result(
                    &mut peer,
                    &commit,
                    serde_json::to_value(event.acknowledgement()).unwrap(),
                )
                .await;
            }
            let complete = peer_frame(&mut peer).await;
            assert_eq!(complete.event, "inventory_complete");
            peer_reply(&mut peer, &complete).await;
        });
        client.reconcile_round(true).await.unwrap();
        worker.await.unwrap();
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 0);
        let status = runtime.status.read().await;
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_equal, 0);
        assert_eq!(upload.notes_completed, 2);
        assert_eq!(upload.notes_remaining, Some(0));
    }

    #[tokio::test]
    async fn a_concurrent_edit_supersedes_a_verified_round_slot_and_remains_pending() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Arc::new(local_context(dir.path()).await);
        let events = enable_test_notes(&ctx).await;
        for event in &events {
            sync::acknowledge_sync_event(&ctx, &event.acknowledgement())
                .await
                .unwrap();
        }
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        let edited = ctx.clone();
        let worker = tokio::spawn(async move {
            let query = peer_frame(&mut peer).await;
            assert_eq!(query.event, "state_query");
            let note = note_pipelines::get_note(&edited, &events[0].note_id)
                .await
                .unwrap()
                .unwrap();
            note_pipelines::update_note(
                &edited,
                &note.id,
                note.revision,
                note_pipelines::SaveNoteInput {
                    title: "Edited during query".into(),
                    content: note.content,
                    labels: vec![("project".into(), "bear_note".into())],
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
            peer_result(&mut peer, &query, equal_response(&query)).await;
            let complete = peer_frame(&mut peer).await;
            assert_eq!(complete.event, "inventory_complete");
            peer_reply(&mut peer, &complete).await;
        });
        client.reconcile_round(true).await.unwrap();
        worker.await.unwrap();
        assert_eq!(sync::sync_status(&ctx).await.unwrap().pending_notes, 1);
        let status = runtime.status.read().await;
        let upload = &status.progress.as_ref().unwrap().upload;
        assert_eq!(upload.notes_equal, 1);
        assert_eq!(upload.notes_superseded, 1);
        assert_eq!(upload.notes_completed, 0);
        assert_eq!(upload.notes_remaining, Some(0));
    }

    #[tokio::test]
    async fn negotiated_inventory_completion_rejects_impossible_totals_and_inflight_notes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        runtime
            .progress(|p| p.download.complete_note("observed"))
            .await;
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        let complete =
            |total| json!({"inventory_id":uuid::Uuid::new_v4().to_string(),"total_notes":total});
        assert!(client
            .receive("inventory_complete", complete(0))
            .await
            .is_err());
        assert!(!client.remote_inventory_complete);
        begin_incoming(&mut client, &incoming_event(&[])).await;
        assert!(client
            .receive("inventory_complete", complete(2))
            .await
            .is_err());
        assert!(!client.remote_inventory_complete);
        client.incoming = None;
        client
            .receive("inventory_complete", complete(2))
            .await
            .unwrap();
        let status = runtime.status.read().await;
        let download = &status.progress.as_ref().unwrap().download;
        assert_eq!(download.notes_total, Some(2));
        assert_eq!(download.notes_remaining, None);
    }

    #[tokio::test]
    async fn a_new_remote_probe_starts_one_fresh_round_for_all_following_notes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let descriptor = enable_test_notes(&ctx).await[0].acknowledgement();
        let runtime = SyncRuntime::default();
        runtime
            .progress(|p| {
                p.download.complete_note("previous-round");
                p.download.notes_total = Some(3);
            })
            .await;
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.state_query = true;
        client.remote_inventory_complete = true;
        let query = Frame::new(
            Some("local-session"),
            Some("next-round"),
            TOPIC,
            "state_query",
            json!({"query_id":uuid::Uuid::new_v4().to_string(),"items":[descriptor]}),
        );
        client
            .handle_message(Some(Ok(Message::Text(query.encode().unwrap().into()))))
            .await
            .unwrap();
        assert!(!client.remote_inventory_complete);
        {
            let status = runtime.status.read().await;
            let download = &status.progress.as_ref().unwrap().download;
            assert_eq!(download.notes_total, None);
            assert_eq!(download.notes_completed, 0);
        }
        begin_incoming(&mut client, &incoming_event(&[])).await;
        runtime
            .progress(|p| p.download.complete_note("first-in-new-round"))
            .await;
        client.incoming = None;
        begin_incoming(&mut client, &incoming_event(&[])).await;
        assert_eq!(
            runtime
                .status
                .read()
                .await
                .progress
                .as_ref()
                .unwrap()
                .download
                .notes_completed,
            1
        );
    }

    fn incoming_event(sizes: &[u64]) -> SyncEvent {
        use note_core::sync::{SyncAttachment, SyncEventKind, SyncSnapshot};
        SyncEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            note_id: uuid::Uuid::new_v4().to_string(),
            generation: Default::default(),
            control: Default::default(),
            version: Default::default(),
            kind: SyncEventKind::Snapshot,
            snapshot_hash: "unused until commit".into(),
            snapshot: Some(SyncSnapshot {
                title: "Incoming".into(),
                content: String::new(),
                labels: vec![],
                created_at: 1,
                updated_at: 1,
                deleted_at: None,
                attachments: sizes
                    .iter()
                    .enumerate()
                    .map(|(index, size)| SyncAttachment {
                        id: format!("attachment-{index}"),
                        path: format!("{index}.txt"),
                        mime: "text/plain".into(),
                        description: String::new(),
                        size_bytes: *size,
                        checksum_sha256: format!("{:x}", Sha256::digest(vec![0; *size as usize])),
                    })
                    .collect(),
            }),
        }
    }

    async fn begin_incoming(client: &mut Session<'_>, event: &SyncEvent) -> String {
        let transfer = uuid::Uuid::new_v4().to_string();
        let metadata = serde_json::to_vec(event).unwrap();
        client
            .receive(
                "transfer_begin",
                json!({
                    "transfer_id":transfer,"size_bytes":metadata.len(),
                    "checksum_sha256":format!("{:x}", Sha256::digest(&metadata)),
                }),
            )
            .await
            .unwrap();
        client
            .receive(
                "blob_chunk",
                json!({
                    "transfer_id":transfer,"blob_id":"event","offset":0,
                    "data":BASE64.encode(&metadata),
                }),
            )
            .await
            .unwrap();
        transfer
    }

    async fn finish_incoming_blob(client: &mut Session<'_>, transfer: &str, blob: &str) {
        client
            .receive("blob_end", json!({"transfer_id":transfer,"blob_id":blob}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn attachment_preparation_waits_beyond_ten_seconds_after_each_verified_blob() {
        for completed in [0, 1] {
            let dir = tempfile::tempdir().unwrap();
            let ctx = local_context(dir.path()).await;
            let runtime = SyncRuntime::default();
            let (socket, mut peer) = local_sockets().await;
            let mut client = local_session(&ctx, &runtime, socket);
            let transfer = begin_incoming(&mut client, &incoming_event(&[0, 0])).await;
            finish_incoming_blob(&mut client, &transfer, "event").await;
            if completed == 1 {
                client
                    .receive(
                        "attachment_begin",
                        json!({
                            "transfer_id":transfer,"blob_id":"attachment:attachment-0",
                        }),
                    )
                    .await
                    .unwrap();
                finish_incoming_blob(&mut client, &transfer, "attachment:attachment-0").await;
            }
            tokio::time::pause();
            let worker = tokio::spawn(async move {
                let request = peer_frame(&mut peer).await;
                tokio::time::sleep(Duration::from_secs(12)).await;
                let begin = Frame::new(
                    request.join_ref.as_deref(),
                    None,
                    TOPIC,
                    "attachment_begin",
                    json!({
                        "transfer_id":transfer,
                        "blob_id":format!("attachment:attachment-{completed}"),
                    }),
                );
                peer.send(Message::Text(begin.encode().unwrap().into()))
                    .await
                    .unwrap();
                peer_reply(&mut peer, &request).await;
            });
            let started = Instant::now();
            let result = client.request("transfer_commit", json!({})).await;
            let elapsed = Instant::now() - started;
            worker.abort();
            let _ = worker.await;
            tokio::time::resume();
            assert!(result.is_ok(), "after {completed} attachments: {result:?}");
            // Tokio timers round wakeups to millisecond resolution.
            let expected = Duration::from_secs(12);
            assert!((expected..=expected + Duration::from_millis(1)).contains(&elapsed));
            assert!(client
                .incoming
                .as_ref()
                .unwrap()
                .blobs
                .contains_key(&format!("attachment:attachment-{completed}")));
        }
    }

    #[tokio::test]
    async fn attachment_preparation_has_a_fixed_bound_despite_unrelated_frames_and_heartbeats() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let transfer = begin_incoming(&mut client, &incoming_event(&[1])).await;
        finish_incoming_blob(&mut client, &transfer, "event").await;
        tokio::time::pause();
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            let unrelated = Frame::new(
                request.join_ref.as_deref(),
                None,
                TOPIC,
                "inventory_complete",
                json!({}),
            );
            let mut ticks = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = ticks.tick() => peer.send(Message::Text(unrelated.encode().unwrap().into())).await.unwrap(),
                    frame = peer.next() => {
                        let Some(Ok(message)) = frame else { break };
                        let frame = Frame::decode(&message.into_text().unwrap()).unwrap();
                        assert_eq!(frame.event, "heartbeat");
                        peer_reply(&mut peer, &frame).await;
                    }
                }
            }
        });
        let started = Instant::now();
        let error = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        let elapsed = Instant::now() - started;
        worker.abort();
        let _ = worker.await;
        tokio::time::resume();
        assert_eq!(error.to_string(), "sync inbound transfer stalled");
        // Tokio timers round wakeups to millisecond resolution.
        let expected = Duration::from_secs(41);
        assert!((expected..=expected + Duration::from_millis(1)).contains(&elapsed));
    }

    #[tokio::test]
    async fn attachment_preparation_does_not_extend_metadata_chunks_or_final_commit_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        for phase in ["metadata", "no_attachments", "begin", "chunk", "final_blob"] {
            let (socket, _peer) = local_sockets().await;
            let mut client = local_session(&ctx, &runtime, socket);
            let sizes: &[u64] = if phase == "no_attachments" { &[] } else { &[1] };
            let transfer = begin_incoming(&mut client, &incoming_event(sizes)).await;
            if phase != "metadata" {
                finish_incoming_blob(&mut client, &transfer, "event").await;
            }
            if matches!(phase, "begin" | "chunk" | "final_blob") {
                client
                    .receive(
                        "attachment_begin",
                        json!({
                            "transfer_id":transfer,"blob_id":"attachment:attachment-0",
                        }),
                    )
                    .await
                    .unwrap();
            }
            if matches!(phase, "chunk" | "final_blob") {
                client
                    .receive(
                        "blob_chunk",
                        json!({
                            "transfer_id":transfer,"blob_id":"attachment:attachment-0",
                            "offset":0,"data":BASE64.encode([0]),
                        }),
                    )
                    .await
                    .unwrap();
            }
            if phase == "final_blob" {
                finish_incoming_blob(&mut client, &transfer, "attachment:attachment-0").await;
            }
            tokio::time::pause();
            tokio::time::advance(DEADLINE - Duration::from_millis(1)).await;
            client.check_liveness().await.unwrap();
            tokio::time::advance(Duration::from_millis(1)).await;
            let error = client.check_liveness().await.unwrap_err();
            tokio::time::resume();
            assert_eq!(
                error.to_string(),
                "sync inbound transfer stalled",
                "{phase}"
            );
        }
    }

    #[test]
    fn attachment_preparation_budget_rounds_and_caps_the_receiver_read_budget() {
        assert_eq!(attachment_prepare_wait(0), Duration::from_secs(40));
        assert_eq!(attachment_prepare_wait(1), Duration::from_secs(41));
        assert_eq!(
            attachment_prepare_wait(1024 * 1024),
            Duration::from_secs(41)
        );
        assert_eq!(
            attachment_prepare_wait(1024 * 1024 + 1),
            Duration::from_secs(42)
        );
        assert_eq!(
            attachment_prepare_wait(DEFAULT_MAX_BLOB_BYTES),
            Duration::from_secs(552)
        );
        assert_eq!(attachment_prepare_wait(u64::MAX), Duration::from_secs(552));
    }

    #[tokio::test]
    async fn attachment_preparation_uses_the_largest_unstarted_manifest_entry() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let bytes = vec![0; 1024 * 1024 + 1];
        let event = incoming_event(&[0, bytes.len() as u64]);
        let transfer = begin_incoming(&mut client, &event).await;
        tokio::time::pause();
        finish_incoming_blob(&mut client, &transfer, "event").await;
        assert_eq!(
            client.incoming.as_ref().unwrap().idle_deadline - Instant::now(),
            Duration::from_secs(42)
        );
        // Complete the larger entry first, regardless of manifest ordering.
        client
            .receive(
                "attachment_begin",
                json!({
                    "transfer_id":transfer,"blob_id":"attachment:attachment-1",
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            client.incoming.as_ref().unwrap().idle_deadline - Instant::now(),
            DEADLINE
        );
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            client
                .receive(
                    "blob_chunk",
                    json!({
                        "transfer_id":transfer,"blob_id":"attachment:attachment-1",
                        "offset":index * CHUNK,"data":BASE64.encode(chunk),
                    }),
                )
                .await
                .unwrap();
        }
        finish_incoming_blob(&mut client, &transfer, "attachment:attachment-1").await;
        // The verified larger blob no longer extends preparation for the sole
        // remaining zero-byte attachment.
        assert_eq!(
            client.incoming.as_ref().unwrap().idle_deadline - Instant::now(),
            Duration::from_secs(40)
        );
        tokio::time::advance(Duration::from_secs(12)).await;
        client.check_liveness().await.unwrap();
        tokio::time::resume();
    }

    #[tokio::test]
    async fn attachment_preparation_does_not_extend_an_interleaved_unfinished_blob() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let transfer = begin_incoming(&mut client, &incoming_event(&[1, 0, 0])).await;
        finish_incoming_blob(&mut client, &transfer, "event").await;
        for index in [0, 1] {
            client
                .receive(
                    "attachment_begin",
                    json!({
                        "transfer_id":transfer,"blob_id":format!("attachment:attachment-{index}"),
                    }),
                )
                .await
                .unwrap();
        }
        tokio::time::pause();
        // B finishes with C still unstarted, but A is waiting for chunks.
        finish_incoming_blob(&mut client, &transfer, "attachment:attachment-1").await;
        tokio::time::advance(DEADLINE - Duration::from_millis(1)).await;
        client.check_liveness().await.unwrap();
        tokio::time::advance(Duration::from_millis(1)).await;
        let error = client.check_liveness().await.unwrap_err();
        tokio::time::resume();
        assert_eq!(error.to_string(), "sync inbound transfer stalled");
    }

    #[test]
    fn commit_budget_accounts_for_bytes_objects_and_the_hard_cap() {
        assert_eq!(commit_wait(0, 0), Duration::from_secs(90));
        assert_eq!(commit_wait(1, 1), Duration::from_secs(97));
        assert_eq!(commit_wait(1024 * 1024, 2), Duration::from_secs(102));
        assert_eq!(commit_wait(1024 * 1024 + 1, 2), Duration::from_secs(104));
        assert_eq!(commit_wait(10_449_995, 1), Duration::from_secs(115));
        assert_eq!(commit_wait(u64::MAX, usize::MAX), Duration::from_secs(930));
    }

    #[tokio::test(start_paused = true)]
    async fn commit_accepts_a_matching_pushed_ack_after_ten_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let expected = SyncAck {
            event_id: "event".into(),
            note_id: "note".into(),
            generation: Default::default(),
            control: Default::default(),
            version: Default::default(),
            kind: note_core::sync::SyncEventKind::Snapshot,
            snapshot_hash: "hash".into(),
        };
        let payload = serde_json::to_value(&expected).unwrap();
        let push_payload = payload.clone();
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            tokio::time::sleep(Duration::from_secs(12)).await;
            let stale = Frame::new(
                Some("old-session"),
                None,
                TOPIC,
                "ack",
                push_payload.clone(),
            );
            peer.send(Message::Text(stale.encode().unwrap().into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = Frame::new(
                request.join_ref.as_deref(),
                None,
                TOPIC,
                "ack",
                push_payload,
            );
            peer.send(Message::Text(current.encode().unwrap().into()))
                .await
                .unwrap();
        });
        let started = Instant::now();
        let response = client
            .request_with_budget(
                "transfer_commit",
                json!({}),
                commit_wait(0, 0),
                Some(&expected),
            )
            .await
            .unwrap();
        assert_eq!(response, payload);
        assert_eq!(Instant::now() - started, Duration::from_secs(13));
        worker.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn commit_preserves_matching_pushed_rejection_after_ten_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        client.outbound = Some(("event".into(), "note".into()));
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            tokio::time::sleep(Duration::from_secs(12)).await;
            let rejection = Frame::new(
                request.join_ref.as_deref(),
                None,
                TOPIC,
                "note_rejected",
                json!({"code":"note_rejected","event_id":"event","note_id":"note","reason":"catalog_conflict"}),
            );
            peer.send(Message::Text(rejection.encode().unwrap().into()))
                .await
                .unwrap();
        });
        let error = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<PeerNoteRejection>().unwrap().reason,
            "catalog_conflict"
        );
        assert!(client.outbound.is_none());
        worker.await.unwrap();
    }

    async fn local_context(dir: &std::path::Path) -> Context {
        Context::new(
            Arc::new(TursoStorage::open(dir.join("db")).await.unwrap()),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(dir.join("attachments"))),
        )
    }

    async fn local_sockets() -> (Socket, WebSocketStream<tokio::io::DuplexStream>) {
        let (client, peer) = tokio::io::duplex(1024 * 1024);
        let client: Box<dyn Transport> = Box::new(client);
        (
            WebSocketStream::from_raw_socket(
                MaybeTlsStream::Plain(client),
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await,
            WebSocketStream::from_raw_socket(
                peer,
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                None,
            )
            .await,
        )
    }

    fn local_session<'a>(
        ctx: &'a Context,
        runtime: &'a SyncRuntime,
        socket: Socket,
    ) -> Session<'a> {
        Session {
            ctx,
            runtime,
            socket: Io::raw(socket),
            join: "local-session".into(),
            counter: 0,
            limit: DEFAULT_MAX_BLOB_BYTES,
            incoming: None,
            heartbeat: None,
            next_heartbeat: Instant::now() + HEARTBEAT,
            outbound: None,
            outbound_transfer: None,
            state_query: false,
            inbound_query: None,
            remote_inventory_complete: false,
            task_sync: false,
            task_origin: uuid::Uuid::nil().to_string(),
            task_sequence: 0,
            received_tasks: Ledger::new(uuid::Uuid::nil().to_string()),
            outgoing_tasks: HashMap::new(),
            task_jobs: HashMap::new(),
            manager_failed: false,
            scan_progress: None,
            manager_sequence: 0,
            remote_manager: None,
            resolved: Arc::new(tempfile::tempdir().unwrap()),
            deferred_abort: None,
        }
    }

    async fn peer_frame(socket: &mut WebSocketStream<tokio::io::DuplexStream>) -> Frame {
        Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap()
    }

    async fn peer_reply(socket: &mut WebSocketStream<tokio::io::DuplexStream>, request: &Frame) {
        let reply = Frame::new(
            request.join_ref.as_deref(),
            request.reference.as_deref(),
            &request.topic,
            "phx_reply",
            json!({"status":"ok","response":{}}),
        );
        socket
            .send(Message::Text(reply.encode().unwrap().into()))
            .await
            .unwrap();
    }

    fn inbound_metadata_frames() -> (SyncEvent, [Frame; 3]) {
        let event = SyncEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            note_id: uuid::Uuid::new_v4().to_string(),
            generation: Default::default(),
            control: Default::default(),
            version: Default::default(),
            kind: note_core::sync::SyncEventKind::Fence,
            snapshot_hash: sync::canonical_snapshot_hash(None).unwrap(),
            snapshot: None,
        };
        let metadata = serde_json::to_vec(&event).unwrap();
        let transfer = uuid::Uuid::new_v4().to_string();
        let frame = |reference, event, payload| {
            Frame::new(Some("local-session"), reference, TOPIC, event, payload)
        };
        let frames = [
            frame(
                Some("incoming-begin"),
                "transfer_begin",
                json!({"transfer_id":transfer,"size_bytes":metadata.len(),
                    "checksum_sha256":format!("{:x}",Sha256::digest(&metadata))}),
            ),
            frame(
                None,
                "blob_chunk",
                json!({"transfer_id":transfer,"blob_id":"event","offset":0,
                    "data":BASE64.encode(&metadata)}),
            ),
            frame(
                Some("incoming-metadata"),
                "blob_end",
                json!({"transfer_id":transfer,"blob_id":"event"}),
            ),
        ];
        (event, frames)
    }

    async fn peer_send_metadata(
        peer: &mut WebSocketStream<tokio::io::DuplexStream>,
        frames: [Frame; 3],
    ) {
        for frame in frames {
            peer.send(Message::Text(frame.encode().unwrap().into()))
                .await
                .unwrap();
            if frame.reference.is_some() {
                let response = peer_frame(peer).await;
                assert_eq!(response.reference, frame.reference);
                assert_eq!(response.payload["status"], "ok");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_transfer_accepts_slow_preflight_before_begin_with_healthy_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let (event, frames) = inbound_metadata_frames();
        let started = Instant::now();
        let worker = tokio::spawn(async move {
            let ready = Instant::now() + Duration::from_secs(35);
            let mut heartbeats = 0;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(ready) => break,
                    frame = peer_frame(&mut peer) => {
                        assert_eq!(frame.event, "heartbeat");
                        peer_reply(&mut peer, &frame).await;
                        heartbeats += 1;
                    }
                }
            }
            peer_send_metadata(&mut peer, frames).await;
            heartbeats
        });
        loop {
            if let Ok(message) = timeout(Duration::from_millis(250), client.socket.next()).await {
                client.handle_message(message).await.unwrap();
            }
            client.check_liveness().await.unwrap();
            if client.incoming.as_ref().is_some_and(|i| i.event.is_some()) {
                break;
            }
            if Instant::now() - started < Duration::from_secs(35) {
                assert!(client.incoming.is_none());
            }
        }
        assert_eq!(
            client.incoming.as_ref().unwrap().event.as_ref(),
            Some(&event)
        );
        assert_eq!(Instant::now() - started, Duration::from_secs(35));
        assert_eq!(worker.await.unwrap(), 1);
        assert!(client.heartbeat.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_transfer_stall_after_metadata_ignores_unrelated_frames_and_healthy_heartbeat()
    {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let (_, frames) = inbound_metadata_frames();
        let heartbeats = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let heartbeats_seen = heartbeats.clone();
        let worker = tokio::spawn(async move {
            peer_send_metadata(&mut peer, frames).await;
            let unrelated = Frame::new(
                Some("local-session"),
                None,
                TOPIC,
                "inventory_complete",
                json!({}),
            );
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        peer.send(Message::Text(unrelated.encode().unwrap().into())).await.unwrap();
                    }
                    frame = peer_frame(&mut peer) => {
                        assert_eq!(frame.event, "heartbeat");
                        peer_reply(&mut peer, &frame).await;
                        heartbeats_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        });
        while client.incoming.as_ref().is_none_or(|i| i.event.is_none()) {
            let message = client.socket.next().await;
            client.handle_message(message).await.unwrap();
        }
        let last_progress = client.incoming.as_ref().unwrap().idle_deadline - DEADLINE;
        client.next_heartbeat = Instant::now() + Duration::from_secs(2);
        let error = loop {
            if let Ok(message) = timeout(Duration::from_millis(250), client.socket.next()).await {
                client.handle_message(message).await.unwrap();
            }
            if let Err(error) = client.check_liveness().await {
                break error;
            }
        };
        assert_eq!(error.to_string(), "sync inbound transfer stalled");
        assert_eq!(Instant::now() - last_progress, DEADLINE);
        assert_eq!(heartbeats.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(client.heartbeat.is_none());
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test(start_paused = true)]
    async fn commit_waits_beyond_ten_seconds_and_drives_heartbeats_without_other_traffic() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let worker = tokio::spawn(async move {
            let commit = peer_frame(&mut peer).await;
            assert_eq!(commit.event, "transfer_commit");
            let heartbeat = peer_frame(&mut peer).await;
            assert_eq!(heartbeat.event, "heartbeat");
            peer_reply(&mut peer, &heartbeat).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            peer_reply(&mut peer, &commit).await;
        });
        let started = Instant::now();
        client.request("transfer_commit", json!({})).await.unwrap();
        assert_eq!(Instant::now() - started, Duration::from_secs(35));
        worker.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn commit_fails_when_an_idle_peer_ignores_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let started = Instant::now();
        let error = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "sync heartbeat reply timed out");
        assert_eq!(Instant::now() - started, HEARTBEAT + DEADLINE);
    }

    #[tokio::test(start_paused = true)]
    async fn unrelated_frames_do_not_extend_commit_deadline_and_ordinary_requests_stay_short() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = local_context(dir.path()).await;
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            assert_eq!(request.event, "transfer_commit");
            let unrelated = Frame::new(
                request.join_ref.as_deref(),
                None,
                TOPIC,
                "inventory_complete",
                json!({}),
            );
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        peer.send(Message::Text(unrelated.encode().unwrap().into())).await.unwrap();
                    }
                    message = peer.next() => {
                        let Some(Ok(message)) = message else { break };
                        let frame = Frame::decode(&message.into_text().unwrap()).unwrap();
                        assert_eq!(frame.event, "heartbeat");
                        peer_reply(&mut peer, &frame).await;
                    }
                }
            }
        });
        let started = Instant::now();
        let error = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "sync Phoenix reply timed out");
        assert_eq!(Instant::now() - started, Duration::from_secs(90));
        worker.abort();
        let _ = worker.await;

        let (socket, _peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let started = Instant::now();
        assert_eq!(
            client
                .request("inventory_start", json!({}))
                .await
                .unwrap_err()
                .to_string(),
            "sync Phoenix reply timed out"
        );
        assert_eq!(Instant::now() - started, DEADLINE);
    }

    #[tokio::test]
    async fn ordinary_request_deadline_does_not_cancel_an_incoming_durable_commit() {
        use note_attachments::{AttachmentStore, PutObjectRequest, StoredObject};
        use note_core::sync::{Hlc, SyncAttachment, SyncEventKind, SyncLabel, SyncSnapshot};

        struct DelayedStore {
            files: FilesystemAttachmentStore,
            started: Notify,
            release: Notify,
        }
        #[async_trait::async_trait]
        impl AttachmentStore for DelayedStore {
            async fn put_immutable(&self, request: PutObjectRequest) -> Result<StoredObject> {
                self.started.notify_one();
                self.release.notified().await;
                self.files.put_immutable(request).await
            }
            async fn read_legacy(&self, note_id: &str, path: &str) -> Result<Vec<u8>> {
                self.files.read_legacy(note_id, path).await
            }
            fn info(&self) -> note_attachments::AttachmentStoreInfo {
                self.files.info()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DelayedStore {
            files: FilesystemAttachmentStore::new(dir.path().join("attachments")),
            started: Notify::new(),
            release: Notify::new(),
        });
        let ctx = Context::new(
            Arc::new(TursoStorage::open(dir.path().join("db")).await.unwrap()),
            Arc::new(StubEmbedder),
            store.clone(),
        );
        note_pipelines::update_system_config(
            &ctx,
            &note_core::SystemConfig {
                sync: note_core::sync::SyncConfig {
                    enabled: true,
                    connection_profile: Some("local".into()),
                    rules: vec![note_core::sync::SyncRule {
                        id: "project".into(),
                        enabled: true,
                        selector: "project=bear_note".into(),
                    }],
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sync::initialize_sync(&ctx).await.unwrap();
        let bytes = b"incoming attachment";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let snapshot = SyncSnapshot {
            title: "Incoming".into(),
            content: "Durable incoming note".into(),
            labels: vec![SyncLabel {
                key: "project".into(),
                value: "bear_note".into(),
                description: String::new(),
                value_type: "text".into(),
            }],
            created_at: 1,
            updated_at: 1,
            deleted_at: None,
            attachments: vec![SyncAttachment {
                id: uuid::Uuid::new_v4().to_string(),
                path: "incoming.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                size_bytes: bytes.len() as u64,
                checksum_sha256: checksum.clone(),
            }],
        };
        let stamp = Hlc {
            millis: 1,
            counter: 0,
            node_id: uuid::Uuid::new_v4().to_string(),
        };
        let event = SyncEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            note_id: uuid::Uuid::new_v4().to_string(),
            generation: stamp.clone(),
            control: stamp.clone(),
            version: stamp,
            kind: SyncEventKind::Attach,
            snapshot_hash: sync::canonical_snapshot_hash(Some(&snapshot)).unwrap(),
            snapshot: Some(snapshot),
        };
        let mut blob = StagedBlob::new(bytes.len() as u64, &checksum, 100).unwrap();
        blob.append(0, bytes).await.unwrap();
        blob.finish().await.unwrap();
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let transfer_id = uuid::Uuid::new_v4().to_string();
        client.incoming = Some(Incoming {
            id: transfer_id.clone(),
            metadata: StagedBlob::new(0, &format!("{:x}", Sha256::digest([])), 100).unwrap(),
            event: Some(event.clone()),
            blobs: HashMap::from([(
                format!(
                    "attachment:{}",
                    event.snapshot.as_ref().unwrap().attachments[0].id
                ),
                blob,
            )]),
            idle_deadline: Instant::now() + DEADLINE,
        });
        let delayed_store = store.clone();
        let worker = tokio::spawn(async move {
            let request = peer_frame(&mut peer).await;
            let commit = Frame::new(
                request.join_ref.as_deref(),
                Some("incoming-commit"),
                TOPIC,
                "transfer_commit",
                json!({"transfer_id":transfer_id}),
            );
            peer.send(Message::Text(commit.encode().unwrap().into()))
                .await
                .unwrap();
            // The ordinary request reply is already queued behind the incoming commit.
            peer_reply(&mut peer, &request).await;
            timeout(Duration::from_secs(5), delayed_store.started.notified())
                .await
                .expect("incoming commit did not start publishing its attachment");
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(12)).await;
            delayed_store.release.notify_one();
            peer
        });
        let result = client.request("inventory_start", json!({})).await;
        let mut peer = worker.await.unwrap();
        tokio::time::resume();
        // The expired request must leave both the durable note and its attachment intact.
        let note = note_pipelines::get_note_metadata(&ctx, &event.note_id)
            .await
            .unwrap()
            .expect("incoming durable commit was cancelled by the request deadline");
        assert_eq!(note.content, "Durable incoming note");
        let object_key = &note.attachments[0].storage.as_ref().unwrap().object_key;
        assert_eq!(store.files.read_object(object_key).await.unwrap(), bytes);
        let ack = timeout(Duration::from_secs(1), peer_frame(&mut peer))
            .await
            .unwrap();
        assert_eq!(ack.event, "ack");
        assert_eq!(
            ack.payload,
            serde_json::to_value(event.acknowledgement()).unwrap()
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            "sync Phoenix reply timed out"
        );
    }

    #[tokio::test]
    async fn task_durable_apply_keeps_heartbeat_live_and_ack_waits_for_commit() {
        use note_attachments::{AttachmentStore, PutObjectRequest, StoredObject};
        use note_core::sync::{Hlc, SyncAttachment, SyncEventKind, SyncLabel, SyncSnapshot};

        struct DelayedStore {
            files: FilesystemAttachmentStore,
            started: Notify,
            release: Notify,
        }
        #[async_trait::async_trait]
        impl AttachmentStore for DelayedStore {
            async fn put_immutable(&self, request: PutObjectRequest) -> Result<StoredObject> {
                self.started.notify_one();
                self.release.notified().await;
                self.files.put_immutable(request).await
            }
            async fn read_legacy(&self, note_id: &str, path: &str) -> Result<Vec<u8>> {
                self.files.read_legacy(note_id, path).await
            }
            fn info(&self) -> note_attachments::AttachmentStoreInfo {
                self.files.info()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DelayedStore {
            files: FilesystemAttachmentStore::new(dir.path().join("attachments")),
            started: Notify::new(),
            release: Notify::new(),
        });
        let ctx = Context::new(
            Arc::new(TursoStorage::open(dir.path().join("db")).await.unwrap()),
            Arc::new(StubEmbedder),
            store.clone(),
        );
        note_pipelines::update_system_config(
            &ctx,
            &note_core::SystemConfig {
                sync: note_core::sync::SyncConfig {
                    enabled: true,
                    connection_profile: Some("local".into()),
                    rules: vec![note_core::sync::SyncRule {
                        id: "project".into(),
                        enabled: true,
                        selector: "project=bear_note".into(),
                    }],
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sync::initialize_sync(&ctx).await.unwrap();
        let bytes = b"incoming attachment";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let snapshot = SyncSnapshot {
            title: "Incoming".into(),
            content: "Durable incoming note".into(),
            labels: vec![SyncLabel {
                key: "project".into(),
                value: "bear_note".into(),
                description: String::new(),
                value_type: "text".into(),
            }],
            created_at: 1,
            updated_at: 1,
            deleted_at: None,
            attachments: vec![SyncAttachment {
                id: uuid::Uuid::new_v4().to_string(),
                path: "incoming.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                size_bytes: bytes.len() as u64,
                checksum_sha256: checksum.clone(),
            }],
        };
        let stamp = Hlc {
            millis: 1,
            counter: 0,
            node_id: uuid::Uuid::new_v4().to_string(),
        };
        let event = SyncEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            note_id: uuid::Uuid::new_v4().to_string(),
            generation: stamp.clone(),
            control: stamp.clone(),
            version: stamp,
            kind: SyncEventKind::Attach,
            snapshot_hash: sync::canonical_snapshot_hash(Some(&snapshot)).unwrap(),
            snapshot: Some(snapshot),
        };
        let mut blob = StagedBlob::new(bytes.len() as u64, &checksum, 100).unwrap();
        blob.append(0, bytes).await.unwrap();
        blob.finish().await.unwrap();
        let runtime = SyncRuntime::default();
        let (socket, mut peer) = local_sockets().await;
        let mut client = local_session(&ctx, &runtime, socket);
        let transfer_id = uuid::Uuid::new_v4().to_string();
        client.incoming = Some(Incoming {
            id: transfer_id.clone(),
            metadata: StagedBlob::new(0, &format!("{:x}", Sha256::digest([])), 100).unwrap(),
            event: Some(event.clone()),
            blobs: HashMap::from([(
                format!(
                    "attachment:{}",
                    event.snapshot.as_ref().unwrap().attachments[0].id
                ),
                blob,
            )]),
            idle_deadline: Instant::now() + DEADLINE,
        });
        client.task_sync = true;
        client.socket.start_pump();
        let task_id = format!("{}:1", uuid::Uuid::nil());
        let request = Frame::new(
            Some("local-session"),
            None,
            TOPIC,
            "task_submit",
            json!({"task_id":task_id,"operation":"transfer_commit","payload":{"transfer_id":transfer_id}}),
        );
        client
            .handle_message(Some(Ok(Message::Text(request.encode().unwrap().into()))))
            .await
            .unwrap();
        store.started.notified().await;
        tokio::time::pause();
        let delayed_store = store.clone();
        let worker = tokio::spawn(async move {
            let mut beats = 0;
            while beats < 2 {
                let frame = peer_frame(&mut peer).await;
                if frame.event == "heartbeat" {
                    beats += 1;
                    peer_reply(&mut peer, &frame).await;
                } else {
                    assert_eq!(frame.event, "task_status");
                    assert_ne!(
                        frame.payload["state"], "succeeded",
                        "no ACK before durable publication"
                    );
                }
            }
            delayed_store.release.notify_one();
            peer
        });
        loop {
            client.check_liveness().await.unwrap();
            if client.received_tasks.get(&task_id).unwrap().terminal() {
                break;
            }
            let wake = client.next_wake();
            tokio::select! {
                message=client.socket.next()=>client.handle_message(message).await.unwrap(),
                _=tokio::time::sleep_until(wake)=>{},
            }
        }
        let mut peer = worker.await.unwrap();
        tokio::time::resume();
        let status = peer_frame(&mut peer).await;
        assert_eq!(status.event, "task_status");
        assert_eq!(status.payload["state"], "succeeded");
        assert_eq!(
            status.payload["result"],
            serde_json::to_value(event.acknowledgement()).unwrap()
        );
        assert!(sync::sync_received_event(&ctx, &event).await.unwrap());
        runtime.drain_workers().await;
    }

    #[tokio::test]
    async fn replacing_a_profile_cancels_a_blocked_join_and_preserves_pending_notes() {
        let mut certs = certificates();
        let old_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        certs.profile.url = format!(
            "wss://localhost:{}/note-sync/websocket?vsn=2.0.0",
            old_listener.local_addr().unwrap().port()
        );
        let (old_ready, old_join) = tokio::sync::oneshot::channel();
        let acceptor = TlsAcceptor::from(certs.server.clone());
        let old_server = tokio::spawn(async move {
            let (stream, _) = old_listener.accept().await.unwrap();
            let mut socket =
                tokio_tungstenite::accept_async(acceptor.accept(stream).await.unwrap())
                    .await
                    .unwrap();
            let request =
                Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(request.event, "phx_join");
            old_ready.send(()).unwrap();
            assert!(!matches!(
                timeout(Duration::from_secs(3), socket.next())
                    .await
                    .unwrap(),
                Some(Ok(_))
            ));
        });
        let storage = Arc::new(
            TursoStorage::open(certs.dir.path().join("hot-update.db"))
                .await
                .unwrap(),
        );
        let ctx = Arc::new(Context::new(
            storage,
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                certs.dir.path().join("attachments"),
            )),
        ));
        note_pipelines::save_note(
            &ctx,
            note_pipelines::SaveNoteInput {
                title: "pending".into(),
                content: "retained through reconnect".into(),
                labels: vec![("project".into(), "bear_note".into())],
                attachments: vec![],
            },
        )
        .await
        .unwrap();
        note_pipelines::update_system_config(
            &ctx,
            &note_core::SystemConfig {
                sync: note_core::sync::SyncConfig {
                    enabled: true,
                    connection_profile: Some("local".into()),
                    rules: vec![note_core::sync::SyncRule {
                        id: "scope".into(),
                        enabled: true,
                        selector: "project=bear_note".into(),
                    }],
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let runtime = SyncRuntime::open(
            &[certs.profile.clone()],
            certs.dir.path().join("connections.json"),
            certs.dir.path().into(),
        )
        .unwrap();
        let pending = sync::sync_status(&ctx).await.unwrap().pending_notes;
        assert_eq!(pending, 1);
        let (stop, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run(ctx.clone(), runtime.clone(), shutdown));
        timeout(Duration::from_secs(3), old_join)
            .await
            .unwrap()
            .unwrap();
        let new_url = format!(
            "wss://localhost:{}/note-sync/websocket?vsn=2.0.0",
            new_listener.local_addr().unwrap().port()
        );
        runtime
            .save_connection(ConnectionInput {
                id: "local".into(),
                mode: None,
                ssh: None,
                url: new_url,
                client_cert: None,
                client_key: None,
                ca_cert: None,
                max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
            })
            .await
            .unwrap();
        let acceptor = TlsAcceptor::from(certs.server.clone());
        let mut new_socket = timeout(Duration::from_secs(3), async {
            let (stream, _) = new_listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(acceptor.accept(stream).await.unwrap())
                .await
                .unwrap()
        })
        .await
        .unwrap();
        let request = timeout(Duration::from_secs(3), new_socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            Frame::decode(&request.into_text().unwrap()).unwrap().event,
            "phx_join"
        );
        old_server.await.unwrap();
        stop.send(true).unwrap();
        timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sync::sync_status(&ctx).await.unwrap().pending_notes,
            pending
        );
    }

    #[tokio::test]
    async fn live_session_checks_reply_identity_rejection_status_and_heartbeat_deadline() {
        let mut certs = certificates();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        certs.profile.url = format!(
            "wss://localhost:{}/note-sync/websocket?vsn=2.0.0",
            listener.local_addr().unwrap().port()
        );
        let acceptor = TlsAcceptor::from(certs.server.clone());
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket =
                tokio_tungstenite::accept_async(acceptor.accept(stream).await.unwrap())
                    .await
                    .unwrap();
            let request =
                Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            let stale = Frame::new(
                Some("previous-session"),
                request.reference.as_deref(),
                TOPIC,
                "phx_reply",
                json!({"status":"ok","response":{"stale":true}}),
            );
            socket
                .send(Message::Text(stale.encode().unwrap().into()))
                .await
                .unwrap();
            let current = Frame::new(
                request.join_ref.as_deref(),
                request.reference.as_deref(),
                TOPIC,
                "phx_reply",
                json!({"status":"ok","response":{"current":true}}),
            );
            socket
                .send(Message::Text(current.encode().unwrap().into()))
                .await
                .unwrap();
            let heartbeat =
                Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(heartbeat.topic, "phoenix");
            assert_eq!(heartbeat.event, "heartbeat");
            assert!(heartbeat.join_ref.is_none());
            let ack = Frame::new(
                None,
                heartbeat.reference.as_deref(),
                "phoenix",
                "phx_reply",
                json!({"status":"ok","response":{}}),
            );
            socket
                .send(Message::Text(ack.encode().unwrap().into()))
                .await
                .unwrap();
            for status in ["unexpected", "error"] {
                let request =
                    Frame::decode(&socket.next().await.unwrap().unwrap().into_text().unwrap())
                        .unwrap();
                assert_eq!(request.event, "transfer_commit");
                let rejection = Frame::new(
                    request.join_ref.as_deref(),
                    request.reference.as_deref(),
                    TOPIC,
                    "phx_reply",
                    json!({"status":status,"response":{
                        "code":"note_rejected","event_id":"event","note_id":"note",
                        "reason":"catalog_conflict"
                    }}),
                );
                socket
                    .send(Message::Text(rejection.encode().unwrap().into()))
                    .await
                    .unwrap();
            }
        });
        let storage = Arc::new(
            TursoStorage::open(certs.dir.path().join("db"))
                .await
                .unwrap(),
        );
        let ctx = Context::new(
            storage,
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                certs.dir.path().join("attachments"),
            )),
        );
        let runtime = SyncRuntime::default();
        let mut client = Session {
            ctx: &ctx,
            runtime: &runtime,
            socket: Io::raw(connect_socket(&certs.profile).await.unwrap()),
            join: "current-session".into(),
            counter: 0,
            limit: 1000,
            incoming: None,
            heartbeat: None,
            next_heartbeat: Instant::now(),
            outbound: None,
            outbound_transfer: None,
            state_query: false,
            inbound_query: None,
            remote_inventory_complete: false,
            task_sync: false,
            task_origin: uuid::Uuid::nil().to_string(),
            task_sequence: 0,
            received_tasks: Ledger::new(uuid::Uuid::nil().to_string()),
            outgoing_tasks: HashMap::new(),
            task_jobs: HashMap::new(),
            manager_failed: false,
            scan_progress: None,
            manager_sequence: 0,
            remote_manager: None,
            resolved: Arc::new(tempfile::tempdir().unwrap()),
            deferred_abort: None,
        };
        assert_eq!(
            client.request("phx_join", json!({})).await.unwrap(),
            json!({"current":true})
        );
        client.check_liveness().await.unwrap();
        assert!(client.heartbeat.is_some());
        let message = client.socket.next().await;
        client.handle_message(message).await.unwrap();
        assert!(client.heartbeat.is_none());
        let invalid = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        assert_eq!(invalid.to_string(), "sync peer rejected channel request");
        assert!(invalid.downcast_ref::<PeerNoteRejection>().is_none());
        let rejected = client
            .request("transfer_commit", json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            rejected.downcast_ref::<PeerNoteRejection>().unwrap().reason,
            "catalog_conflict"
        );
        client.heartbeat = Some((
            "never-answered".into(),
            Instant::now() - DEADLINE - Duration::from_secs(1),
        ));
        assert!(client
            .check_liveness()
            .await
            .unwrap_err()
            .to_string()
            .contains("heartbeat reply timed out"));
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn diagnostic_history_is_bounded_and_redacts_unknown_errors() {
        let runtime = SyncRuntime::new(&[]);
        for _ in 0..25 {
            runtime
                .error(&anyhow::anyhow!(
                    "peer-secret /private/path?token=credential"
                ))
                .await;
        }
        let value = serde_json::to_value(&*runtime.status.read().await).unwrap();
        assert!(!value.to_string().contains("peer-secret"));
        assert_eq!(value["diagnostics"].as_array().unwrap().len(), 20);
        assert_eq!(value["diagnostics"][0]["level"], "error");
        assert!(value["diagnostics"][0]["timestamp"].as_i64().unwrap() > 0);
    }
    #[tokio::test]
    async fn attempts_retry_and_recovery_preserve_recent_diagnostics() {
        let runtime = SyncRuntime::new(&[]);
        runtime.begin_attempt().await;
        let first_attempt = runtime.status.read().await.last_attempt_at.unwrap();
        runtime
            .error(
                &Failure::new(
                    "websocket",
                    "websocket_http_403",
                    "HTTP 403; verify identity.",
                )
                .into(),
            )
            .await;
        runtime.status.write().await.next_retry_at = Some(first_attempt + 5000);
        runtime.retry();
        runtime.begin_attempt().await;
        {
            let status = runtime.status.read().await;
            assert_eq!(status.state, "connecting");
            assert!(status.last_attempt_at.unwrap() >= first_attempt);
            assert!(status.next_retry_at.is_none());
            assert_eq!(status.diagnostics.len(), 1);
        }
        runtime.connected("peer".into()).await;
        let status = runtime.status.read().await;
        assert_eq!(status.state, "connected");
        assert!(status.last_error.is_none());
        assert!(status.next_retry_at.is_none());
        assert_eq!(status.diagnostics.len(), 2);
        assert_eq!(status.diagnostics[0].code, "websocket_http_403");
        assert_eq!(status.diagnostics[1].level, "info");
    }
    #[tokio::test]
    async fn failed_worker_exposes_safe_http_reason_and_scheduled_retry_via_status_api() {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use note_pipelines::org::{OrgContext, SystemOrgClock};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tower::ServiceExt;
        let certs = certificates();
        let storage = Arc::new(
            TursoStorage::open(certs.dir.path().join("http-diagnostics.db"))
                .await
                .unwrap(),
        );
        let ctx = Arc::new(Context::new(
            storage.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                certs.dir.path().join("attachments"),
            )),
        ));
        note_pipelines::update_system_config(
            &ctx,
            &note_core::SystemConfig {
                sync: note_core::sync::SyncConfig {
                    enabled: true,
                    connection_profile: Some("local".into()),
                    rules: vec![note_core::sync::SyncRule {
                        id: "project".into(),
                        enabled: true,
                        selector: "project=bear_note".into(),
                    }],
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut profile = certs.profile.clone();
        profile.mode = ConnectionMode::Direct;
        profile.url = format!(
            "ws://127.0.0.1:{}/note-sync/websocket?vsn=2.0.0",
            listener.local_addr().unwrap().port()
        );
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            stream.read(&mut bytes).await.unwrap();
            stream.write_all(b"HTTP/1.1 403 Forbidden\r\nX-Secret: header-secret\r\nContent-Length: 11\r\n\r\nbody-secret").await.unwrap();
        });
        let runtime = SyncRuntime::new(&[profile]);
        let (stop, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run(ctx.clone(), runtime.clone(), shutdown));
        timeout(Duration::from_secs(3), async {
            loop {
                if runtime.status.read().await.next_retry_at.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let app_state = crate::AppState::new(
            ctx.clone(),
            Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock))),
        )
        .with_sync(runtime.clone());
        let (app, _) = super::super::api::router().split_for_parts();
        let response = app
            .with_state(app_state)
            .oneshot(
                Request::builder()
                    .uri("/api/system/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["state"], "error");
        assert_eq!(status["diagnostics"][0]["code"], "websocket_http_403");
        assert_eq!(status["diagnostics"][0]["stage"], "websocket");
        assert!(status["last_error"].as_str().unwrap().contains("HTTP 403"));
        assert!(status["last_attempt_at"].as_i64().unwrap() > 0);
        assert!(
            status["next_retry_at"].as_i64().unwrap() > status["last_attempt_at"].as_i64().unwrap()
        );
        for secret in ["header-secret", "body-secret", "client.key", "client.pem"] {
            assert!(!status.to_string().contains(secret));
        }
        let mut config = note_pipelines::get_system_config(&ctx).await.unwrap();
        config.sync.enabled = false;
        note_pipelines::update_system_config(&ctx, &config)
            .await
            .unwrap();
        let disabled = runtime.status(&ctx).await.unwrap();
        assert_eq!(disabled.state, "disabled");
        assert!(disabled.next_retry_at.is_none());
        assert_eq!(disabled.diagnostics.len(), 1);
        stop.send(true).unwrap();
        worker.await.unwrap();
        peer.await.unwrap();
    }
}
