use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::workspace_management::WorkspacePolicy;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalCounts {
    pub ready: i64,
    pub assigned: i64,
    pub running: i64,
    pub blocked: i64,
    pub review: i64,
    pub scheduled: i64,
    pub upcoming_deadline: i64,
    pub failed: i64,
    pub expired_lease: i64,
    pub completed: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub archived_at: Option<i64>,
    pub workspace_revision: i64,
    pub evaluated_at: i64,
    pub counts: OperationalCounts,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub id: String,
    pub path: String,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OrgTimestamp {
    pub raw: String,
    pub local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub id: String,
    pub workspace_id: String,
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_type: String,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<OrgTimestamp>,
    pub deadline: Option<OrgTimestamp>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub item: WorkItem,
    pub satisfied: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteLink {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
    pub available: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptNote {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub workspace_id: String,
    pub work_item_id: String,
    pub attempt_number: i64,
    pub actor_id: String,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub error: Option<String>,
    pub result_summary: Option<String>,
    pub review_outcome: Option<String>,
    pub note_refs: Vec<AttemptNote>,
    pub artifacts: Vec<Artifact>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub workspace_id: String,
    pub sequence: i64,
    pub subject_kind: String,
    pub subject_id: String,
    pub actor_id: String,
    pub attempt_id: Option<String>,
    pub event_type: String,
    pub occurred_at: i64,
    pub summary: String,
    pub metadata: Value,
    pub previous_state: Option<String>,
    pub resulting_state: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySegment {
    pub workspace_id: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum Origin {
    WorkItem {
        work_item_id: String,
        item: Option<Box<WorkItem>>,
    },
    Event {
        event_id: String,
        event: Option<Box<Event>>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub id: String,
    pub workspace_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub kind: String,
    pub actor_id: String,
    pub acquired_at: i64,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
    pub status: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalView {
    Ready,
    Assigned,
    Running,
    Blocked,
    Review,
    Scheduled,
    UpcomingDeadline,
    Failed,
    ExpiredLease,
    Completed,
}

impl OperationalView {
    pub const ALL: [Self; 10] = [
        Self::Ready,
        Self::Assigned,
        Self::Running,
        Self::Blocked,
        Self::Review,
        Self::Scheduled,
        Self::UpcomingDeadline,
        Self::Failed,
        Self::ExpiredLease,
        Self::Completed,
    ];

    pub const fn is_agenda(self) -> bool {
        matches!(self, Self::Scheduled | Self::UpcomingDeadline)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyStatus {
    Ready,
    RecoveryCandidate,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLeaseStatus {
    Unleased,
    Active,
    Expired,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalItem {
    pub item: WorkItem,
    pub attempt_count: i64,
    pub current_attempt_status: Option<String>,
    pub retry_exhausted: bool,
    pub ready_status: Option<ReadyStatus>,
    pub review_lease_status: Option<ReviewLeaseStatus>,
    pub lease: Option<Lease>,
    pub completion_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalPage {
    pub items: Vec<OperationalItem>,
    pub next_cursor: Option<String>,
    pub evaluated_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptBudget {
    pub execution_attempt_count: u32,
    pub max_attempts: u32,
    pub remaining_attempts: u32,
    pub retry_exhausted: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryStatus {
    pub eligible: bool,
    pub candidate: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalContext {
    pub classifications: Vec<OperationalView>,
    pub readiness: Option<ReadyStatus>,
    pub blockers: Vec<String>,
    pub attempt_budget: AttemptBudget,
    pub recovery: RecoveryStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemContext {
    pub workspace: Workspace,
    pub workspace_revision: i64,
    pub document: Document,
    pub item: WorkItem,
    pub parent: Option<WorkItem>,
    pub children: Vec<WorkItem>,
    pub dependencies: Vec<Dependency>,
    pub note_links: Vec<NoteLink>,
    pub attempts: Vec<Attempt>,
    pub origin: Option<Origin>,
    pub history_segments: Vec<HistorySegment>,
    pub lease: Option<Lease>,
    pub operational: OperationalContext,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn document_deserializes_lifecycle_state_and_rejects_unknown_fields() {
        let active: Document = serde_json::from_value(json!({
            "id": "20000000-0000-4000-8000-000000000001",
            "path": "work.org",
            "revision": 4,
            "archived_at": null
        }))
        .unwrap();
        assert_eq!(active.archived_at, None);

        let archived: Document = serde_json::from_value(json!({
            "id": "20000000-0000-4000-8000-000000000001",
            "path": "work.org",
            "revision": 5,
            "archived_at": 1787539200
        }))
        .unwrap();
        assert_eq!(archived.archived_at, Some(1787539200));

        assert!(serde_json::from_value::<Document>(json!({
            "id": "20000000-0000-4000-8000-000000000001",
            "path": "work.org",
            "revision": 5,
            "archived_at": null,
            "source": "forbidden"
        }))
        .is_err());
    }

    fn item() -> Value {
        json!({
            "id": "30000000-0000-4000-8000-000000000001",
            "workspace_id": "10000000-0000-4000-8000-000000000001",
            "document_id": "20000000-0000-4000-8000-000000000001",
            "parent_id": null,
            "item_type": "task",
            "title": "Ship console",
            "state": "RUNNING",
            "priority": "A",
            "scheduled": {"raw":"<2026-08-05 Wed 09:00>","local":"2026-08-05T09:00:00","timezone":"Asia/Shanghai","utc_timestamp":1785882000},
            "deadline": null,
            "assignee": "agent-one",
            "requires_review": true,
            "created_at": 1785880000,
            "tags": ["delivery", "org"]
        })
    }

    fn lease() -> Value {
        json!({
            "id":"lease-1","workspace_id":"10000000-0000-4000-8000-000000000001",
            "work_item_id":"30000000-0000-4000-8000-000000000001","attempt_id":"attempt-1",
            "kind":"execution","actor_id":"agent-one","acquired_at":10,
            "last_heartbeat_at":11,"expires_at":20,"status":"active"
        })
    }

    #[test]
    fn deserializes_workspace_summary_and_all_ten_counts() {
        let summary: WorkspaceSummary = serde_json::from_value(json!({
            "workspace_id":"10000000-0000-4000-8000-000000000001","slug":"delivery",
            "display_name":"Delivery","description":"Operations","timezone":"Asia/Shanghai",
            "archived_at":null,"workspace_revision":7,"evaluated_at":20,
            "counts":{"ready":1,"assigned":2,"running":3,"blocked":4,"review":5,
                "scheduled":6,"upcoming_deadline":7,"failed":8,"expired_lease":9,"completed":10}
        }))
        .unwrap();
        assert_eq!(summary.counts.completed, 10);
        assert_eq!(OperationalView::ALL.len(), 10);
    }

    #[test]
    fn deserializes_every_operational_view_and_safe_lease() {
        for view in OperationalView::ALL {
            let row = json!({
                "item":item(),"attempt_count":1,"current_attempt_status":"running",
                "retry_exhausted":false,
                "ready_status": if view == OperationalView::Ready { json!("recovery_candidate") } else { Value::Null },
                "review_lease_status": if view == OperationalView::Review { json!("active") } else { Value::Null },
                "lease":lease(),"completion_at":null
            });
            let page: OperationalPage = serde_json::from_value(
                json!({"items":[row],"next_cursor":"next","evaluated_at":20}),
            )
            .unwrap();
            assert_eq!(page.items[0].lease.as_ref().unwrap().status, "active");
            assert_eq!(
                view.is_agenda(),
                matches!(
                    view,
                    OperationalView::Scheduled | OperationalView::UpcomingDeadline
                )
            );
        }
    }

    #[test]
    fn deserializes_complete_recovery_context_and_ordered_events() {
        let mut child = item();
        child["id"] = json!("30000000-0000-4000-8000-000000000002");
        let mut dependency = item();
        dependency["id"] = json!("30000000-0000-4000-8000-000000000003");
        let event = |id: &str, sequence: i64| {
            json!({
                "id":id,"workspace_id":"10000000-0000-4000-8000-000000000001","sequence":sequence,
                "subject_kind":"work_item","subject_id":"30000000-0000-4000-8000-000000000001",
                "actor_id":"agent-one","attempt_id":"attempt-1","event_type":"progress",
                "occurred_at":100-sequence,"summary":"progress","metadata":{"safe":true},
                "previous_state":"RUNNING","resulting_state":"RUNNING"
            })
        };
        let context: ItemContext = serde_json::from_value(json!({
            "workspace":{"id":"10000000-0000-4000-8000-000000000001","slug":"delivery",
                "display_name":"Delivery","description":"Operations","timezone":"Asia/Shanghai",
                "policy_schema_version":1,"policy":WorkspacePolicy::engineering_default(),"revision":7,"archived_at":null},
            "workspace_revision":7,"document":{"id":"20000000-0000-4000-8000-000000000001","path":"work.org","revision":4,"archived_at":null},
            "item":item(),"parent":null,"children":[child],
            "dependencies":[{"item":dependency,"satisfied":true}],
            "note_links":[{"purpose":"context","note_id":"note-1","description":"deleted source","available":false}],
            "attempts":[{"id":"attempt-1","workspace_id":"10000000-0000-4000-8000-000000000001",
                "work_item_id":"30000000-0000-4000-8000-000000000001","attempt_number":1,
                "actor_id":"agent-one","status":"running","started_at":10,"ended_at":null,
                "error":null,"result_summary":null,"review_outcome":null,
                "note_refs":[{"purpose":"evidence","note_id":"note-2","description":"log"}],
                "artifacts":[{"uri":"artifact://1","media_type":"text/plain","name":"log","description":"build log"}],
                "metadata":{"phase":"build"}}],
            "origin":{"kind":"work_item","work_item_id":"30000000-0000-4000-8000-000000000009","item":null},
            "history_segments":[{"workspace_id":"10000000-0000-4000-8000-000000000001","events":[event("event-1",1),event("event-2",2)]}],
            "lease":lease(),
            "operational":{"classifications":["running","expired_lease"],"readiness":null,"blockers":["lease_expired"],
                "attempt_budget":{"execution_attempt_count":1,"max_attempts":3,"remaining_attempts":2,"retry_exhausted":false},
                "recovery":{"eligible":true,"candidate":true,"blockers":[]}}
        })).unwrap();
        assert!(context.dependencies[0].satisfied);
        assert!(!context.note_links[0].available);
        assert_eq!(
            context.history_segments[0]
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn read_models_reject_sensitive_transport_fields() {
        let mut unsafe_lease = lease();
        unsafe_lease["fencing_token"] = json!("secret");
        assert!(serde_json::from_value::<Lease>(unsafe_lease).is_err());

        let safe = serde_json::to_value(serde_json::from_value::<Lease>(lease()).unwrap()).unwrap();
        let rendered = safe.to_string();
        for forbidden in [
            "fencing_token",
            "token_hash",
            "operation_id",
            "auth",
            "authorization",
        ] {
            assert!(!rendered.contains(forbidden));
        }

        let production_source = include_str!("model.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "fencing_token",
            "token_hash",
            "operation_id",
            "authorization",
            "auth_context",
        ] {
            assert!(
                !production_source.contains(forbidden),
                "read model exposes forbidden field: {forbidden}"
            );
        }
    }
}
