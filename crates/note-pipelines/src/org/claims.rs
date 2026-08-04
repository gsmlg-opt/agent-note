use super::idempotency::request_fingerprint;
use super::{
    execute_idempotent, get_item_context_in_transaction, project_document, projected_to_domain,
    resolve_cas, validate_item_transition, validate_projected_items_for_policy, CommandEnvelope,
    HeartbeatClaimRequest, OrgClaimKind, OrgClaimPhase, OrgClaimResult, OrgCommandKind,
    OrgCommandResult, OrgContext, OrgError, OrgErrorCode, OrgWorkflowPhase, ReleaseClaimRequest,
    StartClaimRequest, ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{
    AttemptPhase, ClaimDecisionError, ClaimKind, ClaimQueue, ClaimRequest, LeaseKind, LeaseStatus,
    ReadinessBlocker, ReadinessContext, SemanticEdit, TransitionTarget,
};
use note_storage::{
    ConditionalUpdate, ExpiredOrgLeaseClosure, NewOrgAttemptAllocation, NewOrgEvent, NewOrgLease,
    OrgAttemptStatus, OrgAttemptUpdate, OrgDocument, OrgDocumentUpdate, OrgEventType, OrgLease,
    OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseHeartbeat, OrgLeaseKind, OrgLeaseProof,
    OrgProjectedWorkItem, OrgWorkspace, StorageTransaction, StoredOrgOperation, TransactionMode,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CLAIM_ITEM: OrgCommandKind = OrgCommandKind::new("claim_item", 1);
const HEARTBEAT_CLAIM: OrgCommandKind = OrgCommandKind::new("heartbeat_claim", 1);
const RELEASE_CLAIM: OrgCommandKind = OrgCommandKind::new("release_claim", 1);

pub async fn claim_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &StartClaimRequest,
) -> Result<OrgClaimResult, OrgError> {
    validate_envelope(envelope)?;
    if request.expected_document_revision <= 0 {
        return Err(OrgError::invalid_input(
            "Expected Org document revision must be positive",
        ));
    }
    let fingerprint = command_fingerprint(CLAIM_ITEM, envelope, request)?;
    let transaction = context
        .storage()
        .begin(TransactionMode::Immediate)
        .await
        .map_err(OrgError::storage)?;

    let existing = match transaction
        .get_org_operation(envelope.workspace_id, &envelope.operation_id)
        .await
    {
        Ok(existing) => existing,
        Err(error) => {
            return finish_claim_write(transaction, Err(OrgError::storage(error))).await;
        }
    };
    if let Some(existing) = existing {
        let result = if existing.request_fingerprint == fingerprint {
            serde_json::from_value(existing.result).map_err(|_| stored_result_error())
        } else {
            Err(idempotency_conflict(envelope))
        };
        return finish_claim_read(transaction, result).await;
    }

    let result = execute_claim_transaction(
        context,
        transaction.as_ref(),
        envelope,
        request,
        &fingerprint,
    )
    .await;
    finish_claim_write(transaction, result).await
}

async fn execute_claim_transaction(
    context: &OrgContext,
    transaction: &dyn StorageTransaction,
    envelope: &CommandEnvelope,
    request: &StartClaimRequest,
    fingerprint: &str,
) -> Result<OrgClaimResult, OrgError> {
    let now = context.clock().now();
    let workspace = require_active_workspace(transaction, envelope.workspace_id).await?;
    let document = load_document(transaction, &workspace, request.document_id).await?;
    let item = transaction
        .get_org_work_item(request.work_item_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|item| item.workspace_id == workspace.id && item.document_id == request.document_id)
        .ok_or_else(|| not_found("work item"))?;
    let projection = transaction
        .list_org_workspace_projection(workspace.id)
        .await
        .map_err(OrgError::storage)?;
    let graph = note_org::validate_dependencies(
        &projection
            .iter()
            .map(projected_to_domain)
            .collect::<Vec<_>>(),
        &workspace.policy.successful_terminal_states,
    )
    .map_err(|error| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org workspace dependency graph is invalid",
            json!({"reason": error.to_string()}),
            false,
        )
    })?;
    let attempts = transaction
        .list_org_attempts(item.id)
        .await
        .map_err(OrgError::storage)?;
    let current_attempt = attempts.last();
    let execution_attempt_count = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
    let open_lease = transaction
        .get_open_org_lease_internal(item.id)
        .await
        .map_err(OrgError::storage)?;
    let lease_status = open_lease
        .as_ref()
        .map(|lease| {
            let kind = domain_lease_kind(lease.kind);
            if lease.expires_at > now {
                LeaseStatus::Active(kind)
            } else {
                LeaseStatus::Expired(kind)
            }
        })
        .unwrap_or(LeaseStatus::None);
    if matches!(lease_status, LeaseStatus::Active(_)) {
        return Err(active_lease(item.id));
    }
    validate_document_revision(&document, request.expected_document_revision)?;
    let active_count = transaction
        .count_active_org_leases(workspace.id, now)
        .await
        .map_err(OrgError::storage)?;
    let readiness = ReadinessContext {
        now,
        dependencies_satisfied: graph.dependencies_satisfied(item.id),
        workspace_active: workspace.archived_at.is_none(),
        lease: lease_status,
        capacity_available: active_count < capacity(&workspace)?,
        actor_id: Some(envelope.actor_id.clone()),
        scheduled_at: item.scheduled.as_ref().map(|value| value.utc_timestamp),
        queue: match request.kind {
            OrgClaimKind::Execution => ClaimQueue::Execution,
            OrgClaimKind::Review => ClaimQueue::Review,
        },
        execution_attempt_count,
        current_attempt: current_attempt.map(|attempt| match attempt.status {
            OrgAttemptStatus::Running => AttemptPhase::Running,
            OrgAttemptStatus::Submitted => AttemptPhase::Submitted,
            OrgAttemptStatus::Completed
            | OrgAttemptStatus::Failed
            | OrgAttemptStatus::Cancelled
            | OrgAttemptStatus::Expired => AttemptPhase::Terminal,
        }),
    };
    let requested_lease_kind = domain_kind(request.kind);
    let claim_kind = if matches!(lease_status, LeaseStatus::Expired(LeaseKind::Execution))
        && request.kind == OrgClaimKind::Execution
    {
        ClaimKind::Reclaim
    } else {
        ClaimKind::Claim
    };
    let decision = note_org::decide_claim(
        &projected_to_domain(&item),
        &workspace.policy,
        &readiness,
        ClaimRequest {
            lease_kind: requested_lease_kind,
            claim_kind,
        },
    )
    .map_err(map_claim_error)?;

    let mut event_ids = Vec::new();
    if let Some(expired) = open_lease.as_ref().filter(|lease| lease.expires_at <= now) {
        if expired.kind != storage_kind(request.kind) {
            return Err(OrgError::new(
                OrgErrorCode::InvalidTransition,
                "Expired Org lease kind does not match the requested claim",
                json!({"work_item_id": item.id}),
                false,
            ));
        }
        event_ids.push(bookkeep_expired_lease(transaction, &workspace, &item, expired, now).await?);
    }

    let raw_token = context.token_source().generate_token()?;
    if raw_token.trim().is_empty() {
        return Err(OrgError::new(
            OrgErrorCode::StorageFailure,
            "Secure Org fencing token generation failed",
            json!({}),
            true,
        ));
    }
    let token_hash = token_hash(&raw_token);

    match &decision.transition_target {
        TransitionTarget::Unchanged => document.clone(),
        TransitionTarget::State(target) => {
            write_claim_state(
                transaction,
                &workspace,
                &document,
                &projection,
                item.id,
                &[target.as_str()],
                now,
            )
            .await?
        }
        TransitionTarget::Recovery {
            intermediate,
            final_state,
        } => {
            write_claim_state(
                transaction,
                &workspace,
                &document,
                &projection,
                item.id,
                &[intermediate.as_str(), final_state.as_str()],
                now,
            )
            .await?
        }
    };
    context.after_claim_phase(OrgClaimPhase::SourceEdit)?;

    let attempt_id = if decision.allocate_execution_attempt {
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let attempt = transaction
            .allocate_next_org_attempt(NewOrgAttemptAllocation {
                id: &attempt_id,
                workspace_id: workspace.id,
                work_item_id: item.id,
                actor_id: &envelope.actor_id,
                started_at: now,
                note_refs: &[],
                artifacts: &[],
                metadata: &json!({}),
            })
            .await
            .map_err(OrgError::storage)?;
        attempt.id
    } else {
        current_attempt
            .filter(|attempt| attempt.status == OrgAttemptStatus::Submitted)
            .map(|attempt| attempt.id.clone())
            .ok_or_else(|| inconsistent_attempt(item.id))?
    };
    context.after_claim_phase(OrgClaimPhase::AttemptAllocation)?;

    let lease_id = uuid::Uuid::new_v4().to_string();
    let expires_at = now.saturating_add(lease_duration(&workspace));
    let lease = transaction
        .insert_org_lease_if_capacity(
            NewOrgLease {
                id: &lease_id,
                workspace_id: workspace.id,
                work_item_id: item.id,
                attempt_id: &attempt_id,
                kind: storage_kind(request.kind),
                actor_id: &envelope.actor_id,
                fencing_token_hash: &token_hash,
                acquired_at: now,
                last_heartbeat_at: now,
                expires_at,
            },
            capacity(&workspace)?,
            now,
        )
        .await
        .map_err(OrgError::storage)?;
    if !matches!(lease, ConditionalUpdate::Applied(_)) {
        let existing = transaction
            .get_open_org_lease_internal(item.id)
            .await
            .map_err(OrgError::storage)?;
        return Err(if existing.is_some_and(|lease| lease.expires_at > now) {
            active_lease(item.id)
        } else {
            concurrency_limit(workspace.id)
        });
    }
    context.after_claim_phase(OrgClaimPhase::LeaseInsert)?;

    let previous_state = item.state.as_deref();
    let resulting_state = match &decision.transition_target {
        TransitionTarget::Unchanged => previous_state,
        TransitionTarget::State(state) => Some(state.as_str()),
        TransitionTarget::Recovery { final_state, .. } => Some(final_state.as_str()),
    };
    event_ids.push(
        append_event(
            transaction,
            &uuid::Uuid::new_v4().to_string(),
            &workspace,
            item.id,
            &envelope.actor_id,
            Some(&attempt_id),
            OrgEventType::Claim,
            now,
            "Claimed Org work item",
            &json!({"schema_version": 1, "lease_kind": request.kind}),
            previous_state,
            resulting_state,
        )
        .await?,
    );
    if request.kind == OrgClaimKind::Execution {
        event_ids.push(
            append_event(
                transaction,
                &uuid::Uuid::new_v4().to_string(),
                &workspace,
                item.id,
                &envelope.actor_id,
                Some(&attempt_id),
                OrgEventType::Start,
                now,
                "Started Org work item execution",
                &json!({"schema_version": 1}),
                previous_state,
                resulting_state,
            )
            .await?,
        );
    }
    context.after_claim_phase(OrgClaimPhase::Events)?;

    let hydrated = get_item_context_in_transaction(transaction, workspace.id, item.id, now).await?;
    context.after_claim_phase(OrgClaimPhase::ContextHydration)?;
    let result = OrgClaimResult {
        schema_version: ORG_COMMAND_SCHEMA_VERSION,
        workspace_id: workspace.id,
        operation_id: envelope.operation_id.clone(),
        lease_id,
        fencing_token: raw_token,
        expires_at,
        event_ids,
        context: hydrated,
    };
    transaction
        .insert_org_operation(&StoredOrgOperation {
            workspace_id: workspace.id,
            operation_id: envelope.operation_id.clone(),
            request_fingerprint: fingerprint.to_string(),
            result: serde_json::to_value(&result)
                .map_err(|_| OrgError::invalid_input("Org claim result cannot be serialized"))?,
            created_at: now,
        })
        .await
        .map_err(OrgError::storage)?;
    context.after_claim_phase(OrgClaimPhase::OperationWrite)?;
    Ok(result)
}

