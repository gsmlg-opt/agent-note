//! Versioned, transport-independent note synchronization wire types.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub connection_profile: Option<String>,
    #[serde(default)]
    pub rules: Vec<SyncRule>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRule {
    pub id: String,
    pub enabled: bool,
    pub selector: String,
}

/// Ordered lexicographically; node_id provides a deterministic tie break.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub millis: i64,
    pub counter: u64,
    pub node_id: String,
}
impl Hlc {
    pub fn advance(&self, now: i64, observed: Option<&Hlc>, node_id: &str) -> Option<Self> {
        let observed_millis = observed.map_or(i64::MIN, |v| v.millis);
        let millis = now.max(self.millis).max(observed_millis);
        let counter = if millis == self.millis || millis == observed_millis {
            let local = if millis == self.millis {
                self.counter
            } else {
                0
            };
            let remote = observed
                .filter(|v| v.millis == millis)
                .map_or(0, |v| v.counter);
            local.max(remote).checked_add(1)?
        } else {
            0
        };
        Some(Self {
            millis,
            counter,
            node_id: node_id.to_owned(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncLabel {
    pub key: String,
    pub value: String,
    pub description: String,
    pub value_type: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAttachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
    pub size_bytes: u64,
    pub checksum_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub title: String,
    pub content: String,
    pub labels: Vec<SyncLabel>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub attachments: Vec<SyncAttachment>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncEventKind {
    Attach,
    Snapshot,
    Detach,
    Purge,
    Fence,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncEvent {
    pub event_id: String,
    pub note_id: String,
    pub generation: Hlc,
    pub control: Hlc,
    pub version: Hlc,
    pub kind: SyncEventKind,
    pub snapshot: Option<SyncSnapshot>,
    pub snapshot_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAck {
    pub event_id: String,
    pub note_id: String,
    pub generation: Hlc,
    pub control: Hlc,
    pub kind: SyncEventKind,
    pub version: Hlc,
    pub snapshot_hash: String,
}
impl SyncEvent {
    pub fn acknowledgement(&self) -> SyncAck {
        SyncAck {
            event_id: self.event_id.clone(),
            note_id: self.note_id.clone(),
            generation: self.generation.clone(),
            control: self.control.clone(),
            kind: self.kind,
            version: self.version.clone(),
            snapshot_hash: self.snapshot_hash.clone(),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub node_id: String,
    pub initialized: bool,
    pub pending_notes: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncInventory {
    pub events: Vec<SyncEvent>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncApplyResult {
    Applied,
    Ignored,
    Duplicate,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_advances_with_backward_wall_clock_and_observed_clock() {
        let old = Hlc {
            millis: 100,
            counter: 4,
            node_id: "a".into(),
        };
        assert_eq!(old.advance(90, None, "a").unwrap().counter, 5);
        let remote = Hlc {
            millis: 200,
            counter: 8,
            node_id: "b".into(),
        };
        let next = old.advance(90, Some(&remote), "a").unwrap();
        assert_eq!((next.millis, next.counter), (200, 9));
        assert!(next > remote);
        assert!(Hlc {
            counter: u64::MAX,
            ..old
        }
        .advance(100, None, "a")
        .is_none());
    }
    #[test]
    fn omitted_sync_config_is_disabled() {
        let config: crate::SystemConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.sync.enabled);
        assert!(config.sync.rules.is_empty());
    }
}
