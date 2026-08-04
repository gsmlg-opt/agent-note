use std::path::PathBuf;
use std::{convert::Infallible, fmt, str::FromStr};

pub const EMBEDDING_DIMENSION: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareAndSwap<T> {
    Applied(T),
    NotFound,
    Conflict { current_revision: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgWorkspace {
    pub id: note_org::WorkspaceId,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: note_org::WorkspacePolicy,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

pub struct NewOrgWorkspace<'a> {
    pub id: note_org::WorkspaceId,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: &'a str,
    pub timezone: &'a str,
    pub policy_schema_version: i64,
    pub policy: &'a note_org::WorkspacePolicy,
    pub now: i64,
}

pub struct OrgWorkspaceUpdate<'a> {
    pub id: note_org::WorkspaceId,
    pub expected_revision: i64,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: &'a str,
    pub timezone: &'a str,
    pub policy_schema_version: i64,
    pub policy: &'a note_org::WorkspacePolicy,
    pub archived_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDocument {
    pub id: note_org::DocumentId,
    pub workspace_id: note_org::WorkspaceId,
    pub path: String,
    pub source: String,
    pub content_hash: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewOrgDocument<'a> {
    pub id: note_org::DocumentId,
    pub workspace_id: note_org::WorkspaceId,
    pub path: &'a str,
    pub source: &'a str,
    pub content_hash: &'a str,
    pub now: i64,
}

pub struct OrgDocumentUpdate<'a> {
    pub id: note_org::DocumentId,
    pub expected_revision: i64,
    pub path: &'a str,
    pub source: &'a str,
    pub content_hash: &'a str,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOrgTimestamp {
    pub raw: String,
    pub local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgProjectedWorkItem {
    pub id: note_org::WorkItemId,
    pub workspace_id: note_org::WorkspaceId,
    pub document_id: note_org::DocumentId,
    pub parent_id: Option<note_org::WorkItemId>,
    pub source_order: i64,
    pub item_type: note_org::WorkItemType,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<StoredOrgTimestamp>,
    pub deadline: Option<StoredOrgTimestamp>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub dependencies: Vec<note_org::WorkItemId>,
    pub note_links: Vec<note_org::NoteLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrgEventType {
    Creation,
    Assignment,
    Claim,
    Heartbeat,
    Release,
    Start,
    Progress,
    Block,
    Unblock,
    ReviewRequest,
    Approval,
    Rejection,
    Completion,
    Failure,
    Retry,
    Cancellation,
    LeaseExpiry,
    DependencyChange,
    ScheduleChange,
    DocumentImport,
    NoteLinkChange,
    WorkspaceChange,
    WorkspaceArchive,
    DocumentMove,
    ItemMove,
    TitleChange,
    PriorityChange,
    DeadlineChange,
    /// Preserves an event name written by schema v3 or a future producer.
    /// Pipelines emit the known variants, while storage keeps audit rows
    /// forward- and backward-readable.
    Other(String),
}

impl OrgEventType {
    pub const KNOWN: [Self; 28] = [
        Self::Creation,
        Self::Assignment,
        Self::Claim,
        Self::Heartbeat,
        Self::Release,
        Self::Start,
        Self::Progress,
        Self::Block,
        Self::Unblock,
        Self::ReviewRequest,
        Self::Approval,
        Self::Rejection,
        Self::Completion,
        Self::Failure,
        Self::Retry,
        Self::Cancellation,
        Self::LeaseExpiry,
        Self::DependencyChange,
        Self::ScheduleChange,
        Self::DocumentImport,
        Self::NoteLinkChange,
        Self::WorkspaceChange,
        Self::WorkspaceArchive,
        Self::DocumentMove,
        Self::ItemMove,
        Self::TitleChange,
        Self::PriorityChange,
        Self::DeadlineChange,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Creation => "creation",
            Self::Assignment => "assignment",
            Self::Claim => "claim",
            Self::Heartbeat => "heartbeat",
            Self::Release => "release",
            Self::Start => "start",
            Self::Progress => "progress",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::ReviewRequest => "review_request",
            Self::Approval => "approval",
            Self::Rejection => "rejection",
            Self::Completion => "completion",
            Self::Failure => "failure",
            Self::Retry => "retry",
            Self::Cancellation => "cancellation",
            Self::LeaseExpiry => "lease_expiry",
            Self::DependencyChange => "dependency_change",
            Self::ScheduleChange => "schedule_change",
            Self::DocumentImport => "document_import",
            Self::NoteLinkChange => "note_link_change",
            Self::WorkspaceChange => "workspace_change",
            Self::WorkspaceArchive => "workspace_archive",
            Self::DocumentMove => "document_move",
            Self::ItemMove => "item_move",
            Self::TitleChange => "title_change",
            Self::PriorityChange => "priority_change",
            Self::DeadlineChange => "deadline_change",
            Self::Other(value) => value,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl fmt::Display for OrgEventType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OrgEventType {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::KNOWN
            .into_iter()
            .find(|event_type| event_type.as_str() == value)
            .unwrap_or_else(|| Self::Other(value.to_owned())))
    }
}

impl serde::Serialize for OrgEventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for OrgEventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_else(|never| match never {}))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgAttemptStatus {
    Running,
    Submitted,
    Completed,
    Failed,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrgArtifactReference {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrgAttemptNoteReference {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgAttempt {
    pub id: String,
    /// Workspace that owned the item when this attempt began.
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub attempt_number: i64,
    pub actor_id: String,
    pub status: OrgAttemptStatus,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub error: Option<String>,
    pub result_summary: Option<String>,
    pub review_outcome: Option<String>,
    pub note_refs: Vec<OrgAttemptNoteReference>,
    pub artifacts: Vec<OrgArtifactReference>,
    pub metadata: serde_json::Value,
}

pub struct NewOrgAttempt<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub attempt_number: i64,
    pub actor_id: &'a str,
    pub status: OrgAttemptStatus,
    pub started_at: i64,
    pub note_refs: &'a [OrgAttemptNoteReference],
    pub artifacts: &'a [OrgArtifactReference],
    pub metadata: &'a serde_json::Value,
}

pub struct OrgAttemptUpdate<'a> {
    pub id: &'a str,
    pub expected_status: OrgAttemptStatus,
    pub status: OrgAttemptStatus,
    pub ended_at: i64,
    pub error: Option<&'a str>,
    pub result_summary: Option<&'a str>,
    pub review_outcome: Option<&'a str>,
    pub note_refs: &'a [OrgAttemptNoteReference],
    pub artifacts: &'a [OrgArtifactReference],
    pub metadata: &'a serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgLeaseKind {
    Execution,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgLeaseEndReason {
    Release,
    Completion,
    Failure,
    Block,
    Cancellation,
    LeaseExpiry,
    ReviewRequest,
    Approval,
    Rejection,
    Reassignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgLease {
    pub id: String,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub attempt_id: String,
    pub kind: OrgLeaseKind,
    pub actor_id: String,
    pub fencing_token_hash: String,
    pub acquired_at: i64,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<OrgLeaseEndReason>,
    pub expiry_event_id: Option<String>,
}

pub struct NewOrgLease<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
    pub attempt_id: &'a str,
    pub kind: OrgLeaseKind,
    pub actor_id: &'a str,
    pub fencing_token_hash: &'a str,
    pub acquired_at: i64,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
}

pub struct OrgLeaseUpdate<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<OrgLeaseEndReason>,
    pub expiry_event_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrgOperationalView {
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

impl OrgOperationalView {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgReadyMarker {
    Ready,
    RecoveryCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgReviewLeaseMarker {
    Unleased,
    Active,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgOperationalCursor {
    pub primary_at: Option<i64>,
    pub priority: Option<char>,
    pub deadline_at: Option<i64>,
    pub scheduled_at: Option<i64>,
    pub created_at: i64,
    pub workspace_id: note_org::WorkspaceId,
    pub work_item_id: note_org::WorkItemId,
}

pub struct OrgOperationalQuery<'a> {
    pub view: OrgOperationalView,
    pub workspace_ids: &'a [note_org::WorkspaceId],
    pub item_type: Option<note_org::WorkItemType>,
    pub state: Option<&'a str>,
    pub priority: Option<char>,
    pub tags: &'a [&'a str],
    pub assignee: Option<&'a str>,
    pub scheduled_from: Option<i64>,
    pub scheduled_to: Option<i64>,
    pub deadline_from: Option<i64>,
    pub deadline_to: Option<i64>,
    pub completed_from: Option<i64>,
    pub completed_to: Option<i64>,
    pub include_archived: bool,
    pub now: i64,
    pub after: Option<&'a OrgOperationalCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgOperationalRow {
    pub item: OrgProjectedWorkItem,
    pub attempt_count: i64,
    pub current_attempt_status: Option<OrgAttemptStatus>,
    pub retry_exhausted: bool,
    pub ready_marker: Option<OrgReadyMarker>,
    pub review_lease_marker: Option<OrgReviewLeaseMarker>,
    pub lease: Option<crate::SanitizedOrgLease>,
    pub completion_at: Option<i64>,
}

impl OrgOperationalRow {
    pub fn cursor(&self, view: OrgOperationalView) -> OrgOperationalCursor {
        let primary_at = match view {
            OrgOperationalView::Scheduled => self
                .item
                .scheduled
                .as_ref()
                .map(|value| value.utc_timestamp),
            OrgOperationalView::UpcomingDeadline => {
                self.item.deadline.as_ref().map(|value| value.utc_timestamp)
            }
            OrgOperationalView::ExpiredLease => self.lease.as_ref().map(|value| value.expires_at),
            _ => None,
        };
        OrgOperationalCursor {
            primary_at,
            priority: self.item.priority,
            deadline_at: self.item.deadline.as_ref().map(|value| value.utc_timestamp),
            scheduled_at: self
                .item
                .scheduled
                .as_ref()
                .map(|value| value.utc_timestamp),
            created_at: self.item.created_at,
            workspace_id: self.item.workspace_id,
            work_item_id: self.item.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgOperationalCounts {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgWorkspaceOperationalSummary {
    pub workspace_id: note_org::WorkspaceId,
    pub timezone: String,
    pub archived_at: Option<i64>,
    pub workspace_revision: i64,
    pub evaluated_at: i64,
    pub counts: OrgOperationalCounts,
}

pub struct NewOrgEvent<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub actor_id: &'a str,
    pub attempt_id: Option<&'a str>,
    pub event_type: OrgEventType,
    pub occurred_at: i64,
    pub summary: &'a str,
    pub metadata: &'a serde_json::Value,
    pub previous_state: Option<&'a str>,
    pub resulting_state: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgEvent {
    pub id: String,
    pub workspace_id: note_org::WorkspaceId,
    pub sequence: i64,
    pub subject_kind: String,
    pub subject_id: String,
    pub actor_id: String,
    pub attempt_id: Option<String>,
    pub event_type: OrgEventType,
    pub occurred_at: i64,
    pub summary: String,
    pub metadata: serde_json::Value,
    pub previous_state: Option<String>,
    pub resulting_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOrgOperation {
    pub workspace_id: note_org::WorkspaceId,
    pub operation_id: String,
    pub request_fingerprint: String,
    pub result: serde_json::Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteChunk {
    pub note_id: String,
    pub chunk_idx: i64,
    pub content_hash: String,
    pub content: String,
    pub note_revision: i64,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJob {
    pub id: i64,
    pub note_id: String,
    pub chunk_idx: i64,
    pub content_hash: String,
    pub content: String,
    pub note_revision: i64,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingEmbeddingNote {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDashboardStatus {
    pub embedded_note_count: usize,
    pub processing_note: Option<ProcessingEmbeddingNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveNoteSource {
    pub id: String,
    pub content: String,
    pub note_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub engine: String,
    pub location: Option<PathBuf>,
    pub size_bytes: Option<u64>,
}

pub struct NewNote<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub created_at: i64,
    pub updated_at: i64,
    pub note_revision: i64,
    pub deleted_at: Option<i64>,
}

pub struct NoteUpdate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
    pub note_revision: i64,
}

pub struct NoteFieldsUpdate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub updated_at: i64,
    pub note_revision: i64,
}

pub struct AttachmentMetadataUpdate<'a> {
    pub id: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
}

pub struct UpsertNoteChunk<'a> {
    pub note_id: &'a str,
    pub chunk_idx: i64,
    pub content_hash: &'a str,
    pub content: &'a str,
    pub note_revision: i64,
    pub status: &'a str,
    pub updated_at: i64,
}
