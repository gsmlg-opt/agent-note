//! Phoenix socket runtime. Durability and convergence remain in pipelines.
use super::{
    connections::Connections,
    diagnostics::{Failure, SyncDiagnostic},
    phoenix::{Frame, FRAME_BYTES},
    tls,
    transfer::{encode_metadata, StagedBlob},
    ConnectionInput, ConnectionMode, ConnectionSummary, SaveError, SyncConnection, SyncProfile,
};
use anyhow::{bail, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{FutureExt, SinkExt, StreamExt};
use note_core::sync::{SyncAck, SyncEvent};
use note_pipelines::{sync, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{watch, Notify, RwLock},
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
type Socket = WebSocketStream<MaybeTlsStream<Box<dyn Transport>>>;

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
        }
    }
}
#[derive(Default)]
pub struct SyncRuntime {
    connections: Arc<Connections>,
    status: RwLock<SyncRuntimeStatus>,
    wake: Notify,
    blocked: std::sync::Mutex<BTreeMap<String, SyncBlockedNote>>,
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
            status: RwLock::new(SyncRuntimeStatus::default()),
            wake: Notify::new(),
            blocked: std::sync::Mutex::new(BTreeMap::new()),
        })
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
        if state.state == "connected" {
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
        }
        Ok(state)
    }
    pub fn retry(&self) {
        self.blocked
            .lock()
            .expect("sync rejection mutex poisoned")
            .clear();
        self.wake.notify_one();
    }
    fn is_blocked(&self, event: &SyncEvent) -> bool {
        let mut blocked = self.blocked.lock().expect("sync rejection mutex poisoned");
        if let Some(old) = blocked.get(&event.note_id) {
            if old.event_id == event.event_id {
                return true;
            }
            blocked.remove(&event.note_id);
        }
        false
    }
    async fn block(&self, event: &SyncEvent, reason: &str) {
        self.blocked
            .lock()
            .expect("sync rejection mutex poisoned")
            .insert(
                event.note_id.clone(),
                SyncBlockedNote {
                    note_id: event.note_id.clone(),
                    event_id: event.event_id.clone(),
                    reason: reason.into(),
                },
            );
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
                _=runtime.wake.notified()=>Ok(()),
                result=session(&ctx,profile,&config,&runtime)=>result,
            }
        } else {
            Err(anyhow::anyhow!(
                "selected sync connection profile is unavailable; add it in System settings"
            ))
        };
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
    socket: Socket,
    join: String,
    counter: u64,
    limit: u64,
    incoming: Option<Incoming>,
    heartbeat: Option<(String, Instant)>,
    next_heartbeat: Instant,
    outbound: Option<(String, String)>,
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
        socket: connect_socket(profile).await?,
        join: uuid::Uuid::new_v4().to_string(),
        counter: 0,
        limit: profile.max_blob_bytes,
        incoming: None,
        heartbeat: None,
        next_heartbeat: Instant::now() + HEARTBEAT,
        outbound: None,
    };
    let peer: Hello = serde_json::from_value(
        client
            .request("phx_join", serde_json::to_value(&hello)?)
            .await?,
    )
    .context("sync peer returned invalid hello")?;
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
    runtime.connected(peer.node_id).await;
    // Ask the server to stream its inventory; page boundaries never encode large snapshots.
    client.request("inventory_start", json!({})).await?;
    let mut cursor = None;
    loop {
        let page = sync::sync_inventory(ctx, cursor.as_deref(), 32).await?;
        for event in page.events {
            if runtime.is_blocked(&event) {
                continue;
            }
            if sync::reserve_sync_event(ctx, &event).await? {
                let result = client.send_event(&event).await;
                sync::release_sync_event(ctx, &event.event_id).await?;
                if let Err(error) = result {
                    if let Some(reason) = blocked_reason(&error) {
                        runtime.block(&event, reason).await;
                    } else {
                        return Err(error);
                    }
                } else {
                    runtime.status.write().await.last_success_at =
                        Some(chrono::Utc::now().timestamp_millis());
                }
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    client.request("inventory_complete", json!({})).await?;
    loop {
        // A config change stops this session before adopting its replacement scope.
        let current = note_pipelines::get_system_config(ctx).await?.sync;
        if &current != config {
            return Ok(());
        }
        if let Some(event) = sync::next_sync_event(ctx).await? {
            let actual_send = !runtime.is_blocked(&event);
            let sent = if !actual_send {
                match timeout(Duration::from_millis(250), client.socket.next()).await {
                    Ok(message) => client.handle_message(message).await?,
                    Err(_) => {}
                }
                Ok(())
            } else {
                client.send_event(&event).await
            };
            // Release the storage lease even when delivery or confirmation failed.
            sync::release_sync_event(ctx, &event.event_id).await?;
            match sent {
                Ok(()) => {
                    if actual_send {
                        runtime.status.write().await.last_success_at =
                            Some(chrono::Utc::now().timestamp_millis());
                    }
                }
                Err(error) if blocked_reason(&error).is_some() => {
                    runtime
                        .block(
                            &event,
                            blocked_reason(&error).expect("guarded rejection reason"),
                        )
                        .await;
                }
                Err(error) => return Err(error),
            }
        } else {
            match timeout(Duration::from_millis(250), client.socket.next()).await {
                Ok(message) => client.handle_message(message).await?,
                Err(_) => {}
            }
        }
        client.check_liveness().await?;
    }
}
impl Session<'_> {
    fn reference(&mut self) -> String {
        self.counter += 1;
        self.counter.to_string()
    }
    async fn send(&mut self, event: &str, payload: Value, reference: Option<&str>) -> Result<()> {
        let frame = Frame::new(Some(&self.join), reference, TOPIC, event, payload);
        timeout(
            DEADLINE,
            self.socket.send(Message::Text(frame.encode()?.into())),
        )
        .await
        .context("sync socket write timed out")?
        .context("sync socket write failed")?;
        Ok(())
    }
    async fn request(&mut self, event: &str, payload: Value) -> Result<Value> {
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
            anyhow::ensure!(Instant::now() < deadline, "sync Phoenix reply timed out");
            self.check_liveness().await?;
            let mut wake = self.next_heartbeat;
            if let Some((_, at)) = &self.heartbeat {
                wake = *at + DEADLINE;
            }
            if let Some(incoming) = &self.incoming {
                wake = wake.min(incoming.idle_deadline);
            }
            let message = tokio::select! {
                message = self.socket.next() => message,
                _ = tokio::time::sleep_until(wake.min(deadline)) => continue,
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
        let now = Instant::now();
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
        if frame.event == "phx_reply" {
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
                if let (Some((event_id, note_id)), Some(reason)) =
                    (committing, sync::sync_note_rejection_reason(&error))
                {
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
                    incoming.event = Some(event);
                } else {
                    incoming
                        .blobs
                        .get_mut(id)
                        .context("sync attachment has not begun")?
                        .finish()
                        .await?;
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
                sync::apply_sync_event(self.ctx, &event, &attachments).await?;
                self.runtime.status.write().await.last_success_at =
                    Some(chrono::Utc::now().timestamp_millis());
                return Ok(serde_json::to_value(event.acknowledgement())?);
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
            "inventory_complete" => {}
            _ => bail!("unknown sync Phoenix event"),
        }
        Ok(json!({}))
    }
    async fn send_blob(&mut self, transfer: &str, id: &str, bytes: &[u8]) -> Result<()> {
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            self.send("blob_chunk",json!({"transfer_id":transfer,"blob_id":id,"offset":index*CHUNK,"data":BASE64.encode(chunk)}),None).await?;
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
        let prepared = async {
            let mut prepared = HashMap::new();
            if let Some(snapshot) = &event.snapshot {
                for attachment in &snapshot.attachments {
                    let bytes =
                        sync::read_sync_attachment(self.ctx, event, &attachment.id, self.limit)
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
            sync::validate_sync_event(event, &prepared).map_err(|_| {
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
        }
        .await
        .map_err(|error| PreparationError(error.to_string()))?;
        self.outbound = Some((event.event_id.clone(), event.note_id.clone()));
        let transfer = uuid::Uuid::new_v4().to_string();
        self.request("transfer_begin",json!({"transfer_id":transfer,"size_bytes":metadata.len(),"checksum_sha256":format!("{:x}",Sha256::digest(&metadata))})).await?;
        self.send_blob(&transfer, "event", &metadata).await?;
        if let Some(snapshot) = &event.snapshot {
            for attachment in &snapshot.attachments {
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
            }
        }
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
        Ok(())
    }
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
    if error.downcast_ref::<PreparationError>().is_some() {
        Some("local_preparation_failed")
    } else {
        error
            .downcast_ref::<PeerNoteRejection>()
            .map(|r| r.reason.as_str())
    }
}

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
mod tests {
    use super::*;
    use crate::sync_client::{tests::certificates, DEFAULT_MAX_BLOB_BYTES};
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage_turso::TursoStorage;
    use tokio_rustls::TlsAcceptor;

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
            socket,
            join: "local-session".into(),
            counter: 0,
            limit: DEFAULT_MAX_BLOB_BYTES,
            incoming: None,
            heartbeat: None,
            next_heartbeat: Instant::now() + HEARTBEAT,
            outbound: None,
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
            socket: connect_socket(&certs.profile).await.unwrap(),
            join: "current-session".into(),
            counter: 0,
            limit: 1000,
            incoming: None,
            heartbeat: None,
            next_heartbeat: Instant::now(),
            outbound: None,
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
