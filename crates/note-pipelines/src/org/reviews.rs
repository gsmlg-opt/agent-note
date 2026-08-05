use super::{
    append_event, bookkeep_expired_open_lease, command_result, execute_idempotent_outcome,
    get_item_context_in_transaction, load_document, require_active_workspace, resolve_lease_update,
    resolve_update, stale_lease, token_hash, unique_successful_target, validate_document_revision,
    validate_item_transition, validate_lease_input, validate_public_payload, write_claim_state,
    ApproveItemRequest, CommandEnvelope, OrgCommandKind, OrgCommandResult, OrgContext, OrgError,
    OrgMutationOutcome, OrgWorkflowPhase, RejectItemRequest, RequestReviewRequest,
};
use note_storage::{
    OrgAttemptStatus, OrgAttemptUpdate, OrgEventType, OrgLeaseClosure, OrgLeaseEndReason,
    OrgLeaseKind, OrgLeaseProof,
};
use serde_json::json;

const REQUEST_REVIEW: OrgCommandKind = OrgCommandKind::new("request_review", 1);
const APPROVE_ITEM: OrgCommandKind = OrgCommandKind::new("approve_item", 1);
const REJECT_ITEM: OrgCommandKind = OrgCommandKind::new("reject_item", 1);

pub async fn request_review(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &RequestReviewRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_schema(request.schema_version)?;
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
        REQUEST_REVIEW,
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
                            end_reason: OrgLeaseEndReason::ReviewRequest,
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
                validate_item_transition(
                    &workspace.policy,
                    &item,
                    &projection,
                    &workspace.policy.review_state,
                    false,
                )?;
                let updated_document = write_claim_state(
                    transaction,
                    &workspace,
                    &document,
                    &projection,
                    item.id,
                    &[workspace.policy.review_state.as_str()],
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
                            status: OrgAttemptStatus::Submitted,
                            ended_at: now,
                            error: None,
                            result_summary: request.result_summary.as_deref(),
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
                let has_partial_result = request.result_summary.is_some()
                    || !request.note_refs.is_empty()
                    || !request.artifacts.is_empty()
                    || request
                        .metadata
                        .as_object()
                        .is_some_and(|value| !value.is_empty());
                let mut event_ids = Vec::new();
                if has_partial_result {
                    event_ids.push(
                        append_event(
                            transaction,
                            &uuid::Uuid::new_v4().to_string(),
                            &workspace,
                            item.id,
                            &command.actor_id,
                            Some(&attempt.id),
                            OrgEventType::ResultSubmission,
                            now,
                            "Submitted partial Org work result",
                            &json!({"schema_version": 1}),
                            item.state.as_deref(),
                            item.state.as_deref(),
                        )
                        .await?,
                    );
                }
                event_ids.push(
                    append_event(
                        transaction,
                        &uuid::Uuid::new_v4().to_string(),
                        &workspace,
                        item.id,
                        &command.actor_id,
                        Some(&attempt.id),
                        OrgEventType::ReviewRequest,
                        now,
                        "Requested review for Org work item",
                        &json!({"schema_version": 1}),
                        item.state.as_deref(),
                        Some(&workspace.policy.review_state),
                    )
                    .await?,
                );
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

pub async fn approve_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ApproveItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_schema(request.schema_version)?;
    validate_lease_input(&request.lease_id, &request.fencing_token)?;
    validate_public_payload(
        &request.fencing_token,
        &json!({"metadata": request.metadata}),
    )?;
    let fingerprint = json!({
        "schema_version": request.schema_version,
        "work_item_id": request.work_item_id,
        "document_id": request.document_id,
        "expected_document_revision": request.expected_document_revision,
        "lease_id": request.lease_id,
        "fencing_token_digest": token_hash(&request.fencing_token),
        "metadata": request.metadata,
    });
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent_outcome(
        context,
        APPROVE_ITEM,
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
                let lease = resolve_lease_update(
                    transaction
                        .close_org_lease(OrgLeaseClosure {
                            proof: OrgLeaseProof {
                                lease_id: &request.lease_id,
                                workspace_id: workspace.id,
                                work_item_id: item.id,
                                fencing_token_hash: &digest,
                                kind: OrgLeaseKind::Review,
                                actor_id: &command.actor_id,
                                now,
                            },
                            ended_at: now,
                            end_reason: OrgLeaseEndReason::Approval,
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
                let target = unique_successful_target(&workspace.policy, &item, &projection, true)?;
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
                            expected_status: OrgAttemptStatus::Submitted,
                            status: OrgAttemptStatus::Completed,
                            ended_at: now,
                            error: attempt.error.as_deref(),
                            result_summary: attempt.result_summary.as_deref(),
                            review_outcome: Some("approved"),
                            note_refs: &attempt.note_refs,
                            artifacts: &attempt.artifacts,
                            metadata: &attempt.metadata,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "attempt",
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
                let approval = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    item.id,
                    &command.actor_id,
                    Some(&attempt.id),
                    OrgEventType::Approval,
                    now,
                    "Approved Org work item",
                    &json!({"schema_version": 1, "review": request.metadata}),
                    item.state.as_deref(),
                    Some(&target),
                )
                .await?;
                let completion = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    item.id,
                    &command.actor_id,
                    Some(&attempt.id),
                    OrgEventType::Completion,
                    now,
                    "Completed approved Org work item",
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
                    vec![approval, completion],
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

pub async fn reject_item(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &RejectItemRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_schema(request.schema_version)?;
    validate_lease_input(&request.lease_id, &request.fencing_token)?;
    validate_public_payload(
        &request.fencing_token,
        &json!({
            "reason": request.reason,
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
        "reason": request.reason,
        "metadata": request.metadata,
    });
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent_outcome(
        context,
        REJECT_ITEM,
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
                let lease = resolve_lease_update(
                    transaction
                        .close_org_lease(OrgLeaseClosure {
                            proof: OrgLeaseProof {
                                lease_id: &request.lease_id,
                                workspace_id: workspace.id,
                                work_item_id: item.id,
                                fencing_token_hash: &digest,
                                kind: OrgLeaseKind::Review,
                                actor_id: &command.actor_id,
                                now,
                            },
                            ended_at: now,
                            end_reason: OrgLeaseEndReason::Rejection,
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
                let target = workspace.policy.review_rejection_state.clone();
                validate_item_transition(&workspace.policy, &item, &projection, &target, false)?;
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
                            expected_status: OrgAttemptStatus::Submitted,
                            status: OrgAttemptStatus::Failed,
                            ended_at: now,
                            error: Some(&request.reason),
                            result_summary: attempt.result_summary.as_deref(),
                            review_outcome: Some("rejected"),
                            note_refs: &attempt.note_refs,
                            artifacts: &attempt.artifacts,
                            metadata: &attempt.metadata,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "attempt",
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
                let event = append_event(
                    transaction,
                    &uuid::Uuid::new_v4().to_string(),
                    &workspace,
                    item.id,
                    &command.actor_id,
                    Some(&attempt.id),
                    OrgEventType::Rejection,
                    now,
                    "Rejected Org work item review",
                    &json!({
                        "schema_version": 1,
                        "reason": request.reason,
                        "review": request.metadata,
                    }),
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
                    vec![event],
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

fn validate_schema(schema_version: u32) -> Result<(), OrgError> {
    if schema_version == 1 {
        Ok(())
    } else {
        Err(OrgError::invalid_input(
            "Unsupported Org workflow request schema version",
        ))
    }
}

fn not_found(resource: &str) -> OrgError {
    OrgError::new(
        super::OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource}),
        false,
    )
}
