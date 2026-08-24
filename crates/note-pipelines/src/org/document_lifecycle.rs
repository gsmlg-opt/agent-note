use super::{
    execute_idempotent, CommandEnvelope, OrgCommandKind, OrgCommandResult, OrgContext, OrgError,
    OrgErrorCode, OrgWorkflowPhase, ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{parse_document, validate_document_path, DocumentId, WorkItemId};
use note_storage::{
    NewOrgDocument, NewOrgEvent, OrgDocument, OrgDocumentLifecycleUpdate, OrgEventType,
    StorageError, StorageErrorKind, StorageTransaction,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CREATE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("create_document", 1);
const RENAME_DOCUMENT: OrgCommandKind = OrgCommandKind::new("rename_document", 1);
const ARCHIVE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("archive_document", 1);
const RESTORE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("restore_document", 1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDocumentRequest {
    pub document_id: DocumentId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameDocumentRequest {
    pub document_id: DocumentId,
    pub new_path: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRevisionRequest {
    pub document_id: DocumentId,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLifecycleData {
    pub document_id: DocumentId,
    pub path: String,
    pub archived_at: Option<i64>,
}

pub async fn create_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &CreateDocumentRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_path(&request.path)?;
    let command = envelope.clone();
    let request = request.clone();
    let fingerprint = request.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        CREATE_DOCUMENT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = transaction
                    .get_org_workspace(command.workspace_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| not_found("workspace"))?;
                if transaction
                    .get_org_document(request.document_id)
                    .await
                    .map_err(OrgError::storage)?
                    .is_some()
                {
                    return Err(OrgError::new(
                        OrgErrorCode::InvalidTransition,
                        "Org document already exists",
                        json!({"document_id": request.document_id}),
                        false,
                    ));
                }
                ensure_path_available(transaction, command.workspace_id, &request.path, None)
                    .await?;

                let source = String::new();
                let parsed = parse_document(source.clone(), &workspace.policy.parse_options())
                    .map_err(parse_error)?;
                let projection = super::project_document(
                    &parsed,
                    workspace.id,
                    request.document_id,
                    &workspace.timezone,
                    &BTreeMap::new(),
                    now,
                )?;
                let hash = content_hash(&source);
                transaction
                    .insert_org_document(NewOrgDocument {
                        id: request.document_id,
                        workspace_id: workspace.id,
                        path: &request.path,
                        source: &source,
                        content_hash: &hash,
                        now,
                    })
                    .await
                    .map_err(|error| map_path_constraint(error, &request.path))?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                transaction
                    .replace_org_document_projection(request.document_id, &projection)
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ProjectionUpdate)?;
                lifecycle_result(
                    transaction,
                    &workflow_context,
                    &command,
                    request.document_id,
                    &request.path,
                    None,
                    1,
                    OrgEventType::Creation,
                    "Created Org document",
                    json!({
                        "path": &request.path,
                        "revision": 1,
                    }),
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub async fn rename_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &RenameDocumentRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_path(&request.new_path)?;
    validate_expected_revision(request.expected_revision)?;
    let command = envelope.clone();
    let request = request.clone();
    let fingerprint = request.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        RENAME_DOCUMENT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let current =
                    owned_document(transaction, command.workspace_id, request.document_id).await?;
                super::validate_document_revision(&current, request.expected_revision)?;
                ensure_path_available(
                    transaction,
                    command.workspace_id,
                    &request.new_path,
                    Some(request.document_id),
                )
                .await?;
                let updated = super::resolve_cas(
                    transaction
                        .compare_and_swap_org_document_lifecycle(OrgDocumentLifecycleUpdate {
                            id: current.id,
                            expected_revision: request.expected_revision,
                            expected_archived_at: current.archived_at,
                            path: &request.new_path,
                            archived_at: current.archived_at,
                            updated_at: now,
                        })
                        .await
                        .map_err(|error| map_path_constraint(error, &request.new_path))?,
                    "document",
                )?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                lifecycle_result(
                    transaction,
                    &workflow_context,
                    &command,
                    updated.id,
                    &updated.path,
                    updated.archived_at,
                    updated.revision,
                    OrgEventType::DocumentRename,
                    "Renamed Org document",
                    json!({
                        "previous_path": &current.path,
                        "resulting_path": &updated.path,
                        "previous_revision": current.revision,
                        "resulting_revision": updated.revision,
                        "archived_at": updated.archived_at,
                    }),
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub async fn archive_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &DocumentRevisionRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_expected_revision(request.expected_revision)?;
    let command = envelope.clone();
    let request = request.clone();
    let fingerprint = request.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        ARCHIVE_DOCUMENT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let current =
                    owned_document(transaction, command.workspace_id, request.document_id).await?;
                super::validate_document_revision(&current, request.expected_revision)?;
                if current.archived_at.is_some() {
                    return Err(invalid_archive_state(current.id, true));
                }
                let blocking = blocking_document_lease_ids(transaction, current.id, now).await?;
                if !blocking.is_empty() {
                    return Err(OrgError::new(
                        OrgErrorCode::ActiveLease,
                        "Org document has active work-item leases",
                        json!({
                            "document_id": current.id,
                            "work_item_ids": blocking,
                        }),
                        false,
                    ));
                }
                change_archive_state(
                    transaction,
                    &workflow_context,
                    &command,
                    &current,
                    request.expected_revision,
                    Some(now),
                    OrgEventType::DocumentArchive,
                    "Archived Org document",
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub async fn restore_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &DocumentRevisionRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_expected_revision(request.expected_revision)?;
    let command = envelope.clone();
    let request = request.clone();
    let fingerprint = request.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        RESTORE_DOCUMENT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let current =
                    owned_document(transaction, command.workspace_id, request.document_id).await?;
                super::validate_document_revision(&current, request.expected_revision)?;
                if current.archived_at.is_none() {
                    return Err(invalid_archive_state(current.id, false));
                }
                change_archive_state(
                    transaction,
                    &workflow_context,
                    &command,
                    &current,
                    request.expected_revision,
                    None,
                    OrgEventType::DocumentRestore,
                    "Restored Org document",
                    now,
                )
                .await
            })
        },
    )
    .await
}

pub(crate) async fn blocking_document_lease_ids(
    transaction: &dyn StorageTransaction,
    document_id: DocumentId,
    now: i64,
) -> Result<Vec<WorkItemId>, OrgError> {
    let projection = transaction
        .list_org_document_projection(document_id)
        .await
        .map_err(OrgError::storage)?;
    let mut blocking = Vec::new();
    for item in projection {
        if transaction
            .get_active_org_lease(item.id, now)
            .await
            .map_err(OrgError::storage)?
            .is_some()
        {
            blocking.push(item.id);
        }
    }
    blocking.sort();
    Ok(blocking)
}

#[allow(clippy::too_many_arguments)]
async fn change_archive_state(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    command: &CommandEnvelope,
    current: &OrgDocument,
    expected_revision: i64,
    archived_at: Option<i64>,
    event_type: OrgEventType,
    summary: &'static str,
    now: i64,
) -> Result<OrgCommandResult, OrgError> {
    let updated = super::resolve_cas(
        transaction
            .compare_and_swap_org_document_lifecycle(OrgDocumentLifecycleUpdate {
                id: current.id,
                expected_revision,
                expected_archived_at: current.archived_at,
                path: &current.path,
                archived_at,
                updated_at: now,
            })
            .await
            .map_err(OrgError::storage)?,
        "document",
    )?;
    context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
    lifecycle_result(
        transaction,
        context,
        command,
        updated.id,
        &updated.path,
        updated.archived_at,
        updated.revision,
        event_type,
        summary,
        json!({
            "path": &current.path,
            "previous_archived_at": current.archived_at,
            "resulting_archived_at": updated.archived_at,
            "previous_revision": current.revision,
            "resulting_revision": updated.revision,
        }),
        now,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn lifecycle_result(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    command: &CommandEnvelope,
    document_id: DocumentId,
    path: &str,
    archived_at: Option<i64>,
    revision: i64,
    event_type: OrgEventType,
    summary: &'static str,
    metadata: serde_json::Value,
    now: i64,
) -> Result<OrgCommandResult, OrgError> {
    let event = transaction
        .append_org_event(NewOrgEvent {
            id: &uuid::Uuid::new_v4().to_string(),
            workspace_id: command.workspace_id,
            subject_kind: "document",
            subject_id: &document_id.to_string(),
            actor_id: &command.actor_id,
            attempt_id: None,
            event_type,
            occurred_at: now,
            summary,
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .map_err(OrgError::storage)?;
    context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    Ok(OrgCommandResult {
        schema_version: ORG_COMMAND_SCHEMA_VERSION,
        workspace_id: command.workspace_id,
        operation_id: command.operation_id.clone(),
        event_ids: vec![event.id],
        workspace_revision: None,
        document_revisions: BTreeMap::from([(document_id.to_string(), revision)]),
        data: serde_json::to_value(DocumentLifecycleData {
            document_id,
            path: path.to_owned(),
            archived_at,
        })
        .map_err(|_| OrgError::invalid_input("Document lifecycle result cannot be serialized"))?,
    })
}

async fn owned_document(
    transaction: &dyn StorageTransaction,
    workspace_id: note_org::WorkspaceId,
    document_id: DocumentId,
) -> Result<OrgDocument, OrgError> {
    transaction
        .get_org_document(document_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|document| document.workspace_id == workspace_id)
        .ok_or_else(|| not_found("document"))
}

async fn ensure_path_available(
    transaction: &dyn StorageTransaction,
    workspace_id: note_org::WorkspaceId,
    path: &str,
    except_document_id: Option<DocumentId>,
) -> Result<(), OrgError> {
    let conflict = transaction
        .list_org_documents(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .any(|document| document.path == path && Some(document.id) != except_document_id);
    if conflict {
        return Err(document_path_conflict(path));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), OrgError> {
    validate_document_path(path).map_err(|_| {
        OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org document path must be a portable relative lowercase .org path",
            json!({"field": "path", "path": path}),
            false,
        )
    })
}

fn validate_expected_revision(revision: i64) -> Result<(), OrgError> {
    if revision < 1 {
        return Err(OrgError::invalid_input(
            "Expected Org document revision must be positive",
        ));
    }
    Ok(())
}

fn content_hash(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

fn map_path_constraint(error: StorageError, path: &str) -> OrgError {
    if error.kind() == StorageErrorKind::Constraint {
        document_path_conflict(path)
    } else {
        OrgError::storage(error)
    }
}

fn document_path_conflict(path: &str) -> OrgError {
    OrgError::new(
        OrgErrorCode::DocumentPathConflict,
        "Org document path is already reserved",
        json!({"path": path}),
        false,
    )
}

fn invalid_archive_state(document_id: DocumentId, archived: bool) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidTransition,
        if archived {
            "Org document is already archived"
        } else {
            "Org document is not archived"
        },
        json!({"document_id": document_id}),
        false,
    )
}

fn parse_error(error: note_org::OrgError) -> OrgError {
    let details = match error {
        note_org::OrgError::Parse { line, .. } => {
            json!({"reason": "invalid Org input", "line": line})
        }
        _ => json!({"reason": "invalid Org input"}),
    };
    OrgError::new(
        OrgErrorCode::InvalidInput,
        "Org document is invalid",
        details,
        false,
    )
}

fn not_found(resource: &'static str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource}),
        false,
    )
}
