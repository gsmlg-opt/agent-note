use super::{
    append_event, bookkeep_expired_open_lease, capacity, command_result, concurrency_limit,
    execute_idempotent, execute_idempotent_outcome, get_item_context_in_transaction,
    lease_duration, load_document, map_claim_error, projected_to_domain, require_active_workspace,
    resolve_lease_update, resolve_update, storage_kind, token_hash, validate_document_revision,
    validate_item_transition, validate_public_payload, write_claim_state, CommandEnvelope,
    OrgClaimKind, OrgClaimResult, OrgCommandKind, OrgCommandResult, OrgContext, OrgError,
    OrgErrorCode, OrgMutationOutcome, OrgWorkflowPhase, ReportProgressRequest, RetryItemRequest,
    SubmitResultRequest, TransitionItemRequest, ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{AttemptPhase, ClaimKind, ClaimQueue, ClaimRequest, LeaseStatus, ReadinessContext};
use note_storage::{
    ConditionalUpdate, NewOrgAttemptAllocation, NewOrgEvent, NewOrgLease, OrgAttemptStatus,
    OrgAttemptUpdate, OrgEventType, OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseKind,
    OrgLeaseProof,
};
use serde_json::json;
use std::collections::BTreeMap;

const REPORT_PROGRESS: OrgCommandKind = OrgCommandKind::new("report_progress", 1);
const SUBMIT_RESULT: OrgCommandKind = OrgCommandKind::new("submit_result", 1);
const TRANSITION_ITEM: OrgCommandKind = OrgCommandKind::new("transition_item", 1);
const RETRY_ITEM: OrgCommandKind = OrgCommandKind::new("retry_item", 1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionLifecycle {
    Approval,
    Rejection,
    Cancellation,
    ReviewRequest,
    Failure,
    Block,
    Unblock,
    Completion,
    Transition,
}

pub async fn report_progress(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ReportProgressRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_request_schema(request.schema_version)?;
    validate_lease_input(&request.lease_id, &request.fencing_token)?;
    validate_public_payload(
        &request.fencing_token,
        &json!({
            "summary": request.summary,
            "metadata": request.metadata,
        }),
    )?;
    let fingerprint = json!({
        "schema_version": request.schema_version,
        "work_item_id": request.work_item_id,
        "lease_id": request.lease_id,
        "kind": request.kind,
        "fencing_token_digest": token_hash(&request.fencing_token),
        "summary": request.summary,
        "metadata": request.metadata,
    });
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        REPORT_PROGRESS,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let item = transaction
                    .get_org_work_item(request.work_item_id)
                    .await
                    .map_err(OrgError::storage)?
                    .filter(|item| item.workspace_id == command.workspace_id)
                    .ok_or_else(|| not_found("work item"))?;
                if request.kind != OrgClaimKind::Execution {
                    return Err(stale_lease());
                }
                let token_hash = token_hash(&request.fencing_token);
                let lease = resolve_lease(
                    transaction
                        .validate_org_lease_proof(OrgLeaseProof {
                            lease_id: &request.lease_id,
                            workspace_id: command.workspace_id,
                            work_item_id: item.id,
                            fencing_token_hash: &token_hash,
                            kind: note_storage::OrgLeaseKind::Execution,
                            actor_id: &command.actor_id,
                            now,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Proof)?;
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: &uuid::Uuid::new_v4().to_string(),
                        workspace_id: command.workspace_id,
                        subject_kind: "work_item",
                        subject_id: &item.id.to_string(),
                        actor_id: &command.actor_id,
                        attempt_id: Some(&lease.attempt_id),
                        event_type: OrgEventType::Progress,
                        occurred_at: now,
                        summary: &request.summary,
                        metadata: &request.metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Events)?;
                let hydrated = get_item_context_in_transaction(
                    transaction,
                    command.workspace_id,
                    item.id,
                    now,
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ContextHydration)?;
                Ok(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: command.workspace_id,
                    operation_id: command.operation_id.clone(),
                    event_ids: vec![event.id],
                    workspace_revision: Some(hydrated.workspace_revision),
                    document_revisions: BTreeMap::from([(
                        hydrated.document.id.to_string(),
                        hydrated.document.revision,
                    )]),
                    data: json!({"context": hydrated}),
                })
            })
        },
    )
    .await
}

