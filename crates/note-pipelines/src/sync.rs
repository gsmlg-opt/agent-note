//! Durable snapshot replication. All state transitions share the note writer transaction.
use crate::{save_note::finish_transaction, Context};
use note_core::{normalize_attachment_path, sync::*, Note, NoteAttachment};
use note_storage::{NewNote, StorageSession, TransactionMode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const DEFAULT_SYNC_MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;
const PAGE: usize = 128;
/// A deterministic note rejection, safe to report only after successful rollback.
#[derive(Debug)]
pub enum SyncNoteRejection {
    ValidationFailed(String),
    CatalogConflict(String),
}
impl SyncNoteRejection {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::ValidationFailed(_) => "validation_failed",
            Self::CatalogConflict(_) => "catalog_conflict",
        }
    }
}
impl std::fmt::Display for SyncNoteRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ValidationFailed(message) | Self::CatalogConflict(message) => {
                formatter.write_str(message)
            }
        }
    }
}
impl std::error::Error for SyncNoteRejection {}
/// Additional failure context (for example failed cleanup) makes a rejection retryable.
pub fn sync_note_rejection_reason(error: &anyhow::Error) -> Option<&'static str> {
    let top: &(dyn std::error::Error + Send + Sync + 'static) = error.as_ref();
    top.downcast_ref::<SyncNoteRejection>()
        .map(SyncNoteRejection::reason)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Identity {
    node_id: String,
    clock: Hlc,
    initialized: bool,
    cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Head {
    #[serde(default)]
    selected: bool,
    version: Hlc,
    hash: String,
    snapshot: Option<SyncSnapshot>,
    attachments: Vec<NoteAttachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Member {
    generation: Hlc,
    control: Hlc,
    attached: bool,
    event: SyncEvent,
    attachments: Vec<NoteAttachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    event: SyncEvent,
    attachments: Vec<NoteAttachment>,
}
async fn get<T: DeserializeOwned>(
    s: &dyn StorageSession,
    ns: &str,
    key: &str,
) -> anyhow::Result<Option<T>> {
    s.get_sync_record(ns, key)
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}
async fn put<T: Serialize>(
    s: &dyn StorageSession,
    ns: &str,
    key: &str,
    value: &T,
) -> anyhow::Result<()> {
    s.put_sync_record(ns, key, &serde_json::to_value(value)?)
        .await?;
    Ok(())
}
async fn identity(s: &dyn StorageSession) -> anyhow::Result<Identity> {
    if let Some(id) = get(s, "identity", "local").await? {
        return Ok(id);
    }
    let node_id = uuid::Uuid::new_v4().to_string();
    let id = Identity {
        clock: Hlc {
            node_id: node_id.clone(),
            ..Default::default()
        },
        node_id,
        initialized: false,
        cursor: None,
    };
    put(s, "identity", "local", &id).await?;
    Ok(id)
}
async fn tick(s: &dyn StorageSession, observed: Option<&Hlc>) -> anyhow::Result<Hlc> {
    let mut id = identity(s).await?;
    id.clock = id
        .clock
        .advance(chrono::Utc::now().timestamp_millis(), observed, &id.node_id)
        .ok_or_else(|| anyhow::anyhow!("sync clock exhausted"))?;
    put(s, "identity", "local", &id).await?;
    Ok(id.clock)
}
fn matches(config: &SyncConfig, labels: &[SyncLabel]) -> bool {
    config.rules.iter().filter(|r| r.enabled).any(|rule| {
        let Ok(selectors) = note_core::try_parse_label_selectors(&rule.selector) else {
            return false;
        };
        !selectors.is_empty()
            && selectors.iter().all(|selector| {
                labels.iter().any(|label| {
                    let Ok(value_type) = label.value_type.parse() else {
                        return false;
                    };
                    note_core::label_matches_selector(
                        &note_core::Label {
                            key: label.key.clone(),
                            value: label.value.clone(),
                            description: label.description.clone(),
                            value_type,
                        },
                        selector,
                    )
                })
            })
    })
}
/// Canonical object-key ordering and UTF-8 JSON bytes are shared with GaoNote.
/// Ordered arrays are labels by (key,value) and attachments by (path,id).
pub fn canonical_snapshot_hash(snapshot: Option<&SyncSnapshot>) -> anyhow::Result<String> {
    let value = serde_json::to_value(snapshot)?;
    let mut bytes = Vec::new();
    write_canonical_json(&value, &mut bytes)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn write_canonical_json(value: &serde_json::Value, out: &mut Vec<u8>) -> anyhow::Result<()> {
    match value {
        serde_json::Value::Object(object) => {
            out.push(b'{');
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',')
                }
                serde_json::to_writer(&mut *out, key)?;
                out.push(b':');
                write_canonical_json(value, out)?;
            }
            out.push(b'}');
        }
        serde_json::Value::Array(values) => {
            out.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(b',')
                }
                write_canonical_json(value, out)?;
            }
            out.push(b']');
        }
        _ => serde_json::to_writer(out, value)?,
    }
    Ok(())
}
async fn snapshot(ctx: &Context, note: &Note) -> anyhow::Result<SyncSnapshot> {
    let mut labels: Vec<_> = note
        .labels
        .iter()
        .map(|l| SyncLabel {
            key: l.key.clone(),
            value: l.value.clone(),
            description: l.description.clone(),
            value_type: l.value_type.as_str().into(),
        })
        .collect();
    labels.sort_by(|a, b| (&a.key, &a.value).cmp(&(&b.key, &b.value)));
    let mut attachments = Vec::new();
    let mut total = 0u64;
    for a in &note.attachments {
        let (size_bytes, checksum_sha256) = match &a.storage {
            Some(storage) => (storage.size_bytes, storage.checksum_sha256.clone()),
            None => {
                let bytes = if !a.content.is_empty() {
                    a.content.clone()
                } else {
                    ctx.attachments()
                        .read_legacy_bounded(&note.id, &a.path, DEFAULT_SYNC_MAX_BLOB_BYTES)
                        .await?
                };
                (bytes.len() as u64, format!("{:x}", Sha256::digest(&bytes)))
            }
        };
        total = total
            .checked_add(size_bytes)
            .ok_or_else(|| anyhow::anyhow!("sync attachments too large"))?;
        attachments.push(SyncAttachment {
            id: a.id.clone(),
            path: normalize_attachment_path(&a.path),
            mime: a.mime.clone(),
            description: a.description.clone(),
            size_bytes,
            checksum_sha256,
        });
    }
    attachments.sort_by(|a, b| (&a.path, &a.id).cmp(&(&b.path, &b.id)));
    Ok(SyncSnapshot {
        title: note.title.clone(),
        content: note.content.clone(),
        labels,
        attachments,
        created_at: note.created_at,
        updated_at: note.updated_at,
        deleted_at: note.deleted_at,
    })
}
fn object_key(note_id: &str, a: &NoteAttachment) -> String {
    match &a.storage {
        Some(storage) => format!("object:{}", storage.object_key),
        None => format!("legacy:{note_id}:{}", normalize_attachment_path(&a.path)),
    }
}
async fn ensure_not_deleting(
    s: &dyn StorageSession,
    note_id: &str,
    attachments: &[NoteAttachment],
) -> anyhow::Result<()> {
    for a in attachments {
        anyhow::ensure!(
            s.get_sync_record("deleting", &object_key(note_id, a))
                .await?
                .is_none(),
            "attachment cleanup has reserved object"
        );
    }
    Ok(())
}
async fn queue(
    s: &dyn StorageSession,
    event: &SyncEvent,
    attachments: &[NoteAttachment],
) -> anyhow::Result<()> {
    ensure_not_deleting(s, &event.note_id, attachments).await?;
    put(
        s,
        "pending",
        &event.note_id,
        &Pending {
            event: event.clone(),
            attachments: attachments.to_vec(),
        },
    )
    .await
}
/// Capture even while paused. A never-configured installation avoids sync records entirely.
pub(crate) async fn capture_note(
    ctx: &Context,
    s: &dyn StorageSession,
    id: &str,
    historical: bool,
) -> anyhow::Result<()> {
    capture_with_mode(ctx, s, id, historical, false, false).await
}
async fn capture_with_mode(
    ctx: &Context,
    s: &dyn StorageSession,
    id: &str,
    historical: bool,
    reconcile: bool,
    recreated: bool,
) -> anyhow::Result<()> {
    let config = s.get_system_config().await?.sync;
    if config.connection_profile.is_none() && !config.enabled {
        return Ok(());
    }
    let Some(note) = s.get_sync_note(id).await? else {
        return Ok(());
    };
    let snap = snapshot(ctx, &note).await?;
    let hash = canonical_snapshot_hash(Some(&snap))?;
    let old: Option<Head> = get(s, "head", id).await?;
    if historical && old.is_some() && !reconcile && get::<Member>(s, "member", id).await?.is_some()
    {
        return Ok(());
    }
    let version = if recreated {
        tick(s, old.as_ref().map(|head| &head.version)).await?
    } else if let Some(old) = old.as_ref().filter(|old| historical || old.hash == hash) {
        old.version.clone()
    } else if historical {
        Hlc {
            millis: note
                .updated_at
                .max(note.deleted_at.unwrap_or(i64::MIN))
                .saturating_mul(1000),
            counter: 0,
            node_id: identity(s).await?.node_id,
        }
    } else {
        tick(s, old.as_ref().map(|head| &head.version)).await?
    };
    let selected = matches(&config, &snap.labels);
    let head = Head {
        selected,
        version: version.clone(),
        hash: hash.clone(),
        snapshot: Some(snap.clone()),
        attachments: note.attachments.clone(),
    };
    put(s, "head", id, &head).await?;
    let member: Option<Member> = get(s, "member", id).await?;
    // A historical rescan must never revive a detached fence.
    if historical && !reconcile && member.as_ref().is_some_and(|m| !m.attached) {
        return Ok(());
    }
    if selected
        && !reconcile
        && !recreated
        && member.as_ref().is_some_and(|member| !member.attached)
        && old.as_ref().is_some_and(|head| head.selected)
    {
        // Closing association does not change the local selector. Ordinary independent
        // edits may reattach only after that selector has actually left and re-entered.
        return Ok(());
    }
    let (kind, generation, control) = match (selected, member.as_ref()) {
        (true, Some(m)) if m.attached && !recreated => (
            if m.event.kind == SyncEventKind::Attach {
                SyncEventKind::Attach
            } else {
                SyncEventKind::Snapshot
            },
            m.generation.clone(),
            m.control.clone(),
        ),
        (true, _) => {
            let stamp = tick(s, Some(&version)).await?;
            (SyncEventKind::Attach, stamp.clone(), stamp)
        }
        (false, Some(m)) if m.attached => (
            SyncEventKind::Detach,
            m.generation.clone(),
            tick(s, None).await?,
        ),
        _ => return Ok(()),
    };
    let event = SyncEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        note_id: id.into(),
        generation: generation.clone(),
        control: control.clone(),
        version,
        kind,
        snapshot: Some(snap),
        snapshot_hash: hash,
    };
    put(
        s,
        "member",
        id,
        &Member {
            generation,
            control,
            attached: selected,
            event: event.clone(),
            attachments: head.attachments.clone(),
        },
    )
    .await?;
    queue(s, &event, &head.attachments).await
}
pub(crate) async fn capture_purge(s: &dyn StorageSession, id: &str) -> anyhow::Result<()> {
    let config = s.get_system_config().await?.sync;
    if config.connection_profile.is_none() && !config.enabled {
        return Ok(());
    }
    let version = tick(s, None).await?;
    let hash = canonical_snapshot_hash(None)?;
    put(
        s,
        "head",
        id,
        &Head {
            selected: false,
            version: version.clone(),
            hash: hash.clone(),
            snapshot: None,
            attachments: Vec::new(),
        },
    )
    .await?;
    let Some(mut member) = get::<Member>(s, "member", id).await? else {
        return Ok(());
    };
    if !member.attached {
        return Ok(());
    }
    let event = SyncEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        note_id: id.into(),
        generation: member.generation.clone(),
        control: member.control.clone(),
        version,
        kind: SyncEventKind::Purge,
        snapshot: None,
        snapshot_hash: hash,
    };
    member.event = event.clone();
    member.attachments.clear();
    put(s, "member", id, &member).await?;
    queue(s, &event, &[]).await
}
pub(crate) async fn reconcile_rules(ctx: &Context, s: &dyn StorageSession) -> anyhow::Result<()> {
    let config = s.get_system_config().await?.sync;
    if config.connection_profile.is_none() && !config.enabled {
        return Ok(());
    }
    let mut after = None;
    loop {
        let ids = s.list_sync_note_ids(after.as_deref(), PAGE).await?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            let Some(note) = s.get_sync_note(id).await? else {
                continue;
            };
            let selected = matches(&config, &snapshot(ctx, &note).await?.labels);
            let head: Option<Head> = get(s, "head", id).await?;
            if head.as_ref().is_none_or(|head| head.selected != selected) {
                capture_with_mode(ctx, s, id, true, true, false).await?;
            }
        }
        after = ids.last().cloned();
    }
    Ok(())
}
pub async fn initialize_sync(ctx: &Context) -> anyhow::Result<()> {
    // Establish identity/capture before scanning. Every page commits independently.
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        identity(tx.as_ref()).await?;
        anyhow::Ok(())
    }
    .await;
    finish_transaction(tx, result).await?;
    loop {
        let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
        let result = async {
            let mut id = identity(tx.as_ref()).await?;
            if id.initialized {
                return Ok(true);
            }
            let config = tx.get_system_config().await?.sync;
            if config.connection_profile.is_none() {
                return Ok(true);
            }
            let ids = tx.list_sync_note_ids(id.cursor.as_deref(), PAGE).await?;
            for note_id in &ids {
                capture_note(ctx, tx.as_ref(), note_id, true).await?;
            }
            // Reload the clock because capture may have advanced it.
            id = identity(tx.as_ref()).await?;
            id.cursor = ids.last().cloned().or(id.cursor);
            id.initialized = ids.len() < PAGE;
            put(tx.as_ref(), "identity", "local", &id).await?;
            Ok(id.initialized)
        }
        .await;
        if finish_transaction(tx, result).await? {
            break;
        }
    }
    Ok(())
}
pub async fn pin_sync_peer(
    ctx: &Context,
    profile_id: &str,
    peer_node_id: &str,
) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(peer_node_id)?;
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        anyhow::ensure!(
            tx.get_system_config()
                .await?
                .sync
                .connection_profile
                .as_deref()
                == Some(profile_id),
            "sync connection profile changed during registration"
        );
        let local = identity(tx.as_ref()).await?;
        anyhow::ensure!(
            local.node_id != peer_node_id,
            "peer identity equals local identity"
        );
        if let Some(old) = get::<String>(tx.as_ref(), "peer", profile_id).await? {
            anyhow::ensure!(
                old == peer_node_id,
                "Peer identity changed; operator must restore registered GaoNote identity"
            );
        }
        put(tx.as_ref(), "peer", profile_id, &peer_node_id).await
    }
    .await;
    finish_transaction(tx, result).await
}
pub async fn sync_status(ctx: &Context) -> anyhow::Result<SyncStatus> {
    let s = ctx.storage().session().await?;
    let config = s.get_system_config().await?.sync;
    let id: Option<Identity> = get(s.as_ref(), "identity", "local").await?;
    let mut pending_notes = 0;
    let mut after = None;
    loop {
        let rows = s
            .list_sync_records("pending", after.as_deref(), PAGE)
            .await?;
        if rows.is_empty() {
            break;
        }
        pending_notes += rows.len();
        after = rows.last().map(|r| r.0.clone());
    }
    Ok(SyncStatus {
        enabled: config.enabled,
        node_id: id.as_ref().map(|i| i.node_id.clone()).unwrap_or_default(),
        initialized: id.as_ref().is_some_and(|i| i.initialized),
        pending_notes,
    })
}
pub async fn sync_inventory(
    ctx: &Context,
    after: Option<&str>,
    limit: usize,
) -> anyhow::Result<SyncInventory> {
    let s = ctx.storage().session().await?;
    let rows = s
        .list_sync_records("member", after, limit.clamp(1, 1000))
        .await?;
    let next_cursor = rows.last().map(|r| r.0.clone());
    let events = rows
        .into_iter()
        .map(|(_, v)| serde_json::from_value::<Member>(v).map(|m| m.event))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncInventory {
        events,
        next_cursor,
    })
}
pub async fn next_sync_event(ctx: &Context) -> anyhow::Result<Option<SyncEvent>> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        if !tx.get_system_config().await?.sync.enabled {
            return Ok(None);
        }
        let cursor: Option<String> = get(tx.as_ref(), "scheduler", "cursor").await?;
        let mut rows = tx
            .list_sync_records("pending", cursor.as_deref(), 1)
            .await?;
        if rows.is_empty() && cursor.is_some() {
            rows = tx.list_sync_records("pending", None, 1).await?;
        }
        let Some((key, value)) = rows.into_iter().next() else {
            return Ok(None);
        };
        let pending: Pending = serde_json::from_value(value)?;
        put(tx.as_ref(), "scheduler", "cursor", &key).await?;
        ensure_not_deleting(tx.as_ref(), &pending.event.note_id, &pending.attachments).await?;
        put(tx.as_ref(), "inflight", &pending.event.event_id, &pending).await?;
        Ok(Some(pending.event))
    }
    .await;
    finish_transaction(tx, result).await
}
pub async fn release_sync_event(ctx: &Context, event_id: &str) -> anyhow::Result<()> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = tx
        .delete_sync_record("inflight", event_id)
        .await
        .map_err(Into::into);
    finish_transaction(tx, result).await
}
pub async fn acknowledge_sync_event(ctx: &Context, ack: &SyncAck) -> anyhow::Result<()> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let lease = get::<Pending>(tx.as_ref(), "inflight", &ack.event_id).await?;
        if let Some(lease) = &lease {
            anyhow::ensure!(
                lease.event.acknowledgement() == *ack,
                "sync ACK does not match durable inflight event"
            );
        }
        if let Some(pending) = get::<Pending>(tx.as_ref(), "pending", &ack.note_id).await? {
            if pending.event.acknowledgement() == *ack {
                tx.delete_sync_record("pending", &ack.note_id).await?;
                put(tx.as_ref(), "ack", &ack.note_id, ack).await?;
            }
        }
        // Inventory events can have no pending row; their exact ACK still retires control state.
        if let Some(mut member) = get::<Member>(tx.as_ref(), "member", &ack.note_id).await? {
            if member.event.acknowledgement() == *ack {
                if member.event.kind == SyncEventKind::Attach {
                    member.event.kind = SyncEventKind::Snapshot;
                    member.event.event_id = uuid::Uuid::new_v4().to_string();
                }
                if member.event.kind == SyncEventKind::Detach {
                    member.event.kind = SyncEventKind::Fence;
                    member.event.snapshot = None;
                    member.event.event_id = uuid::Uuid::new_v4().to_string();
                    member.attachments.clear();
                }
                put(tx.as_ref(), "member", &ack.note_id, &member).await?;
            }
        }
        if lease.is_some() {
            tx.delete_sync_record("inflight", &ack.event_id).await?;
        }
        Ok(())
    }
    .await;
    finish_transaction(tx, result).await
}
pub async fn read_sync_attachment(
    ctx: &Context,
    event: &SyncEvent,
    attachment_id: &str,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    let s = ctx.storage().session().await?;
    let pending = get::<Pending>(s.as_ref(), "inflight", &event.event_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("sync event read lease is absent"))?;
    anyhow::ensure!(
        pending.event == *event,
        "sync event does not match durable read lease"
    );
    let descriptor = event
        .snapshot
        .as_ref()
        .and_then(|s| s.attachments.iter().find(|a| a.id == attachment_id))
        .ok_or_else(|| anyhow::anyhow!("unknown sync attachment"))?;
    anyhow::ensure!(
        descriptor.size_bytes <= max_bytes.min(DEFAULT_SYNC_MAX_BLOB_BYTES),
        "sync attachment exceeds negotiated limit"
    );
    let attachment = pending
        .attachments
        .iter()
        .find(|a| a.id == attachment_id)
        .ok_or_else(|| anyhow::anyhow!("attachment metadata missing"))?;
    let bytes = match &attachment.storage {
        Some(storage) => {
            ctx.attachments()
                .read_object_bounded(&storage.object_key, descriptor.size_bytes)
                .await?
        }
        None => {
            ctx.attachments()
                .read_legacy_bounded(&event.note_id, &attachment.path, descriptor.size_bytes)
                .await?
        }
    };
    anyhow::ensure!(
        bytes.len() as u64 == descriptor.size_bytes
            && format!("{:x}", Sha256::digest(&bytes)) == descriptor.checksum_sha256,
        "sync attachment integrity mismatch"
    );
    Ok(bytes)
}
fn has_uri_scheme(path: &str) -> bool {
    path.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}