pub async fn heartbeat_claim(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &HeartbeatClaimRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_token_request(&request.lease_id, &request.fencing_token)?;
    let fingerprint = json!({
        "work_item_id": request.work_item_id,
        "lease_id": request.lease_id,
        "kind": request.kind,
        "fencing_token_digest": token_hash(&request.fencing_token),
    });
    let request = request.clone();
    let command = envelope.clone();
    execute_idempotent(
        context,
        HEARTBEAT_CLAIM,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_active_workspace(transaction, command.workspace_id).await?;
                let token_hash = token_hash(&request.fencing_token);
                let expires_at = now.saturating_add(lease_duration(&workspace));
                let lease = resolve_lease_update(
                    transaction
                        .heartbeat_org_lease(OrgLeaseHeartbeat {
                            proof: lease_proof(&command, &request, &token_hash, now),
                            last_heartbeat_at: now,
                            expires_at,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                )?;
                let event_id = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    request.work_item_id,
                    &command.actor_id,
                    Some(&lease.attempt_id),
                    OrgEventType::Heartbeat,
                    now,
                    "Renewed Org claim lease",
                    &json!({"schema_version": 1, "lease_kind": request.kind}),
                    None,
                    None,
                )
                .await?;
                let hydrated = get_item_context_in_transaction(
                    transaction,
                    workspace.id,
                    request.work_item_id,
                    now,
                )
                .await?;
                Ok(command_result(
                    &command,
                    &workspace,
                    vec![event_id],
                    &hydrated,
                    json!({"lease": hydrated.lease, "context": hydrated}),
                ))
            })
        },
    )
    .await
}