pub(crate) fn resolve_lease<T>(value: ConditionalUpdate<T>) -> Result<T, OrgError> {
    match value {
        ConditionalUpdate::Applied(value) => Ok(value),
        ConditionalUpdate::NotFound | ConditionalUpdate::Conflict => Err(stale_lease()),
    }
}

pub async fn submit_result(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &SubmitResultRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_request_schema(request.schema_version)?;
    validate_lease_input(&request.lease_id, &request.fencing_token)?;
    validate_public_payload(
        &request.fencing_token,
        &json!({
            "result_summary": request.result_summary,
            "note_refs": request.note_refs,
            "artifacts": request.artifacts,
            "metadata": request.metadata,
        }),
    )?;
    let fingerprint = json!({
        "schema_version": request.schema_version,
        "work_item_id": request.work_item_id,
        "document_id": request.document_id,
        "expected_document_revision": request.expected_document_revision,
        "lease_id": request.lease_id,
        "fencing_token_digest": token_hash(&request.fencing_token),
        "result_summary": request.result_summary,
        "note_refs": request.note_refs,
        "artifacts": request.artifacts,
        "metadata": request.metadata,
    });
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent_outcome(
        context,
        SUBMIT_RESULT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_active_workspace(transaction, command.workspace_id).await?;
                let document = load_document(transaction, &workspace, request.document_id).await?;
                let item = transaction
                    .get_org_work_item(request.work_item_id)
                    .await
                    .map_err(OrgError::storage)?
                    .filter(|item| {
                        item.workspace_id == workspace.id && item.document_id == document.id
                    })
                    .ok_or_else(|| not_found("work item"))?;
                if bookkeep_expired_open_lease(transaction, &workspace, &item, now).await? {
                    return Ok(OrgMutationOutcome::CommitError(stale_lease()));
                }
                let digest = token_hash(&request.fencing_token);
                let needs_review = item.requires_review
                    || workspace
                        .policy
                        .review_required_types
                        .contains(&item.item_type);
                let lease = resolve_lease_update(
                    transaction
                        .close_org_lease(OrgLeaseClosure {
                            proof: OrgLeaseProof {
                                lease_id: &request.lease_id,
                                workspace_id: workspace.id,
                                work_item_id: item.id,
                                fencing_token_hash: &digest,
                                kind: OrgLeaseKind::Execution,
                                actor_id: &command.actor_id,
                                now,
                            },
                            ended_at: now,
                            end_reason: if needs_review {
                                OrgLeaseEndReason::ReviewRequest
                            } else {
                                OrgLeaseEndReason::Completion
                            },
                        })
                        .await
                        .map_err(OrgError::storage)?,
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Proof)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::LeaseUpdate)?;
                validate_document_revision(&document, request.expected_document_revision)?;
                let projection = transaction
                    .list_org_workspace_projection(workspace.id)
                    .await
                    .map_err(OrgError::storage)?;
                let (target, next_status, lifecycle_event, lifecycle_summary) = if needs_review {
                    let target = workspace.policy.review_state.clone();
                    validate_item_transition(
                        &workspace.policy,
                        &item,
                        &projection,
                        &target,
                        false,
                    )?;
                    (
                        target,
                        OrgAttemptStatus::Submitted,
                        OrgEventType::ReviewRequest,
                        "Requested review for Org work item",
                    )
                } else {
                    (
                        unique_successful_target(&workspace.policy, &item, &projection, false)?,
                        OrgAttemptStatus::Completed,
                        OrgEventType::Completion,
                        "Completed Org work item",
                    )
                };
                let updated_document = write_claim_state(
                    transaction,
                    &workspace,
                    &document,
                    &projection,
                    item.id,
                    &[target.as_str()],
                    now,
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                let attempt = transaction
                    .get_org_attempt(&lease.attempt_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| not_found("attempt"))?;
                resolve_update(
                    transaction
                        .update_org_attempt(OrgAttemptUpdate {
                            id: &attempt.id,
                            expected_status: OrgAttemptStatus::Running,
                            status: next_status,
                            ended_at: now,
                            error: None,
                            result_summary: Some(&request.result_summary),
                            review_outcome: None,
                            note_refs: &request.note_refs,
                            artifacts: &request.artifacts,
                            metadata: &request.metadata,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "attempt",
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
                let result_event = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    item.id,
                    &command.actor_id,
                    Some(&attempt.id),
                    OrgEventType::ResultSubmission,
                    now,
                    "Submitted Org work result",
                    &json!({"schema_version": 1}),
                    item.state.as_deref(),
                    item.state.as_deref(),
                )
                .await?;
                let lifecycle_event_id = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    item.id,
                    &command.actor_id,
                    Some(&attempt.id),
                    lifecycle_event,
                    now,
                    lifecycle_summary,
                    &json!({"schema_version": 1}),
                    item.state.as_deref(),
                    Some(&target),
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Events)?;
                let hydrated =
                    get_item_context_in_transaction(transaction, workspace.id, item.id, now)
                        .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ContextHydration)?;
                let mut result = command_result(
                    &command,
                    &workspace,
                    vec![result_event, lifecycle_event_id],
                    &hydrated,
                    json!({"context": hydrated}),
                );
                result
                    .document_revisions
                    .insert(document.id.to_string(), updated_document.revision);
                Ok(OrgMutationOutcome::Success(result))
            })
        },
    )
    .await
}

