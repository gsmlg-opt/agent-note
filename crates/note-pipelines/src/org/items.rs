use super::{
    execute_idempotent, CommandEnvelope, ConservativeLeaseGuard, LeaseGuard, LeaseGuardOperation,
    OrgCommandKind, OrgCommandResult, OrgContext, OrgError, OrgErrorCode,
    ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{
    parse_document, DocumentId, NewWorkItem, NoteLink, PropertyKey, SemanticEdit, WorkItemId,
    WorkItemType, WorkspaceId, WorkspacePolicy,
};
use note_storage::{
    NewOrgEvent, OrgDocument, OrgDocumentUpdate, OrgEventType, OrgProjectedWorkItem, OrgWorkspace,
    StorageTransaction,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const CREATE_ITEM: OrgCommandKind = OrgCommandKind::new("create_item", 1);
const CREATE_FOLLOW_UP: OrgCommandKind = OrgCommandKind::new("create_follow_up", 1);
const MOVE_ITEM: OrgCommandKind = OrgCommandKind::new("move_item", 1);
const REPARENT_ITEM: OrgCommandKind = OrgCommandKind::new("reparent_item", 1);
const ASSIGN_ITEM: OrgCommandKind = OrgCommandKind::new("assign_item", 1);
const SCHEDULE_ITEM: OrgCommandKind = OrgCommandKind::new("schedule_item", 1);
const ADD_DEPENDENCY: OrgCommandKind = OrgCommandKind::new("add_dependency", 1);
const REMOVE_DEPENDENCY: OrgCommandKind = OrgCommandKind::new("remove_dependency", 1);
const LINK_NOTE: OrgCommandKind = OrgCommandKind::new("link_note", 1);
const UNLINK_NOTE: OrgCommandKind = OrgCommandKind::new("unlink_note", 1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateItemRequest {
    pub document_id: DocumentId,
    pub parent_id: Option<WorkItemId>,
    pub item_id: WorkItemId,
    pub item_type: WorkItemType,
    pub title: String,
    pub initial_state: Option<String>,
    pub priority: Option<char>,
    pub tags: BTreeSet<String>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowUpOrigin {
    WorkItem(WorkItemId),
    Event(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateFollowUpRequest {
    pub item: CreateItemRequest,
    pub origin: FollowUpOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveItemRequest {
    pub item_id: WorkItemId,
    pub source_document_id: DocumentId,
    pub target_document_id: DocumentId,
    pub target_parent_id: Option<WorkItemId>,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReparentItemRequest {
    pub item_id: WorkItemId,
    pub document_id: DocumentId,
    pub target_parent_id: Option<WorkItemId>,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignItemRequest {
    pub item_id: WorkItemId,
    pub document_id: DocumentId,
    pub assignee: Option<String>,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleItemRequest {
    pub item_id: WorkItemId,
    pub document_id: DocumentId,
    pub scheduled: OrgFieldPatch<String>,
    pub deadline: OrgFieldPatch<String>,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub enum OrgFieldPatch<T> {
    Unchanged,
    Set(T),
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRequest {
    pub item_id: WorkItemId,
    pub dependency_id: WorkItemId,
    pub document_id: DocumentId,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLinkRequest {
    pub item_id: WorkItemId,
    pub document_id: DocumentId,
    pub purpose: String,
    pub note_id: uuid::Uuid,
    pub description: String,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkNoteRequest {
    pub item_id: WorkItemId,
    pub document_id: DocumentId,
    pub purpose: String,
    pub note_id: uuid::Uuid,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
}

pub async fn create_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &CreateItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_create(context, envelope, CREATE_ITEM, request.clone(), None).await
}

pub async fn create_follow_up(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &CreateFollowUpRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_create(
        context,
        envelope,
        CREATE_FOLLOW_UP,
        request.item.clone(),
        Some(request.origin.clone()),
    )
    .await
}

async fn execute_create(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    kind: OrgCommandKind,
    request: CreateItemRequest,
    origin: Option<FollowUpOrigin>,
) -> Result<OrgCommandResult, OrgError> {
    validate_create_request(&request)?;
    let fingerprint_request = json!({"item": request, "origin": origin});
    let request = request.clone();
    let origin = origin.clone();
    let command = envelope.clone();
    execute_idempotent(
        context,
        kind,
        envelope,
        &fingerprint_request,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_workspace(transaction, command.workspace_id).await?;
                if let Some(origin) = &origin {
                    validate_follow_up_origin(transaction, workspace.id, request.item_id, origin)
                        .await?;
                }
                let state = request
                    .initial_state
                    .clone()
                    .unwrap_or_else(|| workspace.policy.initial_state.clone());
                if state == workspace.policy.running_state || state == workspace.policy.review_state
                {
                    return Err(OrgError::invalid_input(
                        "New Org work cannot start in a lease-controlled state",
                    ));
                }
                if state != workspace.policy.initial_state
                    && !workspace.policy.executable_states.contains(&state)
                {
                    return Err(OrgError::invalid_input(
                        "Requested Org initial state is not an allowed initial state",
                    ));
                }
                let loaded = load_documents(
                    transaction,
                    &workspace,
                    &request.expected_revisions,
                    BTreeSet::from([request.document_id]),
                )
                .await?;
                let document = &loaded[&request.document_id];
                let edited = document
                    .parsed
                    .apply(SemanticEdit::AppendItem {
                        parent_id: request.parent_id,
                        item: NewWorkItem {
                            id: request.item_id,
                            item_type: request.item_type,
                            title: request.title.clone(),
                            state: state.clone(),
                            priority: request.priority,
                            tags: request.tags.clone(),
                        },
                    })
                    .map_err(edit_error)?;
                let mut source = edited.source;
                if request.assignee.is_some() {
                    source = apply_edit(
                        &source,
                        &workspace.policy,
                        SemanticEdit::SetProperty {
                            item_id: request.item_id,
                            key: PropertyKey::Assignee,
                            value: request.assignee.clone(),
                        },
                    )?;
                }
                if request.requires_review {
                    source = apply_edit(
                        &source,
                        &workspace.policy,
                        SemanticEdit::SetProperty {
                            item_id: request.item_id,
                            key: PropertyKey::RequiresReview,
                            value: Some("true".into()),
                        },
                    )?;
                }
                let metadata = json!({
                    "schema_version": 1,
                    "document_id": request.document_id,
                    "parent_id": request.parent_id,
                    "origin": origin.as_ref().map(origin_metadata),
                    "initial_state": state,
                    "item_type": request.item_type,
                    "title": request.title,
                    "priority": request.priority,
                    "tags": request.tags,
                    "requires_review": request.requires_review,
                    "initial_assignee": request.assignee,
                });
                let mut events = vec![PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::Creation,
                    summary: "Created Org work item",
                    metadata,
                    previous_state: None,
                    resulting_state: None,
                }];
                if let Some(assignee) = &request.assignee {
                    events.push(PendingEvent {
                        subject_id: request.item_id,
                        event_type: OrgEventType::Assignment,
                        summary: "Assigned newly created Org work item",
                        metadata: json!({
                            "previous_assignee": null,
                            "resulting_assignee": assignee,
                        }),
                        previous_state: Some(state.clone()),
                        resulting_state: Some(state.clone()),
                    });
                }
                commit_sources(
                    transaction,
                    &workspace,
                    &command,
                    loaded,
                    BTreeMap::from([(request.document_id, source)]),
                    BTreeSet::new(),
                    None,
                    events,
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub async fn move_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &MoveItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_move(context, envelope, MOVE_ITEM, request.clone()).await
}

pub async fn reparent_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ReparentItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_move(
        context,
        envelope,
        REPARENT_ITEM,
        MoveItemRequest {
            item_id: request.item_id,
            source_document_id: request.document_id,
            target_document_id: request.document_id,
            target_parent_id: request.target_parent_id,
            expected_revisions: request.expected_revisions.clone(),
        },
    )
    .await
}

async fn execute_move(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    kind: OrgCommandKind,
    request: MoveItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    let required = BTreeSet::from([request.source_document_id, request.target_document_id]);
    validate_expected_keys(&request.expected_revisions, &required)?;
    let fingerprint = request.clone();
    let command = envelope.clone();
    execute_idempotent(
        context,
        kind,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_workspace(transaction, command.workspace_id).await?;
                let loaded = load_documents(
                    transaction,
                    &workspace,
                    &request.expected_revisions,
                    required,
                )
                .await?;
                let source_document = &loaded[&request.source_document_id];
                let root = source_document
                    .parsed
                    .item(request.item_id)
                    .ok_or_else(|| not_found("work item"))?;
                let previous_parent = root.parent_id;
                if request.source_document_id == request.target_document_id
                    && previous_parent == request.target_parent_id
                {
                    return Err(OrgError::invalid_input(
                        "Org item move does not change its parent or document",
                    ));
                }
                let previous_state = root.state.clone();
                let moved_ids = subtree_ids(&source_document.parsed, request.item_id)?;
                let sources = if request.source_document_id == request.target_document_id {
                    let edited = note_org::reparent_item(
                        &source_document.parsed,
                        request.item_id,
                        request.target_parent_id,
                    )
                    .map_err(edit_error)?;
                    BTreeMap::from([(request.source_document_id, edited.source)])
                } else {
                    let target_document = &loaded[&request.target_document_id];
                    let edited = note_org::move_item(
                        &source_document.parsed,
                        &target_document.parsed,
                        request.item_id,
                        request.target_parent_id,
                    )
                    .map_err(edit_error)?;
                    BTreeMap::from([
                        (request.source_document_id, edited.source),
                        (request.target_document_id, edited.target),
                    ])
                };
                let metadata = json!({
                    "source_document_id": request.source_document_id,
                    "target_document_id": request.target_document_id,
                    "previous_parent_id": previous_parent,
                    "resulting_parent_id": request.target_parent_id,
                });
                commit_sources(
                    transaction,
                    &workspace,
                    &command,
                    loaded,
                    sources,
                    moved_ids,
                    Some(LeaseGuardOperation::ItemMove),
                    vec![PendingEvent {
                        subject_id: request.item_id,
                        event_type: OrgEventType::ItemMove,
                        summary: "Moved Org work item",
                        metadata,
                        previous_state: previous_state.clone(),
                        resulting_state: previous_state,
                    }],
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub async fn assign_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &AssignItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    if request.assignee.as_ref().is_some_and(|value| {
        value.trim().is_empty() || value != value.trim() || value.contains(['\r', '\n'])
    }) {
        return Err(OrgError::invalid_input(
            "Org assignee must be one trimmed nonblank line",
        ));
    }
    let request = request.clone();
    let fingerprint = request.clone();
    execute_single_item_edit(
        context,
        envelope,
        ASSIGN_ITEM,
        &fingerprint,
        request.document_id,
        request.item_id,
        request.expected_revisions.clone(),
        Some(LeaseGuardOperation::Assignment),
        move |item, document, policy| {
            if item.assignee == request.assignee {
                return Err(OrgError::invalid_input(
                    "Org assignment mutation does not change the assignee",
                ));
            }
            let source = apply_edit(
                document.source(),
                policy,
                SemanticEdit::SetProperty {
                    item_id: request.item_id,
                    key: PropertyKey::Assignee,
                    value: request.assignee.clone(),
                },
            )?;
            Ok((
                source,
                vec![PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::Assignment,
                    summary: "Changed Org work-item assignment",
                    metadata: json!({
                        "previous_assignee": item.assignee,
                        "resulting_assignee": request.assignee,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                }],
            ))
        },
    )
    .await
}

pub async fn schedule_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ScheduleItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    let request = request.clone();
    let fingerprint = request.clone();
    execute_single_item_edit(
        context,
        envelope,
        SCHEDULE_ITEM,
        &fingerprint,
        request.document_id,
        request.item_id,
        request.expected_revisions.clone(),
        None,
        move |item, document, policy| {
            let mut source = document.source().to_string();
            let mut events = Vec::new();
            let previous_scheduled = item.scheduled.as_ref().map(|value| value.raw.clone());
            let resulting_scheduled = apply_field_patch(&previous_scheduled, &request.scheduled);
            if previous_scheduled != resulting_scheduled {
                source = apply_edit(
                    &source,
                    policy,
                    SemanticEdit::SetScheduled {
                        item_id: request.item_id,
                        value: resulting_scheduled.clone(),
                    },
                )?;
                events.push(PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::ScheduleChange,
                    summary: "Changed Org work-item schedule",
                    metadata: json!({
                        "previous_scheduled": previous_scheduled,
                        "resulting_scheduled": resulting_scheduled,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                });
            }
            let previous_deadline = item.deadline.as_ref().map(|value| value.raw.clone());
            let resulting_deadline = apply_field_patch(&previous_deadline, &request.deadline);
            if previous_deadline != resulting_deadline {
                source = apply_edit(
                    &source,
                    policy,
                    SemanticEdit::SetDeadline {
                        item_id: request.item_id,
                        value: resulting_deadline.clone(),
                    },
                )?;
                events.push(PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::DeadlineChange,
                    summary: "Changed Org work-item deadline",
                    metadata: json!({
                        "previous_deadline": previous_deadline,
                        "resulting_deadline": resulting_deadline,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                });
            }
            if events.is_empty() {
                return Err(OrgError::invalid_input(
                    "Org schedule mutation has no changes",
                ));
            }
            Ok((source, events))
        },
    )
    .await
}

pub async fn add_dependency(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &DependencyRequest,
) -> Result<OrgCommandResult, OrgError> {
    change_dependency(context, envelope, ADD_DEPENDENCY, request, true).await
}

pub async fn remove_dependency(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &DependencyRequest,
) -> Result<OrgCommandResult, OrgError> {
    change_dependency(context, envelope, REMOVE_DEPENDENCY, request, false).await
}

async fn change_dependency(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    kind: OrgCommandKind,
    request: &DependencyRequest,
    add: bool,
) -> Result<OrgCommandResult, OrgError> {
    if add && request.item_id == request.dependency_id {
        return Err(OrgError::invalid_input("Work item cannot depend on itself"));
    }
    let request = request.clone();
    let fingerprint = request.clone();
    execute_single_item_edit(
        context,
        envelope,
        kind,
        &fingerprint,
        request.document_id,
        request.item_id,
        request.expected_revisions.clone(),
        None,
        move |item, document, policy| {
            let previous_dependencies = item.dependencies.clone();
            let mut dependencies = item.dependencies.iter().copied().collect::<BTreeSet<_>>();
            let changed = if add {
                dependencies.insert(request.dependency_id)
            } else {
                dependencies.remove(&request.dependency_id)
            };
            if !changed {
                return Err(OrgError::invalid_input(
                    "Org dependency mutation does not change the dependency set",
                ));
            }
            let value = (!dependencies.is_empty()).then(|| {
                dependencies
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            let source = apply_edit(
                document.source(),
                policy,
                SemanticEdit::SetProperty {
                    item_id: request.item_id,
                    key: PropertyKey::DependsOn,
                    value,
                },
            )?;
            Ok((
                source,
                vec![PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::DependencyChange,
                    summary: if add {
                        "Added Org work-item dependency"
                    } else {
                        "Removed Org work-item dependency"
                    },
                    metadata: json!({
                        "dependency_id": request.dependency_id,
                        "action": if add { "add" } else { "remove" },
                        "previous_dependencies": previous_dependencies,
                        "resulting_dependencies": dependencies,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                }],
            ))
        },
    )
    .await
}

pub async fn link_note(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &NoteLinkRequest,
) -> Result<OrgCommandResult, OrgError> {
    let request = request.clone();
    let fingerprint = request.clone();
    execute_single_item_edit(
        context,
        envelope,
        LINK_NOTE,
        &fingerprint,
        request.document_id,
        request.item_id,
        request.expected_revisions.clone(),
        None,
        move |item, document, policy| {
            let source = apply_edit(
                document.source(),
                policy,
                SemanticEdit::AddNoteLink {
                    item_id: request.item_id,
                    link: NoteLink {
                        purpose: request.purpose.clone(),
                        note_id: request.note_id,
                        description: request.description.clone(),
                    },
                },
            )?;
            Ok((
                source,
                vec![PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::NoteLinkChange,
                    summary: "Linked Markdown note to Org work item",
                    metadata: json!({
                        "action": "link",
                        "purpose": request.purpose,
                        "note_id": request.note_id,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                }],
            ))
        },
    )
    .await
}

pub async fn unlink_note(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &UnlinkNoteRequest,
) -> Result<OrgCommandResult, OrgError> {
    let request = request.clone();
    let fingerprint = request.clone();
    execute_single_item_edit(
        context,
        envelope,
        UNLINK_NOTE,
        &fingerprint,
        request.document_id,
        request.item_id,
        request.expected_revisions.clone(),
        None,
        move |item, document, policy| {
            let source = apply_edit(
                document.source(),
                policy,
                SemanticEdit::RemoveNoteLink {
                    item_id: request.item_id,
                    purpose: request.purpose.clone(),
                    note_id: request.note_id,
                },
            )?;
            Ok((
                source,
                vec![PendingEvent {
                    subject_id: request.item_id,
                    event_type: OrgEventType::NoteLinkChange,
                    summary: "Unlinked Markdown note from Org work item",
                    metadata: json!({
                        "action": "unlink",
                        "purpose": request.purpose,
                        "note_id": request.note_id,
                    }),
                    previous_state: item.state.clone(),
                    resulting_state: item.state.clone(),
                }],
            ))
        },
    )
    .await
}

pub fn validate_item_transition(
    policy: &WorkspacePolicy,
    item: &OrgProjectedWorkItem,
    workspace_items: &[OrgProjectedWorkItem],
    target_state: &str,
    review_approved: bool,
) -> Result<(), OrgError> {
    let domain = workspace_items
        .iter()
        .map(projected_to_domain)
        .collect::<Vec<_>>();
    let graph = note_org::validate_dependencies(&domain, &policy.successful_terminal_states)
        .map_err(|error| {
            OrgError::new(
                OrgErrorCode::InvalidInput,
                "Org workspace dependency graph is invalid",
                json!({"reason": error.to_string()}),
                false,
            )
        })?;
    let from = item
        .state
        .clone()
        .ok_or_else(|| OrgError::invalid_input("Org work item has no workflow state"))?;
    note_org::validate_transition(
        policy,
        &note_org::TransitionInput {
            item_type: item.item_type,
            from,
            to: target_state.to_string(),
            item_requires_review: item.requires_review,
            review_approved,
            dependencies_satisfied: graph.dependencies_satisfied(item.id),
        },
    )
    .map_err(|error| match error {
        note_org::TransitionError::DependenciesIncomplete => OrgError::new(
            OrgErrorCode::DependencyBlocked,
            "Org work-item dependencies are incomplete",
            json!({"work_item_id": item.id}),
            false,
        ),
        note_org::TransitionError::ReviewRequired(_) => OrgError::new(
            OrgErrorCode::ReviewRequired,
            "Org work item requires review before successful completion",
            json!({"work_item_id": item.id}),
            false,
        ),
        other => OrgError::new(
            OrgErrorCode::InvalidTransition,
            "Org work-item transition is not allowed",
            json!({"reason": other.to_string()}),
            false,
        ),
    })
}

async fn execute_single_item_edit<R, F>(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    kind: OrgCommandKind,
    fingerprint: &R,
    document_id: DocumentId,
    item_id: WorkItemId,
    expected_revisions: BTreeMap<DocumentId, i64>,
    guard_operation: Option<LeaseGuardOperation>,
    edit: F,
) -> Result<OrgCommandResult, OrgError>
where
    R: Serialize + ?Sized,
    F: FnOnce(
            &OrgProjectedWorkItem,
            &note_org::OrgDocument,
            &WorkspacePolicy,
        ) -> Result<(String, Vec<PendingEvent>), OrgError>
        + Send
        + 'static,
{
    validate_expected_keys(&expected_revisions, &BTreeSet::from([document_id]))?;
    let fingerprint = serde_json::to_value(fingerprint)
        .map_err(|_| OrgError::invalid_input("Org item request cannot be serialized"))?;
    let command = envelope.clone();
    execute_idempotent(
        context,
        kind,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_workspace(transaction, command.workspace_id).await?;
                let loaded = load_documents(
                    transaction,
                    &workspace,
                    &expected_revisions,
                    BTreeSet::from([document_id]),
                )
                .await?;
                let projected = transaction
                    .get_org_work_item(item_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| not_found("work item"))?;
                if projected.workspace_id != workspace.id || projected.document_id != document_id {
                    return Err(not_found("work item"));
                }
                let (source, events) =
                    edit(&projected, &loaded[&document_id].parsed, &workspace.policy)?;
                commit_sources(
                    transaction,
                    &workspace,
                    &command,
                    loaded,
                    BTreeMap::from([(document_id, source)]),
                    BTreeSet::from([item_id]),
                    guard_operation,
                    events,
                    now,
                )
                .await
            })
        },
    )
    .await
}

struct LoadedDocument {
    stored: OrgDocument,
    parsed: note_org::OrgDocument,
}

struct PendingEvent {
    subject_id: WorkItemId,
    event_type: OrgEventType,
    summary: &'static str,
    metadata: serde_json::Value,
    previous_state: Option<String>,
    resulting_state: Option<String>,
}

async fn load_documents(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    expected_revisions: &BTreeMap<DocumentId, i64>,
    required: BTreeSet<DocumentId>,
) -> Result<BTreeMap<DocumentId, LoadedDocument>, OrgError> {
    validate_expected_keys(expected_revisions, &required)?;
    let mut loaded = BTreeMap::new();
    for document_id in required {
        let stored = transaction
            .get_org_document(document_id)
            .await
            .map_err(OrgError::storage)?
            .ok_or_else(|| not_found("document"))?;
        if stored.workspace_id != workspace.id {
            return Err(not_found("document"));
        }
        let expected = expected_revisions[&document_id];
        if stored.revision != expected {
            return Err(OrgError::new(
                OrgErrorCode::StaleRevision,
                "Org document revision is stale",
                json!({"document_id": document_id, "current_revision": stored.revision}),
                true,
            ));
        }
        let parsed = parse_document(stored.source.clone(), &workspace.policy.parse_options())
            .map_err(parse_error)?;
        loaded.insert(document_id, LoadedDocument { stored, parsed });
    }
    Ok(loaded)
}

#[allow(clippy::too_many_arguments)]
async fn commit_sources(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    command: &CommandEnvelope,
    loaded: BTreeMap<DocumentId, LoadedDocument>,
    sources: BTreeMap<DocumentId, String>,
    guard_item_ids: BTreeSet<WorkItemId>,
    guard_operation: Option<LeaseGuardOperation>,
    events: Vec<PendingEvent>,
    now: i64,
) -> Result<OrgCommandResult, OrgError> {
    if sources.keys().copied().collect::<BTreeSet<_>>()
        != loaded.keys().copied().collect::<BTreeSet<_>>()
    {
        return Err(OrgError::invalid_input(
            "Org mutation must update every expected affected document",
        ));
    }
    let old_projection = transaction
        .list_org_workspace_projection(workspace.id)
        .await
        .map_err(OrgError::storage)?;
    let created_at = old_projection
        .iter()
        .map(|item| (item.id, item.created_at))
        .collect::<BTreeMap<_, _>>();
    let affected = loaded.keys().copied().collect::<BTreeSet<_>>();
    let mut candidate = old_projection
        .iter()
        .filter(|item| !affected.contains(&item.document_id))
        .cloned()
        .collect::<Vec<_>>();
    for (document_id, source) in &sources {
        let parsed = parse_document(source.clone(), &workspace.policy.parse_options())
            .map_err(parse_error)?;
        candidate.extend(super::project_document(
            &parsed,
            workspace.id,
            *document_id,
            &workspace.timezone,
            &created_at,
            now,
        )?);
    }
    super::validate_projected_items_for_policy(&candidate, &workspace.policy)?;
    validate_new_note_targets(transaction, &old_projection, &candidate).await?;
    let candidate_by_id = candidate
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    let mut guarded = old_projection
        .iter()
        .filter(|item| guard_item_ids.contains(&item.id))
        .cloned()
        .collect::<Vec<_>>();
    guarded.extend(
        guard_item_ids
            .iter()
            .filter_map(|id| candidate_by_id.get(id).copied().cloned()),
    );
    if let Some(operation) = guard_operation {
        ConservativeLeaseGuard.validate(operation, &workspace.policy, &guarded)?;
    }

    let mut revisions = BTreeMap::new();
    for (document_id, source) in &sources {
        let stored = &loaded[document_id].stored;
        let updated = super::resolve_cas(
            transaction
                .compare_and_swap_org_document(OrgDocumentUpdate {
                    id: *document_id,
                    expected_revision: stored.revision,
                    path: &stored.path,
                    source,
                    content_hash: &content_hash(source),
                    updated_at: now,
                })
                .await
                .map_err(OrgError::storage)?,
            "document",
        )?;
        revisions.insert(document_id.to_string(), updated.revision);
    }
    transaction
        .rebuild_org_workspace_projection(workspace.id, &candidate)
        .await
        .map_err(OrgError::storage)?;
    let result_data = command_result_data(&candidate, &events, sources.len());
    let mut event_ids = Vec::new();
    for event in events {
        let appended = transaction
            .append_org_event(NewOrgEvent {
                id: &uuid::Uuid::new_v4().to_string(),
                workspace_id: workspace.id,
                subject_kind: "work_item",
                subject_id: &event.subject_id.to_string(),
                actor_id: &command.actor_id,
                attempt_id: None,
                event_type: event.event_type,
                occurred_at: now,
                summary: event.summary,
                metadata: &event.metadata,
                previous_state: event.previous_state.as_deref(),
                resulting_state: event.resulting_state.as_deref(),
            })
            .await
            .map_err(OrgError::storage)?;
        event_ids.push(appended.id);
    }
    Ok(OrgCommandResult {
        schema_version: ORG_COMMAND_SCHEMA_VERSION,
        workspace_id: workspace.id,
        operation_id: command.operation_id.clone(),
        event_ids,
        workspace_revision: Some(workspace.revision),
        document_revisions: revisions,
        data: result_data,
    })
}

async fn validate_follow_up_origin(
    transaction: &dyn StorageTransaction,
    workspace_id: WorkspaceId,
    new_item_id: WorkItemId,
    origin: &FollowUpOrigin,
) -> Result<(), OrgError> {
    match origin {
        FollowUpOrigin::WorkItem(work_item_id) => {
            if *work_item_id == new_item_id {
                return Err(OrgError::invalid_input(
                    "Org follow-up cannot reference itself as its origin",
                ));
            }
            let item = transaction
                .get_org_work_item(*work_item_id)
                .await
                .map_err(OrgError::storage)?
                .ok_or_else(|| not_found("follow-up origin work item"))?;
            if item.workspace_id != workspace_id {
                return Err(not_found("follow-up origin work item"));
            }
        }
        FollowUpOrigin::Event(event_id) => {
            if event_id.trim().is_empty() || event_id != event_id.trim() {
                return Err(OrgError::invalid_input(
                    "Org follow-up event origin must be nonblank and trimmed",
                ));
            }
            let mut after_sequence = None;
            let mut found = false;
            loop {
                let page = transaction
                    .list_org_events(workspace_id, after_sequence, 200)
                    .await
                    .map_err(OrgError::storage)?;
                if page.iter().any(|event| event.id == *event_id) {
                    found = true;
                    break;
                }
                if page.len() < 200 {
                    break;
                }
                after_sequence = page.last().map(|event| event.sequence);
            }
            if !found {
                return Err(not_found("follow-up origin event"));
            }
        }
    }
    Ok(())
}

fn apply_field_patch<T: Clone>(current: &Option<T>, patch: &OrgFieldPatch<T>) -> Option<T> {
    match patch {
        OrgFieldPatch::Unchanged => current.clone(),
        OrgFieldPatch::Set(value) => Some(value.clone()),
        OrgFieldPatch::Clear => None,
    }
}

fn command_result_data(
    candidate: &[OrgProjectedWorkItem],
    events: &[PendingEvent],
    affected_document_count: usize,
) -> serde_json::Value {
    let subject_ids = events
        .iter()
        .map(|event| event.subject_id)
        .collect::<BTreeSet<_>>();
    let resulting_items = candidate
        .iter()
        .filter(|item| subject_ids.contains(&item.id))
        .map(projected_item_metadata)
        .collect::<Vec<_>>();
    let operation = events.first().map(|event| event.metadata.clone());
    json!({
        "affected_document_count": affected_document_count,
        "resulting_items": resulting_items,
        "operation": operation,
    })
}

fn projected_item_metadata(item: &OrgProjectedWorkItem) -> serde_json::Value {
    json!({
        "id": item.id,
        "workspace_id": item.workspace_id,
        "document_id": item.document_id,
        "parent_id": item.parent_id,
        "item_type": item.item_type,
        "title": item.title,
        "state": item.state,
        "priority": item.priority,
        "scheduled": item.scheduled.as_ref().map(|value| &value.raw),
        "deadline": item.deadline.as_ref().map(|value| &value.raw),
        "assignee": item.assignee,
        "requires_review": item.requires_review,
        "created_at": item.created_at,
        "tags": item.tags,
        "dependencies": item.dependencies,
        "note_links": item.note_links,
    })
}

async fn validate_new_note_targets(
    transaction: &dyn StorageTransaction,
    old: &[OrgProjectedWorkItem],
    candidate: &[OrgProjectedWorkItem],
) -> Result<(), OrgError> {
    let old_links = old
        .iter()
        .flat_map(|item| {
            item.note_links
                .iter()
                .map(move |link| (item.id, link.purpose.clone(), link.note_id))
        })
        .collect::<BTreeSet<_>>();
    for item in candidate {
        for link in &item.note_links {
            if old_links.contains(&(item.id, link.purpose.clone(), link.note_id)) {
                continue;
            }
            if transaction
                .get_note(&link.note_id.to_string())
                .await
                .map_err(OrgError::storage)?
                .is_none()
            {
                return Err(OrgError::new(
                    OrgErrorCode::NoteUnavailable,
                    "Linked Markdown note is unavailable",
                    json!({"note_id": link.note_id}),
                    false,
                ));
            }
        }
    }
    Ok(())
}

fn validate_create_request(request: &CreateItemRequest) -> Result<(), OrgError> {
    if request.title.trim().is_empty()
        || request.title != request.title.trim()
        || request.title.contains(['\r', '\n'])
    {
        return Err(OrgError::invalid_input(
            "Org work-item title must be one trimmed nonblank line",
        ));
    }
    if request.assignee.as_ref().is_some_and(|value| {
        value.trim().is_empty() || value != value.trim() || value.contains(['\r', '\n'])
    }) {
        return Err(OrgError::invalid_input(
            "Org assignee must be one trimmed nonblank line",
        ));
    }
    validate_expected_keys(
        &request.expected_revisions,
        &BTreeSet::from([request.document_id]),
    )
}

fn validate_expected_keys(
    expected: &BTreeMap<DocumentId, i64>,
    required: &BTreeSet<DocumentId>,
) -> Result<(), OrgError> {
    if expected.keys().copied().collect::<BTreeSet<_>>() != *required
        || expected.values().any(|revision| *revision < 1)
    {
        return Err(OrgError::invalid_input(
            "Expected document revisions must exactly match every affected document",
        ));
    }
    Ok(())
}

fn apply_edit(
    source: &str,
    policy: &WorkspacePolicy,
    edit: SemanticEdit,
) -> Result<String, OrgError> {
    parse_document(source.to_string(), &policy.parse_options())
        .map_err(parse_error)?
        .apply(edit)
        .map(|edited| edited.source)
        .map_err(edit_error)
}

fn subtree_ids(
    document: &note_org::OrgDocument,
    root: WorkItemId,
) -> Result<BTreeSet<WorkItemId>, OrgError> {
    if document.item(root).is_none() {
        return Err(not_found("work item"));
    }
    let parents = document
        .items()
        .iter()
        .map(|item| (item.id, item.parent_id))
        .collect::<BTreeMap<_, _>>();
    Ok(document
        .items()
        .iter()
        .filter(|item| {
            let mut current = Some(item.id);
            while let Some(id) = current {
                if id == root {
                    return true;
                }
                current = parents.get(&id).copied().flatten();
            }
            false
        })
        .map(|item| item.id)
        .collect())
}

fn origin_metadata(origin: &FollowUpOrigin) -> serde_json::Value {
    match origin {
        FollowUpOrigin::WorkItem(work_item_id) => {
            json!({"schema_version": 1, "kind": "work_item", "work_item_id": work_item_id})
        }
        FollowUpOrigin::Event(event_id) => {
            json!({"schema_version": 1, "kind": "event", "event_id": event_id})
        }
    }
}

fn projected_to_domain(item: &OrgProjectedWorkItem) -> note_org::WorkItem {
    note_org::WorkItem {
        id: item.id,
        item_type: item.item_type,
        parent_id: item.parent_id,
        level: 1,
        title: item.title.clone(),
        state: item.state.clone(),
        priority: item.priority,
        tags: item.tags.iter().cloned().collect(),
        scheduled: None,
        deadline: None,
        assignee: item.assignee.clone(),
        depends_on: item.dependencies.iter().copied().collect(),
        requires_review: item.requires_review,
        note_links: item.note_links.clone(),
    }
}

async fn require_workspace(
    transaction: &dyn StorageTransaction,
    workspace_id: WorkspaceId,
) -> Result<OrgWorkspace, OrgError> {
    transaction
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| not_found("workspace"))
}

fn content_hash(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

fn parse_error(error: note_org::OrgError) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidInput,
        "Org document is invalid",
        json!({"reason": error.to_string()}),
        false,
    )
}

fn edit_error(error: note_org::OrgError) -> OrgError {
    match error {
        note_org::OrgError::ItemNotFound(_) => not_found("work item"),
        note_org::OrgError::UnsafeEdit(_) => OrgError::new(
            OrgErrorCode::UnsupportedSemanticEdit,
            "Org semantic edit cannot preserve opaque source safely",
            json!({}),
            false,
        ),
        other => OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org semantic edit is invalid",
            json!({"reason": other.to_string()}),
            false,
        ),
    }
}

fn not_found(resource: &'static str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource}),
        false,
    )
}
