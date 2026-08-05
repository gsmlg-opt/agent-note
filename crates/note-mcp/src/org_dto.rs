#![allow(
    dead_code,
    reason = "later delivery tasks register remaining Org schemas before invoking their fields"
)]

use std::borrow::Cow;
use std::collections::BTreeMap;

use note_pipelines::org::{
    CommandEnvelope, CreateWorkspaceRequest, OrgClaimResult, OrgCommandResult,
    OrgDocumentSourceView, OrgDocumentView, OrgError, OrgEventView, OrgItemContext, OrgItemView,
    OrgLeaseView, OrgOperationalPage, OrgReadPage, OrgReadQuery, OrgTimestampView,
    OrgWorkspaceExport, OrgWorkspaceView, WorkspaceSummary,
};
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

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

impl From<ListInput> for OrgReadQuery {
    fn from(value: ListInput) -> Self {
        Self {
            cursor: value.cursor,
            limit: Some(usize::from(value.limit)),
            include_archived: value.include_archived,
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceReadInput {
    pub workspace_id: String,
}

impl WorkspaceReadInput {
    pub(crate) fn into_workspace_id<T>(self) -> Result<T, OrgError>
    where
        T: std::str::FromStr,
    {
        parse_id(self.workspace_id, "workspace_id")
    }
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

impl WorkspaceListInput {
    pub(crate) fn into_pipeline<T>(self) -> Result<(T, OrgReadQuery), OrgError>
    where
        T: std::str::FromStr,
    {
        Ok((
            parse_id(self.workspace_id, "workspace_id")?,
            OrgReadQuery {
                cursor: self.cursor,
                limit: Some(usize::from(self.limit)),
                include_archived: self.include_archived,
            },
        ))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentReadInput {
    pub workspace_id: String,
    pub document_id: String,
}

impl DocumentReadInput {
    pub(crate) fn into_pipeline<W, D>(self) -> Result<(W, D), OrgError>
    where
        W: std::str::FromStr,
        D: std::str::FromStr,
    {
        Ok((
            parse_id(self.workspace_id, "workspace_id")?,
            parse_id(self.document_id, "document_id")?,
        ))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemReadInput {
    pub workspace_id: String,
    pub item_id: String,
}

impl ItemReadInput {
    pub(crate) fn into_pipeline<W, I>(self) -> Result<(W, I), OrgError>
    where
        W: std::str::FromStr,
        I: std::str::FromStr,
    {
        Ok((
            parse_id(self.workspace_id, "workspace_id")?,
            parse_id(self.item_id, "item_id")?,
        ))
    }
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
pub(crate) struct OperationalQueryInput<V> {
    #[schemars(length(min = 1))]
    pub workspace_ids: Vec<String>,
    pub view: V,
    pub item_type: Option<WorkItemTypeInput>,
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

pub(crate) trait IntoOperationalView {
    fn into_operational_view(self) -> note_pipelines::org::OperationalView;
}

impl<V: IntoOperationalView> OperationalQueryInput<V> {
    pub(crate) fn into_pipeline(self) -> Result<note_pipelines::org::OperationalQuery, OrgError> {
        let mut workspace_ids = Vec::with_capacity(self.workspace_ids.len());
        let mut canonical_ids = std::collections::BTreeSet::new();
        for id in self.workspace_ids {
            let parsed = parse_id(id, "workspace_id")?;
            if !canonical_ids.insert(parsed) {
                return Err(OrgError::invalid_input(
                    "Org operational workspace IDs must be unique",
                ));
            }
            workspace_ids.push(parsed);
        }
        Ok(note_pipelines::org::OperationalQuery {
            workspace_ids,
            view: self.view.into_operational_view(),
            item_type: self
                .item_type
                .map(|value| adapt_input(value, "item type"))
                .transpose()?,
            state: self.state,
            priority: self.priority,
            tags: self.tags,
            assignee: self.assignee,
            scheduled_from: self.scheduled_from,
            scheduled_to: self.scheduled_to,
            deadline_from: self.deadline_from,
            deadline_to: self.deadline_to,
            completed_from: self.completed_from,
            completed_to: self.completed_to,
            from: self.from,
            to: self.to,
            include_archived: self.include_archived,
            cursor: self.cursor,
            limit: Some(usize::from(self.limit)),
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueueViewInput {
    Ready,
    Assigned,
    Running,
    Blocked,
    Review,
    Failed,
    ExpiredLease,
    Completed,
}

impl IntoOperationalView for QueueViewInput {
    fn into_operational_view(self) -> note_pipelines::org::OperationalView {
        match self {
            Self::Ready => note_pipelines::org::OperationalView::Ready,
            Self::Assigned => note_pipelines::org::OperationalView::Assigned,
            Self::Running => note_pipelines::org::OperationalView::Running,
            Self::Blocked => note_pipelines::org::OperationalView::Blocked,
            Self::Review => note_pipelines::org::OperationalView::Review,
            Self::Failed => note_pipelines::org::OperationalView::Failed,
            Self::ExpiredLease => note_pipelines::org::OperationalView::ExpiredLease,
            Self::Completed => note_pipelines::org::OperationalView::Completed,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgendaViewInput {
    Scheduled,
    UpcomingDeadline,
}

impl IntoOperationalView for AgendaViewInput {
    fn into_operational_view(self) -> note_pipelines::org::OperationalView {
        match self {
            Self::Scheduled => note_pipelines::org::OperationalView::Scheduled,
            Self::UpcomingDeadline => note_pipelines::org::OperationalView::UpcomingDeadline,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkItemTypeInput {
    Project,
    Epic,
    Issue,
    Task,
    Subtask,
    Review,
    Approval,
    Incident,
    Milestone,
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

impl WorkspaceCreateInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, CreateWorkspaceRequest), OrgError> {
        let policy = adapt_input(self.policy, "workspace policy")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            CreateWorkspaceRequest {
                slug: self.slug,
                display_name: self.display_name,
                description: self.description,
                timezone: self.timezone,
                policy_schema_version: self.policy_schema_version,
                policy,
            },
        ))
    }
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

impl WorkspaceUpdateInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::UpdateWorkspaceRequest), OrgError> {
        let policy = adapt_input(self.policy, "workspace policy")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::UpdateWorkspaceRequest {
                expected_revision: self.expected_revision,
                slug: self.slug,
                display_name: self.display_name,
                description: self.description,
                timezone: self.timezone,
                policy_schema_version: self.policy_schema_version,
                policy,
            },
        ))
    }
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

impl WorkspaceArchiveInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<
        (
            CommandEnvelope,
            note_pipelines::org::ArchiveWorkspaceRequest,
        ),
        OrgError,
    > {
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::ArchiveWorkspaceRequest {
                expected_revision: self.expected_revision,
            },
        ))
    }
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

impl PutDocumentInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::PutDocumentRequest), OrgError> {
        let lease_proofs = adapt_id_map(self.lease_proofs, "work_item_id", "lease proof")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::PutDocumentRequest {
                document_id: parse_id(self.document_id, "document_id")?,
                path: self.path,
                source: self.source,
                expected_revision: self.expected_revision,
                lease_proofs,
            },
        ))
    }
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

impl MoveDocumentInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::MoveDocumentRequest), OrgError> {
        let lease_proofs = adapt_id_map(self.lease_proofs, "work_item_id", "lease proof")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::MoveDocumentRequest {
                document_id: parse_id(self.document_id, "document_id")?,
                target_workspace_id: parse_id(self.target_workspace_id, "target_workspace_id")?,
                expected_document_revision: self.expected_document_revision,
                expected_source_workspace_revision: self.expected_source_workspace_revision,
                expected_target_workspace_revision: self.expected_target_workspace_revision,
                lease_proofs,
            },
        ))
    }
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

impl ImportWorkspaceInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::ImportDocumentsRequest), OrgError> {
        let documents = self
            .documents
            .into_iter()
            .map(|document| {
                Ok(note_pipelines::org::DocumentImport {
                    document_id: parse_id(document.document_id, "document_id")?,
                    path: document.path,
                    source: document.source,
                })
            })
            .collect::<Result<Vec<_>, OrgError>>()?;
        let expected_revisions =
            adapt_id_map(self.expected_revisions, "document_id", "document revision")?;
        let lease_proofs = adapt_id_map(self.lease_proofs, "work_item_id", "lease proof")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::ImportDocumentsRequest {
                documents,
                expected_revisions,
                lease_proofs,
            },
        ))
    }
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
    pub item_type: WorkItemTypeInput,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<String, i64>,
}

impl CreateItemInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::CreateItemRequest), OrgError> {
        let request = create_item_request(self.clone().into())?;
        let envelope = mutation_envelope(
            self.schema_version,
            self.workspace_id,
            self.actor_id,
            self.operation_id,
        )?;
        Ok((envelope, request))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
#[schemars(deny_unknown_fields)]
pub(crate) enum FollowUpOriginInput {
    WorkItem { id: String },
    Event { id: String },
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
    pub item_type: WorkItemTypeInput,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<String, i64>,
}

impl From<CreateItemInput> for CreateItemPayload {
    fn from(value: CreateItemInput) -> Self {
        Self {
            document_id: value.document_id,
            parent_id: value.parent_id,
            item_id: value.item_id,
            item_type: value.item_type,
            title: value.title,
            initial_state: value.initial_state,
            priority: value.priority,
            tags: value.tags,
            assignee: value.assignee,
            requires_review: value.requires_review,
            expected_revisions: value.expected_revisions,
        }
    }
}

fn create_item_request(
    value: CreateItemPayload,
) -> Result<note_pipelines::org::CreateItemRequest, OrgError> {
    Ok(note_pipelines::org::CreateItemRequest {
        document_id: parse_id(value.document_id, "document_id")?,
        parent_id: value
            .parent_id
            .map(|id| parse_id(id, "parent_id"))
            .transpose()?,
        item_id: parse_id(value.item_id, "item_id")?,
        item_type: adapt_input(value.item_type, "item type")?,
        title: value.title,
        initial_state: value.initial_state,
        priority: value.priority,
        tags: value.tags.into_iter().collect(),
        assignee: value.assignee,
        requires_review: value.requires_review,
        expected_revisions: adapt_id_map(
            value.expected_revisions,
            "document_id",
            "document revision",
        )?,
    })
}

impl CreateFollowUpInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::CreateFollowUpRequest), OrgError> {
        let envelope = mutation_envelope(
            self.schema_version,
            self.workspace_id,
            self.actor_id,
            self.operation_id,
        )?;
        let origin = match self.origin {
            FollowUpOriginInput::WorkItem { id } => {
                note_pipelines::org::FollowUpOrigin::WorkItem(parse_id(id, "origin.id")?)
            }
            FollowUpOriginInput::Event { id } => note_pipelines::org::FollowUpOrigin::Event(id),
        };
        Ok((
            envelope,
            note_pipelines::org::CreateFollowUpRequest {
                item: create_item_request(self.item)?,
                origin,
            },
        ))
    }
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

impl MoveItemInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::MoveItemRequest), OrgError> {
        let expected_revisions =
            adapt_id_map(self.expected_revisions, "document_id", "document revision")?;
        let lease_proofs = adapt_id_map(self.lease_proofs, "work_item_id", "lease proof")?;
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::MoveItemRequest {
                item_id: parse_id(self.item_id, "item_id")?,
                source_document_id: parse_id(self.source_document_id, "source_document_id")?,
                target_document_id: parse_id(self.target_document_id, "target_document_id")?,
                target_parent_id: self
                    .target_parent_id
                    .map(|id| parse_id(id, "target_parent_id"))
                    .transpose()?,
                expected_revisions,
                lease_proofs,
            },
        ))
    }
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

