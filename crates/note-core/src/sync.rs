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

/// Optional state_query_v1 messages; queries never substitute for durable ACKs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncStateQuery {
    pub query_id: String,
    pub items: Vec<SyncAck>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAssociation {
    Missing,
    Attached,
    Detached,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncStateHead {
    pub version: Hlc,
    pub snapshot_hash: String,
    pub attachments_available: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncStateItem {
    pub requested: SyncAck,
    pub association: SyncAssociation,
    pub member: Option<SyncAck>,
    pub head: Option<SyncStateHead>,
}
impl SyncStateItem {
    /// Only normalized, nonpending snapshots may use this content equivalence.
    /// The caller must atomically recheck that the local member is unchanged and
    /// has no pending event before accepting the result.
    pub fn matches_snapshot(&self, local: &SyncAck) -> bool {
        self.requested == *local
            && local.kind == SyncEventKind::Snapshot
            && self.association == SyncAssociation::Attached
            && self.member.as_ref().is_some_and(|member| {
                member.note_id == local.note_id
                    && matches!(member.kind, SyncEventKind::Attach | SyncEventKind::Snapshot)
                    && member.generation == local.generation
                    && member.control == local.control
                    && member.version == local.version
                    && member.snapshot_hash == local.snapshot_hash
            })
            && self.head.as_ref().is_some_and(|head| {
                head.version == local.version
                    && head.snapshot_hash == local.snapshot_hash
                    && head.attachments_available == Some(true)
            })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncStateResponse {
    pub query_id: String,
    pub items: Vec<SyncStateItem>,
}

fn state_uuid(value: &str) -> bool {
    if value.len() == 32 {
        return value.bytes().all(|b| b.is_ascii_hexdigit());
    }
    let value = value
        .strip_prefix("urn:uuid:")
        .or_else(|| value.strip_prefix('{').and_then(|v| v.strip_suffix('}')))
        .unwrap_or(value);
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn state_clock(clock: &Hlc) -> bool {
    clock.millis >= 0 && state_uuid(&clock.node_id)
}

fn state_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn state_descriptor(descriptor: &SyncAck) -> bool {
    state_uuid(&descriptor.event_id)
        && state_uuid(&descriptor.note_id)
        && state_clock(&descriptor.generation)
        && state_clock(&descriptor.control)
        && state_clock(&descriptor.version)
        && descriptor.control >= descriptor.generation
        && (!matches!(
            descriptor.kind,
            SyncEventKind::Detach | SyncEventKind::Fence
        ) || descriptor.control > descriptor.generation)
        && state_hash(&descriptor.snapshot_hash)
}

impl SyncStateResponse {
    pub fn matches_query(&self, query: &SyncStateQuery) -> bool {
        let agree = |version: &Hlc, hash: &str, other_version: &Hlc, other_hash: &str| {
            version != other_version || hash == other_hash
        };
        let mut note_ids = std::collections::HashSet::new();
        let mut event_ids = std::collections::HashSet::new();
        state_uuid(&query.query_id)
            && !query.items.is_empty()
            && query.items.len() <= 32
            && self.query_id == query.query_id
            && self.items.len() == query.items.len()
            && self.items.iter().zip(&query.items).all(|(item, expected)| {
                if !note_ids.insert(&expected.note_id)
                    || !event_ids.insert(&expected.event_id)
                    || !state_descriptor(expected)
                    || item.requested != *expected
                {
                    return false;
                }
                if item.association == SyncAssociation::Missing {
                    return item.member.is_none() && item.head.is_none();
                }
                let Some(member) = &item.member else {
                    return false;
                };
                state_descriptor(member)
                    && member.note_id == expected.note_id
                    && match item.association {
                        SyncAssociation::Attached => matches!(
                            member.kind,
                            SyncEventKind::Attach | SyncEventKind::Snapshot | SyncEventKind::Purge
                        ),
                        SyncAssociation::Detached => {
                            item.head.is_none()
                                && matches!(
                                    member.kind,
                                    SyncEventKind::Detach | SyncEventKind::Fence
                                )
                        }
                        SyncAssociation::Missing => false,
                    }
                    && agree(
                        &member.version,
                        &member.snapshot_hash,
                        &expected.version,
                        &expected.snapshot_hash,
                    )
                    && item.head.as_ref().is_none_or(|head| {
                        state_clock(&head.version)
                            && state_hash(&head.snapshot_hash)
                            && agree(
                                &head.version,
                                &head.snapshot_hash,
                                &expected.version,
                                &expected.snapshot_hash,
                            )
                            && agree(
                                &head.version,
                                &head.snapshot_hash,
                                &member.version,
                                &member.snapshot_hash,
                            )
                    })
            })
    }
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
    fn descriptor(kind: SyncEventKind) -> SyncAck {
        let stamp = Hlc {
            millis: 1,
            counter: 0,
            node_id: "44444444-4444-4444-8444-444444444444".into(),
        };
        SyncAck {
            event_id: "11111111-1111-4111-8111-111111111111".into(),
            note_id: "22222222-2222-4222-8222-222222222222".into(),
            generation: stamp.clone(),
            control: stamp.clone(),
            version: stamp,
            kind,
            snapshot_hash: "a".repeat(64),
        }
    }
    #[test]
    fn comparison_requires_current_attached_head_and_available_objects() {
        let local = descriptor(SyncEventKind::Snapshot);
        let mut remote_member = local.clone();
        remote_member.kind = SyncEventKind::Attach;
        remote_member.event_id = "33333333-3333-4333-8333-333333333333".into();
        let equal = SyncStateItem {
            requested: local.clone(),
            association: SyncAssociation::Attached,
            member: Some(remote_member),
            head: Some(SyncStateHead {
                version: local.version.clone(),
                snapshot_hash: local.snapshot_hash.clone(),
                attachments_available: Some(true),
            }),
        };
        assert!(equal.matches_snapshot(&local));
        let mut unavailable = equal.clone();
        unavailable.head.as_mut().unwrap().attachments_available = None;
        assert!(!unavailable.matches_snapshot(&local));
        unavailable.head.as_mut().unwrap().attachments_available = Some(false);
        assert!(!unavailable.matches_snapshot(&local));
        let mut stale_head = equal.clone();
        stale_head.head.as_mut().unwrap().version.counter += 1;
        assert!(!stale_head.matches_snapshot(&local));
        let mut changed_control = equal.clone();
        changed_control.member.as_mut().unwrap().control.counter += 1;
        assert!(!changed_control.matches_snapshot(&local));
        let mut missing = equal.clone();
        missing.association = SyncAssociation::Missing;
        assert!(!missing.matches_snapshot(&local));
        for kind in [
            SyncEventKind::Attach,
            SyncEventKind::Detach,
            SyncEventKind::Fence,
            SyncEventKind::Purge,
        ] {
            assert!(!equal.matches_snapshot(&descriptor(kind)));
        }
    }

    #[test]
    fn state_query_replies_must_echo_all_descriptors_exactly_once() {
        let query = SyncStateQuery {
            query_id: "55555555-5555-4555-8555-555555555555".into(),
            items: vec![descriptor(SyncEventKind::Snapshot)],
        };
        let mut response = SyncStateResponse {
            query_id: query.query_id.clone(),
            items: vec![SyncStateItem {
                requested: query.items[0].clone(),
                association: SyncAssociation::Missing,
                member: None,
                head: None,
            }],
        };
        assert!(response.matches_query(&query));
        response.items.push(response.items[0].clone());
        assert!(!response.matches_query(&query));
        response.items.pop();
        response.items[0].requested.version.counter += 1;
        assert!(!response.matches_query(&query));
        response.items[0].requested = query.items[0].clone();
        response.query_id = "old-session-query".into();
        assert!(!response.matches_query(&query));
    }

    #[test]
    fn state_response_rejects_event_ids_reused_for_distinct_notes() {
        let first = descriptor(SyncEventKind::Snapshot);
        let mut second = first.clone();
        second.note_id = "66666666-6666-4666-8666-666666666666".into();
        let query = SyncStateQuery {
            query_id: "55555555-5555-4555-8555-555555555555".into(),
            items: vec![first, second],
        };
        let response = SyncStateResponse {
            query_id: query.query_id.clone(),
            items: query
                .items
                .iter()
                .map(|requested| SyncStateItem {
                    requested: requested.clone(),
                    association: SyncAssociation::Missing,
                    member: None,
                    head: None,
                })
                .collect(),
        };
        assert!(!response.matches_query(&query));
    }

    fn attached_response() -> (SyncStateQuery, SyncStateResponse) {
        let requested = descriptor(SyncEventKind::Snapshot);
        let query = SyncStateQuery {
            query_id: "55555555-5555-4555-8555-555555555555".into(),
            items: vec![requested.clone()],
        };
        let response = SyncStateResponse {
            query_id: query.query_id.clone(),
            items: vec![SyncStateItem {
                requested: requested.clone(),
                association: SyncAssociation::Attached,
                member: Some(requested.clone()),
                head: Some(SyncStateHead {
                    version: requested.version,
                    snapshot_hash: requested.snapshot_hash,
                    attachments_available: Some(true),
                }),
            }],
        };
        (query, response)
    }

    #[test]
    fn state_response_rejects_invalid_head_clock_and_hash() {
        let (query, valid) = attached_response();
        assert!(valid.matches_query(&query));
        let mut unknown_head = valid.clone();
        unknown_head.items[0].head = None;
        assert!(unknown_head.matches_query(&query));
        assert!(!unknown_head.items[0].matches_snapshot(&query.items[0]));
        for invalid in 0..4 {
            let mut response = valid.clone();
            let head = response.items[0].head.as_mut().unwrap();
            match invalid {
                0 => head.version.millis = -1,
                1 => head.version.node_id = "invalid".into(),
                2 => head.snapshot_hash = "garbage".into(),
                _ => head.snapshot_hash = "G".repeat(64),
            }
            assert!(!response.matches_query(&query), "invalid head {invalid}");
        }
    }

    #[test]
    fn state_response_rejects_invalid_or_incoherent_membership() {
        let (query, valid) = attached_response();
        for invalid in 0..8 {
            let mut response = valid.clone();
            let member = response.items[0].member.as_mut().unwrap();
            match invalid {
                0 => member.event_id = "invalid".into(),
                1 => member.generation.node_id = "invalid".into(),
                2 => member.control.millis = 0,
                3 => member.snapshot_hash = "invalid".into(),
                4 => member.kind = SyncEventKind::Detach,
                5 => member.kind = SyncEventKind::Fence,
                6 => response.items[0].association = SyncAssociation::Detached,
                _ => response.items[0].member = None,
            }
            assert!(!response.matches_query(&query), "invalid member {invalid}");
        }
        let mut detached = valid;
        detached.items[0].association = SyncAssociation::Detached;
        detached.items[0].head = None;
        let member = detached.items[0].member.as_mut().unwrap();
        member.kind = SyncEventKind::Fence;
        member.control.counter += 1;
        assert!(detached.matches_query(&query));
        detached.items[0].head = Some(SyncStateHead {
            version: query.items[0].version.clone(),
            snapshot_hash: query.items[0].snapshot_hash.clone(),
            attachments_available: Some(true),
        });
        assert!(!detached.matches_query(&query));
        detached.items[0].head = None;
        detached.items[0].member.as_mut().unwrap().control.counter = 0;
        assert!(!detached.matches_query(&query));
    }

    #[test]
    fn state_response_rejects_equal_version_conflicting_hashes() {
        let (query, valid) = attached_response();
        for conflict in 0..3 {
            let mut response = valid.clone();
            match conflict {
                0 => response.items[0].head.as_mut().unwrap().snapshot_hash = "b".repeat(64),
                1 => response.items[0].member.as_mut().unwrap().snapshot_hash = "b".repeat(64),
                _ => {
                    let member = response.items[0].member.as_mut().unwrap();
                    member.version.counter += 1;
                    let head = response.items[0].head.as_mut().unwrap();
                    head.version.counter += 1;
                    head.snapshot_hash = "b".repeat(64);
                }
            }
            assert!(!response.matches_query(&query), "conflict {conflict}");
        }
        let mut newer = valid;
        let head = newer.items[0].head.as_mut().unwrap();
        head.version.counter += 1;
        head.snapshot_hash = "b".repeat(64);
        assert!(newer.matches_query(&query));
        assert!(!newer.items[0].matches_snapshot(&query.items[0]));
    }
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