/// Validates the canonical wire boundary without storage or transport I/O.
pub fn validate_sync_event(
    event: &SyncEvent,
    bytes: &HashMap<String, Vec<u8>>,
) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(&event.note_id)?;
    uuid::Uuid::parse_str(&event.event_id)?;
    for stamp in [&event.generation, &event.control, &event.version] {
        uuid::Uuid::parse_str(&stamp.node_id)?;
        anyhow::ensure!(
            stamp.millis >= 0 && !stamp.node_id.is_empty(),
            "invalid sync clock"
        );
    }
    anyhow::ensure!(
        event.control >= event.generation,
        "control precedes association generation"
    );
    anyhow::ensure!(
        event.snapshot_hash.len() == 64
            && event
                .snapshot_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid sync snapshot hash"
    );
    if matches!(event.kind, SyncEventKind::Detach | SyncEventKind::Fence) {
        anyhow::ensure!(
            event.control > event.generation,
            "closure control must follow association generation"
        );
    }
    if event.kind == SyncEventKind::Fence {
        anyhow::ensure!(
            event.snapshot.is_none() && bytes.is_empty(),
            "fence cannot carry content"
        );
        return Ok(());
    }
    anyhow::ensure!(
        (event.kind == SyncEventKind::Purge) == event.snapshot.is_none(),
        "invalid sync event snapshot"
    );
    anyhow::ensure!(
        canonical_snapshot_hash(event.snapshot.as_ref())? == event.snapshot_hash,
        "sync snapshot hash mismatch"
    );
    let Some(snap) = &event.snapshot else {
        anyhow::ensure!(bytes.is_empty(), "purge cannot carry attachments");
        return Ok(());
    };
    anyhow::ensure!(
        !snap.title.trim().is_empty() && !snap.content.trim().is_empty(),
        "empty synchronized note"
    );
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    let mut total = 0u64;
    let mut keys = HashSet::new();
    for label in &snap.labels {
        note_core::validate_label_key(&label.key)?;
        let value_type = label.value_type.parse().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            keys.insert(&label.key) && note_core::validate_label_value(value_type, &label.value),
            "invalid sync label"
        );
    }
    let mut sorted = snap.clone();
    sorted
        .labels
        .sort_by(|a, b| (&a.key, &a.value).cmp(&(&b.key, &b.value)));
    sorted
        .attachments
        .sort_by(|a, b| (&a.path, &a.id).cmp(&(&b.path, &b.id)));
    anyhow::ensure!(sorted == *snap, "sync arrays must have canonical ordering");
    for a in &snap.attachments {
        note_core::validate_attachments(&[NoteAttachment {
            id: a.id.clone(),
            path: a.path.clone(),
            mime: a.mime.clone(),
            description: a.description.clone(),
            content: Vec::new(),
            storage: None,
        }])?;
        anyhow::ensure!(
            normalize_attachment_path(&a.path) == a.path
                && !has_uri_scheme(&a.path)
                && !a.path.starts_with("./"),
            "noncanonical sync attachment path"
        );
        anyhow::ensure!(
            ids.insert(&a.id) && paths.insert(&a.path),
            "duplicate sync attachment path or id"
        );
        total = total
            .checked_add(a.size_bytes)
            .ok_or_else(|| anyhow::anyhow!("sync attachments too large"))?;
        anyhow::ensure!(
            total <= DEFAULT_SYNC_MAX_BLOB_BYTES,
            "sync attachment snapshot exceeds bounded limit"
        );
        let content = bytes
            .get(&a.id)
            .ok_or_else(|| anyhow::anyhow!("sync attachment bytes missing"))?;
        anyhow::ensure!(
            content.len() as u64 == a.size_bytes
                && format!("{:x}", Sha256::digest(content)) == a.checksum_sha256,
            "sync attachment integrity mismatch"
        );
    }
    anyhow::ensure!(bytes.len() == ids.len(), "unexpected sync attachment bytes");
    Ok(())
}
pub async fn apply_sync_event(
    ctx: &Context,
    event: &SyncEvent,
    attachment_bytes: &HashMap<String, Vec<u8>>,
) -> anyhow::Result<SyncApplyResult> {
    validate_sync_event(event, attachment_bytes)
        .map_err(|error| SyncNoteRejection::ValidationFailed(error.to_string()))?;
    if event.kind == SyncEventKind::Fence {
        return apply_fence(ctx, event).await;
    }
    let attachments = event
        .snapshot
        .as_ref()
        .map(|snap| {
            snap.attachments
                .iter()
                .map(|a| NoteAttachment {
                    id: a.id.clone(),
                    path: a.path.clone(),
                    mime: a.mime.clone(),
                    description: a.description.clone(),
                    content: attachment_bytes[&a.id].clone(),
                    storage: None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // All attachment bytes have been validated before publishing or opening the note transaction.
    let published = crate::generated_attachments::publish_generated_attachments(
        ctx.attachments(),
        &event.note_id,
        &attachments,
    )
    .await?;
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let config = tx.get_system_config().await?.sync;
        anyhow::ensure!(config.enabled, "sync is paused");
        if get::<Head>(tx.as_ref(), "head", &event.note_id)
            .await?
            .is_none()
            && tx.get_sync_note(&event.note_id).await?.is_some()
        {
            capture_note(ctx, tx.as_ref(), &event.note_id, true).await?;
        }
        let member: Option<Member> = get(tx.as_ref(), "member", &event.note_id).await?;
        if let Some(old) = get::<SyncEvent>(tx.as_ref(), "received", &event.event_id).await? {
            anyhow::ensure!(old == *event, "event ID reused with different payload");
            return Ok(SyncApplyResult::Duplicate);
        }
        let head: Option<Head> = get(tx.as_ref(), "head", &event.note_id).await?;
        if member.as_ref().is_some_and(|m| !m.attached)
            && event.kind == SyncEventKind::Detach
            && head
                .as_ref()
                .zip(member.as_ref())
                .is_some_and(|(head, member)| head.version != member.event.version)
        {
            // An edit after our frozen closure belongs only to the independent local copy.
            return Ok(SyncApplyResult::Ignored);
        }
        if let Some(head) = &head {
            if head.version == event.version {
                anyhow::ensure!(
                    head.hash == event.snapshot_hash,
                    "same sync version has different snapshot hash"
                );
            }
        }
        let member_allowed = match (&member, event.kind) {
            (None, SyncEventKind::Attach) => true,
            (Some(m), SyncEventKind::Attach) => {
                event.generation > m.generation
                    || (event.generation == m.generation
                        && m.attached
                        && event.control >= m.control)
            }
            (Some(m), _) => {
                (m.attached || event.kind == SyncEventKind::Detach)
                    && event.generation == m.generation
                    && event.control >= m.control
            }
            _ => false,
        };
        if !member_allowed {
            return Ok(SyncApplyResult::Ignored);
        }
        if event.kind == SyncEventKind::Attach || event.kind == SyncEventKind::Snapshot {
            anyhow::ensure!(
                event
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| matches(&config, &s.labels)),
                "remote note does not match authorized sync rules"
            );
        }
        tick(
            tx.as_ref(),
            Some(
                &event
                    .version
                    .clone()
                    .max(event.control.clone())
                    .max(event.generation.clone()),
            ),
        )
        .await?;
        let winning = head
            .as_ref()
            .is_none_or(|head| event.version > head.version);
        if winning {
            let existing = tx.get_sync_note(&event.note_id).await?;
            if let Some(existing) = &existing {
                crate::generated_attachments::enqueue_attachment_cleanup(
                    tx.as_ref(),
                    &event.note_id,
                    &existing.attachments,
                    chrono::Utc::now().timestamp(),
                )
                .await?;
            }
            if let Some(snap) = &event.snapshot {
                let catalog: HashMap<_, _> = tx
                    .list_label_keys()
                    .await?
                    .into_iter()
                    .map(|k| (k.key, (k.value_type, k.description)))
                    .collect();
                for label in &snap.labels {
                    let value_type = label.value_type.parse().map_err(anyhow::Error::msg)?;
                    if let Some(existing) = catalog.get(&label.key) {
                        if existing.0 != value_type {
                            return Err(SyncNoteRejection::CatalogConflict(format!(
                                "synchronized label {} has incompatible catalog type",
                                label.key
                            ))
                            .into());
                        }
                        if existing.1 != label.description {
                            return Err(SyncNoteRejection::CatalogConflict(format!(
                                "synchronized label {} has incompatible catalog description",
                                label.key
                            ))
                            .into());
                        }
                    } else {
                        tx.insert_label_key_with_type(&label.key, &label.description, value_type)
                            .await?;
                    }
                }
                tx.put_sync_note(NewNote {
                    id: &event.note_id,
                    title: &snap.title,
                    content: &snap.content,
                    attachments: &published.metadata,
                    created_at: snap.created_at,
                    updated_at: snap.updated_at,
                    note_revision: 1,
                    deleted_at: snap.deleted_at,
                })
                .await?;
                tx.clear_note_labels(&event.note_id).await?;
                for label in &snap.labels {
                    tx.attach_label(&event.note_id, &label.key, &label.value)
                        .await?;
                }
                let revision = tx.get_sync_note(&event.note_id).await?.unwrap().revision;
                if snap.deleted_at.is_none() {
                    crate::sync_note_embedding_jobs(
                        tx.as_ref(),
                        &event.note_id,
                        &crate::chunk_content(&snap.content),
                        revision,
                        snap.updated_at,
                    )
                    .await?;
                } else {
                    tx.clear_note_search_data(&event.note_id).await?;
                }
            } else {
                tx.purge_sync_note(&event.note_id).await?;
            }
            put(
                tx.as_ref(),
                "head",
                &event.note_id,
                &Head {
                    selected: event
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| matches(&config, &snapshot.labels)),
                    version: event.version.clone(),
                    hash: event.snapshot_hash.clone(),
                    snapshot: event.snapshot.clone(),
                    attachments: published.metadata.clone(),
                },
            )
            .await?;
        }
        let attached = event.kind != SyncEventKind::Detach;
        let mut member = Member {
            generation: event.generation.clone(),
            control: event.control.clone(),
            attached,
            event: event.clone(),
            attachments: published.metadata.clone(),
        };
        if head.as_ref().is_some_and(|h| h.version > event.version) {
            let head = head.as_ref().unwrap();
            let winner = SyncEvent {
                event_id: uuid::Uuid::new_v4().to_string(),
                note_id: event.note_id.clone(),
                generation: event.generation.clone(),
                control: event.control.clone(),
                version: head.version.clone(),
                kind: if head.snapshot.is_none() && !attached {
                    SyncEventKind::Fence
                } else if head.snapshot.is_none() {
                    SyncEventKind::Purge
                } else if !attached {
                    SyncEventKind::Detach
                } else if event.kind == SyncEventKind::Attach {
                    SyncEventKind::Attach
                } else {
                    SyncEventKind::Snapshot
                },
                snapshot: head.snapshot.clone(),
                snapshot_hash: head.hash.clone(),
            };
            queue(tx.as_ref(), &winner, &head.attachments).await?;
            member.event = winner;
            member.attachments = head.attachments.clone();
        } else if let Some(pending) = get::<Pending>(tx.as_ref(), "pending", &event.note_id).await?
        {
            if pending.event.generation <= event.generation
                && (pending.event.version <= event.version || !attached)
            {
                tx.delete_sync_record("pending", &event.note_id).await?;
            }
        }
        if !winning && head.as_ref().is_some_and(|h| h.version == event.version) {
            member.attachments = head.as_ref().unwrap().attachments.clone();
        }
        put(tx.as_ref(), "member", &event.note_id, &member).await?;
        put(tx.as_ref(), "received", &event.event_id, event).await?;
        Ok(if winning {
            SyncApplyResult::Applied
        } else {
            SyncApplyResult::Ignored
        })
    }
    .await;
    match result {
        Ok(result) => {
            tx.commit().await?;
            if result != SyncApplyResult::Applied {
                published.cleanup_best_effort(ctx.attachments()).await;
            }
            ctx.wake_embedding_jobs();
            ctx.notify_note_mutated();
            Ok(result)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(published
                .cleanup_with_primary(ctx.attachments(), error)
                .await)
        }
    }
}
/// Atomically reserves deletion while excluding current notes, queued snapshots and read leases.
/// Reservations remain after deletion, so no late pin may reference a deleted immutable object.
pub async fn reserve_sync_attachment_cleanup(
    ctx: &Context,
    note_id: &str,
    object_key_raw: &str,
    legacy: bool,
) -> anyhow::Result<bool> {
    let config = ctx
        .storage()
        .session()
        .await?
        .get_system_config()
        .await?
        .sync;
    if config.connection_profile.is_none() && !config.enabled {
        // A never-configured installation has no sync references or read leases.
        return Ok(true);
    }
    let key = if legacy {
        format!(
            "legacy:{note_id}:{}",
            normalize_attachment_path(object_key_raw)
        )
    } else {
        format!("object:{object_key_raw}")
    };
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let mut after = None;
        loop {
            let ids = tx.list_sync_note_ids(after.as_deref(), PAGE).await?;
            if ids.is_empty() {
                break;
            }
            for id in &ids {
                if let Some(note) = tx.get_sync_note(id).await? {
                    if note.attachments.iter().any(|a| object_key(id, a) == key) {
                        return Ok(false);
                    }
                }
            }
            after = ids.last().cloned();
        }
        for namespace in ["pending", "inflight", "member"] {
            let mut after = None;
            loop {
                let rows = tx
                    .list_sync_records(namespace, after.as_deref(), PAGE)
                    .await?;
                if rows.is_empty() {
                    break;
                }
                after = rows.last().map(|r| r.0.clone());
                for (_, value) in rows {
                    let (event, attachments) = if namespace == "member" {
                        let m: Member = serde_json::from_value(value)?;
                        (m.event, m.attachments)
                    } else {
                        let p: Pending = serde_json::from_value(value)?;
                        (p.event, p.attachments)
                    };
                    if attachments
                        .iter()
                        .any(|a| object_key(&event.note_id, a) == key)
                    {
                        return Ok(false);
                    }
                }
            }
        }
        put(tx.as_ref(), "deleting", &key, &true).await?;
        Ok(true)
    }
    .await;
    finish_transaction(tx, result).await
}