pub async fn release_claim(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ReleaseClaimRequest,
) -> Result<OrgCommandResult, OrgError> {
    if request.schema_version != 1 || request.expected_document_revision <= 0 {
        return Err(OrgError::invalid_input(
            "Invalid Org release request schema or revision",
        ));
    }
    validate_envelope(envelope)?;
    validate_token_request(&request.lease_id, &request.fencing_token)?;
    let fingerprint_request = json!({
        "schema_version": request.schema_version,
        "work_item_id": request.work_item_id,
        "document_id": request.document_id,
        "expected_document_revision": request.expected_document_revision,
        "lease_id": request.lease_id,
        "kind": request.kind,
        "fencing_token_digest": token_hash(&request.fencing_token),
        "target_state": request.target_state,
    });
    let fingerprint = command_fingerprint(RELEASE_CLAIM, envelope, &fingerprint_request)?;
    let transaction = context
        .storage()
        .begin(TransactionMode::Immediate)
        .await
        .map_err(OrgError::storage)?;
    let existing = match transaction
        .get_org_operation(envelope.workspace_id, &envelope.operation_id)
        .await
    {
        Ok(existing) => existing,
        Err(error) => {
            return finish_release_error(transaction, OrgError::storage(error)).await;
        }
    };
    if let Some(existing) = existing {
        let result = if existing.request_fingerprint == fingerprint {
            serde_json::from_value(existing.result).map_err(|_| stored_result_error())
        } else {
            Err(idempotency_conflict(envelope))
        };
        transaction.rollback().await.map_err(OrgError::storage)?;
        return result;
    }

    let now = context.clock().now();
    let outcome = execute_release_transaction(transaction.as_ref(), envelope, request, now).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return finish_release_error(transaction, error).await,
    };
    match outcome {
        ReleaseTransactionOutcome::ExpiredBookkept => {
            transaction.commit().await.map_err(OrgError::storage)?;
            Err(stale_lease())
        }
        ReleaseTransactionOutcome::Released(result) => {
            let stored_result = serde_json::to_value(&result)
                .map_err(|_| OrgError::invalid_input("Org release result cannot be serialized"));
            let stored_result = match stored_result {
                Ok(stored_result) => stored_result,
                Err(error) => return finish_release_error(transaction, error).await,
            };
            if let Err(error) = transaction
                .insert_org_operation(&StoredOrgOperation {
                    workspace_id: envelope.workspace_id,
                    operation_id: envelope.operation_id.clone(),
                    request_fingerprint: fingerprint,
                    result: stored_result,
                    created_at: now,
                })
                .await
            {
                return finish_release_error(transaction, OrgError::storage(error)).await;
            }
            transaction.commit().await.map_err(OrgError::storage)?;
            Ok(result)
        }
    }
}

