use super::{
    execute_idempotent, execute_idempotent_create, resolve_cas, CommandEnvelope, OrgCommandKind,
    OrgCommandResult, OrgContext, OrgError, OrgErrorCode, ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{validate_policy, WorkspacePolicy};
use note_storage::{NewOrgEvent, NewOrgWorkspace, OrgEventType, OrgWorkspaceUpdate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

const CREATE_WORKSPACE: OrgCommandKind = OrgCommandKind::new("create_workspace", 1);
const UPDATE_WORKSPACE: OrgCommandKind = OrgCommandKind::new("update_workspace", 1);

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
