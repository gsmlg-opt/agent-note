use std::collections::BTreeMap;

use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId};
use note_pipelines::org::{
    ApproveItemRequest, AssignItemRequest, CommandEnvelope, CreateFollowUpRequest,
    CreateItemRequest, DependencyRequest, DocumentImport, FollowUpOrigin, HeartbeatClaimRequest,
    ImportDocumentsRequest, LeaseProofInput, MoveDocumentRequest, MoveItemRequest, NoteLinkRequest,
    OperationalQuery, OperationalView, OrgClaimKind, OrgClaimResult, OrgError, OrgEventQuery,
    OrgFieldPatch, OrgReadQuery, PutDocumentRequest, RejectItemRequest, ReleaseClaimRequest,
    ReportProgressRequest, RequestReviewRequest, RetryItemRequest, ScheduleItemRequest,
    StartClaimRequest, SubmitResultRequest, TransitionItemRequest, UnlinkNoteRequest,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

fn default_limit() -> u16 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgMutationEnvelope {
    pub schema_version: u32,
    pub actor_id: String,
    pub operation_id: String,
}

impl OrgMutationEnvelope {
    pub fn into_pipeline(self, workspace_id: &str) -> Result<CommandEnvelope, OrgError> {
        let workspace_id = workspace_id
            .parse::<WorkspaceId>()
            .map_err(|_| OrgError::invalid_input("workspace_id must be a valid UUID"))?;
        Ok(CommandEnvelope {
            schema_version: self.schema_version,
            workspace_id,
            actor_id: self.actor_id,
            operation_id: self.operation_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub struct OrgPageQuery {
    /// Opaque cursor returned by the preceding page; clients must not inspect or modify it.
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schema(default = 50, minimum = 1, maximum = 200)]
    pub limit: u16,
    #[serde(default)]
    #[schema(default = false)]
    pub include_archived: bool,
}

impl From<OrgPageQuery> for OrgReadQuery {
    fn from(value: OrgPageQuery) -> Self {
        Self {
            cursor: value.cursor,
            limit: Some(usize::from(value.limit)),
            include_archived: value.include_archived,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePath {
    pub workspace_id: String,
}

impl WorkspacePath {
    pub fn parse(self) -> Result<WorkspaceId, OrgError> {
        parse_id(self.workspace_id, "workspace_id")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentPath {
    pub document_id: String,
}

impl DocumentPath {
    pub fn parse(self) -> Result<DocumentId, OrgError> {
        parse_id(self.document_id, "document_id")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemPath {
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemReadQuery {
    pub workspace_id: String,
}

impl ItemReadQuery {
    pub fn parse(self) -> Result<WorkspaceId, OrgError> {
        parse_id(self.workspace_id, "workspace_id")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemTypeBody {
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

impl WorkItemTypeBody {
    fn into_pipeline(self) -> Result<WorkItemType, OrgError> {
        adapt_input(self, "item type")
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateItemPayloadBody {
    pub document_id: String,
    pub parent_id: Option<String>,
    pub item_id: String,
    pub item_type: WorkItemTypeBody,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<String, i64>,
}

impl CreateItemPayloadBody {
    fn into_pipeline(self) -> Result<CreateItemRequest, OrgError> {
        Ok(CreateItemRequest {
            document_id: parse_id(self.document_id, "document_id")?,
            parent_id: self
                .parent_id
                .map(|id| parse_id(id, "parent_id"))
                .transpose()?,
            item_id: parse_id(self.item_id, "item_id")?,
            item_type: self.item_type.into_pipeline()?,
            title: self.title,
            initial_state: self.initial_state,
            priority: self.priority,
            tags: self.tags.into_iter().collect(),
            assignee: self.assignee,
            requires_review: self.requires_review,
            expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
        })
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    #[serde(flatten)]
    pub item: CreateItemPayloadBody,
}

impl CreateItemBody {
    pub fn into_pipeline(
        self,
        workspace_id: WorkspaceId,
    ) -> Result<(CommandEnvelope, CreateItemRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&workspace_id.to_string())?,
            self.item.into_pipeline()?,
        ))
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum FollowUpOriginBody {
    WorkItem { id: String },
    Event { id: String },
}

impl FollowUpOriginBody {
    fn into_pipeline(self) -> Result<FollowUpOrigin, OrgError> {
        match self {
            Self::WorkItem { id } => Ok(FollowUpOrigin::WorkItem(parse_id(id, "origin.id")?)),
            Self::Event { id } => Ok(FollowUpOrigin::Event(id)),
        }
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateFollowUpBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub item: CreateItemPayloadBody,
    pub origin: FollowUpOriginBody,
}

impl CreateFollowUpBody {
    pub fn into_pipeline(
        self,
        path_item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, CreateFollowUpRequest), OrgError> {
        let item = self.item.into_pipeline()?;
        if item.item_id != path_item_id {
            return Err(OrgError::invalid_input(
                "path item_id must match the new follow-up item_id",
            ));
        }
        let origin = self.origin.into_pipeline()?;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            CreateFollowUpRequest { item, origin },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub assignee: Option<String>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl AssignItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, AssignItemRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            AssignItemRequest {
                item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                assignee: self.assignee,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(
    deny_unknown_fields,
    rename_all = "snake_case",
    tag = "action",
    content = "value"
)]
pub enum FieldPatchBody {
    Unchanged,
    Set(String),
    Clear,
}

impl From<FieldPatchBody> for OrgFieldPatch<String> {
    fn from(value: FieldPatchBody) -> Self {
        match value {
            FieldPatchBody::Unchanged => Self::Unchanged,
            FieldPatchBody::Set(value) => Self::Set(value),
            FieldPatchBody::Clear => Self::Clear,
        }
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub scheduled: FieldPatchBody,
    pub deadline: FieldPatchBody,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl ScheduleItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, ScheduleItemRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            ScheduleItemRequest {
                item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                scheduled: self.scheduled.into(),
                deadline: self.deadline.into(),
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

pub trait IntoOperationalView {
    fn into_pipeline(self) -> OperationalView;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueViewBody {
    Ready,
    Assigned,
    Running,
    Blocked,
    Review,
    Failed,
    ExpiredLease,
    Completed,
}

impl IntoOperationalView for QueueViewBody {
    fn into_pipeline(self) -> OperationalView {
        match self {
            Self::Ready => OperationalView::Ready,
            Self::Assigned => OperationalView::Assigned,
            Self::Running => OperationalView::Running,
            Self::Blocked => OperationalView::Blocked,
            Self::Review => OperationalView::Review,
            Self::Failed => OperationalView::Failed,
            Self::ExpiredLease => OperationalView::ExpiredLease,
            Self::Completed => OperationalView::Completed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgendaViewBody {
    Scheduled,
    UpcomingDeadline,
}

impl IntoOperationalView for AgendaViewBody {
    fn into_pipeline(self) -> OperationalView {
        match self {
            Self::Scheduled => OperationalView::Scheduled,
            Self::UpcomingDeadline => OperationalView::UpcomingDeadline,
        }
    }
}

macro_rules! operational_query_body {
    ($name:ident, $view:ty) => {
        #[derive(Debug, Clone, Deserialize, ToSchema, utoipa::IntoParams)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            /// Comma-separated workspace UUIDs. Multiple workspaces require every selected policy to opt in.
            pub workspace_ids: String,
            pub view: $view,
            pub item_type: Option<WorkItemTypeBody>,
            pub state: Option<String>,
            #[param(pattern = r"^(none|[A-Z])$")]
            pub priority: Option<String>,
            /// Comma-separated tag intersection filter.
            pub tags: Option<String>,
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
            #[schema(default = false)]
            pub include_archived: bool,
            /// Opaque cursor returned by the preceding page; clients must not inspect or modify it.
            pub cursor: Option<String>,
            #[serde(default = "default_limit")]
            #[schema(default = 50, minimum = 1, maximum = 200)]
            pub limit: u16,
        }

        impl $name {
            pub fn into_pipeline(self) -> Result<OperationalQuery, OrgError> {
                let (priority, priority_is_none) = operational_priority_filter(self.priority)?;
                let mut workspace_ids = Vec::new();
                let mut canonical_ids = std::collections::BTreeSet::new();
                for id in self.workspace_ids.split(',') {
                    let parsed = parse_id(id.to_owned(), "workspace_id")?;
                    if !canonical_ids.insert(parsed) {
                        return Err(OrgError::invalid_input(
                            "workspace_ids must contain unique UUIDs",
                        ));
                    }
                    workspace_ids.push(parsed);
                }
                Ok(OperationalQuery {
                    workspace_ids,
                    view: self.view.into_pipeline(),
                    item_type: self
                        .item_type
                        .map(WorkItemTypeBody::into_pipeline)
                        .transpose()?,
                    state: self.state,
                    priority,
                    priority_is_none,
                    tags: self
                        .tags
                        .map(|tags| tags.split(',').map(str::to_owned).collect())
                        .unwrap_or_default(),
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
    };
}

fn operational_priority_filter(value: Option<String>) -> Result<(Option<char>, bool), OrgError> {
    match value.as_deref() {
        None => Ok((None, false)),
        Some("none") => Ok((None, true)),
        Some(value) => {
            let mut chars = value.chars();
            match (chars.next(), chars.next()) {
                (Some(priority), None) if priority.is_ascii_uppercase() => {
                    Ok((Some(priority), false))
                }
                _ => Err(OrgError::invalid_input(
                    "Org priority filter must be 'none' or one uppercase ASCII letter",
                )),
            }
        }
    }
}

operational_query_body!(QueueQueryBody, QueueViewBody);
operational_query_body!(AgendaQueryBody, AgendaViewBody);

impl ItemPath {
    pub fn parse(self) -> Result<WorkItemId, OrgError> {
        parse_id(self.item_id, "item_id")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DependencyPath {
    pub item_id: String,
    pub dependency_item_id: String,
}

impl DependencyPath {
    pub fn parse(self) -> Result<(WorkItemId, WorkItemId), OrgError> {
        Ok((
            parse_id(self.item_id, "item_id")?,
            parse_id(self.dependency_item_id, "dependency_item_id")?,
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddDependencyBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub dependency_id: String,
    pub document_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl AddDependencyBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, DependencyRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            DependencyRequest {
                item_id,
                dependency_id: parse_id(self.dependency_id, "dependency_id")?,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveDependencyBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl RemoveDependencyBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
        dependency_id: WorkItemId,
    ) -> Result<(CommandEnvelope, DependencyRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            DependencyRequest {
                item_id,
                dependency_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkNoteBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub purpose: String,
    pub note_id: String,
    pub description: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl LinkNoteBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, NoteLinkRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            NoteLinkRequest {
                item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                purpose: self.purpose,
                note_id: parse_id(self.note_id, "note_id")?,
                description: self.description,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnlinkNoteBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub purpose: String,
    pub note_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease: Option<LeaseProofBody>,
}

impl UnlinkNoteBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, UnlinkNoteRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            UnlinkNoteRequest {
                item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                purpose: self.purpose,
                note_id: parse_id(self.note_id, "note_id")?,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease: self.lease.map(Into::into),
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NotePath {
    pub note_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub struct EventQueryBody {
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    /// Opaque sequence cursor bound to workspace and subject filters.
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schema(default = 50, minimum = 1, maximum = 200)]
    pub limit: u16,
}

impl EventQueryBody {
    pub fn into_pipeline(self, workspace_id: WorkspaceId) -> OrgEventQuery {
        OrgEventQuery {
            workspace_id,
            subject_kind: self.subject_kind,
            subject_id: self.subject_id,
            cursor: self.cursor,
            limit: Some(usize::from(self.limit)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentReadQuery {
    pub workspace_id: String,
}

impl DocumentReadQuery {
    pub fn parse(self) -> Result<WorkspaceId, OrgError> {
        parse_id(self.workspace_id, "workspace_id")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicyInput {
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

impl WorkspacePolicyInput {
    fn into_pipeline(self) -> Result<note_org::WorkspacePolicy, OrgError> {
        adapt_input(self, "workspace policy")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TagRuleInput {
    pub allowed: Vec<String>,
    pub required: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicyInput,
}

impl CreateWorkspaceBody {
    pub fn into_pipeline(
        self,
    ) -> Result<(CommandEnvelope, note_pipelines::org::CreateWorkspaceRequest), OrgError> {
        let workspace_id = self
            .workspace_id
            .parse::<WorkspaceId>()
            .map_err(|_| OrgError::invalid_input("workspace_id must be a valid UUID"))?;
        let policy = self.policy.into_pipeline()?;
        let command = self.command.into_pipeline(&workspace_id.to_string())?;
        Ok((
            command,
            note_pipelines::org::CreateWorkspaceRequest {
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkspaceBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub expected_revision: i64,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicyInput,
}

impl UpdateWorkspaceBody {
    pub fn into_pipeline(
        self,
        workspace_id: WorkspaceId,
    ) -> Result<(CommandEnvelope, note_pipelines::org::UpdateWorkspaceRequest), OrgError> {
        let policy = self.policy.into_pipeline()?;
        let command = self.command.into_pipeline(&workspace_id.to_string())?;
        Ok((
            command,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveWorkspaceBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub expected_revision: i64,
}

impl ArchiveWorkspaceBody {
    pub fn into_pipeline(
        self,
        workspace_id: WorkspaceId,
    ) -> Result<
        (
            CommandEnvelope,
            note_pipelines::org::ArchiveWorkspaceRequest,
        ),
        OrgError,
    > {
        Ok((
            self.command.into_pipeline(&workspace_id.to_string())?,
            note_pipelines::org::ArchiveWorkspaceRequest {
                expected_revision: self.expected_revision,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseProofBody {
    pub lease_id: String,
    pub kind: ClaimKindBody,
    /// Sensitive raw fencing token; never returned by read operations.
    pub fencing_token: String,
}

impl From<LeaseProofBody> for LeaseProofInput {
    fn from(value: LeaseProofBody) -> Self {
        Self {
            lease_id: value.lease_id,
            kind: value.kind.into(),
            fencing_token: value.fencing_token,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKindBody {
    Execution,
    Review,
}

impl From<ClaimKindBody> for OrgClaimKind {
    fn from(value: ClaimKindBody) -> Self {
        match value {
            ClaimKindBody::Execution => Self::Execution,
            ClaimKindBody::Review => Self::Review,
        }
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub kind: ClaimKindBody,
}

impl ClaimItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, StartClaimRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            StartClaimRequest {
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                kind: self.kind.into(),
            },
        ))
    }
}

#[derive(Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimResultBody {
    pub schema_version: u32,
    pub workspace_id: String,
    pub operation_id: String,
    pub lease_id: String,
    /// Sensitive raw fencing token. Store securely and send only in lease-bound mutation bodies.
    pub fencing_token: String,
    pub expires_at: i64,
    pub event_ids: Vec<String>,
    pub context: serde_json::Value,
}

impl TryFrom<OrgClaimResult> for ClaimResultBody {
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
            context: safe_json(value.context)?,
        })
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatClaimBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub lease_id: String,
    pub kind: ClaimKindBody,
    /// Sensitive raw fencing token obtained from a successful claim or retry.
    pub fencing_token: String,
}

impl HeartbeatClaimBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, HeartbeatClaimRequest), OrgError> {
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            HeartbeatClaimRequest {
                work_item_id: item_id,
                lease_id: self.lease_id,
                kind: self.kind.into(),
                fencing_token: self.fencing_token,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseClaimBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    pub kind: ClaimKindBody,
    /// Sensitive raw fencing token obtained from a successful claim or retry.
    pub fencing_token: String,
    pub target_state: Option<String>,
}

impl ReleaseClaimBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, ReleaseClaimRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            ReleaseClaimRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                lease_id: self.lease_id,
                kind: self.kind.into(),
                fencing_token: self.fencing_token,
                target_state: self.target_state,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportProgressBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub lease_id: String,
    pub kind: ClaimKindBody,
    /// Sensitive raw fencing token obtained from a successful claim or retry.
    pub fencing_token: String,
    pub summary: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ReportProgressBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, ReportProgressRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            ReportProgressRequest {
                schema_version,
                work_item_id: item_id,
                lease_id: self.lease_id,
                kind: self.kind.into(),
                fencing_token: self.fencing_token,
                summary: self.summary,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptNoteBody {
    pub purpose: String,
    pub note_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBody {
    pub uri: String,
    pub media_type: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitResultBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    /// Sensitive raw fencing token obtained from a successful execution claim.
    pub fencing_token: String,
    pub result_summary: String,
    pub note_refs: Vec<AttemptNoteBody>,
    pub artifacts: Vec<ArtifactBody>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl SubmitResultBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, SubmitResultRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            SubmitResultRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                lease_id: self.lease_id,
                fencing_token: self.fencing_token,
                result_summary: self.result_summary,
                note_refs: adapt_input(self.note_refs, "attempt note references")?,
                artifacts: adapt_input(self.artifacts, "artifact references")?,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub target_state: String,
    pub lease: Option<LeaseProofBody>,
    pub error: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl TransitionItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, TransitionItemRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            TransitionItemRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                target_state: self.target_state,
                lease: self.lease.map(Into::into),
                error: self.error,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
}

impl RetryItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, RetryItemRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            RetryItemRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestReviewBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    /// Sensitive raw fencing token obtained from a successful execution claim.
    pub fencing_token: String,
    pub result_summary: Option<String>,
    pub note_refs: Vec<AttemptNoteBody>,
    pub artifacts: Vec<ArtifactBody>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl RequestReviewBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, RequestReviewRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            RequestReviewRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                lease_id: self.lease_id,
                fencing_token: self.fencing_token,
                result_summary: self.result_summary,
                note_refs: adapt_input(self.note_refs, "attempt note references")?,
                artifacts: adapt_input(self.artifacts, "artifact references")?,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    /// Sensitive raw fencing token obtained from a successful review claim.
    pub fencing_token: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ApproveItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, ApproveItemRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            ApproveItemRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                lease_id: self.lease_id,
                fencing_token: self.fencing_token,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub document_id: String,
    pub expected_document_revision: i64,
    pub lease_id: String,
    /// Sensitive raw fencing token obtained from a successful review claim.
    pub fencing_token: String,
    pub reason: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl RejectItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, RejectItemRequest), OrgError> {
        let schema_version = self.command.schema_version;
        Ok((
            self.command.into_pipeline(&self.workspace_id)?,
            RejectItemRequest {
                schema_version,
                work_item_id: item_id,
                document_id: parse_id(self.document_id, "document_id")?,
                expected_document_revision: self.expected_document_revision,
                lease_id: self.lease_id,
                fencing_token: self.fencing_token,
                reason: self.reason,
                metadata: metadata_value(self.metadata),
            },
        ))
    }
}

fn metadata_value(values: BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Object(values.into_iter().collect())
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutDocumentBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub path: String,
    pub source: String,
    pub expected_revision: Option<i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofBody>,
}

impl PutDocumentBody {
    pub fn into_pipeline(
        self,
        document_id: DocumentId,
    ) -> Result<(CommandEnvelope, PutDocumentRequest), OrgError> {
        let command = self.command.into_pipeline(&self.workspace_id)?;
        Ok((
            command,
            PutDocumentRequest {
                document_id,
                path: self.path,
                source: self.source,
                expected_revision: self.expected_revision,
                lease_proofs: adapt_id_map(self.lease_proofs, "work_item_id")?,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveDocumentBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub target_workspace_id: String,
    pub expected_document_revision: i64,
    pub expected_source_workspace_revision: i64,
    pub expected_target_workspace_revision: i64,
    pub lease_proofs: BTreeMap<String, LeaseProofBody>,
}

impl MoveDocumentBody {
    pub fn into_pipeline(
        self,
        document_id: DocumentId,
    ) -> Result<(CommandEnvelope, MoveDocumentRequest), OrgError> {
        let command = self.command.into_pipeline(&self.workspace_id)?;
        Ok((
            command,
            MoveDocumentRequest {
                document_id,
                target_workspace_id: parse_id(self.target_workspace_id, "target_workspace_id")?,
                expected_document_revision: self.expected_document_revision,
                expected_source_workspace_revision: self.expected_source_workspace_revision,
                expected_target_workspace_revision: self.expected_target_workspace_revision,
                lease_proofs: adapt_id_map(self.lease_proofs, "work_item_id")?,
            },
        ))
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveItemBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub source_document_id: String,
    pub target_document_id: String,
    pub target_parent_id: Option<String>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofBody>,
}

impl MoveItemBody {
    pub fn into_pipeline(
        self,
        item_id: WorkItemId,
    ) -> Result<(CommandEnvelope, MoveItemRequest), OrgError> {
        let command = self.command.into_pipeline(&self.workspace_id)?;
        Ok((
            command,
            MoveItemRequest {
                item_id,
                source_document_id: parse_id(self.source_document_id, "source_document_id")?,
                target_document_id: parse_id(self.target_document_id, "target_document_id")?,
                target_parent_id: self
                    .target_parent_id
                    .map(|id| parse_id(id, "target_parent_id"))
                    .transpose()?,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease_proofs: adapt_id_map(self.lease_proofs, "work_item_id")?,
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentImportBody {
    pub document_id: String,
    pub path: String,
    pub source: String,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportWorkspaceBody {
    #[serde(flatten)]
    pub command: OrgMutationEnvelope,
    pub documents: Vec<DocumentImportBody>,
    pub expected_revisions: BTreeMap<String, i64>,
    pub lease_proofs: BTreeMap<String, LeaseProofBody>,
}

impl ImportWorkspaceBody {
    pub fn into_pipeline(
        self,
        workspace_id: WorkspaceId,
    ) -> Result<(CommandEnvelope, ImportDocumentsRequest), OrgError> {
        let documents = self
            .documents
            .into_iter()
            .map(|document| {
                Ok(DocumentImport {
                    document_id: parse_id(document.document_id, "document_id")?,
                    path: document.path,
                    source: document.source,
                    archived_at: None,
                })
            })
            .collect::<Result<Vec<_>, OrgError>>()?;
        Ok((
            self.command.into_pipeline(&workspace_id.to_string())?,
            ImportDocumentsRequest {
                documents,
                expected_revisions: adapt_id_map(self.expected_revisions, "document_id")?,
                lease_proofs: adapt_id_map(self.lease_proofs, "work_item_id")?,
            },
        ))
    }
}

fn parse_id<T>(value: String, name: &str) -> Result<T, OrgError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| OrgError::invalid_input(format!("{name} must be a valid UUID")))
}

fn adapt_input<T, U>(value: T, name: &str) -> Result<U, OrgError>
where
    T: Serialize,
    U: for<'de> Deserialize<'de>,
{
    serde_json::from_value(
        serde_json::to_value(value)
            .map_err(|_| OrgError::invalid_input(format!("invalid {name}")))?,
    )
    .map_err(|_| OrgError::invalid_input(format!("invalid {name}")))
}

fn adapt_id_map<K, V, U>(
    values: BTreeMap<String, V>,
    key_name: &str,
) -> Result<BTreeMap<K, U>, OrgError>
where
    K: Ord + std::str::FromStr,
    U: From<V>,
{
    let mut adapted = BTreeMap::new();
    for (id, value) in values {
        if adapted
            .insert(parse_id(id, key_name)?, value.into())
            .is_some()
        {
            return Err(OrgError::invalid_input(format!(
                "{key_name} keys must be unique UUIDs"
            )));
        }
    }
    Ok(adapted)
}

pub fn safe_json<T: Serialize>(value: T) -> Result<serde_json::Value, OrgError> {
    let mut value = serde_json::to_value(value).map_err(|_| {
        OrgError::new(
            note_pipelines::org::OrgErrorCode::StorageFailure,
            "Org pipeline returned an incompatible response",
            serde_json::json!({}),
            false,
        )
    })?;
    redact_sensitive_values(&mut value);
    Ok(value)
}

fn redact_sensitive_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                redact_sensitive_values(value);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if is_sensitive_key(key) {
                    *value = serde_json::Value::String("[REDACTED]".to_owned());
                } else {
                    redact_sensitive_values(value);
                }
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
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
    let compact = normalized.replace('_', "");
    normalized.split('_').any(|word| {
        matches!(
            word,
            "token" | "hash" | "secret" | "password" | "authentication" | "authorization"
        )
    }) || matches!(compact.as_str(), "apikey" | "accesstoken")
        || ["token", "hash", "secret", "password"]
            .iter()
            .any(|suffix| compact.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_envelope_conversion_preserves_transport_neutral_fields() {
        let rest = OrgMutationEnvelope {
            schema_version: 1,
            actor_id: "agent-a".to_owned(),
            operation_id: "operation-a".to_owned(),
        };
        let rest_json = serde_json::to_value(&rest).unwrap();
        let pipeline = rest
            .into_pipeline("10000000-0000-4000-8000-000000000001")
            .unwrap();
        let pipeline_json = serde_json::to_value(pipeline).unwrap();

        for field in ["schema_version", "actor_id", "operation_id"] {
            assert_eq!(rest_json[field], pipeline_json[field]);
        }
        assert_eq!(
            pipeline_json["workspace_id"],
            "10000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn page_query_conversion_uses_documented_default_and_bounds_type() {
        let query: OrgPageQuery = serde_json::from_value(serde_json::json!({
            "cursor": "opaque-cursor"
        }))
        .unwrap();
        assert_eq!(query.limit, 50);

        let pipeline: OrgReadQuery = query.into();
        assert_eq!(pipeline.cursor.as_deref(), Some("opaque-cursor"));
        assert_eq!(pipeline.limit, Some(50));
        assert!(!pipeline.include_archived);
    }

    #[test]
    fn id_maps_reject_distinct_spellings_that_normalize_to_one_uuid() {
        let mut values = BTreeMap::new();
        values.insert("20000000-0000-4000-8000-00000000000a".to_owned(), 1_i64);
        values.insert("20000000-0000-4000-8000-00000000000A".to_owned(), 2_i64);

        let error = adapt_id_map::<DocumentId, _, i64>(values, "document_id").unwrap_err();

        assert_eq!(error.code, note_pipelines::org::OrgErrorCode::InvalidInput);
        assert_eq!(error.message, "document_id keys must be unique UUIDs");
    }

    #[test]
    fn ordinary_document_import_rejects_archived_state() {
        assert!(
            serde_json::from_value::<DocumentImportBody>(serde_json::json!({
                "document_id": "20000000-0000-4000-8000-000000000001",
                "path": "main.org",
                "source": "",
                "archived_at": 1
            }))
            .is_err()
        );
    }
}
