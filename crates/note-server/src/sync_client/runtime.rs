//! mTLS Phoenix socket runtime. Durability and convergence remain in pipelines.
use super::{
    phoenix::{Frame, FRAME_BYTES},
    tls,
    transfer::{encode_metadata, StagedBlob},
    SyncConnection, SyncProfile,
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
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
    status: RwLock<SyncRuntimeStatus>,
    wake: Notify,
    blocked: std::sync::Mutex<BTreeMap<String, SyncBlockedNote>>,
}
impl SyncRuntime {
    pub fn new(profiles: &[SyncConnection]) -> Arc<Self> {
        Arc::new(Self {
            status: RwLock::new(SyncRuntimeStatus {
                profiles: profiles
                    .iter()
                    .map(SyncConnection::public_profile)
                    .collect(),
                ..Default::default()
            }),
            wake: Notify::new(),
            blocked: std::sync::Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn status(&self, ctx: &Context) -> Result<SyncRuntimeStatus> {
        let durable = sync::sync_status(ctx).await?;
        let mut state = self.status.read().await.clone();
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
    async fn error(&self, error: &anyhow::Error) {
        let mut status = self.status.write().await;
        status.state = "error".into();
        // Errors constructed by this module never include credential paths, URLs or payloads.
        status.last_error = Some(error.to_string());
    }
}

pub async fn connect_socket(profile: &SyncConnection) -> Result<Socket> {
    let tls = tls::client_config(profile)?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(FRAME_BYTES))
        .max_frame_size(Some(FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(FRAME_BYTES * 2);
    let result = timeout(
        DEADLINE,
        tokio_tungstenite::connect_async_tls_with_config(
            &profile.url,
            Some(config),
            true,
            Some(tokio_tungstenite::Connector::Rustls(tls)),
        ),
    )
    .await
    .context("sync WSS connection timed out")?;
    // connect_async does not follow HTTP redirects. All TLS errors are fail closed.
    result.map(|(socket,_)|socket).map_err(|_|anyhow::anyhow!("sync WSS connection failed; verify peer availability, CA, hostname and client certificate"))
}

/// The worker polls durable state; process termination never acknowledges pending work.
pub async fn run(
    ctx: Arc<Context>,
    profiles: Vec<SyncConnection>,
    runtime: Arc<SyncRuntime>,
    mut shutdown: watch::Receiver<bool>,
) {
    if let Err(error) = sync::release_abandoned_sync_events(&ctx).await {
        runtime.error(&error).await;
        return;
    }
    let mut failures = 0u32;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let config = match note_pipelines::get_system_config(&ctx).await {
            Ok(config) => config.sync,
            Err(error) => {
                runtime.error(&error).await;
                if pause(&runtime, &mut shutdown, Duration::from_secs(2)).await {
                    break;
                }
                continue;
            }
        };
        if !config.enabled {
            runtime.status.write().await.state = "disabled".into();
            if pause(&runtime, &mut shutdown, Duration::from_secs(1)).await {
                break;
            }
            continue;
        }
        let profile = profiles
            .iter()
            .find(|p| Some(&p.id) == config.connection_profile.as_ref());
        let result = if let Some(profile) = profile {
            runtime.status.write().await.state = "connecting".into();
            tokio::select! {
                biased;
                _=shutdown.changed()=>break,
                result=watch_config(&ctx,&config)=>result,
                _=runtime.wake.notified()=>Ok(()),
                result=session(&ctx,profile,&config,&runtime)=>result,
            }
        } else {
            Err(anyhow::anyhow!(
                "selected sync connection profile is unavailable; provision it in server TOML"
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
        if pause(
            &runtime,
            &mut shutdown,
            Duration::from_millis((base * 1000 + jitter).min(60_000)),
        )
        .await
        {
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
    delay: Duration,
) -> bool {
    tokio::select! { _=shutdown.changed()=>true,_=runtime.wake.notified()=>false,_=tokio::time::sleep(delay)=>false }
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
    last_activity: Instant,
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
    {
        let mut state = runtime.status.write().await;
        state.state = "connected".into();
        state.last_error = runtime
            .blocked
            .lock()
            .expect("sync rejection mutex poisoned")
            .values()
            .next()
            .map(|note| format!("Note {} cannot sync: {}", note.note_id, note.reason));
        state.peer_node_id = Some(peer.node_id);
    }
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
        let reference = self.reference();
        self.send(event, payload, Some(&reference)).await?;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let message = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .context("sync Phoenix reply timed out")?;
            if let Some(Ok(Message::Text(text))) = &message {
                let frame = Frame::decode(text).context("invalid sync Phoenix frame")?;
                if frame.is_current(&self.join, &reference)
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
                        bail!("sync peer rejected channel request");
                    }
                    return Ok(frame.payload["response"].clone());
                }
            }
            self.handle_message(message).await?;
            self.check_liveness().await?;
        }
    }
    async fn check_liveness(&mut self) -> Result<()> {
        let now = Instant::now();
        if self
            .heartbeat
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) > DEADLINE)
        {
            bail!("sync heartbeat reply timed out");
        }
        if self
            .incoming
            .as_ref()
            .is_some_and(|i| now.duration_since(i.last_activity) > DEADLINE)
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
                    last_activity: Instant::now(),
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
                incoming.last_activity = Instant::now();
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
                incoming.last_activity = Instant::now();
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
                incoming.last_activity = Instant::now();
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
            .map_or(Some(0), |s| {
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
        let committed = self
            .request("transfer_commit", json!({"transfer_id":transfer}))
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
    use crate::sync_client::tests::certificates;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage_turso::TursoStorage;
    use tokio_rustls::TlsAcceptor;

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
}
