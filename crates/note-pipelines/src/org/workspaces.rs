use super::{
    bookkeep_expired_lease_with_phases, execute_idempotent, execute_idempotent_create,
    execute_idempotent_outcome, resolve_cas, CommandEnvelope, OrgCommandKind, OrgCommandResult,
    OrgContext, OrgError, OrgErrorCode, OrgMutationOutcome, OrgWorkflowPhase,
    ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{validate_policy, WorkspacePolicy};
use note_storage::{NewOrgEvent, NewOrgWorkspace, OrgEventType, OrgWorkspaceUpdate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub async fn get_workspace(
    context: &OrgContext,
    workspace_id: note_org::WorkspaceId,
) -> Result<super::OrgWorkspaceView, OrgError> {
    context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .map(map_workspace_view)
        .ok_or_else(|| {
            OrgError::new(
                OrgErrorCode::NotFound,
                "Org workspace was not found",
                json!({"workspace_id": workspace_id}),
                false,
            )
        })
}

pub async fn list_workspaces(
    context: &OrgContext,
    query: &super::OrgReadQuery,
) -> Result<super::OrgReadPage<super::WorkspaceSummary>, OrgError> {
    let now = context.clock().now();
    if now <= 0 {
        return Err(OrgError::invalid_input(
            "Org operational evaluation time must be positive",
        ));
    }
    let transaction = context
        .storage()
        .begin(note_storage::TransactionMode::Deferred)
        .await
        .map_err(OrgError::storage)?;
    let result = async {
        // Page the lightweight workspace identities first. Operational
        // summaries and readiness scans are then bounded by the requested page
        // size instead of the total workspace count.
        let workspace_page = super::paginate_read(
            transaction
                .list_org_workspaces(query.include_archived)
                .await
                .map_err(OrgError::storage)?,
            query,
            context.cursor_signer(),
            "workspaces",
            None,
            |workspace| workspace.id.to_string(),
        )?;
        let mut summaries = Vec::with_capacity(workspace_page.items.len());
        for workspace in workspace_page.items {
            summaries.push(workspace_summary_for(transaction.as_ref(), workspace, now).await?);
        }
        Ok(super::OrgReadPage {
            items: summaries,
            next_cursor: workspace_page.next_cursor,
        })
    }
    .await;
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

pub async fn get_workspace_summary(
    context: &OrgContext,
    workspace_id: note_org::WorkspaceId,
) -> Result<super::WorkspaceSummary, OrgError> {
    let now = context.clock().now();
    if now <= 0 {
        return Err(OrgError::invalid_input(
            "Org operational evaluation time must be positive",
        ));
    }
    let transaction = context
        .storage()
        .begin(note_storage::TransactionMode::Deferred)
        .await
        .map_err(OrgError::storage)?;
    let result = workspace_summary_in_transaction(transaction.as_ref(), workspace_id, now).await;
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

async fn workspace_summary_in_transaction(
    transaction: &dyn note_storage::StorageTransaction,
    workspace_id: note_org::WorkspaceId,
    now: i64,
) -> Result<super::WorkspaceSummary, OrgError> {
    let workspace = transaction
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| {
            OrgError::new(
                OrgErrorCode::NotFound,
                "Org workspace was not found",
                json!({"workspace_id": workspace_id}),
                false,
            )
        })?;
    workspace_summary_for(transaction, workspace, now).await
}

async fn workspace_summary_for(
    transaction: &dyn note_storage::StorageTransaction,
    workspace: note_storage::OrgWorkspace,
    now: i64,
) -> Result<super::WorkspaceSummary, OrgError> {
    let raw = transaction
        .get_org_workspace_operational_summary(workspace.id, now)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| {
            OrgError::new(
                OrgErrorCode::StorageFailure,
                "Org workspace summary is inconsistent",
                json!({"workspace_id": workspace.id}),
                false,
            )
        })?;
    let ready = super::count_ready_in_transaction(transaction, workspace.id, now).await?;
    Ok(super::WorkspaceSummary {
        workspace_id: workspace.id,
        slug: workspace.slug,
        display_name: workspace.display_name,
        description: workspace.description,
        timezone: raw.timezone,
        archived_at: raw.archived_at,
        workspace_revision: raw.workspace_revision,
        evaluated_at: raw.evaluated_at,
        counts: super::OperationalCounts {
            ready,
            assigned: raw.counts.assigned,
            running: raw.counts.running,
            blocked: raw.counts.blocked,
            review: raw.counts.review,
            scheduled: raw.counts.scheduled,
            upcoming_deadline: raw.counts.upcoming_deadline,
            failed: raw.counts.failed,
            expired_lease: raw.counts.expired_lease,
            completed: raw.counts.completed,
        },
    })
}

pub(crate) fn map_workspace_view(workspace: note_storage::OrgWorkspace) -> super::OrgWorkspaceView {
    super::OrgWorkspaceView {
        id: workspace.id,
        slug: workspace.slug,
        display_name: workspace.display_name,
        description: workspace.description,
        timezone: workspace.timezone,
        policy_schema_version: workspace.policy_schema_version,
        policy: workspace.policy,
        revision: workspace.revision,
        archived_at: workspace.archived_at,
    }
}

const CREATE_WORKSPACE: OrgCommandKind = OrgCommandKind::new("create_workspace", 1);
const UPDATE_WORKSPACE: OrgCommandKind = OrgCommandKind::new("update_workspace", 1);
const ARCHIVE_WORKSPACE: OrgCommandKind = OrgCommandKind::new("archive_workspace", 1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateWorkspaceRequest {
    pub expected_revision: i64,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveWorkspaceRequest {
    pub expected_revision: i64,
}

pub async fn create_workspace(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &CreateWorkspaceRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_workspace_fields(
        &request.slug,
        &request.display_name,
        &request.timezone,
        request.policy_schema_version,
        &request.policy,
    )?;
    let request = request.clone();
    let fingerprint_request = request.clone();
    let command_envelope = envelope.clone();
    execute_idempotent_create(
        context,
        CREATE_WORKSPACE,
        envelope,
        &fingerprint_request,
        move |transaction, now| {
            Box::pin(async move {
                transaction
                    .insert_org_workspace(NewOrgWorkspace {
                        id: command_envelope.workspace_id,
                        slug: &request.slug,
                        display_name: &request.display_name,
                        description: &request.description,
                        timezone: &request.timezone,
                        policy_schema_version: request.policy_schema_version,
                        policy: &request.policy,
                        now,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                let metadata = json!({
                    "slug": request.slug,
                    "timezone": request.timezone,
                    "policy_schema_version": request.policy_schema_version,
                });
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: &uuid::Uuid::new_v4().to_string(),
                        workspace_id: command_envelope.workspace_id,
                        subject_kind: "workspace",
                        subject_id: &command_envelope.workspace_id.to_string(),
                        actor_id: &command_envelope.actor_id,
                        attempt_id: None,
                        event_type: OrgEventType::Creation,
                        occurred_at: now,
                        summary: "Created Org workspace",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: command_envelope.workspace_id,
                    operation_id: command_envelope.operation_id,
                    event_ids: vec![event.id],
                    workspace_revision: Some(1),
                    document_revisions: BTreeMap::new(),
                    data: json!({
                        "workspace_id": command_envelope.workspace_id,
                        "revision": 1,
                    }),
                })
            })
        },
    )
    .await
}

pub async fn update_workspace(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &UpdateWorkspaceRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_workspace_fields(
        &request.slug,
        &request.display_name,
        &request.timezone,
        request.policy_schema_version,
        &request.policy,
    )?;
    if request.expected_revision < 1 {
        return Err(OrgError::invalid_input(
            "Expected workspace revision must be positive",
        ));
    }
    let request = request.clone();
    let fingerprint_request = request.clone();
    let command_envelope = envelope.clone();
    execute_idempotent(
        context,
        UPDATE_WORKSPACE,
        envelope,
        &fingerprint_request,
        move |transaction, now| {
            Box::pin(async move {
                let current_workspace = transaction
                    .get_org_workspace(command_envelope.workspace_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| {
                        OrgError::new(
                            OrgErrorCode::NotFound,
                            "Org workspace was not found",
                            json!({"workspace_id": command_envelope.workspace_id}),
                            false,
                        )
                    })?;
                let current_items = transaction
                    .list_org_workspace_projection(command_envelope.workspace_id)
                    .await
                    .map_err(OrgError::storage)?;
                super::validate_policy_role_compatibility(
                    &current_workspace.policy,
                    &request.policy,
                    &current_items,
                )?;
                let projection = super::reproject_workspace(
                    transaction,
                    command_envelope.workspace_id,
                    &request.timezone,
                    &request.policy,
                    now,
                )
                .await?;
                let workspace = resolve_cas(
                    transaction
                        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                            id: command_envelope.workspace_id,
                            expected_revision: request.expected_revision,
                            slug: &request.slug,
                            display_name: &request.display_name,
                            description: &request.description,
                            timezone: &request.timezone,
                            policy_schema_version: request.policy_schema_version,
                            policy: &request.policy,
                            archived_at: None,
                            updated_at: now,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "workspace",
                )?;
                transaction
                    .rebuild_org_workspace_projection(command_envelope.workspace_id, &projection)
                    .await
                    .map_err(OrgError::storage)?;
                let metadata = json!({
                    "revision": workspace.revision,
                    "slug": workspace.slug,
                    "timezone": workspace.timezone,
                    "policy_schema_version": workspace.policy_schema_version,
                });
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: &uuid::Uuid::new_v4().to_string(),
                        workspace_id: command_envelope.workspace_id,
                        subject_kind: "workspace",
                        subject_id: &command_envelope.workspace_id.to_string(),
                        actor_id: &command_envelope.actor_id,
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "Updated Org workspace",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: command_envelope.workspace_id,
                    operation_id: command_envelope.operation_id,
                    event_ids: vec![event.id],
                    workspace_revision: Some(workspace.revision),
                    document_revisions: BTreeMap::new(),
                    data: json!({
                        "workspace_id": command_envelope.workspace_id,
                        "revision": workspace.revision,
                    }),
                })
            })
        },
    )
    .await
}

pub async fn archive_workspace(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ArchiveWorkspaceRequest,
) -> Result<OrgCommandResult, OrgError> {
    if request.expected_revision < 1 {
        return Err(OrgError::invalid_input(
            "Expected workspace revision must be positive",
        ));
    }
    let request = request.clone();
    let command = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent_outcome(
        context,
        ARCHIVE_WORKSPACE,
        envelope,
        &request,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = transaction
                    .get_org_workspace(command.workspace_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| {
                        OrgError::new(
                            OrgErrorCode::NotFound,
                            "Org workspace was not found",
                            json!({"workspace_id": command.workspace_id}),
                            false,
                        )
                    })?;
                let projection = transaction
                    .list_org_workspace_projection(workspace.id)
                    .await
                    .map_err(OrgError::storage)?;
                let mut expiry_event_ids = Vec::new();
                for item in &projection {
                    let Some(lease) = transaction
                        .get_open_org_lease_internal(item.id)
                        .await
                        .map_err(OrgError::storage)?
                    else {
                        continue;
                    };
                    if lease.expires_at <= now {
                        expiry_event_ids.push(
                            bookkeep_expired_lease_with_phases(
                                transaction,
                                &workspace,
                                item,
                                &lease,
                                now,
                                |phase| workflow_context.after_workflow_phase(phase),
                            )
                            .await?,
                        );
                    }
                }
                let active_count = transaction
                    .count_active_org_leases(workspace.id, now)
                    .await
                    .map_err(OrgError::storage)?;
                if active_count > 0 {
                    let error = OrgError::new(
                        OrgErrorCode::ActiveLease,
                        "Org workspace has active leases",
                        json!({"workspace_id": workspace.id}),
                        true,
                    );
                    return if expiry_event_ids.is_empty() {
                        Err(error)
                    } else {
                        Ok(OrgMutationOutcome::CommitError(error))
                    };
                }
                let archived = resolve_cas(
                    transaction
                        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                            id: workspace.id,
                            expected_revision: request.expected_revision,
                            slug: &workspace.slug,
                            display_name: &workspace.display_name,
                            description: &workspace.description,
                            timezone: &workspace.timezone,
                            policy_schema_version: workspace.policy_schema_version,
                            policy: &workspace.policy,
                            archived_at: Some(now),
                            updated_at: now,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "workspace",
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: &uuid::Uuid::new_v4().to_string(),
                        workspace_id: workspace.id,
                        subject_kind: "workspace",
                        subject_id: &workspace.id.to_string(),
                        actor_id: &command.actor_id,
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceArchive,
                        occurred_at: now,
                        summary: "Archived Org workspace",
                        metadata: &json!({"schema_version": 1, "revision": archived.revision}),
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Events)?;
                expiry_event_ids.push(event.id);
                Ok(OrgMutationOutcome::Success(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: workspace.id,
                    operation_id: command.operation_id,
                    event_ids: expiry_event_ids,
                    workspace_revision: Some(archived.revision),
                    document_revisions: BTreeMap::new(),
                    data: json!({"workspace_id": workspace.id, "archived_at": now}),
                }))
            })
        },
    )
    .await
}