enum ReleaseTransactionOutcome {
    Released(OrgCommandResult),
    ExpiredBookkept,
}

async fn execute_release_transaction(
    transaction: &dyn StorageTransaction,
    command: &CommandEnvelope,
    request: &ReleaseClaimRequest,
    now: i64,
) -> Result<ReleaseTransactionOutcome, OrgError> {
    let workspace = require_active_workspace(transaction, command.workspace_id).await?;
    let document = load_document(transaction, &workspace, request.document_id).await?;
    let item = transaction
        .get_org_work_item(request.work_item_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|item| item.workspace_id == workspace.id && item.document_id == document.id)
        .ok_or_else(|| not_found("work item"))?;
    let lease = transaction
        .get_open_org_lease_internal(item.id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(stale_lease)?;
    if lease.expires_at <= now {
        bookkeep_expired_lease(transaction, &workspace, &item, &lease, now).await?;
        return Ok(ReleaseTransactionOutcome::ExpiredBookkept);
    }

    let token_hash = token_hash(&request.fencing_token);
    resolve_lease_update(
        transaction
            .close_org_lease(OrgLeaseClosure {
                proof: OrgLeaseProof {
                    lease_id: &request.lease_id,
                    workspace_id: workspace.id,
                    work_item_id: item.id,
                    fencing_token_hash: &token_hash,
                    kind: storage_kind(request.kind),
                    actor_id: &command.actor_id,
                    now,
                },
                ended_at: now,
                end_reason: OrgLeaseEndReason::Release,
            })
            .await
            .map_err(OrgError::storage)?,
    )?;
    validate_document_revision(&document, request.expected_document_revision)?;
    let target = release_target(&workspace, &item, request)?;
    let projection = transaction
        .list_org_workspace_projection(workspace.id)
        .await
        .map_err(OrgError::storage)?;
    if let Some(target) = &target {
        validate_item_transition(&workspace.policy, &item, &projection, target, false)?;
    }
    let resulting_document = if let Some(target) = &target {
        write_claim_state(
            transaction,
            &workspace,
            &document,
            &projection,
            item.id,
            &[target.as_str()],
            now,
        )
        .await?
    } else {
        document.clone()
    };
    update_attempt_for_release(
        transaction,
        &lease.attempt_id,
        request.kind,
        target.as_deref(),
        &workspace,
        now,
    )
    .await?;
    let state_changed = target.is_some();
    let event_id = append_event(
        transaction,
        &uuid::Uuid::new_v4().to_string(),
        &workspace,
        item.id,
        &command.actor_id,
        Some(&lease.attempt_id),
        OrgEventType::Release,
        now,
        "Released Org work item claim",
        &json!({
            "schema_version": 1,
            "lease_kind": request.kind,
            "target_state": target.as_deref().unwrap_or_else(|| item.state.as_deref().unwrap_or("")),
        }),
        state_changed.then_some(item.state.as_deref()).flatten(),
        target.as_deref(),
    )
    .await?;
    let hydrated = get_item_context_in_transaction(transaction, workspace.id, item.id, now).await?;
    let mut result = command_result(
        command,
        &workspace,
        vec![event_id],
        &hydrated,
        json!({"context": hydrated}),
    );
    result
        .document_revisions
        .insert(document.id.to_string(), resulting_document.revision);
    Ok(ReleaseTransactionOutcome::Released(result))
}

pub(crate) async fn bookkeep_expired_open_lease(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    item: &OrgProjectedWorkItem,
    now: i64,
) -> Result<bool, OrgError> {
    let Some(lease) = transaction
        .get_open_org_lease_internal(item.id)
        .await
        .map_err(OrgError::storage)?
    else {
        return Ok(false);
    };
    if lease.expires_at > now {
        return Ok(false);
    }
    bookkeep_expired_lease(transaction, workspace, item, &lease, now).await?;
    Ok(true)
}

pub(crate) async fn bookkeep_expired_lease(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    item: &OrgProjectedWorkItem,
    lease: &OrgLease,
    now: i64,
) -> Result<String, OrgError> {
    bookkeep_expired_lease_with_phases(transaction, workspace, item, lease, now, |_| Ok(())).await
}

pub(crate) async fn bookkeep_expired_lease_with_phases<F>(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    item: &OrgProjectedWorkItem,
    lease: &OrgLease,
    now: i64,
    mut after_phase: F,
) -> Result<String, OrgError>
where
    F: FnMut(OrgWorkflowPhase) -> Result<(), OrgError>,
{
    let expiry_event_id = uuid::Uuid::new_v4().to_string();
    let closed = transaction
        .close_expired_org_lease(ExpiredOrgLeaseClosure {
            lease_id: &lease.id,
            work_item_id: item.id,
            now,
            ended_at: now,
            expiry_event_id: &expiry_event_id,
        })
        .await
        .map_err(OrgError::storage)?;
    if !matches!(closed, ConditionalUpdate::Applied(_)) {
        return Err(stale_lease());
    }
    after_phase(OrgWorkflowPhase::LeaseUpdate)?;
    if lease.kind == OrgLeaseKind::Execution {
        let attempt = transaction
            .get_org_attempt(&lease.attempt_id)
            .await
            .map_err(OrgError::storage)?
            .ok_or_else(|| inconsistent_attempt_for_id(&lease.attempt_id))?;
        resolve_update(
            transaction
                .update_org_attempt(OrgAttemptUpdate {
                    id: &attempt.id,
                    expected_status: OrgAttemptStatus::Running,
                    status: OrgAttemptStatus::Expired,
                    ended_at: now,
                    error: Some("Lease expired"),
                    result_summary: None,
                    review_outcome: None,
                    note_refs: &attempt.note_refs,
                    artifacts: &attempt.artifacts,
                    metadata: &attempt.metadata,
                })
                .await
                .map_err(OrgError::storage)?,
            "attempt",
        )?;
        after_phase(OrgWorkflowPhase::AttemptUpdate)?;
    }
    let event_id = append_event(
        transaction,
        &expiry_event_id,
        workspace,
        item.id,
        "system",
        Some(&lease.attempt_id),
        OrgEventType::LeaseExpiry,
        now,
        "Org lease expired",
        &json!({"schema_version": 1, "lease_kind": dto_kind(lease.kind)}),
        item.state.as_deref(),
        item.state.as_deref(),
    )
    .await?;
    after_phase(OrgWorkflowPhase::Events)?;
    Ok(event_id)
}

async fn finish_release_error(
    transaction: Box<dyn StorageTransaction>,
    error: OrgError,
) -> Result<OrgCommandResult, OrgError> {
    transaction.rollback().await.map_err(OrgError::storage)?;
    Err(error)
}

async fn update_attempt_for_release(
    transaction: &dyn StorageTransaction,
    attempt_id: &str,
    kind: OrgClaimKind,
    target: Option<&str>,
    workspace: &OrgWorkspace,
    now: i64,
) -> Result<(), OrgError> {
    let Some(attempt) = transaction
        .get_org_attempt(attempt_id)
        .await
        .map_err(OrgError::storage)?
    else {
        return Err(inconsistent_attempt_for_id(attempt_id));
    };
    let next = match kind {
        OrgClaimKind::Execution => OrgAttemptStatus::Cancelled,
        OrgClaimKind::Review
            if target.is_none() || target == Some(workspace.policy.review_state.as_str()) =>
        {
            return Ok(())
        }
        OrgClaimKind::Review if target == Some(workspace.policy.cancelled_state.as_str()) => {
            OrgAttemptStatus::Cancelled
        }
        OrgClaimKind::Review => OrgAttemptStatus::Failed,
    };
    resolve_update(
        transaction
            .update_org_attempt(OrgAttemptUpdate {
                id: &attempt.id,
                expected_status: attempt.status,
                status: next,
                ended_at: now,
                error: attempt.error.as_deref(),
                result_summary: attempt.result_summary.as_deref(),
                review_outcome: if kind == OrgClaimKind::Review {
                    Some("released")
                } else {
                    None
                },
                note_refs: &attempt.note_refs,
                artifacts: &attempt.artifacts,
                metadata: &attempt.metadata,
            })
            .await
            .map_err(OrgError::storage)?,
        "attempt",
    )?;
    Ok(())
}

fn release_target(
    workspace: &OrgWorkspace,
    item: &OrgProjectedWorkItem,
    request: &ReleaseClaimRequest,
) -> Result<Option<String>, OrgError> {
    let target = match request.kind {
        OrgClaimKind::Execution => Ok(Some(
            request
                .target_state
                .clone()
                .unwrap_or_else(|| workspace.policy.release_state.clone()),
        )),
        OrgClaimKind::Review => match &request.target_state {
            None => Ok(None),
            Some(target) if item.state.as_deref() == Some(target.as_str()) => Ok(None),
            Some(target) => Ok(Some(target.clone())),
        },
    }?;
    if target.as_ref().is_some_and(|target| {
        target == &workspace.policy.running_state
            || workspace.policy.successful_terminal_states.contains(target)
            || (request.kind == OrgClaimKind::Execution && target == &workspace.policy.review_state)
    }) {
        return Err(OrgError::new(
            OrgErrorCode::InvalidTransition,
            "Org release cannot enter running, review, or successful state",
            json!({"work_item_id": item.id}),
            false,
        ));
    }
    Ok(target)
}

pub(crate) async fn write_claim_state(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    document: &OrgDocument,
    old_projection: &[OrgProjectedWorkItem],
    item_id: note_org::WorkItemId,
    targets: &[&str],
    now: i64,
) -> Result<OrgDocument, OrgError> {
    let mut source = document.source.clone();
    for target in targets {
        source = note_org::parse_document(source, &workspace.policy.parse_options())
            .map_err(parse_error)?
            .apply(SemanticEdit::SetState {
                item_id,
                state: (*target).to_string(),
            })
            .map_err(edit_error)?
            .source;
    }
    let parsed = note_org::parse_document(source.clone(), &workspace.policy.parse_options())
        .map_err(parse_error)?;
    let created_at = old_projection
        .iter()
        .map(|item| (item.id, item.created_at))
        .collect::<BTreeMap<_, _>>();
    let projected_document = project_document(
        &parsed,
        workspace.id,
        document.id,
        &workspace.timezone,
        &created_at,
        now,
    )?;
    let mut candidate = old_projection
        .iter()
        .filter(|item| item.document_id != document.id)
        .cloned()
        .collect::<Vec<_>>();
    candidate.extend(projected_document.iter().cloned());
    validate_projected_items_for_policy(&candidate, &workspace.policy)?;
    let hash = format!("sha256:{:x}", Sha256::digest(source.as_bytes()));
    let updated = resolve_cas(
        transaction
            .compare_and_swap_org_document(OrgDocumentUpdate {
                id: document.id,
                expected_revision: document.revision,
                path: &document.path,
                source: &source,
                content_hash: &hash,
                updated_at: now,
            })
            .await
            .map_err(OrgError::storage)?,
        "document",
    )?;
    transaction
        .replace_org_document_projection(document.id, &projected_document)
        .await
        .map_err(OrgError::storage)?;
    Ok(updated)
}

pub(crate) async fn append_event(
    transaction: &dyn StorageTransaction,
    event_id: &str,
    workspace: &OrgWorkspace,
    item_id: note_org::WorkItemId,
    actor_id: &str,
    attempt_id: Option<&str>,
    event_type: OrgEventType,
    now: i64,
    summary: &str,
    metadata: &serde_json::Value,
    previous_state: Option<&str>,
    resulting_state: Option<&str>,
) -> Result<String, OrgError> {
    let event = NewOrgEvent {
        id: event_id,
        workspace_id: workspace.id,
        subject_kind: "work_item",
        subject_id: &item_id.to_string(),
        actor_id,
        attempt_id,
        event_type,
        occurred_at: now,
        summary,
        metadata,
        previous_state,
        resulting_state,
    };
    let result = if actor_id == "system" {
        transaction.append_internal_org_event(event).await
    } else {
        transaction.append_org_event(event).await
    };
    result.map(|event| event.id).map_err(OrgError::storage)
}

pub(crate) fn command_result(
    command: &CommandEnvelope,
    workspace: &OrgWorkspace,
    event_ids: Vec<String>,
    hydrated: &super::OrgItemContext,
    data: serde_json::Value,
) -> OrgCommandResult {
    OrgCommandResult {
        schema_version: ORG_COMMAND_SCHEMA_VERSION,
        workspace_id: workspace.id,
        operation_id: command.operation_id.clone(),
        event_ids,
        workspace_revision: Some(workspace.revision),
        document_revisions: BTreeMap::from([(
            hydrated.document.id.to_string(),
            hydrated.document.revision,
        )]),
        data,
    }
}

pub(crate) async fn require_active_workspace(
    transaction: &dyn StorageTransaction,
    workspace_id: note_org::WorkspaceId,
) -> Result<OrgWorkspace, OrgError> {
    let workspace = transaction
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| not_found("workspace"))?;
    if workspace.archived_at.is_some() {
        return Err(OrgError::new(
            OrgErrorCode::ArchivedWorkspace,
            "Archived Org workspaces are read-only",
            json!({"workspace_id": workspace_id}),
            false,
        ));
    }
    Ok(workspace)
}