pub(crate) fn unique_successful_target(
    policy: &note_org::WorkspacePolicy,
    item: &note_storage::OrgProjectedWorkItem,
    projection: &[note_storage::OrgProjectedWorkItem],
    review_approved: bool,
) -> Result<String, OrgError> {
    let valid = policy
        .successful_terminal_states
        .iter()
        .filter(|target| {
            validate_item_transition(policy, item, projection, target, review_approved).is_ok()
        })
        .cloned()
        .collect::<Vec<_>>();
    match valid.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err(OrgError::new(
            OrgErrorCode::InvalidTransition,
            "No successful Org terminal state is reachable",
            json!({"work_item_id": item.id}),
            false,
        )),
        _ => Err(OrgError::new(
            OrgErrorCode::InvalidTransition,
            "Successful Org terminal state is ambiguous",
            json!({"work_item_id": item.id, "targets": valid}),
            false,
        )),
    }
}

pub async fn transition_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &TransitionItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_request_schema(request.schema_version)?;
    if request.expected_document_revision <= 0 || request.target_state.trim().is_empty() {
        return Err(OrgError::invalid_input("Invalid Org transition request"));
    }
    if let Some(proof) = &request.lease {
        validate_lease_input(&proof.lease_id, &proof.fencing_token)?;
        validate_public_payload(
            &proof.fencing_token,
            &json!({
                "target_state": request.target_state,
                "error": request.error,
                "metadata": request.metadata,
            }),
        )?;
    }
    let lease_fingerprint = request.lease.as_ref().map(|proof| {
        json!({
            "lease_id": proof.lease_id,
            "kind": proof.kind,
            "fencing_token_digest": token_hash(&proof.fencing_token),
        })
    });
    let fingerprint = json!({
        "schema_version": request.schema_version,
        "work_item_id": request.work_item_id,
        "document_id": request.document_id,
        "expected_document_revision": request.expected_document_revision,
        "target_state": request.target_state,
        "lease": lease_fingerprint,
        "error": request.error,
        "metadata": request.metadata,
    });
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent_outcome(
        context,
        TRANSITION_ITEM,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_active_workspace(transaction, command.workspace_id).await?;
                let document = load_document(transaction, &workspace, request.document_id).await?;
                let item = transaction
                    .get_org_work_item(request.work_item_id)
                    .await
                    .map_err(OrgError::storage)?
                    .filter(|item| {
                        item.workspace_id == workspace.id && item.document_id == document.id
                    })
                    .ok_or_else(|| not_found("work item"))?;
                if bookkeep_expired_open_lease(transaction, &workspace, &item, now).await? {
                    return Ok(OrgMutationOutcome::CommitError(stale_lease()));
                }
                let from = item.state.as_deref().ok_or_else(|| {
                    OrgError::invalid_input("Org work item has no workflow state")
                })?;
                let lifecycle = classify_transition(&workspace.policy, from, &request.target_state);
                let open_lease = transaction
                    .get_open_org_lease_internal(item.id)
                    .await
                    .map_err(OrgError::storage)?;
                let lease_required = open_lease.is_some()
                    || from == workspace.policy.running_state
                    || from == workspace.policy.review_state;
                let proof = request.lease.as_ref();
                if lease_required && proof.is_none() {
                    return Err(stale_lease());
                }
                let close_active = proof.is_some() && closes_lease(lifecycle);

                let lease = if let Some(proof) = proof {
                    let digest = token_hash(&proof.fencing_token);
                    if close_active {
                        Some(resolve_lease_update(
                            transaction
                                .close_org_lease(OrgLeaseClosure {
                                    proof: OrgLeaseProof {
                                        lease_id: &proof.lease_id,
                                        workspace_id: workspace.id,
                                        work_item_id: item.id,
                                        fencing_token_hash: &digest,
                                        kind: storage_kind(proof.kind),
                                        actor_id: &command.actor_id,
                                        now,
                                    },
                                    ended_at: now,
                                    end_reason: lifecycle_end_reason(lifecycle),
                                })
                                .await
                                .map_err(OrgError::storage)?,
                        )?)
                    } else {
                        Some(resolve_lease(
                            transaction
                                .validate_org_lease_proof(OrgLeaseProof {
                                    lease_id: &proof.lease_id,
                                    workspace_id: workspace.id,
                                    work_item_id: item.id,
                                    fencing_token_hash: &digest,
                                    kind: storage_kind(proof.kind),
                                    actor_id: &command.actor_id,
                                    now,
                                })
                                .await
                                .map_err(OrgError::storage)?,
                        )?)
                    }
                } else {
                    None
                };
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Proof)?;
                if close_active {
                    workflow_context.after_workflow_phase(OrgWorkflowPhase::LeaseUpdate)?;
                }
                validate_document_revision(&document, request.expected_document_revision)?;

                if from == workspace.policy.review_state
                    && !matches!(
                        lifecycle,
                        TransitionLifecycle::Approval
                            | TransitionLifecycle::Rejection
                            | TransitionLifecycle::Cancellation
                    )
                {
                    return Err(OrgError::new(
                        OrgErrorCode::InvalidTransition,
                        "Submitted review work only accepts approval, rejection, or cancellation",
                        json!({"work_item_id": item.id}),
                        false,
                    ));
                }
                let projection = transaction
                    .list_org_workspace_projection(workspace.id)
                    .await
                    .map_err(OrgError::storage)?;
                validate_item_transition(
                    &workspace.policy,
                    &item,
                    &projection,
                    &request.target_state,
                    lifecycle == TransitionLifecycle::Approval,
                )?;
                let updated_document = write_claim_state(
                    transaction,
                    &workspace,
                    &document,
                    &projection,
                    item.id,
                    &[request.target_state.as_str()],
                    now,
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;

                let attempt = if close_active {
                    let lease = lease.as_ref().ok_or_else(stale_lease)?;
                    let attempt = transaction
                        .get_org_attempt(&lease.attempt_id)
                        .await
                        .map_err(OrgError::storage)?
                        .ok_or_else(|| not_found("attempt"))?;
                    let (expected, status, review_outcome) =
                        lifecycle_attempt(lifecycle, from == workspace.policy.review_state);
                    resolve_update(
                        transaction
                            .update_org_attempt(OrgAttemptUpdate {
                                id: &attempt.id,
                                expected_status: expected,
                                status,
                                ended_at: now,
                                error: request.error.as_deref().or(attempt.error.as_deref()),
                                result_summary: attempt.result_summary.as_deref(),
                                review_outcome,
                                note_refs: &attempt.note_refs,
                                artifacts: &attempt.artifacts,
                                metadata: &attempt.metadata,
                            })
                            .await
                            .map_err(OrgError::storage)?,
                        "attempt",
                    )?;
                    workflow_context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
                    Some(attempt)
                } else {
                    None
                };
                let attempt_id = attempt
                    .as_ref()
                    .map(|attempt| attempt.id.as_str())
                    .or_else(|| lease.as_ref().map(|lease| lease.attempt_id.as_str()));
                let mut event_ids = Vec::new();
                for event_type in lifecycle_events(lifecycle) {
                    event_ids.push(
                        append_event(
                            transaction,
                            &uuid::Uuid::new_v4().to_string(),
                            &workspace,
                            item.id,
                            &command.actor_id,
                            attempt_id,
                            event_type.clone(),
                            now,
                            lifecycle_summary(event_type),
                            &json!({
                                "schema_version": 1,
                                "error": request.error,
                                "context": request.metadata,
                            }),
                            item.state.as_deref(),
                            Some(&request.target_state),
                        )
                        .await?,
                    );
                }
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Events)?;
                let hydrated =
                    get_item_context_in_transaction(transaction, workspace.id, item.id, now)
                        .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ContextHydration)?;
                let mut result = command_result(
                    &command,
                    &workspace,
                    event_ids,
                    &hydrated,
                    json!({"context": hydrated}),
                );
                result
                    .document_revisions
                    .insert(document.id.to_string(), updated_document.revision);
                Ok(OrgMutationOutcome::Success(result))
            })
        },
    )
    .await
}