impl AssignItemInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::AssignItemRequest), OrgError> {
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::AssignItemRequest {
                item_id: parse_id(self.item_id, "item_id")?,
                document_id: parse_id(self.document_id, "document_id")?,
                assignee: self.assignee,
                expected_revisions: adapt_id_map(
                    self.expected_revisions,
                    "document_id",
                    "document revision",
                )?,
                lease: self
                    .lease
                    .map(|lease| adapt_input(lease, "lease proof"))
                    .transpose()?,
            },
        ))
    }
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

impl ScheduleItemInput {
    pub(crate) fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::ScheduleItemRequest), OrgError> {
        Ok((
            mutation_envelope(
                self.schema_version,
                self.workspace_id,
                self.actor_id,
                self.operation_id,
            )?,
            note_pipelines::org::ScheduleItemRequest {
                item_id: parse_id(self.item_id, "item_id")?,
                document_id: parse_id(self.document_id, "document_id")?,
                scheduled: self.scheduled.into(),
                deadline: self.deadline.into(),
                expected_revisions: adapt_id_map(
                    self.expected_revisions,
                    "document_id",
                    "document revision",
                )?,
                lease: self
                    .lease
                    .map(|lease| adapt_input(lease, "lease proof"))
                    .transpose()?,
            },
        ))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(
    deny_unknown_fields,
    rename_all = "snake_case",
    tag = "action",
    content = "value"
)]
#[schemars(deny_unknown_fields)]
pub(crate) enum FieldPatchInput {
    Unchanged,
    Set(String),
    Clear,
}

