use super::{
    bookkeep_expired_lease, resolve_lease, stale_lease, storage_kind, token_hash,
    validate_lease_input, LeaseProofInput, OrgError, OrgErrorCode,
};
use note_org::{
    resolve_org_timestamp, validate_dependencies, validate_item, OrgDocument as ParsedDocument,
    WorkItem, WorkspacePolicy,
};
use note_storage::{
    OrgLeaseProof, OrgProjectedWorkItem, OrgWorkspace, SanitizedOrgLease, StorageTransaction,
    StoredOrgTimestamp,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct LeaseGuardResult {
    pub active: BTreeMap<note_org::WorkItemId, SanitizedOrgLease>,
    pub expired: Vec<(OrgProjectedWorkItem, note_storage::OrgLease)>,
}

/// Validates the exact proof set for all actively leased touched items.
/// Expired open leases are bookkept only after the full proof shape is valid.
pub(crate) async fn validate_touched_lease_proofs(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    actor_id: &str,
    touched: &[OrgProjectedWorkItem],
    proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    now: i64,
) -> Result<LeaseGuardResult, OrgError> {
    validate_touched_lease_proofs_except(
        transaction,
        workspace,
        actor_id,
        touched,
        proofs,
        &BTreeSet::new(),
        now,
    )
    .await
}

/// Validates the exact proof set while deferring the conditional proof operation
/// for items whose lease will be closed by the caller in this transaction.
pub(crate) async fn validate_touched_lease_proofs_except(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    actor_id: &str,
    touched: &[OrgProjectedWorkItem],
    proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    deferred: &BTreeSet<note_org::WorkItemId>,
    now: i64,
) -> Result<LeaseGuardResult, OrgError> {
    let mut result = inspect_touched_lease_proofs_except(
        transaction,
        workspace,
        actor_id,
        touched,
        proofs,
        deferred,
        now,
    )
    .await?;
    for (item, lease) in &result.expired {
        bookkeep_expired_lease(transaction, workspace, item, lease, now).await?;
    }
    result.expired.clear();
    Ok(result)
}

/// Performs exact proof discovery and validation without mutation. Callers that
/// need to validate revisions before expiry bookkeeping can defer all writes.
pub(crate) async fn inspect_touched_lease_proofs_except(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    actor_id: &str,
    touched: &[OrgProjectedWorkItem],
    proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    deferred: &BTreeSet<note_org::WorkItemId>,
    now: i64,
) -> Result<LeaseGuardResult, OrgError> {
    let touched_by_id = touched
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    let mut active = BTreeMap::new();
    let mut expired = Vec::new();
    for item in touched_by_id.values() {
        let Some(lease) = transaction
            .get_open_org_lease_internal(item.id)
            .await
            .map_err(OrgError::storage)?
        else {
            continue;
        };
        if lease.expires_at <= now {
            expired.push(((*item).clone(), lease));
        } else {
            active.insert(item.id, lease);
        }
    }
    if proofs.keys().copied().collect::<BTreeSet<_>>()
        != active.keys().copied().collect::<BTreeSet<_>>()
    {
        return Err(stale_lease());
    }

    for (item_id, lease) in &active {
        let proof = &proofs[item_id];
        validate_lease_input(&proof.lease_id, &proof.fencing_token)?;
        let digest = token_hash(&proof.fencing_token);
        if lease.id != proof.lease_id
            || lease.workspace_id != workspace.id
            || lease.work_item_id != *item_id
            || lease.fencing_token_hash != digest
            || lease.kind != storage_kind(proof.kind)
            || lease.actor_id != actor_id
        {
            return Err(stale_lease());
        }
        if !deferred.contains(item_id) {
            resolve_lease(
                transaction
                    .validate_org_lease_proof(OrgLeaseProof {
                        lease_id: &proof.lease_id,
                        workspace_id: workspace.id,
                        work_item_id: *item_id,
                        fencing_token_hash: &digest,
                        kind: storage_kind(proof.kind),
                        actor_id,
                        now,
                    })
                    .await
                    .map_err(OrgError::storage)?,
            )?;
        }
    }

    Ok(LeaseGuardResult {
        active: active
            .into_iter()
            .map(|(id, lease)| (id, lease.into()))
            .collect(),
        expired,
    })
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
