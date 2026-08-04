use super::idempotency::request_fingerprint;
use super::{
    CommandEnvelope, OrgCommandKind, OrgCommandResult, OrgContext, OrgError, OrgErrorCode,
    OrgWorkflowPhase,
};
use note_storage::{CompareAndSwap, StorageTransaction, StoredOrgOperation, TransactionMode};
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

pub(crate) type OrgMutationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<OrgCommandResult, OrgError>> + Send + 'a>>;

pub(crate) enum OrgMutationOutcome {
    Success(OrgCommandResult),
    CommitError(OrgError),
}

pub(crate) type OrgMutationOutcomeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<OrgMutationOutcome, OrgError>> + Send + 'a>>;

pub(crate) async fn execute_idempotent<R, F>(
    context: &OrgContext,
    command_kind: OrgCommandKind,
    envelope: &CommandEnvelope,
    request: &R,
    mutation: F,
) -> Result<OrgCommandResult, OrgError>
where
    R: Serialize + ?Sized,
    F: for<'a> FnOnce(&'a dyn StorageTransaction, i64) -> OrgMutationFuture<'a>,
{
    execute_idempotent_outcome_with_workspace(
        context,
        command_kind,
        envelope,
        request,
        WorkspaceRequirement::Existing,
        move |transaction, now| {
            let mutation = mutation(transaction, now);
            Box::pin(async move { mutation.await.map(OrgMutationOutcome::Success) })
        },
    )
    .await
}

pub(crate) async fn execute_idempotent_outcome<R, F>(
    context: &OrgContext,
    command_kind: OrgCommandKind,
    envelope: &CommandEnvelope,
    request: &R,
    mutation: F,
) -> Result<OrgCommandResult, OrgError>
where
    R: Serialize + ?Sized,
    F: for<'a> FnOnce(&'a dyn StorageTransaction, i64) -> OrgMutationOutcomeFuture<'a>,
{
    execute_idempotent_outcome_with_workspace(
        context,
        command_kind,
        envelope,
        request,
        WorkspaceRequirement::Existing,
        mutation,
    )
    .await
}

pub(crate) async fn execute_idempotent_create<R, F>(
    context: &OrgContext,
    command_kind: OrgCommandKind,
    envelope: &CommandEnvelope,
    request: &R,
    mutation: F,
) -> Result<OrgCommandResult, OrgError>
where
    R: Serialize + ?Sized,
    F: for<'a> FnOnce(&'a dyn StorageTransaction, i64) -> OrgMutationFuture<'a>,
{
    execute_idempotent_outcome_with_workspace(
        context,
        command_kind,
        envelope,
        request,
        WorkspaceRequirement::Missing,
        move |transaction, now| {
            let mutation = mutation(transaction, now);
            Box::pin(async move { mutation.await.map(OrgMutationOutcome::Success) })
        },
    )
    .await
}

#[derive(Clone, Copy)]
enum WorkspaceRequirement {
    Existing,
    Missing,
}

async fn execute_idempotent_outcome_with_workspace<R, F>(
    context: &OrgContext,
    command_kind: OrgCommandKind,
    envelope: &CommandEnvelope,
    request: &R,
    workspace_requirement: WorkspaceRequirement,
    mutation: F,
) -> Result<OrgCommandResult, OrgError>
where
    R: Serialize + ?Sized,
    F: for<'a> FnOnce(&'a dyn StorageTransaction, i64) -> OrgMutationOutcomeFuture<'a>,
{
    validate_envelope(envelope)?;
    validate_command_kind(command_kind)?;
    let fingerprint = request_fingerprint(&serde_json::json!({
        "schema_version": envelope.schema_version,
        "command_kind": {
            "name": command_kind.name(),
            "schema_version": command_kind.schema_version(),
        },
        "workspace_id": envelope.workspace_id,
        "actor_id": envelope.actor_id,
        "request": serde_json::to_value(request)
            .map_err(|_| OrgError::invalid_input("Org command cannot be serialized"))?,
    }))?;
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
            return finish_write(transaction, Err(OrgError::storage(error))).await;
        }
    };
    if let Some(existing) = existing {
        let result = if existing.request_fingerprint == fingerprint {
            serde_json::from_value(existing.result).map_err(|_| {
                OrgError::new(
                    OrgErrorCode::StorageFailure,
                    "Stored Org operation result is invalid",
                    serde_json::json!({}),
                    false,
                )
            })
        } else {
            Err(OrgError::new(
                OrgErrorCode::IdempotencyConflict,
                "Operation ID was already used for a different Org command",
                serde_json::json!({"operation_id": envelope.operation_id}),
                false,
            ))
        };
        return finish_read(transaction, result).await;
    }

    let workspace = match transaction.get_org_workspace(envelope.workspace_id).await {
        Ok(workspace) => workspace,
        Err(error) => return finish_write(transaction, Err(OrgError::storage(error))).await,
    };
    match (workspace_requirement, workspace) {
        (WorkspaceRequirement::Existing, None) => {
            return finish_write(
                transaction,
                Err(OrgError::new(
                    OrgErrorCode::NotFound,
                    "Org workspace was not found",
                    serde_json::json!({"workspace_id": envelope.workspace_id}),
                    false,
                )),
            )
            .await;
        }
        (WorkspaceRequirement::Existing, Some(workspace)) if workspace.archived_at.is_some() => {
            return finish_write(
                transaction,
                Err(OrgError::new(
                    OrgErrorCode::ArchivedWorkspace,
                    "Archived Org workspaces are read-only",
                    serde_json::json!({"workspace_id": envelope.workspace_id}),
                    false,
                )),
            )
            .await;
        }
        (WorkspaceRequirement::Missing, Some(_)) => {
            return finish_write(
                transaction,
                Err(OrgError::new(
                    OrgErrorCode::InvalidTransition,
                    "Org workspace already exists",
                    serde_json::json!({"workspace_id": envelope.workspace_id}),
                    false,
                )),
            )
            .await;
        }
        _ => {}
    }

    let now = context.clock().now();
    let result = mutation(transaction.as_ref(), now).await;
    let result = match result {
        Ok(OrgMutationOutcome::Success(result)) => result,
        Ok(OrgMutationOutcome::CommitError(error)) => {
            transaction.commit().await.map_err(OrgError::storage)?;
            return Err(error);
        }
        Err(error) => return finish_write(transaction, Err(error)).await,
    };
    if result.schema_version != super::ORG_COMMAND_SCHEMA_VERSION
        || result.workspace_id != envelope.workspace_id
        || result.operation_id != envelope.operation_id
    {
        return finish_write(
            transaction,
            Err(OrgError::invalid_input(
                "Org command result does not match its command envelope",
            )),
        )
        .await;
    }
    let stored_result = match serde_json::to_value(&result) {
        Ok(result) => result,
        Err(_) => {
            return finish_write(
                transaction,
                Err(OrgError::invalid_input(
                    "Org command result cannot be serialized",
                )),
            )
            .await
        }
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
        return finish_write(transaction, Err(OrgError::storage(error))).await;
    }
    if let Err(error) = context.after_workflow_phase(OrgWorkflowPhase::OperationWrite) {
        return finish_write(transaction, Err(error)).await;
    }
    finish_write(transaction, Ok(result)).await
}

