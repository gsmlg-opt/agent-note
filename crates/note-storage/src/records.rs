use std::path::PathBuf;
use std::{convert::Infallible, fmt, str::FromStr};

use crate::{StorageError, StorageErrorKind};

pub const EMBEDDING_DIMENSION: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentOperationKind {
    DeleteObject,
}

impl AttachmentOperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeleteObject => "delete_object",
        }
    }
}

impl fmt::Display for AttachmentOperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AttachmentOperationKind {
    type Err = StorageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "delete_object" => Ok(Self::DeleteObject),
            _ => Err(StorageError::new(
                StorageErrorKind::Corrupt,
                "attachment operation has an unknown kind",
            )),
        }
    }
}

impl<'de> serde::Deserialize<'de> for AttachmentOperationKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_str(&value).map_err(|error| serde::de::Error::custom(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentOperationStatus {
    Pending,
    Running,
    Completed,
    Dead,
}

impl AttachmentOperationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Dead => "dead",
        }
    }
}

impl fmt::Display for AttachmentOperationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AttachmentOperationStatus {
    type Err = StorageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "dead" => Ok(Self::Dead),
            _ => Err(StorageError::new(
                StorageErrorKind::Corrupt,
                "attachment operation has an unknown status",
            )),
        }
    }
}

impl<'de> serde::Deserialize<'de> for AttachmentOperationStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_str(&value).map_err(|error| serde::de::Error::custom(error.to_string()))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NewAttachmentOperation {
    pub id: String,
    pub kind: AttachmentOperationKind,
    pub note_id: String,
    pub attachment_id: String,
    pub storage_generation: String,
    pub object_key: String,
    pub next_attempt_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentOperation {
    pub id: String,
    pub kind: AttachmentOperationKind,
    pub note_id: String,
    pub attachment_id: String,
    pub storage_generation: String,
    pub object_key: String,
    pub status: AttachmentOperationStatus,
    pub attempts: i64,
    pub next_attempt_at: Option<i64>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl fmt::Debug for NewAttachmentOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewAttachmentOperation")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("note_id", &self.note_id)
            .field("attachment_id", &self.attachment_id)
            .field("storage_generation", &self.storage_generation)
            .field("object_key", &self.object_key)
            .field("next_attempt_at", &self.next_attempt_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl fmt::Debug for AttachmentOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentOperation")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("note_id", &self.note_id)
            .field("attachment_id", &self.attachment_id)
            .field("storage_generation", &self.storage_generation)
            .field("object_key", &self.object_key)
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field("next_attempt_at", &self.next_attempt_at)
            .field("lease_owner", &self.lease_owner)
            .field("lease_expires_at", &self.lease_expires_at)
            .field(
                "last_error",
                &self.last_error.as_ref().map(|_| "[REDACTED]"),
            )
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareAndSwap<T> {
    Applied(T),
    NotFound,
    Conflict { current_revision: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteMutationResult<T> {
    Applied {
        value: T,
        revision: i64,
    },
    NotFound,
    Conflict {
        expected_revision: i64,
        current_revision: i64,
    },
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
    pub archived_at: Option<i64>,
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
    ResultSubmission,
    Transition,
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
    pub const KNOWN: [Self; 30] = [
        Self::Creation,
        Self::Assignment,
        Self::Claim,
        Self::Heartbeat,
        Self::Release,
        Self::Start,
        Self::Progress,
        Self::ResultSubmission,
        Self::Transition,
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
            Self::ResultSubmission => "result_submission",
            Self::Transition => "transition",
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

#[derive(Clone, PartialEq, Eq)]
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

impl fmt::Debug for OrgLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrgLease")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("work_item_id", &self.work_item_id)
            .field("attempt_id", &self.attempt_id)
            .field("kind", &self.kind)
            .field("actor_id", &self.actor_id)
            .field("fencing_token_hash", &"[REDACTED]")
            .field("acquired_at", &self.acquired_at)
            .field("last_heartbeat_at", &self.last_heartbeat_at)
            .field("expires_at", &self.expires_at)
            .field("ended_at", &self.ended_at)
            .field("end_reason", &self.end_reason)
            .field("expiry_event_id", &self.expiry_event_id)
            .finish()
    }
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
    pub priority_is_none: bool,
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

#[derive(Clone, PartialEq, Eq)]
pub struct StoredOrgOperation {
    pub workspace_id: note_org::WorkspaceId,
    pub operation_id: String,
    pub request_fingerprint: String,
    pub result: serde_json::Value,
    pub created_at: i64,
}

impl fmt::Debug for StoredOrgOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredOrgOperation")
            .field("workspace_id", &self.workspace_id)
            .field("operation_id", &self.operation_id)
            .field("request_fingerprint", &self.request_fingerprint)
            .field("result", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .finish()
    }
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
    pub expected_revision: i64,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
}

pub struct NoteFieldsUpdate<'a> {
    pub id: &'a str,
    pub expected_revision: i64,
    pub title: &'a str,
    pub content: &'a str,
    pub updated_at: i64,
}

pub struct AttachmentMetadataUpdate<'a> {
    pub id: &'a str,
    pub expected_revision: i64,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedNoteSnapshot {
    pub content: String,
    pub revision: i64,
    pub attachments: Vec<note_core::NoteAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PersistedAttachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
}

#[derive(serde::Deserialize)]
struct PersistedAttachmentJson {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    object_key: PresentField<String>,
    #[serde(default)]
    storage_generation: PresentField<String>,
    #[serde(default)]
    size_bytes: PresentField<u64>,
    #[serde(default)]
    checksum_sha256: PresentField<String>,
}

enum PresentField<T> {
    Missing,
    Value(T),
}

impl<T> Default for PresentField<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> PresentField<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Self::Missing => None,
            Self::Value(value) => Some(value),
        }
    }
}

impl<'de, T> serde::Deserialize<'de> for PresentField<T>
where
    T: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Value)
    }
}