pub async fn retry_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &RetryItemRequest,
) -> Result<OrgClaimResult, OrgError> {
    validate_request_schema(request.schema_version)?;
    if request.expected_document_revision <= 0 {
        return Err(OrgError::invalid_input(
            "Expected Org document revision must be positive",
        ));
    }
    let request = request.clone();
    let fingerprint = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    let generic = execute_idempotent_outcome(
        context,
        RETRY_ITEM,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = require_active_workspace(transaction, command.workspace_id).await?;
                let document = load_document(transaction, &workspace, request.document_id).await?;
                let item = transaction
                    .get_org_work_item(request.work_item_id)
                    .await
                    .map_err(OrgError::storage)?
                    .filter(|item| {
                        item.workspace_id == workspace.id && item.document_id == document.id
                    })
                    .ok_or_else(|| not_found("work item"))?;
                if bookkeep_expired_open_lease(transaction, &workspace, &item, now).await? {
                    return Ok(OrgMutationOutcome::CommitError(stale_lease()));
                }
                validate_document_revision(&document, request.expected_document_revision)?;
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
                if item.state.as_deref() == Some(workspace.policy.running_state.as_str())
                    && current_attempt
                        .is_some_and(|attempt| attempt.status == OrgAttemptStatus::Expired)
                {
                    return Err(stale_lease());
                }
                let open_lease = transaction
                    .get_open_org_lease_internal(item.id)
                    .await
                    .map_err(OrgError::storage)?;
                let lease_status = open_lease
                    .as_ref()
                    .map(|lease| {
                        if lease.expires_at > now {
                            LeaseStatus::Active(match lease.kind {
                                OrgLeaseKind::Execution => note_org::LeaseKind::Execution,
                                OrgLeaseKind::Review => note_org::LeaseKind::Review,
                            })
                        } else {
                            LeaseStatus::Expired(match lease.kind {
                                OrgLeaseKind::Execution => note_org::LeaseKind::Execution,
                                OrgLeaseKind::Review => note_org::LeaseKind::Review,
                            })
                        }
                    })
                    .unwrap_or(LeaseStatus::None);
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
                    actor_id: Some(command.actor_id.clone()),
                    scheduled_at: item.scheduled.as_ref().map(|value| value.utc_timestamp),
                    queue: ClaimQueue::Execution,
                    execution_attempt_count: u32::try_from(attempts.len()).unwrap_or(u32::MAX),
                    current_attempt: current_attempt.map(|attempt| match attempt.status {
                        OrgAttemptStatus::Running => AttemptPhase::Running,
                        OrgAttemptStatus::Submitted => AttemptPhase::Submitted,
                        OrgAttemptStatus::Completed
                        | OrgAttemptStatus::Failed
                        | OrgAttemptStatus::Cancelled
                        | OrgAttemptStatus::Expired => AttemptPhase::Terminal,
                    }),
                };
                let decision = note_org::decide_claim(
                    &projected_to_domain(&item),
                    &workspace.policy,
                    &readiness,
                    ClaimRequest {
                        lease_kind: note_org::LeaseKind::Execution,
                        claim_kind: ClaimKind::Retry,
                    },
                )
                .map_err(map_claim_error)?;
                let target = match decision.transition_target {
                    note_org::TransitionTarget::State(target) => target,
                    _ => {
                        return Err(OrgError::new(
                            OrgErrorCode::InvalidTransition,
                            "Org retry decision did not enter running",
                            json!({"work_item_id": item.id}),
                            false,
                        ))
                    }
                };
                let raw_token = workflow_context.token_source().generate_token()?;
                if raw_token.trim().is_empty() {
                    return Err(OrgError::new(
                        OrgErrorCode::StorageFailure,
                        "Secure Org fencing token generation failed",
                        json!({}),
                        true,
                    ));
                }
                let digest = token_hash(&raw_token);
                write_claim_state(
                    transaction,
                    &workspace,
                    &document,
                    &projection,
                    item.id,
                    &[target.as_str()],
                    now,
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                let attempt_id = uuid::Uuid::new_v4().to_string();
                let attempt = transaction
                    .allocate_next_org_attempt(NewOrgAttemptAllocation {
                        id: &attempt_id,
                        workspace_id: workspace.id,
                        work_item_id: item.id,
                        actor_id: &command.actor_id,
                        started_at: now,
                        note_refs: &[],
                        artifacts: &[],
                        metadata: &json!({}),
                    })
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
                let lease_id = uuid::Uuid::new_v4().to_string();
                let expires_at = now.saturating_add(lease_duration(&workspace));
                let inserted = transaction
                    .insert_org_lease_if_capacity(
                        NewOrgLease {
                            id: &lease_id,
                            workspace_id: workspace.id,
                            work_item_id: item.id,
                            attempt_id: &attempt.id,
                            kind: OrgLeaseKind::Execution,
                            actor_id: &command.actor_id,
                            fencing_token_hash: &digest,
                            acquired_at: now,
                            last_heartbeat_at: now,
                            expires_at,
                        },
                        capacity(&workspace)?,
                        now,
                    )
                    .await
                    .map_err(OrgError::storage)?;
                if !matches!(inserted, ConditionalUpdate::Applied(_)) {
                    return Err(concurrency_limit(workspace.id));
                }
                workflow_context.after_workflow_phase(OrgWorkflowPhase::LeaseUpdate)?;
                let mut event_ids = Vec::new();
                for (event_type, summary) in [
                    (OrgEventType::Retry, "Retried Org work item"),
                    (OrgEventType::Claim, "Claimed retried Org work item"),
                    (OrgEventType::Start, "Started retried Org work item"),
                ] {
                    event_ids.push(
                        append_event(
                            transaction,
                            &uuid::Uuid::new_v4().to_string(),
                            &workspace,
                            item.id,
                            &command.actor_id,
                            Some(&attempt.id),
                            event_type,
                            now,
                            summary,
                            &json!({"schema_version": 1}),
                            item.state.as_deref(),
                            Some(&target),
                        )
                        .await?,
                    );
                }
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Events)?;
                let hydrated =
                    get_item_context_in_transaction(transaction, workspace.id, item.id, now)
                        .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ContextHydration)?;
                let claim = OrgClaimResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: workspace.id,
                    operation_id: command.operation_id.clone(),
                    lease_id,
                    fencing_token: raw_token,
                    expires_at,
                    event_ids: event_ids.clone(),
                    context: hydrated.clone(),
                };
                Ok(OrgMutationOutcome::Success(command_result(
                    &command,
                    &workspace,
                    event_ids,
                    &hydrated,
                    serde_json::to_value(&claim).map_err(|_| {
                        OrgError::invalid_input("Org retry result cannot be serialized")
                    })?,
                )))
            })
        },
    )
    .await?;
    decode_retry_result(generic.data)
}