pub(crate) async fn load_document(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    document_id: note_org::DocumentId,
) -> Result<OrgDocument, OrgError> {
    transaction
        .get_org_document(document_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|document| document.workspace_id == workspace.id)
        .ok_or_else(|| not_found("document"))
}

pub(crate) fn validate_document_revision(
    document: &OrgDocument,
    expected_revision: i64,
) -> Result<(), OrgError> {
    if document.revision != expected_revision {
        return Err(OrgError::new(
            OrgErrorCode::StaleRevision,
            "Org document revision is stale",
            json!({"document_id": document.id, "current_revision": document.revision}),
            true,
        ));
    }
    Ok(())
}

fn command_fingerprint<T: serde::Serialize>(
    kind: OrgCommandKind,
    envelope: &CommandEnvelope,
    request: &T,
) -> Result<String, OrgError> {
    request_fingerprint(&json!({
        "schema_version": envelope.schema_version,
        "command_kind": {"name": kind.name(), "schema_version": kind.schema_version()},
        "workspace_id": envelope.workspace_id,
        "actor_id": envelope.actor_id,
        "request": request,
    }))
}

fn validate_envelope(envelope: &CommandEnvelope) -> Result<(), OrgError> {
    if envelope.schema_version != ORG_COMMAND_SCHEMA_VERSION {
        return Err(OrgError::invalid_input(
            "Unsupported Org command schema version",
        ));
    }
    if envelope.actor_id.trim().is_empty()
        || envelope.actor_id != envelope.actor_id.trim()
        || envelope.actor_id == "system"
        || envelope.operation_id.trim().is_empty()
        || envelope.operation_id != envelope.operation_id.trim()
    {
        return Err(OrgError::invalid_input("Invalid Org command envelope"));
    }
    Ok(())
}

