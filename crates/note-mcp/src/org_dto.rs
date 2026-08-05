#![allow(
    dead_code,
    reason = "Task 1 schema adapters are registered before Task 2 invokes their fields"
)]

use std::borrow::Cow;
use std::collections::BTreeMap;

use note_pipelines::org::{
    CommandEnvelope, OrgClaimResult, OrgDocumentView, OrgEventView, OrgItemContext, OrgItemView,
    OrgLeaseView, OrgTimestampView, OrgWorkspaceView,
};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

fn default_limit() -> u16 {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ListInput {
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schemars(default = "default_limit", range(min = 1, max = 200))]
    pub limit: u16,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceReadInput {
    pub workspace_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceListInput {
    pub workspace_id: String,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schemars(default = "default_limit", range(min = 1, max = 200))]
    pub limit: u16,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentReadInput {
    pub workspace_id: String,
    pub document_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemReadInput {
    pub workspace_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct NoteItemsInput {
    pub note_id: String,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schemars(default = "default_limit", range(min = 1, max = 200))]
    pub limit: u16,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct OperationalQueryInput {
    pub workspace_ids: Vec<String>,
    pub view: String,
    pub item_type: Option<String>,
    pub state: Option<String>,
    pub priority: Option<char>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub scheduled_from: Option<i64>,
    pub scheduled_to: Option<i64>,
    pub deadline_from: Option<i64>,
    pub deadline_to: Option<i64>,
    pub completed_from: Option<i64>,
    pub completed_to: Option<i64>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    #[serde(default)]
    pub include_archived: bool,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schemars(default = "default_limit", range(min = 1, max = 200))]
    pub limit: u16,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct EventListInput {
    pub workspace_id: String,
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schemars(default = "default_limit", range(min = 1, max = 200))]
    pub limit: u16,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MutationEnvelope {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
}

impl TryFrom<MutationEnvelope> for CommandEnvelope {
    type Error = note_pipelines::org::OrgError;

    fn try_from(value: MutationEnvelope) -> Result<Self, Self::Error> {
        let workspace_id = value
            .workspace_id
            .parse()
            .map_err(|_| note_pipelines::org::OrgError::invalid_input("invalid workspace_id"))?;
        Ok(Self {
            schema_version: value.schema_version,
            workspace_id,
            actor_id: value.actor_id,
            operation_id: value.operation_id,
        })
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceCreateInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicyInput,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspacePolicyInput {
    pub allow_cross_workspace_agenda: bool,
    pub allowed_types: Vec<String>,
    pub states: Vec<String>,
    pub transitions: Vec<(String, String)>,
    pub initial_state: String,
    pub running_state: String,
    pub executable_states: Vec<String>,
    pub review_state: String,
    pub failed_state: String,
    pub cancelled_state: String,
    pub successful_terminal_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub release_state: String,
    pub review_rejection_state: String,
    pub lease_expiry_recovery_state: String,
    pub review_required_types: Vec<String>,
    pub claim_policy: String,
    pub lease_duration_secs: u64,
    pub retry_limit: u32,
    pub concurrency_limit: usize,
    pub tag_rules: BTreeMap<String, TagRuleInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TagRuleInput {
    pub allowed: Vec<String>,
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceUpdateInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub expected_revision: i64,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicyInput,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceArchiveInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub expected_revision: i64,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct PutDocumentInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub document_id: String,
    pub path: String,
    pub source: String,
    pub expected_revision: Option<i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MoveDocumentInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub document_id: String,
    pub target_workspace_id: String,
    pub expected_document_revision: i64,
    pub expected_source_workspace_revision: i64,
    pub expected_target_workspace_revision: i64,
    pub lease_proofs: BTreeMap<String, LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ImportWorkspaceInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub documents: Vec<DocumentImportInput>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofInput>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentImportInput {
    pub document_id: String,
    pub path: String,
    pub source: String,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CreateItemInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_id: String,
    pub item_type: String,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct FollowUpOriginInput {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CreateFollowUpInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item: CreateItemPayload,
    pub origin: FollowUpOriginInput,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CreateItemPayload {
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_id: String,
    pub item_type: String,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<String, i64>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MoveItemInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub source_document_id: String,
    pub target_document_id: String,
    pub target_parent_id: Option<String>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AssignItemInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub document_id: String,
    pub assignee: Option<String>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ScheduleItemInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub document_id: String,
    pub scheduled: FieldPatchInput,
    pub deadline: FieldPatchInput,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofInput>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub(crate) enum FieldPatchInput {
    Unchanged,
    Set(String),
    Clear,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ClaimInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub kind: String,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct HeartbeatClaimInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub lease_id: String,
    pub kind: String,
    pub fencing_token: String,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ReleaseClaimInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub kind: String,
    pub fencing_token: String,
    pub target_state: Option<String>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ProgressInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub lease_id: String,
    pub kind: String,
    pub fencing_token: String,
    pub summary: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AttemptNoteInput {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ArtifactInput {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct SubmitResultInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub result_summary: String,
    pub note_refs: Vec<AttemptNoteInput>,
    pub artifacts: Vec<ArtifactInput>,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TransitionInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub target_state: String,
    pub lease: Option<LeaseProofInput>,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RetryInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RequestReviewInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub result_summary: Option<String>,
    pub note_refs: Vec<AttemptNoteInput>,
    pub artifacts: Vec<ArtifactInput>,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ApproveInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RejectInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub work_item_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub fencing_token: String,
    pub reason: String,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DependencyInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub dependency_id: String,
    pub document_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct NoteLinkInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub document_id: String,
    pub purpose: String,
    pub note_id: String,
    pub description: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct NoteUnlinkInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub item_id: String,
    pub document_id: String,
    pub purpose: String,
    pub note_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofInput>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct LeaseProofInput {
    pub lease_id: String,
    pub kind: String,
    pub fencing_token: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct PageOutput<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CountsOutput {
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceSummaryOutput {
    pub workspace_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub archived_at: Option<i64>,
    pub workspace_revision: i64,
    pub evaluated_at: i64,
    pub counts: CountsOutput,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceOutput {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicyInput,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentOutput {
    pub id: String,
    pub path: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentSourceOutput {
    pub id: String,
    pub workspace_id: String,
    pub path: String,
    pub source: String,
    pub content_hash: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceExportOutput {
    pub workspace: WorkspaceOutput,
    pub documents: Vec<DocumentSourceOutput>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TimestampOutput {
    pub raw: String,
    pub local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemOutput {
    pub id: String,
    pub workspace_id: String,
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_type: String,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<TimestampOutput>,
    pub deadline: Option<TimestampOutput>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct LeaseOutput {
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct OperationalItemOutput {
    pub item: ItemOutput,
    pub attempt_count: i64,
    pub current_attempt_status: Option<String>,
    pub retry_exhausted: bool,
    pub ready_status: Option<String>,
    pub review_lease_status: Option<String>,
    pub lease: Option<LeaseOutput>,
    pub completion_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct OperationalPageOutput {
    pub items: Vec<OperationalItemOutput>,
    pub next_cursor: Option<String>,
    pub evaluated_at: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DependencyOutput {
    pub item: ItemOutput,
    pub satisfied: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct NoteLinkOutput {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct EventOutput {
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
    pub metadata: SafeJsonOutput,
    pub previous_state: Option<String>,
    pub resulting_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub(crate) struct SafeJsonOutput(serde_json::Value);

impl JsonSchema for SafeJsonOutput {
    fn schema_name() -> Cow<'static, str> {
        "SafeJsonOutput".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let recursive = generator.subschema_for::<Self>();
        json_schema!({
            "anyOf": [
                {"type": "null"},
                {"type": "boolean"},
                {"type": "number"},
                {"type": "string"},
                {"type": "array", "items": recursive.clone()},
                {
                    "type": "object",
                    "additionalProperties": recursive
                }
            ]
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AttemptNoteOutput {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ArtifactOutput {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AttemptOutput {
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
    pub note_refs: Vec<AttemptNoteOutput>,
    pub artifacts: Vec<ArtifactOutput>,
    pub metadata: SafeJsonOutput,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum OriginOutput {
    WorkItem {
        work_item_id: String,
        item: Option<Box<ItemOutput>>,
    },
    Event {
        event_id: String,
        event: Option<Box<EventOutput>>,
    },
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct HistorySegmentOutput {
    pub workspace_id: String,
    pub events: Vec<EventOutput>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AttemptBudgetOutput {
    pub execution_attempt_count: u32,
    pub max_attempts: u32,
    pub remaining_attempts: u32,
    pub retry_exhausted: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RecoveryOutput {
    pub eligible: bool,
    pub candidate: bool,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct OperationalContextOutput {
    pub classifications: Vec<String>,
    pub readiness: Option<String>,
    pub blockers: Vec<String>,
    pub attempt_budget: AttemptBudgetOutput,
    pub recovery: RecoveryOutput,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemContextOutput {
    pub workspace: WorkspaceOutput,
    pub workspace_revision: i64,
    pub document: DocumentOutput,
    pub item: ItemOutput,
    pub parent: Option<ItemOutput>,
    pub children: Vec<ItemOutput>,
    pub dependencies: Vec<DependencyOutput>,
    pub note_links: Vec<NoteLinkOutput>,
    pub attempts: Vec<AttemptOutput>,
    pub origin: Option<OriginOutput>,
    pub history_segments: Vec<HistorySegmentOutput>,
    pub lease: Option<LeaseOutput>,
    pub operational: OperationalContextOutput,
}

#[derive(Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CommandOutput<T> {
    pub schema_version: u32,
    pub workspace_id: String,
    pub operation_id: String,
    pub event_ids: Vec<String>,
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub data: T,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceCommandData {
    pub workspace_id: String,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentCommandData {
    pub document_count: Option<usize>,
    pub document_id: Option<String>,
    pub source_workspace_id: Option<String>,
    pub target_workspace_id: Option<String>,
    pub source_workspace_revision: Option<i64>,
    pub target_workspace_revision: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemCommandData {
    pub item_id: Option<String>,
    pub source_document_id: Option<String>,
    pub target_document_id: Option<String>,
    pub context: Option<ItemContextOutput>,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct LeaseCommandData {
    pub context: Option<ItemContextOutput>,
    pub released: Option<bool>,
    pub review_outcome: Option<String>,
}

#[derive(Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ClaimOutput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub operation_id: String,
    pub lease_id: String,
    pub fencing_token: String,
    pub expires_at: i64,
    pub event_ids: Vec<String>,
    pub context: ItemContextOutput,
}

impl From<OrgClaimResult> for ClaimOutput {
    fn from(value: OrgClaimResult) -> Self {
        Self {
            schema_version: value.schema_version,
            workspace_id: value.workspace_id.to_string(),
            operation_id: value.operation_id,
            lease_id: value.lease_id,
            fencing_token: value.fencing_token,
            expires_at: value.expires_at,
            event_ids: value.event_ids,
            context: ItemContextOutput::from(value.context),
        }
    }
}

impl From<OrgWorkspaceView> for WorkspaceOutput {
    fn from(value: OrgWorkspaceView) -> Self {
        Self {
            id: value.id.to_string(),
            slug: value.slug,
            display_name: value.display_name,
            description: value.description,
            timezone: value.timezone,
            policy_schema_version: value.policy_schema_version,
            policy: serde_json::from_value(
                serde_json::to_value(value.policy).expect("workspace policy serializes"),
            )
            .expect("workspace policy wire adapter matches"),
            revision: value.revision,
            archived_at: value.archived_at,
        }
    }
}

impl From<OrgDocumentView> for DocumentOutput {
    fn from(value: OrgDocumentView) -> Self {
        Self {
            id: value.id.to_string(),
            path: value.path,
            revision: value.revision,
        }
    }
}

impl From<OrgTimestampView> for TimestampOutput {
    fn from(value: OrgTimestampView) -> Self {
        Self {
            raw: value.raw,
            local: value.local,
            timezone: value.timezone,
            utc_timestamp: value.utc_timestamp,
        }
    }
}

impl From<OrgItemView> for ItemOutput {
    fn from(value: OrgItemView) -> Self {
        Self {
            id: value.id.to_string(),
            workspace_id: value.workspace_id.to_string(),
            document_id: value.document_id.to_string(),
            parent_id: value.parent_id.map(|id| id.to_string()),
            item_type: wire_name(value.item_type),
            title: value.title,
            state: value.state,
            priority: value.priority,
            scheduled: value.scheduled.map(Into::into),
            deadline: value.deadline.map(Into::into),
            assignee: value.assignee,
            requires_review: value.requires_review,
            created_at: value.created_at,
            tags: value.tags,
        }
    }
}

impl From<OrgLeaseView> for LeaseOutput {
    fn from(value: OrgLeaseView) -> Self {
        Self {
            id: value.id,
            workspace_id: value.workspace_id.to_string(),
            work_item_id: value.work_item_id.to_string(),
            attempt_id: value.attempt_id,
            kind: value.kind,
            actor_id: value.actor_id,
            acquired_at: value.acquired_at,
            last_heartbeat_at: value.last_heartbeat_at,
            expires_at: value.expires_at,
            status: value.status,
        }
    }
}

impl From<OrgEventView> for EventOutput {
    fn from(value: OrgEventView) -> Self {
        Self {
            id: value.id,
            workspace_id: value.workspace_id.to_string(),
            sequence: value.sequence,
            subject_kind: value.subject_kind,
            subject_id: value.subject_id,
            actor_id: value.actor_id,
            attempt_id: value.attempt_id,
            event_type: value.event_type,
            occurred_at: value.occurred_at,
            summary: value.summary,
            metadata: value.metadata.into(),
            previous_state: value.previous_state,
            resulting_state: value.resulting_state,
        }
    }
}

impl From<OrgItemContext> for ItemContextOutput {
    fn from(value: OrgItemContext) -> Self {
        let operational = OperationalContextOutput {
            classifications: value
                .operational
                .classifications
                .into_iter()
                .map(wire_name)
                .collect(),
            readiness: value.operational.readiness.map(wire_name),
            blockers: value.operational.blockers,
            attempt_budget: AttemptBudgetOutput {
                execution_attempt_count: value.operational.attempt_budget.execution_attempt_count,
                max_attempts: value.operational.attempt_budget.max_attempts,
                remaining_attempts: value.operational.attempt_budget.remaining_attempts,
                retry_exhausted: value.operational.attempt_budget.retry_exhausted,
            },
            recovery: RecoveryOutput {
                eligible: value.operational.recovery.eligible,
                candidate: value.operational.recovery.candidate,
                blockers: value.operational.recovery.blockers,
            },
        };
        Self {
            workspace: value.workspace.into(),
            workspace_revision: value.workspace_revision,
            document: value.document.into(),
            item: value.item.into(),
            parent: value.parent.map(Into::into),
            children: value.children.into_iter().map(Into::into).collect(),
            dependencies: value
                .dependencies
                .into_iter()
                .map(|dependency| DependencyOutput {
                    item: dependency.item.into(),
                    satisfied: dependency.satisfied,
                })
                .collect(),
            note_links: value
                .note_links
                .into_iter()
                .map(|link| NoteLinkOutput {
                    purpose: link.purpose,
                    note_id: link.note_id,
                    description: link.description,
                    available: link.available,
                })
                .collect(),
            attempts: value
                .attempts
                .into_iter()
                .map(|attempt| AttemptOutput {
                    id: attempt.id,
                    workspace_id: attempt.workspace_id.to_string(),
                    work_item_id: attempt.work_item_id.to_string(),
                    attempt_number: attempt.attempt_number,
                    actor_id: attempt.actor_id,
                    status: attempt.status,
                    started_at: attempt.started_at,
                    ended_at: attempt.ended_at,
                    error: attempt.error,
                    result_summary: attempt.result_summary,
                    review_outcome: attempt.review_outcome,
                    note_refs: attempt
                        .note_refs
                        .into_iter()
                        .map(|note| AttemptNoteOutput {
                            purpose: note.purpose,
                            note_id: note.note_id,
                            description: note.description,
                        })
                        .collect(),
                    artifacts: attempt
                        .artifacts
                        .into_iter()
                        .map(|artifact| ArtifactOutput {
                            uri: artifact.uri,
                            media_type: artifact.media_type,
                            name: artifact.name,
                            description: artifact.description,
                        })
                        .collect(),
                    metadata: attempt.metadata.into(),
                })
                .collect(),
            origin: value.origin.map(|origin| match origin {
                note_pipelines::org::OrgOriginView::WorkItem { work_item_id, item } => {
                    OriginOutput::WorkItem {
                        work_item_id: work_item_id.to_string(),
                        item: item.map(|item| Box::new((*item).into())),
                    }
                }
                note_pipelines::org::OrgOriginView::Event { event_id, event } => {
                    OriginOutput::Event {
                        event_id,
                        event: event.map(|event| Box::new((*event).into())),
                    }
                }
            }),
            history_segments: value
                .history_segments
                .into_iter()
                .map(|segment| HistorySegmentOutput {
                    workspace_id: segment.workspace_id.to_string(),
                    events: segment.events.into_iter().map(Into::into).collect(),
                })
                .collect(),
            lease: value.lease.map(Into::into),
            operational,
        }
    }
}

impl From<serde_json::Value> for SafeJsonOutput {
    fn from(mut value: serde_json::Value) -> Self {
        redact_sensitive_metadata(&mut value);
        Self(value)
    }
}

fn redact_sensitive_metadata(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                redact_sensitive_metadata(value);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if is_sensitive_metadata_key(key) {
                    *value = serde_json::Value::String("[REDACTED]".to_string());
                } else {
                    redact_sensitive_metadata(value);
                }
            }
        }
        _ => {}
    }
}

fn is_sensitive_metadata_key(key: &str) -> bool {
    let key = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "fencingtoken",
        "secret",
        "authentication",
        "authorization",
        "password",
        "apikey",
        "accesstoken",
    ]
    .iter()
    .any(|forbidden| key.contains(forbidden))
}

fn wire_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("wire enum serializes")
        .as_str()
        .expect("wire enum is a string")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn safe_metadata_preserves_structure_and_number_types_and_redacts_tokens() {
        let source = json!({
            "negative": -4,
            "unsigned": u64::MAX,
            "float": 1.25,
            "nested": [true, {
                "label": "value",
                "fencing-token": "nested-token",
                "fencing token": "nested-space-token"
            }],
            "fencingToken": "raw-token",
            "authentication-secret": "raw-secret",
            "apiKey": "raw-key",
            "accessToken": "raw-access-token"
        });
        let round_trip = serde_json::to_value(SafeJsonOutput::from(source)).unwrap();

        assert_eq!(round_trip["negative"], -4);
        assert_eq!(round_trip["unsigned"], u64::MAX);
        assert_eq!(round_trip["float"], 1.25);
        assert_eq!(round_trip["nested"][0], true);
        assert_eq!(round_trip["nested"][1]["label"], "value");
        assert_eq!(round_trip["nested"][1]["fencing-token"], "[REDACTED]");
        assert_eq!(round_trip["nested"][1]["fencing token"], "[REDACTED]");
        assert_eq!(round_trip["fencingToken"], "[REDACTED]");
        assert_eq!(round_trip["authentication-secret"], "[REDACTED]");
        assert_eq!(round_trip["apiKey"], "[REDACTED]");
        assert_eq!(round_trip["accessToken"], "[REDACTED]");
    }

    #[test]
    fn mutation_metadata_accepts_array_and_scalar_json() {
        let base = json!({
            "schema_version": 1,
            "workspace_id": "workspace",
            "actor_id": "actor",
            "operation_id": "operation",
            "work_item_id": "item",
            "lease_id": "lease",
            "kind": "execution",
            "fencing_token": "token",
            "summary": "progress"
        });
        for metadata in [json!([1, "two"]), json!(7), json!(true)] {
            let mut request = base.clone();
            request["metadata"] = metadata.clone();
            let parsed: ProgressInput = serde_json::from_value(request).unwrap();
            assert_eq!(parsed.metadata, metadata);
        }
    }

    #[test]
    fn safe_metadata_schema_is_recursive_plain_json_without_tagged_wrappers() {
        let schema = serde_json::to_value(schemars::schema_for!(SafeJsonOutput)).unwrap();
        let object = schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["type"] == "object")
            .unwrap();
        assert_eq!(object["additionalProperties"]["$ref"], "#");
        assert!(object.get("propertyNames").is_none());
        assert!(schema["anyOf"].as_array().unwrap().iter().all(|branch| {
            !branch
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|properties| {
                    properties.contains_key("type") || properties.contains_key("value")
                })
        }));
    }
}