fn decode_retry_result(value: serde_json::Value) -> Result<OrgClaimResult, OrgError> {
    serde_json::from_value(value).map_err(|_| {
        OrgError::new(
            OrgErrorCode::StorageFailure,
            "Stored Org retry result is invalid",
            json!({}),
            false,
        )
    })
}

pub(crate) fn classify_transition(
    policy: &note_org::WorkspacePolicy,
    from: &str,
    target: &str,
) -> TransitionLifecycle {
    if from == policy.review_state && policy.successful_terminal_states.contains(target) {
        TransitionLifecycle::Approval
    } else if from == policy.review_state && target == policy.review_rejection_state {
        TransitionLifecycle::Rejection
    } else if target == policy.cancelled_state {
        TransitionLifecycle::Cancellation
    } else if from == policy.running_state && target == policy.review_state {
        TransitionLifecycle::ReviewRequest
    } else if target == policy.failed_state {
        TransitionLifecycle::Failure
    } else if policy.states.contains("BLOCKED") && target == "BLOCKED" {
        TransitionLifecycle::Block
    } else if from == "BLOCKED" {
        TransitionLifecycle::Unblock
    } else if policy.successful_terminal_states.contains(target) {
        TransitionLifecycle::Completion
    } else {
        TransitionLifecycle::Transition
    }
}