fn validate_token_request(lease_id: &str, token: &str) -> Result<(), OrgError> {
    if lease_id.trim().is_empty()
        || lease_id != lease_id.trim()
        || token.trim().is_empty()
        || token != token.trim()
    {
        return Err(stale_lease());
    }
    Ok(())
}

pub(crate) fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn lease_proof<'a>(
    command: &'a CommandEnvelope,
    request: &'a HeartbeatClaimRequest,
    token_hash: &'a str,
    now: i64,
) -> OrgLeaseProof<'a> {
    OrgLeaseProof {
        lease_id: &request.lease_id,
        workspace_id: command.workspace_id,
        work_item_id: request.work_item_id,
        fencing_token_hash: token_hash,
        kind: storage_kind(request.kind),
        actor_id: &command.actor_id,
        now,
    }
}

fn domain_kind(kind: OrgClaimKind) -> LeaseKind {
    match kind {
        OrgClaimKind::Execution => LeaseKind::Execution,
        OrgClaimKind::Review => LeaseKind::Review,
    }
}

pub(crate) fn storage_kind(kind: OrgClaimKind) -> OrgLeaseKind {
    match kind {
        OrgClaimKind::Execution => OrgLeaseKind::Execution,
        OrgClaimKind::Review => OrgLeaseKind::Review,
    }
}