fn validate_command_kind(command_kind: OrgCommandKind) -> Result<(), OrgError> {
    if command_kind.name().trim().is_empty()
        || command_kind.name() != command_kind.name().trim()
        || command_kind.schema_version() == 0
    {
        return Err(OrgError::invalid_input(
            "Org command kind must have a stable nonblank name and positive schema version",
        ));
    }
    Ok(())
}

pub(crate) fn resolve_cas<T>(
    value: CompareAndSwap<T>,
    resource: &'static str,
) -> Result<T, OrgError> {
    match value {
        CompareAndSwap::Applied(value) => Ok(value),
        CompareAndSwap::NotFound => Err(OrgError::new(
            OrgErrorCode::NotFound,
            format!("{resource} was not found"),
            serde_json::json!({"resource": resource}),
            false,
        )),
        CompareAndSwap::Conflict { current_revision } => Err(OrgError::new(
            OrgErrorCode::StaleRevision,
            format!("{resource} revision is stale"),
            serde_json::json!({"resource": resource, "current_revision": current_revision}),
            true,
        )),
    }
}

fn validate_envelope(envelope: &CommandEnvelope) -> Result<(), OrgError> {
    if envelope.schema_version != super::ORG_COMMAND_SCHEMA_VERSION {
        return Err(OrgError::invalid_input(
            "Unsupported Org command schema version",
        ));
    }
    if envelope.actor_id.trim().is_empty() || envelope.actor_id != envelope.actor_id.trim() {
        return Err(OrgError::invalid_input(
            "Org actor ID must be nonblank and trimmed",
        ));
    }
    if envelope.actor_id == "system" {
        return Err(OrgError::invalid_input(
            "Org actor ID system is reserved for internal events",
        ));
    }
    if envelope.operation_id.trim().is_empty()
        || envelope.operation_id != envelope.operation_id.trim()
    {
        return Err(OrgError::invalid_input(
            "Org operation ID must be nonblank and trimmed",
        ));
    }
    Ok(())
}

async fn finish_read(
    transaction: Box<dyn StorageTransaction>,
    result: Result<OrgCommandResult, OrgError>,
) -> Result<OrgCommandResult, OrgError> {
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

async fn finish_write(
    transaction: Box<dyn StorageTransaction>,
    result: Result<OrgCommandResult, OrgError>,
) -> Result<OrgCommandResult, OrgError> {
    match result {
        Ok(result) => {
            transaction.commit().await.map_err(OrgError::storage)?;
            Ok(result)
        }
        Err(primary) => match transaction.rollback().await {
            Ok(()) => Err(primary),
            Err(rollback) => Err(OrgError::new(
                OrgErrorCode::StorageFailure,
                "Org mutation failed and rollback also failed",
                serde_json::json!({
                    "primary_code": primary.code,
                    "rollback_kind": storage_kind_for_rollback(rollback.kind()),
                }),
                true,
            )),
        },
    }
}

fn storage_kind_for_rollback(kind: note_storage::StorageErrorKind) -> &'static str {
    match kind {
        note_storage::StorageErrorKind::IncompatibleDatabase => "incompatible_database",
        note_storage::StorageErrorKind::Constraint => "constraint",
        note_storage::StorageErrorKind::Conflict => "conflict",
        note_storage::StorageErrorKind::Corrupt => "corrupt",
        note_storage::StorageErrorKind::Operation => "operation",
        note_storage::StorageErrorKind::Transaction => "transaction",
        note_storage::StorageErrorKind::Unavailable => "unavailable",
        note_storage::StorageErrorKind::UnsupportedSchema => "unsupported_schema",
    }
}