pub(crate) async fn catalog_note_ids(
    s: &dyn StorageSession,
    key: &str,
) -> anyhow::Result<Vec<String>> {
    let config = s.get_system_config().await?.sync;
    if config.connection_profile.is_none() && !config.enabled {
        return Ok(Vec::new());
    }
    let mut selected = Vec::new();
    let mut after = None;
    loop {
        let ids = s.list_sync_note_ids(after.as_deref(), PAGE).await?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            if let Some(note) = s.get_sync_note(id).await? {
                if note.labels.iter().any(|l| l.key == key) {
                    selected.push(id.clone());
                }
            }
        }
        after = ids.last().cloned();
    }
    Ok(selected)
}
pub(crate) async fn capture_catalog(
    ctx: &Context,
    s: &dyn StorageSession,
    key: &str,
) -> anyhow::Result<()> {
    for id in catalog_note_ids(s, key).await? {
        capture_note(ctx, s, &id, false).await?;
    }
    Ok(())
}
/// Reserves an inventory snapshot only while it remains the current association event.
pub async fn reserve_sync_event(ctx: &Context, event: &SyncEvent) -> anyhow::Result<bool> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let Some(member) = get::<Member>(tx.as_ref(), "member", &event.note_id).await? else {
            return Ok(false);
        };
        if member.event != *event {
            return Ok(false);
        }
        ensure_not_deleting(tx.as_ref(), &event.note_id, &member.attachments).await?;
        put(
            tx.as_ref(),
            "inflight",
            &event.event_id,
            &Pending {
                event: event.clone(),
                attachments: member.attachments,
            },
        )
        .await?;
        Ok(true)
    }
    .await;
    finish_transaction(tx, result).await
}