fn dto_kind(kind: OrgLeaseKind) -> OrgClaimKind {
    match kind {
        OrgLeaseKind::Execution => OrgClaimKind::Execution,
        OrgLeaseKind::Review => OrgClaimKind::Review,
    }
}

fn domain_lease_kind(kind: OrgLeaseKind) -> LeaseKind {
    match kind {
        OrgLeaseKind::Execution => LeaseKind::Execution,
        OrgLeaseKind::Review => LeaseKind::Review,
    }
}

pub(crate) fn lease_duration(workspace: &OrgWorkspace) -> i64 {
    i64::try_from(workspace.policy.lease_duration_secs).unwrap_or(i64::MAX)
}

pub(crate) fn capacity(workspace: &OrgWorkspace) -> Result<i64, OrgError> {
    i64::try_from(workspace.policy.concurrency_limit)
        .map_err(|_| OrgError::invalid_input("Org concurrency limit is too large"))
}

pub(crate) fn map_claim_error(error: ClaimDecisionError) -> OrgError {
    match error {
        ClaimDecisionError::Blocked(blockers) => {
            let code = if blockers.contains(&ReadinessBlocker::ActiveLease) {
                OrgErrorCode::ActiveLease
            } else if blockers.contains(&ReadinessBlocker::ConcurrencyLimit) {
                OrgErrorCode::ConcurrencyLimit
            } else if blockers.contains(&ReadinessBlocker::RetryLimit) {
                OrgErrorCode::RetryLimit
            } else if blockers.contains(&ReadinessBlocker::DependenciesIncomplete) {
                OrgErrorCode::DependencyBlocked
            } else if blockers.contains(&ReadinessBlocker::WorkspaceArchived) {
                OrgErrorCode::ArchivedWorkspace
            } else {
                OrgErrorCode::InvalidTransition
            };
            OrgError::new(
                code,
                "Org work item is not ready for the requested claim",
                json!({"blockers": blockers.iter().map(|value| format!("{value:?}")).collect::<Vec<_>>()}),
                matches!(
                    code,
                    OrgErrorCode::ActiveLease | OrgErrorCode::ConcurrencyLimit
                ),
            )
        }
        ClaimDecisionError::Transition(error) => OrgError::new(
            OrgErrorCode::InvalidTransition,
            "Org claim transition is not allowed",
            json!({"reason": error.to_string()}),
            false,
        ),
    }
}