pub(crate) fn closes_lease(lifecycle: TransitionLifecycle) -> bool {
    matches!(
        lifecycle,
        TransitionLifecycle::Approval
            | TransitionLifecycle::Rejection
            | TransitionLifecycle::Cancellation
            | TransitionLifecycle::ReviewRequest
            | TransitionLifecycle::Failure
            | TransitionLifecycle::Block
            | TransitionLifecycle::Completion
    )
}

pub(crate) fn lifecycle_end_reason(lifecycle: TransitionLifecycle) -> OrgLeaseEndReason {
    match lifecycle {
        TransitionLifecycle::Approval => OrgLeaseEndReason::Approval,
        TransitionLifecycle::Rejection => OrgLeaseEndReason::Rejection,
        TransitionLifecycle::Cancellation => OrgLeaseEndReason::Cancellation,
        TransitionLifecycle::ReviewRequest => OrgLeaseEndReason::ReviewRequest,
        TransitionLifecycle::Failure => OrgLeaseEndReason::Failure,
        TransitionLifecycle::Block => OrgLeaseEndReason::Block,
        TransitionLifecycle::Completion => OrgLeaseEndReason::Completion,
        TransitionLifecycle::Unblock | TransitionLifecycle::Transition => {
            unreachable!("non-closing transition has no lease end reason")
        }
    }
}