async fn apply_fence(ctx: &Context, event: &SyncEvent) -> anyhow::Result<SyncApplyResult> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        anyhow::ensure!(tx.get_system_config().await?.sync.enabled, "sync is paused");
        let member: Option<Member> = get(tx.as_ref(), "member", &event.note_id).await?;
        if member.as_ref().is_some_and(|m| {
            m.generation > event.generation
                || (m.generation == event.generation && m.control > event.control)
        }) {
            return Ok(SyncApplyResult::Ignored);
        }
        if let Some(old) = get::<SyncEvent>(tx.as_ref(), "received", &event.event_id).await? {
            anyhow::ensure!(old == *event, "event ID reused with different payload");
            return Ok(SyncApplyResult::Duplicate);
        }
        tick(tx.as_ref(), Some(&event.control)).await?;
        put(
            tx.as_ref(),
            "member",
            &event.note_id,
            &Member {
                generation: event.generation.clone(),
                control: event.control.clone(),
                attached: false,
                event: event.clone(),
                attachments: Vec::new(),
            },
        )
        .await?;
        if let Some(p) = get::<Pending>(tx.as_ref(), "pending", &event.note_id).await? {
            if p.event.generation <= event.generation {
                tx.delete_sync_record("pending", &event.note_id).await?;
            }
        }
        put(tx.as_ref(), "received", &event.event_id, event).await?;
        Ok(SyncApplyResult::Applied)
    }
    .await;
    finish_transaction(tx, result).await
}
/// Call only when the sole synchronization worker has no active session: at startup
/// or after dropping a session on retry, reconfiguration, failure, or shutdown.
pub async fn release_abandoned_sync_events(ctx: &Context) -> anyhow::Result<()> {
    let tx = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        loop {
            let rows = tx.list_sync_records("inflight", None, PAGE).await?;
            if rows.is_empty() {
                break;
            }
            for (key, _) in rows {
                tx.delete_sync_record("inflight", &key).await?;
            }
        }
        Ok(())
    }
    .await;
    finish_transaction(tx, result).await
}