fn validate_workspace_fields(
    slug: &str,
    display_name: &str,
    timezone: &str,
    policy_schema_version: i64,
    policy: &WorkspacePolicy,
) -> Result<(), OrgError> {
    for (name, value) in [
        ("slug", slug),
        ("display name", display_name),
        ("timezone", timezone),
    ] {
        if value.trim().is_empty() || value != value.trim() {
            return Err(OrgError::invalid_input(format!(
                "Org workspace {name} must be nonblank and trimmed"
            )));
        }
    }
    if policy_schema_version != 1 {
        return Err(OrgError::invalid_input(
            "Unsupported Org policy schema version",
        ));
    }
    validate_policy(policy).map_err(|error| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org workspace policy is invalid",
            json!({"reason": error.to_string()}),
            false,
        )
    })?;
    let probe = note_org::OrgTimestamp {
        raw: "<2000-01-01 Sat 12:00>".into(),
        local: chrono::NaiveDate::from_ymd_opt(2000, 1, 1)
            .expect("fixed date is valid")
            .and_hms_opt(12, 0, 0)
            .expect("fixed time is valid"),
    };
    note_org::resolve_org_timestamp(&probe, timezone).map_err(|error| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org workspace timezone is invalid",
            json!({"reason": error.to_string()}),
            false,
        )
    })?;
    Ok(())
}