pub(crate) fn resolve_lease_update<T>(value: ConditionalUpdate<T>) -> Result<T, OrgError> {
    match value {
        ConditionalUpdate::Applied(value) => Ok(value),
        ConditionalUpdate::NotFound | ConditionalUpdate::Conflict => Err(stale_lease()),
    }
}

pub(crate) fn resolve_update<T>(
    value: ConditionalUpdate<T>,
    resource: &str,
) -> Result<T, OrgError> {
    match value {
        ConditionalUpdate::Applied(value) => Ok(value),
        ConditionalUpdate::NotFound => Err(not_found(resource)),
        ConditionalUpdate::Conflict => Err(OrgError::new(
            OrgErrorCode::InvalidTransition,
            "Org attempt state changed concurrently",
            json!({"resource": resource}),
            true,
        )),
    }
}

fn not_found(resource: &str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource}),
        false,
    )
}

pub(crate) fn stale_lease() -> OrgError {
    OrgError::new(
        OrgErrorCode::StaleLease,
        "Org lease proof is stale or invalid",
        json!({}),
        true,
    )
}

fn active_lease(item_id: note_org::WorkItemId) -> OrgError {
    OrgError::new(
        OrgErrorCode::ActiveLease,
        "Org work item already has an active lease",
        json!({"work_item_id": item_id}),
        true,
    )
}

pub(crate) fn concurrency_limit(workspace_id: note_org::WorkspaceId) -> OrgError {
    OrgError::new(
        OrgErrorCode::ConcurrencyLimit,
        "Org workspace claim capacity is exhausted",
        json!({"workspace_id": workspace_id}),
        true,
    )
}

fn inconsistent_attempt(item_id: note_org::WorkItemId) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidTransition,
        "Org work-item attempt history is inconsistent",
        json!({"work_item_id": item_id}),
        false,
    )
}

fn inconsistent_attempt_for_id(attempt_id: &str) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidTransition,
        "Org work-item attempt history is inconsistent",
        json!({"attempt_id": attempt_id}),
        false,
    )
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
    OrgError::new(
        OrgErrorCode::UnsupportedSemanticEdit,
        "Org semantic edit cannot preserve canonical source safely",
        json!({"reason": error.to_string()}),
        false,
    )
}

fn stored_result_error() -> OrgError {
    OrgError::new(
        OrgErrorCode::StorageFailure,
        "Stored Org operation result is invalid",
        json!({}),
        false,
    )
}

fn idempotency_conflict(envelope: &CommandEnvelope) -> OrgError {
    OrgError::new(
        OrgErrorCode::IdempotencyConflict,
        "Operation ID was already used for a different Org command",
        json!({"operation_id": envelope.operation_id}),
        false,
    )
}

async fn finish_claim_read(
    transaction: Box<dyn StorageTransaction>,
    result: Result<OrgClaimResult, OrgError>,
) -> Result<OrgClaimResult, OrgError> {
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

async fn finish_claim_write(
    transaction: Box<dyn StorageTransaction>,
    result: Result<OrgClaimResult, OrgError>,
) -> Result<OrgClaimResult, OrgError> {
    match result {
        Ok(result) => {
            transaction.commit().await.map_err(OrgError::storage)?;
            Ok(result)
        }
        Err(error) => {
            transaction.rollback().await.map_err(OrgError::storage)?;
            Err(error)
        }
    }
}
