use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{OrgArtifactReference, OrgAttemptNoteReference};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ORG_COMMAND_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgCommandKind {
    name: &'static str,
    schema_version: u32,
}

impl OrgCommandKind {
    pub const fn new(name: &'static str, schema_version: u32) -> Self {
        Self {
            name,
            schema_version,
        }
    }

    pub const fn name(self) -> &'static str {
        self.name
    }

    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub actor_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgCommandResult {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub operation_id: String,
    pub event_ids: Vec<String>,
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgWorkspaceView {
    pub id: WorkspaceId,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgDocumentView {
    pub id: DocumentId,
    pub path: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgTimestampView {
    pub raw: String,
    pub local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgItemView {
    pub id: WorkItemId,
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    pub parent_id: Option<WorkItemId>,
    pub item_type: WorkItemType,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<OrgTimestampView>,
    pub deadline: Option<OrgTimestampView>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgDependencyView {
    pub item: OrgItemView,
    pub satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgNoteLinkView {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgAttemptNoteView {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgArtifactView {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgAttemptView {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub work_item_id: WorkItemId,
    pub attempt_number: i64,
    pub actor_id: String,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub error: Option<String>,
    pub result_summary: Option<String>,
    pub review_outcome: Option<String>,
    pub note_refs: Vec<OrgAttemptNoteView>,
    pub artifacts: Vec<OrgArtifactView>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgEventView {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub sequence: i64,
    pub subject_kind: String,
    pub subject_id: String,
    pub actor_id: String,
    pub attempt_id: Option<String>,
    pub event_type: String,
    pub occurred_at: i64,
    pub summary: String,
    pub metadata: serde_json::Value,
    pub previous_state: Option<String>,
    pub resulting_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgHistorySegment {
    pub workspace_id: WorkspaceId,
    pub events: Vec<OrgEventView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum OrgOriginView {
    WorkItem {
        work_item_id: WorkItemId,
        item: Option<Box<OrgItemView>>,
    },
    Event {
        event_id: String,
        event: Option<Box<OrgEventView>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgLeaseView {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub work_item_id: WorkItemId,
    pub attempt_id: String,
    pub kind: String,
    pub actor_id: String,
    pub acquired_at: i64,
    pub last_heartbeat_at: i64,
    pub expires_at: i64,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgItemContext {
    pub workspace: OrgWorkspaceView,
    pub workspace_revision: i64,
    pub document: OrgDocumentView,
    pub item: OrgItemView,
    pub parent: Option<OrgItemView>,
    pub children: Vec<OrgItemView>,
    pub dependencies: Vec<OrgDependencyView>,
    pub note_links: Vec<OrgNoteLinkView>,
    pub attempts: Vec<OrgAttemptView>,
    pub origin: Option<OrgOriginView>,
    pub history_segments: Vec<OrgHistorySegment>,
    pub lease: Option<OrgLeaseView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgClaimKind {
    Execution,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartClaimRequest {
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub kind: OrgClaimKind,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatClaimRequest {
    pub work_item_id: WorkItemId,
    pub lease_id: String,
    pub kind: OrgClaimKind,
    pub fencing_token: String,
}

impl std::fmt::Debug for HeartbeatClaimRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeartbeatClaimRequest")
            .field("work_item_id", &self.work_item_id)
            .field("lease_id", &self.lease_id)
            .field("kind", &self.kind)
            .field("fencing_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseClaimRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub kind: OrgClaimKind,
    pub fencing_token: String,
    pub target_state: Option<String>,
}

impl std::fmt::Debug for ReleaseClaimRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReleaseClaimRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("lease_id", &self.lease_id)
            .field("kind", &self.kind)
            .field("fencing_token", &"[REDACTED]")
            .field("target_state", &self.target_state)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgClaimResult {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub operation_id: String,
    pub lease_id: String,
    pub fencing_token: String,
    pub expires_at: i64,
    pub event_ids: Vec<String>,
    pub context: OrgItemContext,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportProgressRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub lease_id: String,
    pub kind: OrgClaimKind,
    pub fencing_token: String,
    pub summary: String,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for ReportProgressRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReportProgressRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("lease_id", &self.lease_id)
            .field("kind", &self.kind)
            .field("fencing_token", &"[REDACTED]")
            .field("summary", &self.summary)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResultRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub result_summary: String,
    pub note_refs: Vec<OrgAttemptNoteReference>,
    pub artifacts: Vec<OrgArtifactReference>,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for SubmitResultRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubmitResultRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("lease_id", &self.lease_id)
            .field("fencing_token", &"[REDACTED]")
            .field("result_summary", &self.result_summary)
            .field("note_refs", &self.note_refs)
            .field("artifacts", &self.artifacts)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestReviewRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub result_summary: Option<String>,
    pub note_refs: Vec<OrgAttemptNoteReference>,
    pub artifacts: Vec<OrgArtifactReference>,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for RequestReviewRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestReviewRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("lease_id", &self.lease_id)
            .field("fencing_token", &"[REDACTED]")
            .field("result_summary", &self.result_summary)
            .field("note_refs", &self.note_refs)
            .field("artifacts", &self.artifacts)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveItemRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for ApproveItemRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApproveItemRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("lease_id", &self.lease_id)
            .field("fencing_token", &"[REDACTED]")
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectItemRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub reason: String,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for RejectItemRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RejectItemRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("lease_id", &self.lease_id)
            .field("fencing_token", &"[REDACTED]")
            .field("reason", &self.reason)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseProofInput {
    pub lease_id: String,
    pub kind: OrgClaimKind,
    pub fencing_token: String,
}

impl std::fmt::Debug for LeaseProofInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeaseProofInput")
            .field("lease_id", &self.lease_id)
            .field("kind", &self.kind)
            .field("fencing_token", &"[REDACTED]")
            .finish()
    }
}

pub type TransitionLeaseProof = LeaseProofInput;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionItemRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
    pub target_state: String,
    pub lease: Option<TransitionLeaseProof>,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for TransitionItemRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransitionItemRequest")
            .field("schema_version", &self.schema_version)
            .field("work_item_id", &self.work_item_id)
            .field("document_id", &self.document_id)
            .field(
                "expected_document_revision",
                &self.expected_document_revision,
            )
            .field("target_state", &self.target_state)
            .field("lease", &self.lease)
            .field("error", &self.error)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryItemRequest {
    pub schema_version: u32,
    pub work_item_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_document_revision: i64,
}

impl std::fmt::Debug for OrgClaimResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OrgClaimResult")
            .field("schema_version", &self.schema_version)
            .field("workspace_id", &self.workspace_id)
            .field("operation_id", &self.operation_id)
            .field("lease_id", &self.lease_id)
            .field("fencing_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("event_ids", &self.event_ids)
            .field("context", &self.context)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgEventQuery {
    pub workspace_id: WorkspaceId,
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgEventPage {
    pub events: Vec<OrgEventView>,
    pub next_cursor: Option<String>,
}
