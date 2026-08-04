use super::{OrgError, OrgErrorCode};
use note_org::{
    resolve_org_timestamp, validate_dependencies, validate_item, OrgDocument as ParsedDocument,
    WorkItem, WorkspacePolicy,
};
use note_storage::{OrgProjectedWorkItem, StorageTransaction, StoredOrgTimestamp};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseGuardOperation {
    Archive,
    RawImport,
    DocumentMove,
    ItemMove,
    Assignment,
}

/// Fail-closed Slice 3 seam. Slice 4 replaces this state-based check with
/// authoritative lease lookup and per-item fencing-token validation.
pub trait LeaseGuard: Send + Sync {
    fn validate(
        &self,
        operation: LeaseGuardOperation,
        policy: &WorkspacePolicy,
        items: &[OrgProjectedWorkItem],
    ) -> Result<(), OrgError>;
}

#[derive(Debug, Default)]
pub struct ConservativeLeaseGuard;

impl LeaseGuard for ConservativeLeaseGuard {
    fn validate(
        &self,
        operation: LeaseGuardOperation,
        policy: &WorkspacePolicy,
        items: &[OrgProjectedWorkItem],
    ) -> Result<(), OrgError> {
        let unsafe_item = items.iter().find(|item| {
            item.state.as_ref().is_some_and(|state| {
                state == &policy.running_state || state == &policy.review_state
            })
        });
        if let Some(item) = unsafe_item {
            return Err(OrgError::new(
                OrgErrorCode::InvalidTransition,
                "Org mutation is unavailable until active lease fencing is validated",
                json!({
                    "work_item_id": item.id,
                    "operation": format!("{operation:?}"),
                }),
                false,
            ));
        }
        Ok(())
    }
}

pub(crate) fn project_document(
    document: &ParsedDocument,
    workspace_id: note_org::WorkspaceId,
    document_id: note_org::DocumentId,
    timezone: &str,
    existing_created_at: &BTreeMap<note_org::WorkItemId, i64>,
    now: i64,
) -> Result<Vec<OrgProjectedWorkItem>, OrgError> {
    document
        .items()
        .iter()
        .enumerate()
        .map(|(source_order, item)| {
            let scheduled = item
                .scheduled
                .as_ref()
                .map(|value| stored_timestamp(value, timezone))
                .transpose()?;
            let deadline = item
                .deadline
                .as_ref()
                .map(|value| stored_timestamp(value, timezone))
                .transpose()?;
            Ok(OrgProjectedWorkItem {
                id: item.id,
                workspace_id,
                document_id,
                parent_id: item.parent_id,
                source_order: i64::try_from(source_order).map_err(|_| {
                    OrgError::invalid_input("Org document contains too many work items")
                })?,
                item_type: item.item_type,
                title: item.title.clone(),
                state: item.state.clone(),
                priority: item.priority,
                scheduled,
                deadline,
                assignee: item.assignee.clone(),
                requires_review: item.requires_review,
                created_at: existing_created_at.get(&item.id).copied().unwrap_or(now),
                tags: item.tags.iter().cloned().collect(),
                dependencies: item.depends_on.iter().copied().collect(),
                note_links: item.note_links.clone(),
            })
        })
        .collect()
}

pub(crate) async fn reproject_workspace(
    transaction: &dyn StorageTransaction,
    workspace_id: note_org::WorkspaceId,
    timezone: &str,
    policy: &WorkspacePolicy,
    now: i64,
) -> Result<Vec<OrgProjectedWorkItem>, OrgError> {
    let old = transaction
        .list_org_workspace_projection(workspace_id)
        .await
        .map_err(OrgError::storage)?;
    validate_projected_items_for_policy(&old, policy)?;
    let created_at = old
        .iter()
        .map(|item| (item.id, item.created_at))
        .collect::<BTreeMap<_, _>>();
    let documents = transaction
        .list_org_documents(workspace_id)
        .await
        .map_err(OrgError::storage)?;
    let mut candidate = Vec::new();
    for document in documents {
        let parsed = note_org::parse_document(document.source, &policy.parse_options()).map_err(
            |error| {
                OrgError::new(
                    OrgErrorCode::InvalidInput,
                    "Candidate workspace policy cannot parse canonical Org source",
                    json!({"reason": error.to_string(), "document_id": document.id}),
                    false,
                )
            },
        )?;
        candidate.extend(project_document(
            &parsed,
            workspace_id,
            document.id,
            timezone,
            &created_at,
            now,
        )?);
    }
    validate_projected_items_for_policy(&candidate, policy)?;
    let old_by_id = old
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    let candidate_by_id = candidate
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    if old_by_id.len() != candidate_by_id.len()
        || old_by_id.iter().any(|(id, old)| {
            candidate_by_id
                .get(id)
                .is_none_or(|candidate| !same_policy_reprojection_semantics(old, candidate))
        })
    {
        return Err(OrgError::new(
            OrgErrorCode::InvalidInput,
            "Candidate workspace policy would reinterpret canonical Org work-item semantics",
            json!({}),
            false,
        ));
    }
    Ok(candidate)
}