pub(crate) fn lifecycle_attempt(
    lifecycle: TransitionLifecycle,
    from_review: bool,
) -> (OrgAttemptStatus, OrgAttemptStatus, Option<&'static str>) {
    match lifecycle {
        TransitionLifecycle::Approval => (
            OrgAttemptStatus::Submitted,
            OrgAttemptStatus::Completed,
            Some("approved"),
        ),
        TransitionLifecycle::Rejection => (
            OrgAttemptStatus::Submitted,
            OrgAttemptStatus::Failed,
            Some("rejected"),
        ),
        TransitionLifecycle::Cancellation => (
            if from_review {
                OrgAttemptStatus::Submitted
            } else {
                OrgAttemptStatus::Running
            },
            OrgAttemptStatus::Cancelled,
            None,
        ),
        TransitionLifecycle::ReviewRequest => {
            (OrgAttemptStatus::Running, OrgAttemptStatus::Submitted, None)
        }
        TransitionLifecycle::Failure => (OrgAttemptStatus::Running, OrgAttemptStatus::Failed, None),
        TransitionLifecycle::Block => {
            (OrgAttemptStatus::Running, OrgAttemptStatus::Cancelled, None)
        }
        TransitionLifecycle::Completion => {
            (OrgAttemptStatus::Running, OrgAttemptStatus::Completed, None)
        }
        TransitionLifecycle::Unblock | TransitionLifecycle::Transition => {
            unreachable!("non-closing transition does not update an attempt")
        }
    }
}