impl<'de> serde::Deserialize<'de> for PersistedAttachment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let attachment =
            <PersistedAttachmentJson as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            object_key: attachment.object_key.into_option(),
            storage_generation: attachment.storage_generation.into_option(),
            size_bytes: attachment.size_bytes.into_option(),
            checksum_sha256: attachment.checksum_sha256.into_option(),
        })
    }
}

impl From<&note_core::NoteAttachment> for PersistedAttachment {
    fn from(attachment: &note_core::NoteAttachment) -> Self {
        let (object_key, storage_generation, size_bytes, checksum_sha256) = attachment
            .storage
            .as_ref()
            .map(|storage| {
                (
                    Some(storage.object_key.clone()),
                    Some(storage.storage_generation.clone()),
                    Some(storage.size_bytes),
                    Some(storage.checksum_sha256.clone()),
                )
            })
            .unwrap_or((None, None, None, None));

        Self {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
            object_key,
            storage_generation,
            size_bytes,
            checksum_sha256,
        }
    }
}

impl TryFrom<PersistedAttachment> for note_core::NoteAttachment {
    type Error = StorageError;

    fn try_from(attachment: PersistedAttachment) -> Result<Self, Self::Error> {
        let storage = match (
            attachment.object_key,
            attachment.storage_generation,
            attachment.size_bytes,
            attachment.checksum_sha256,
        ) {
            (None, None, None, None) => None,
            (
                Some(object_key),
                Some(storage_generation),
                Some(size_bytes),
                Some(checksum_sha256),
            ) => Some(note_core::AttachmentStorageMetadata {
                object_key,
                storage_generation,
                size_bytes,
                checksum_sha256,
            }),
            _ => {
                return Err(StorageError::new(
                    StorageErrorKind::Operation,
                    "persisted attachment has partial generated storage metadata",
                ));
            }
        };

        Ok(Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content: Vec::new(),
            storage,
        })
    }
}

#[cfg(test)]
mod attachment_record_tests {
    use super::*;
    use note_core::{AttachmentStorageMetadata, NoteAttachment};

    fn generated_attachment() -> NoteAttachment {
        NoteAttachment {
            id: "generated".into(),
            path: "generated.bin".into(),
            mime: "application/octet-stream".into(),
            description: "Generated".into(),
            content: b"must not be persisted".to_vec(),
            storage: Some(AttachmentStorageMetadata {
                object_key: "notes/note-1/generated.bin/generation-1".into(),
                storage_generation: "generation-1".into(),
                size_bytes: 23,
                checksum_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .into(),
            }),
        }
    }

    #[test]
    fn persisted_attachment_roundtrips_generated_metadata_without_content() {
        let attachment = generated_attachment();
        let record = PersistedAttachment::from(&attachment);
        let json = serde_json::to_value(&record).unwrap();

        assert!(json.get("content").is_none());
        assert!(json.get("content_base64").is_none());
        assert_eq!(
            json["object_key"],
            attachment.storage.as_ref().unwrap().object_key
        );

        let roundtrip = NoteAttachment::try_from(record).unwrap();
        assert_eq!(roundtrip.id, attachment.id);
        assert_eq!(roundtrip.path, attachment.path);
        assert_eq!(roundtrip.mime, attachment.mime);
        assert_eq!(roundtrip.description, attachment.description);
        assert!(roundtrip.content.is_empty());
        assert_eq!(roundtrip.storage, attachment.storage);
    }

    #[test]
    fn persisted_attachment_legacy_shape_omits_generated_fields() {
        let record: PersistedAttachment = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "path": "legacy.txt",
            "mime": "text/plain",
            "description": "Legacy"
        }))
        .unwrap();

        let attachment = NoteAttachment::try_from(record.clone()).unwrap();
        assert_eq!(attachment.storage, None);
        let json = serde_json::to_value(record).unwrap();
        for field in [
            "object_key",
            "storage_generation",
            "size_bytes",
            "checksum_sha256",
            "content",
            "content_base64",
        ] {
            assert!(json.get(field).is_none(), "unexpected {field}");
        }
    }

    #[test]
    fn persisted_attachment_rejects_partial_generated_metadata() {
        let record: PersistedAttachment = serde_json::from_value(serde_json::json!({
            "id": "partial",
            "path": "partial.txt",
            "mime": "text/plain",
            "description": "Partial",
            "object_key": "notes/note-1/partial.txt/generation-1"
        }))
        .unwrap();

        let error = NoteAttachment::try_from(record).unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
    }

    #[test]
    fn persisted_attachment_rejects_explicit_null_generated_metadata() {
        let result = serde_json::from_value::<PersistedAttachment>(serde_json::json!({
            "id": "partial",
            "path": "partial.txt",
            "mime": "text/plain",
            "description": "Partial",
            "object_key": null
        }));

        assert!(result.is_err());
    }
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