fn same_policy_reprojection_semantics(
    old: &OrgProjectedWorkItem,
    candidate: &OrgProjectedWorkItem,
) -> bool {
    old.id == candidate.id
        && old.workspace_id == candidate.workspace_id
        && old.document_id == candidate.document_id
        && old.parent_id == candidate.parent_id
        && old.source_order == candidate.source_order
        && old.item_type == candidate.item_type
        && old.title == candidate.title
        && old.state == candidate.state
        && old.priority == candidate.priority
        && same_local_timestamp(old.scheduled.as_ref(), candidate.scheduled.as_ref())
        && same_local_timestamp(old.deadline.as_ref(), candidate.deadline.as_ref())
        && old.assignee == candidate.assignee
        && old.requires_review == candidate.requires_review
        && old.created_at == candidate.created_at
        && old.tags == candidate.tags
        && old.dependencies == candidate.dependencies
        && old.note_links == candidate.note_links
}

fn same_local_timestamp(
    old: Option<&StoredOrgTimestamp>,
    candidate: Option<&StoredOrgTimestamp>,
) -> bool {
    match (old, candidate) {
        (None, None) => true,
        (Some(old), Some(candidate)) => old.raw == candidate.raw && old.local == candidate.local,
        _ => false,
    }
}

fn stored_timestamp(
    value: &note_org::OrgTimestamp,
    timezone: &str,
) -> Result<StoredOrgTimestamp, OrgError> {
    let resolved = resolve_org_timestamp(value, timezone).map_err(|error| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org timestamp cannot be resolved in the workspace timezone",
            json!({"reason": error.to_string()}),
            false,
        )
    })?;
    Ok(StoredOrgTimestamp {
        raw: resolved.raw,
        local: resolved.local.to_string(),
        timezone: resolved.timezone,
        utc_timestamp: resolved.utc_timestamp,
    })
}

pub(crate) fn validate_projected_items_for_policy(
    items: &[OrgProjectedWorkItem],
    policy: &WorkspacePolicy,
) -> Result<(), OrgError> {
    for item in items {
        let domain = projected_to_domain(item);
        validate_item(policy, &domain).map_err(policy_item_error)?;
        if let Some(state) = &item.state {
            if !policy.states.contains(state) {
                return Err(OrgError::new(
                    OrgErrorCode::InvalidInput,
                    "Org work item state is not allowed by workspace policy",
                    json!({"work_item_id": item.id, "state": state}),
                    false,
                ));
            }
        }
        if policy.review_required_types.contains(&item.item_type)
            && item
                .state
                .as_ref()
                .is_some_and(|state| policy.successful_terminal_states.contains(state))
            && !item.requires_review
        {
            return Err(OrgError::new(
                OrgErrorCode::ReviewRequired,
                "Candidate policy would invalidate completed work review requirements",
                json!({"work_item_id": item.id}),
                false,
            ));
        }
    }
    let domain = items.iter().map(projected_to_domain).collect::<Vec<_>>();
    validate_dependencies(&domain, &policy.successful_terminal_states).map_err(|error| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org workspace dependency graph is invalid",
            json!({"reason": error.to_string()}),
            false,
        )
    })?;
    Ok(())
}

pub(crate) fn validate_policy_role_compatibility(
    current: &WorkspacePolicy,
    candidate: &WorkspacePolicy,
    items: &[OrgProjectedWorkItem],
) -> Result<(), OrgError> {
    for item in items {
        let Some(state) = item.state.as_ref() else {
            continue;
        };
        let current_roles = state_roles(current, state);
        let candidate_roles = state_roles(candidate, state);
        if current_roles != candidate_roles {
            return Err(OrgError::new(
                OrgErrorCode::InvalidInput,
                "Candidate workspace policy would change an existing work item's workflow role",
                json!({"work_item_id": item.id, "state": state}),
                false,
            ));
        }
    }
    Ok(())
}

fn state_roles(policy: &WorkspacePolicy, state: &str) -> [bool; 11] {
    [
        policy.initial_state == state,
        policy.running_state == state,
        policy.review_state == state,
        policy.failed_state == state,
        policy.cancelled_state == state,
        policy.release_state == state,
        policy.review_rejection_state == state,
        policy.lease_expiry_recovery_state == state,
        policy.executable_states.contains(state),
        policy.successful_terminal_states.contains(state),
        policy.terminal_states.contains(state),
    ]
}

pub(crate) fn projected_to_domain(item: &OrgProjectedWorkItem) -> WorkItem {
    WorkItem {
        id: item.id,
        item_type: item.item_type,
        parent_id: item.parent_id,
        level: 1,
        title: item.title.clone(),
        state: item.state.clone(),
        priority: item.priority,
        tags: item.tags.iter().cloned().collect::<BTreeSet<_>>(),
        scheduled: None,
        deadline: None,
        assignee: item.assignee.clone(),
        depends_on: item.dependencies.iter().copied().collect(),
        requires_review: item.requires_review,
        note_links: item.note_links.clone(),
    }
}

fn policy_item_error(error: note_org::PolicyError) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidInput,
        "Org work item is not allowed by workspace policy",
        json!({"reason": error.to_string()}),
        false,
    )
}