pub(crate) async fn reset_unregistered_sync_profile(s: &dyn StorageSession) -> anyhow::Result<()> {
    // No remote peer has ever been bound. Retain local content versions and node identity.
    for namespace in [
        "member",
        "pending",
        "inflight",
        "ack",
        "received",
        "scheduler",
    ] {
        loop {
            let rows = s.list_sync_records(namespace, None, PAGE).await?;
            if rows.is_empty() {
                break;
            }
            for (key, _) in rows {
                s.delete_sync_record(namespace, &key).await?;
            }
        }
    }
    if let Some(mut id) = get::<Identity>(s, "identity", "local").await? {
        id.initialized = false;
        id.cursor = None;
        put(s, "identity", "local", &id).await?;
    }
    Ok(())
}

/// First import retains history; an actual recreation of a known UUID is a new local edit.
pub(crate) async fn capture_imported_note(
    ctx: &Context,
    s: &dyn StorageSession,
    id: &str,
) -> anyhow::Result<()> {
    let config = s.get_system_config().await?.sync;
    if config.connection_profile.is_none() && !config.enabled {
        return Ok(());
    }
    let recreated = get::<Head>(s, "head", id).await?.is_some();
    capture_with_mode(ctx, s, id, !recreated, false, recreated).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct BoundedLegacyOnlyStore;
    #[async_trait::async_trait]
    impl note_attachments::AttachmentStore for BoundedLegacyOnlyStore {
        async fn read_legacy(&self, _note_id: &str, _path: &str) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("unbounded legacy read was used")
        }
        async fn read_legacy_bounded(
            &self,
            _note_id: &str,
            _path: &str,
            max_bytes: u64,
        ) -> anyhow::Result<Vec<u8>> {
            assert_eq!(max_bytes, DEFAULT_SYNC_MAX_BLOB_BYTES);
            Ok(b"bounded legacy".to_vec())
        }
        fn info(&self) -> note_attachments::AttachmentStoreInfo {
            note_attachments::AttachmentStoreInfo {
                engine: "bounded-test".into(),
                location: None,
            }
        }
    }

    #[tokio::test]
    async fn legacy_snapshot_hashing_uses_bounded_reads() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(
            note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let ctx = Context::new(
            backend,
            Arc::new(note_embedding::StubEmbedder),
            Arc::new(BoundedLegacyOnlyStore),
        );
        let note = Note {
            id: uuid::Uuid::new_v4().to_string(),
            title: "Legacy".into(),
            content: "Legacy note".into(),
            labels: vec![],
            attachments: vec![NoteAttachment {
                id: "file".into(),
                path: "legacy.txt".into(),
                mime: "text/plain".into(),
                description: "".into(),
                content: vec![],
                storage: None,
            }],
            created_at: 1,
            updated_at: 1,
            deleted_at: None,
            revision: 1,
        };
        let captured = snapshot(&ctx, &note).await.unwrap();
        assert_eq!(captured.attachments[0].size_bytes, 14);
        assert_eq!(
            captured.attachments[0].checksum_sha256,
            format!("{:x}", Sha256::digest(b"bounded legacy"))
        );
    }

    #[tokio::test]
    async fn cleanup_reservation_prevents_writer_from_committing_new_reference() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(
            note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let ctx = Context::new(
            backend.clone(),
            Arc::new(note_embedding::StubEmbedder),
            Arc::new(note_attachments::FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        );
        let mut config = note_core::SystemConfig::default();
        config.sync = SyncConfig {
            enabled: true,
            connection_profile: Some("gao_note".into()),
            rules: vec![SyncRule {
                id: "project".into(),
                enabled: true,
                selector: "project=bear_note".into(),
            }],
        };
        crate::update_system_config(&ctx, &config).await.unwrap();
        initialize_sync(&ctx).await.unwrap();
        let note = crate::save_note(
            &ctx,
            crate::SaveNoteInput {
                title: "Source".into(),
                content: "Original".into(),
                labels: vec![("project".into(), "bear_note".into())],
                attachments: vec![NoteAttachment {
                    id: "file".into(),
                    path: "file.txt".into(),
                    mime: "text/plain".into(),
                    description: "".into(),
                    content: b"immutable".to_vec(),
                    storage: None,
                }],
            },
        )
        .await
        .unwrap();
        let key = note.attachments[0]
            .storage
            .as_ref()
            .unwrap()
            .object_key
            .clone();
        crate::update_note(
            &ctx,
            &note.id,
            note.revision,
            crate::SaveNoteInput {
                title: note.title,
                content: "Replaced without attachment".into(),
                labels: vec![("project".into(), "bear_note".into())],
                attachments: vec![],
            },
        )
        .await
        .unwrap();
        assert!(reserve_sync_attachment_cleanup(&ctx, &note.id, &key, false)
            .await
            .unwrap());
        let identity_before = ctx
            .storage()
            .session()
            .await
            .unwrap()
            .get_sync_record("identity", "local")
            .await
            .unwrap();
        let reference_id = uuid::Uuid::new_v4().to_string();
        let mut attachment = note.attachments[0].clone();
        attachment.content.clear();
        let tx = ctx
            .storage()
            .begin(TransactionMode::Immediate)
            .await
            .unwrap();
        tx.insert_note(NewNote {
            id: &reference_id,
            title: "New reference",
            content: "References a reserved immutable object",
            attachments: &[attachment],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
        tx.attach_label(&reference_id, "project", "bear_note")
            .await
            .unwrap();
        let result = capture_note(&ctx, tx.as_ref(), &reference_id, false).await;
        let error = finish_transaction(tx, result).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("attachment cleanup has reserved object"));
        let session = ctx.storage().session().await.unwrap();
        assert!(session
            .get_sync_note(&reference_id)
            .await
            .unwrap()
            .is_none());
        for namespace in ["head", "member", "pending"] {
            assert!(session
                .get_sync_record(namespace, &reference_id)
                .await
                .unwrap()
                .is_none());
        }
        assert_eq!(
            session.get_sync_record("identity", "local").await.unwrap(),
            identity_before
        );
        assert_eq!(
            session
                .get_sync_record("deleting", &format!("object:{key}"))
                .await
                .unwrap(),
            Some(serde_json::json!(true))
        );
    }
}