impl From<FieldPatchInput> for note_pipelines::org::OrgFieldPatch<String> {
    fn from(value: FieldPatchInput) -> Self {
        match value {
            FieldPatchInput::Unchanged => Self::Unchanged,
            FieldPatchInput::Set(value) => Self::Set(value),
            FieldPatchInput::Clear => Self::Clear,
        }
    }
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

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceRevisionData {
    pub workspace_id: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct WorkspaceArchiveData {
    pub workspace_id: String,
    pub archived_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DocumentCountData {
    pub document_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MoveDocumentData {
    pub document_id: String,
    pub source_workspace_id: String,
    pub target_workspace_id: String,
    pub source_workspace_revision: i64,
    pub target_workspace_revision: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MoveItemData {
    pub affected_document_count: usize,
    pub resulting_items: Vec<ResultingItemData>,
    pub operation: MoveItemOperationData,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ResultingItemData {
    pub id: String,
    pub workspace_id: String,
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_type: String,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<String>,
    pub deadline: Option<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub dependencies: Vec<String>,
    pub note_links: Vec<ResultingNoteLinkData>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ResultingNoteLinkData {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct MoveItemOperationData {
    pub source_document_id: String,
    pub target_document_id: String,
    pub previous_parent_id: Option<String>,
    pub resulting_parent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ItemCommandData {
    pub affected_document_count: usize,
    pub resulting_items: Vec<ResultingItemData>,
    pub operation: ItemOperationData,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum ItemOperationData {
    Create(CreateItemOperationData),
    Assignment(AssignmentOperationData),
    Schedule(ScheduleOperationData),
    Deadline(DeadlineOperationData),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CreateItemOperationData {
    pub schema_version: u32,
    pub document_id: String,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub parent_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub origin: Option<FollowUpOperationOrigin>,
    pub initial_state: String,
    pub item_type: String,
    pub title: String,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub requires_review: bool,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub initial_assignee: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
#[schemars(deny_unknown_fields)]
pub(crate) enum FollowUpOperationOrigin {
    WorkItem {
        schema_version: u32,
        work_item_id: String,
    },
    Event {
        schema_version: u32,
        event_id: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct AssignmentOperationData {
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub previous_assignee: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub resulting_assignee: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ScheduleOperationData {
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub previous_scheduled: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub resulting_scheduled: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DeadlineOperationData {
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub previous_deadline: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required)]
    pub resulting_deadline: Option<String>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
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

impl TryFrom<OrgClaimResult> for ClaimOutput {
    type Error = OrgError;

    fn try_from(value: OrgClaimResult) -> Result<Self, Self::Error> {
        Ok(Self {
            schema_version: value.schema_version,
            workspace_id: value.workspace_id.to_string(),
            operation_id: value.operation_id,
            lease_id: value.lease_id,
            fencing_token: value.fencing_token,
            expires_at: value.expires_at,
            event_ids: value.event_ids,
            context: value.context.try_into()?,
        })
    }
}

impl TryFrom<OrgWorkspaceView> for WorkspaceOutput {
    type Error = OrgError;

    fn try_from(value: OrgWorkspaceView) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id.to_string(),
            slug: value.slug,
            display_name: value.display_name,
            description: value.description,
            timezone: value.timezone,
            policy_schema_version: value.policy_schema_version,
            policy: adapt_output(value.policy, "workspace policy")?,
            revision: value.revision,
            archived_at: value.archived_at,
        })
    }
}

impl From<WorkspaceSummary> for WorkspaceSummaryOutput {
    fn from(value: WorkspaceSummary) -> Self {
        Self {
            workspace_id: value.workspace_id.to_string(),
            slug: value.slug,
            display_name: value.display_name,
            description: value.description,
            timezone: value.timezone,
            archived_at: value.archived_at,
            workspace_revision: value.workspace_revision,
            evaluated_at: value.evaluated_at,
            counts: CountsOutput {
                ready: value.counts.ready,
                assigned: value.counts.assigned,
                running: value.counts.running,
                blocked: value.counts.blocked,
                review: value.counts.review,
                scheduled: value.counts.scheduled,
                upcoming_deadline: value.counts.upcoming_deadline,
                failed: value.counts.failed,
                expired_lease: value.counts.expired_lease,
                completed: value.counts.completed,
            },
        }
    }
}

impl<T, U> From<OrgReadPage<T>> for PageOutput<U>
where
    U: From<T>,
{
    fn from(value: OrgReadPage<T>) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
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

impl From<OrgDocumentSourceView> for DocumentSourceOutput {
    fn from(value: OrgDocumentSourceView) -> Self {
        Self {
            id: value.id.to_string(),
            workspace_id: value.workspace_id.to_string(),
            path: value.path,
            source: value.source,
            content_hash: value.content_hash,
            revision: value.revision,
        }
    }
}

impl TryFrom<OrgWorkspaceExport> for WorkspaceExportOutput {
    type Error = OrgError;

    fn try_from(value: OrgWorkspaceExport) -> Result<Self, Self::Error> {
        Ok(Self {
            workspace: value.workspace.try_into()?,
            documents: value.documents.into_iter().map(Into::into).collect(),
        })
    }
}

pub(crate) fn command_output<T>(value: OrgCommandResult) -> Result<CommandOutput<T>, OrgError>
where
    T: DeserializeOwned,
{
    let data = serde_json::from_value(value.data).map_err(|_| {
        OrgError::new(
            note_pipelines::org::OrgErrorCode::StorageFailure,
            "Org pipeline returned an incompatible command result",
            serde_json::json!({}),
            false,
        )
    })?;
    Ok(CommandOutput {
        schema_version: value.schema_version,
        workspace_id: value.workspace_id.to_string(),
        operation_id: value.operation_id,
        event_ids: value.event_ids,
        workspace_revision: value.workspace_revision,
        document_revisions: value.document_revisions,
        data,
    })
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

impl TryFrom<OrgItemView> for ItemOutput {
    type Error = OrgError;

    fn try_from(value: OrgItemView) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id.to_string(),
            workspace_id: value.workspace_id.to_string(),
            document_id: value.document_id.to_string(),
            parent_id: value.parent_id.map(|id| id.to_string()),
            item_type: wire_name(value.item_type)?,
            title: value.title,
            state: value.state,
            priority: value.priority,
            scheduled: value.scheduled.map(Into::into),
            deadline: value.deadline.map(Into::into),
            assignee: value.assignee,
            requires_review: value.requires_review,
            created_at: value.created_at,
            tags: value.tags,
        })
    }
}

impl TryFrom<OrgOperationalPage> for OperationalPageOutput {
    type Error = OrgError;

    fn try_from(value: OrgOperationalPage) -> Result<Self, Self::Error> {
        Ok(Self {
            items: value
                .items
                .into_iter()
                .map(|summary| {
                    Ok(OperationalItemOutput {
                        item: summary.item.try_into()?,
                        attempt_count: summary.attempt_count,
                        current_attempt_status: summary.current_attempt_status,
                        retry_exhausted: summary.retry_exhausted,
                        ready_status: summary.ready_status.map(wire_name).transpose()?,
                        review_lease_status: summary
                            .review_lease_status
                            .map(wire_name)
                            .transpose()?,
                        lease: summary.lease.map(Into::into),
                        completion_at: summary.completion_at,
                    })
                })
                .collect::<Result<Vec<_>, OrgError>>()?,
            next_cursor: value.next_cursor,
            evaluated_at: value.evaluated_at,
        })
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

impl TryFrom<OrgItemContext> for ItemContextOutput {
    type Error = OrgError;

    fn try_from(value: OrgItemContext) -> Result<Self, Self::Error> {
        let operational = OperationalContextOutput {
            classifications: value
                .operational
                .classifications
                .into_iter()
                .map(wire_name)
                .collect::<Result<Vec<_>, _>>()?,
            readiness: value.operational.readiness.map(wire_name).transpose()?,
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
        Ok(Self {
            workspace: value.workspace.try_into()?,
            workspace_revision: value.workspace_revision,
            document: value.document.into(),
            item: value.item.try_into()?,
            parent: value.parent.map(TryInto::try_into).transpose()?,
            children: value
                .children
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
            dependencies: value
                .dependencies
                .into_iter()
                .map(|dependency| {
                    Ok(DependencyOutput {
                        item: dependency.item.try_into()?,
                        satisfied: dependency.satisfied,
                    })
                })
                .collect::<Result<Vec<_>, OrgError>>()?,
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
            origin: value
                .origin
                .map(|origin| match origin {
                    note_pipelines::org::OrgOriginView::WorkItem { work_item_id, item } => {
                        Ok(OriginOutput::WorkItem {
                            work_item_id: work_item_id.to_string(),
                            item: item
                                .map(|item| (*item).try_into().map(Box::new))
                                .transpose()?,
                        })
                    }
                    note_pipelines::org::OrgOriginView::Event { event_id, event } => {
                        Ok(OriginOutput::Event {
                            event_id,
                            event: event.map(|event| Box::new((*event).into())),
                        })
                    }
                })
                .transpose()?,
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
        })
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
    let mut normalized = String::new();
    let mut previous_was_lower_or_digit = false;
    for character in key.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_was_lower_or_digit {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
        } else if !normalized.ends_with('_') && !normalized.is_empty() {
            normalized.push('_');
            previous_was_lower_or_digit = false;
        }
    }
    let normalized = normalized.trim_matches('_');
    let contains_sensitive_word = normalized.split('_').any(|word| {
        matches!(
            word,
            "token" | "hash" | "secret" | "password" | "authentication" | "authorization"
        )
    });
    let compact = normalized.replace('_', "");
    contains_sensitive_word
        || matches!(compact.as_str(), "apikey" | "accesstoken")
        || ["token", "hash", "secret", "password"]
            .iter()
            .any(|suffix| compact.ends_with(suffix))
}

fn mutation_envelope(
    schema_version: u32,
    workspace_id: String,
    actor_id: String,
    operation_id: String,
) -> Result<CommandEnvelope, OrgError> {
    Ok(CommandEnvelope {
        schema_version,
        workspace_id: parse_id(workspace_id, "workspace_id")?,
        actor_id,
        operation_id,
    })
}

fn parse_id<T>(value: String, field: &str) -> Result<T, OrgError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| OrgError::invalid_input(format!("invalid {field}")))
}

fn adapt_input<T, S>(value: S, label: &str) -> Result<T, OrgError>
where
    T: DeserializeOwned,
    S: Serialize,
{
    serde_json::to_value(value)
        .map_err(|_| OrgError::invalid_input(format!("invalid {label}")))
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|_| OrgError::invalid_input(format!("invalid {label}")))
        })
}

fn adapt_output<T, S>(value: S, label: &str) -> Result<T, OrgError>
where
    T: DeserializeOwned,
    S: Serialize,
{
    serde_json::to_value(value)
        .map_err(|_| adapter_error(label))
        .and_then(|value| serde_json::from_value(value).map_err(|_| adapter_error(label)))
}

fn adapter_error(label: &str) -> OrgError {
    OrgError::new(
        note_pipelines::org::OrgErrorCode::StorageFailure,
        format!("Org pipeline returned an incompatible {label}"),
        serde_json::json!({}),
        false,
    )
}

fn adapt_id_map<K, V, S>(
    values: BTreeMap<String, S>,
    key_label: &str,
    value_label: &str,
) -> Result<BTreeMap<K, V>, OrgError>
where
    K: std::str::FromStr + Ord,
    V: DeserializeOwned,
    S: Serialize,
{
    let mut adapted = BTreeMap::new();
    for (key, value) in values {
        let key = parse_id(key, key_label)?;
        if adapted.contains_key(&key) {
            return Err(OrgError::invalid_input(format!(
                "duplicate canonical {key_label}"
            )));
        }
        adapted.insert(key, adapt_input(value, value_label)?);
    }
    Ok(adapted)
}

fn wire_name<T: Serialize>(value: T) -> Result<String, OrgError> {
    serde_json::to_value(value)
        .map_err(|_| adapter_error("wire enum"))?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| adapter_error("wire enum"))
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
    fn sensitive_metadata_matching_redacts_exact_or_suffix_tokens_without_false_positives() {
        let source = json!({
            "token": "one",
            "token_hash": "two",
            "lease_token": "three",
            "lease_hash": "four",
            "fencingToken": "five",
            "fencing_token_digest": "six",
            "token_digest": "seven",
            "hash_value": "eight",
            "fencingtoken": "nine",
            "secretary": "kept",
            "passwordless": "also-kept"
        });
        let output = serde_json::to_value(SafeJsonOutput::from(source)).unwrap();
        for key in [
            "token",
            "token_hash",
            "lease_token",
            "lease_hash",
            "fencingToken",
            "fencing_token_digest",
            "token_digest",
            "hash_value",
            "fencingtoken",
        ] {
            assert_eq!(output[key], "[REDACTED]");
        }
        assert_eq!(output["secretary"], "kept");
        assert_eq!(output["passwordless"], "also-kept");
    }

    #[test]
    fn field_patch_contract_rejects_unknown_fields_and_advertises_closed_variants() {
        assert!(serde_json::from_value::<FieldPatchInput>(json!({
            "action": "set",
            "value": "<2027-01-15 Fri>",
            "unexpected": true
        }))
        .is_err());
        let schema = serde_json::to_value(schemars::schema_for!(FieldPatchInput)).unwrap();
        assert!(schema
            .to_string()
            .contains("\"additionalProperties\":false"));
    }

    #[test]
    fn item_operation_contract_rejects_missing_null_extra_and_wrong_shapes() {
        let valid = json!({
            "previous_assignee": "agent-one",
            "resulting_assignee": "agent-two"
        });
        assert!(serde_json::from_value::<ItemOperationData>(valid.clone()).is_ok());

        let mut extra = valid.clone();
        extra
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        assert!(serde_json::from_value::<ItemOperationData>(extra).is_err());
        assert!(serde_json::from_value::<ItemOperationData>(json!({
            "previous_assignee": "agent-one"
        }))
        .is_err());
        assert!(serde_json::from_value::<ItemOperationData>(serde_json::Value::Null).is_err());
        assert!(serde_json::from_value::<ItemOperationData>(json!({
            "previous_assignee": 42,
            "resulting_assignee": "agent-two"
        }))
        .is_err());
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

    #[test]
    fn move_item_operation_schema_is_required_and_non_nullable() {
        fn resolve<'a>(
            root: &'a serde_json::Value,
            node: &'a serde_json::Value,
        ) -> &'a serde_json::Value {
            node.get("$ref")
                .and_then(serde_json::Value::as_str)
                .and_then(|reference| root.pointer(reference.strip_prefix('#').unwrap()))
                .unwrap_or(node)
        }

        let schema =
            serde_json::to_value(schemars::schema_for!(CommandOutput<MoveItemData>)).unwrap();
        let data = resolve(&schema, &schema["properties"]["data"]);
        assert!(data["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "operation"));
        let operation = resolve(&schema, &data["properties"]["operation"]);
        assert_eq!(operation["type"], "object");
        assert!(operation.get("anyOf").is_none());
    }

    #[test]
    fn command_output_rejects_extra_fields_and_nullable_move_operation() {
        let result = |data| OrgCommandResult {
            schema_version: 1,
            workspace_id: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
            operation_id: "move-item".into(),
            event_ids: vec![],
            workspace_revision: Some(1),
            document_revisions: BTreeMap::new(),
            data,
        };
        let valid_operation = json!({
            "source_document_id": "source",
            "target_document_id": "target",
            "previous_parent_id": null,
            "resulting_parent_id": null
        });
        for invalid in [
            json!({
                "affected_document_count": 2,
                "resulting_items": [],
                "operation": valid_operation,
                "unexpected": true
            }),
            json!({
                "affected_document_count": 2,
                "resulting_items": [],
                "operation": null
            }),
        ] {
            let error = command_output::<MoveItemData>(result(invalid))
                .err()
                .expect("invalid pipeline data must be rejected");
            assert_eq!(
                error.code,
                note_pipelines::org::OrgErrorCode::StorageFailure
            );
            assert_eq!(
                error.message,
                "Org pipeline returned an incompatible command result"
            );
            assert_eq!(error.details, json!({}));
            assert!(!error.retryable);
        }
    }
}