pub(crate) fn lifecycle_events(lifecycle: TransitionLifecycle) -> &'static [OrgEventType] {
    match lifecycle {
        TransitionLifecycle::Approval => &[OrgEventType::Approval, OrgEventType::Completion],
        TransitionLifecycle::Rejection => &[OrgEventType::Rejection],
        TransitionLifecycle::Cancellation => &[OrgEventType::Cancellation],
        TransitionLifecycle::ReviewRequest => &[OrgEventType::ReviewRequest],
        TransitionLifecycle::Failure => &[OrgEventType::Failure],
        TransitionLifecycle::Block => &[OrgEventType::Block],
        TransitionLifecycle::Unblock => &[OrgEventType::Unblock],
        TransitionLifecycle::Completion => &[OrgEventType::Completion],
        TransitionLifecycle::Transition => &[OrgEventType::Transition],
    }
}

pub(crate) fn lifecycle_summary(event_type: &OrgEventType) -> &'static str {
    match event_type {
        OrgEventType::Approval => "Approved Org work item",
        OrgEventType::Rejection => "Rejected Org work item",
        OrgEventType::Cancellation => "Cancelled Org work item",
        OrgEventType::ReviewRequest => "Requested review for Org work item",
        OrgEventType::Failure => "Failed Org work item",
        OrgEventType::Block => "Blocked Org work item",
        OrgEventType::Unblock => "Unblocked Org work item",
        OrgEventType::Completion => "Completed Org work item",
        OrgEventType::Transition => "Transitioned Org work item",
        _ => "Changed Org work item",
    }
}

pub(crate) fn validate_lease_input(lease_id: &str, token: &str) -> Result<(), OrgError> {
    if lease_id.trim().is_empty()
        || lease_id != lease_id.trim()
        || token.trim().is_empty()
        || token != token.trim()
    {
        return Err(stale_lease());
    }
    Ok(())
}

fn stale_lease() -> OrgError {
    OrgError::new(
        OrgErrorCode::StaleLease,
        "Org lease proof is stale or invalid",
        json!({}),
        true,
    )
}

fn not_found(resource: &str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource}),
        false,
    )
}

fn validate_request_schema(schema_version: u32) -> Result<(), OrgError> {
    if schema_version == 1 {
        Ok(())
    } else {
        Err(OrgError::invalid_input(
            "Unsupported Org workflow request schema version",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_stored_retry_payload_is_a_storage_failure() {
        let error = decode_retry_result(json!({"not": "an org claim"})).unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure);
        assert!(!error.retryable);
    }
}
