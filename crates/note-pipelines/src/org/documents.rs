use super::{
    capacity, classify_transition, closes_lease, concurrency_limit, execute_idempotent,
    execute_idempotent_create, inspect_touched_lease_proofs_except, lifecycle_attempt,
    lifecycle_end_reason, lifecycle_events, lifecycle_summary, resolve_lease_update,
    resolve_update, stale_lease, storage_kind, token_hash, validate_item_transition,
    validate_lease_input, validate_public_payload, validate_touched_lease_proofs_except,
    CommandEnvelope, LeaseProofInput, OrgCommandKind, OrgCommandResult, OrgContext, OrgError,
    OrgErrorCode, OrgWorkflowPhase, TransitionLifecycle, ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{
    parse_document, validate_document_path, ClaimPolicy, DocumentId, WorkspaceId, WorkspacePolicy,
};
use note_storage::{
    ConditionalUpdate, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, OrgDocument,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEventType,
    OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseKind, OrgLeaseOwnershipMove, OrgLeaseProof,
    OrgProjectedWorkItem, OrgWorkspace, OrgWorkspaceUpdate, StorageTransaction,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const PUT_DOCUMENT: OrgCommandKind = OrgCommandKind::new("put_document", 1);
const IMPORT_OFFLINE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("import_offline_document", 1);
const IMPORT_DOCUMENTS: OrgCommandKind = OrgCommandKind::new("import_documents", 1);
const IMPORT_WORKSPACE_SNAPSHOT: OrgCommandKind =
    OrgCommandKind::new("import_workspace_snapshot", 1);
const IMPORT_OFFLINE_WORKSPACE_SNAPSHOT: OrgCommandKind =
    OrgCommandKind::new("import_offline_workspace_snapshot", 1);
const MOVE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("move_document", 1);
const MAX_SNAPSHOT_CREATE_REVISION: i64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutDocumentRequest {
    pub document_id: DocumentId,
    pub path: String,
    pub source: String,
    pub expected_revision: Option<i64>,
    pub lease_proofs: BTreeMap<note_org::WorkItemId, LeaseProofInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentImport {
    pub document_id: DocumentId,
    pub path: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDocumentsRequest {
    pub documents: Vec<DocumentImport>,
    pub expected_revisions: BTreeMap<DocumentId, i64>,
    pub lease_proofs: BTreeMap<note_org::WorkItemId, LeaseProofInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceImportMode {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshotMetadata {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWorkspaceSnapshotRequest {
    pub mode: WorkspaceImportMode,
    pub workspace: WorkspaceSnapshotMetadata,
    pub documents: Vec<DocumentImport>,
    pub document_revisions: BTreeMap<DocumentId, i64>,
    pub lease_proofs: BTreeMap<note_org::WorkItemId, LeaseProofInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveDocumentRequest {
    pub document_id: DocumentId,
    pub target_workspace_id: WorkspaceId,
    pub expected_document_revision: i64,
    pub expected_source_workspace_revision: i64,
    pub expected_target_workspace_revision: i64,
    pub lease_proofs: BTreeMap<note_org::WorkItemId, LeaseProofInput>,
}

pub async fn put_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &PutDocumentRequest,
) -> Result<OrgCommandResult, OrgError> {
    put_document_with_mode(context, envelope, request, DocumentImportMode::Online).await
}

/// Imports one canonical Org document without consulting Markdown note storage.
/// Offline snapshots are portable independently of the note database; their
/// agent-note links remain weak references until resolved by a live service.
pub async fn import_offline_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &PutDocumentRequest,
) -> Result<OrgCommandResult, OrgError> {
    put_document_with_mode(context, envelope, request, DocumentImportMode::Offline).await
}

async fn put_document_with_mode(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &PutDocumentRequest,
    mode: DocumentImportMode,
) -> Result<OrgCommandResult, OrgError> {
    let mut expected_revisions = BTreeMap::new();
    if let Some(revision) = request.expected_revision {
        expected_revisions.insert(request.document_id, revision);
    }
    let batch = ImportDocumentsRequest {
        documents: vec![DocumentImport {
            document_id: request.document_id,
            path: request.path.clone(),
            source: request.source.clone(),
        }],
        expected_revisions,
        lease_proofs: request.lease_proofs.clone(),
    };
    let command_kind = match mode {
        DocumentImportMode::Online => PUT_DOCUMENT,
        DocumentImportMode::Offline => IMPORT_OFFLINE_DOCUMENT,
    };
    execute_import(context, envelope, command_kind, batch, mode).await
}

pub async fn import_documents(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportDocumentsRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_import(
        context,
        envelope,
        IMPORT_DOCUMENTS,
        request.clone(),
        DocumentImportMode::Online,
    )
    .await
}

/// Restores a complete portable workspace snapshot through one transaction.
/// This adapter intentionally reuses the normal raw-import preparation and
/// lease/revision guards instead of implementing offline-specific policy.
pub async fn import_workspace_snapshot(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportWorkspaceSnapshotRequest,
) -> Result<OrgCommandResult, OrgError> {
    import_workspace_snapshot_with_mode(context, envelope, request, DocumentImportMode::Online)
        .await
}

/// Restores a portable Org snapshot without reading or writing Markdown notes.
pub async fn import_offline_workspace_snapshot(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportWorkspaceSnapshotRequest,
) -> Result<OrgCommandResult, OrgError> {
    import_workspace_snapshot_with_mode(context, envelope, request, DocumentImportMode::Offline)
        .await
}

async fn import_workspace_snapshot_with_mode(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportWorkspaceSnapshotRequest,
    import_mode: DocumentImportMode,
) -> Result<OrgCommandResult, OrgError> {
    super::validate_workspace_fields(
        &request.workspace.slug,
        &request.workspace.display_name,
        &request.workspace.timezone,
        request.workspace.policy_schema_version,
        &request.workspace.policy,
    )?;
    if request.workspace.revision < 1 {
        return Err(OrgError::invalid_input(
            "Snapshot workspace revision must be positive",
        ));
    }
    let documents = ImportDocumentsRequest {
        documents: request.documents.clone(),
        expected_revisions: match request.mode {
            WorkspaceImportMode::Create => BTreeMap::new(),
            WorkspaceImportMode::Update => request.document_revisions.clone(),
        },
        lease_proofs: request.lease_proofs.clone(),
    };
    validate_import_shape_inner(&documents, true)?;
    let document_ids = request
        .documents
        .iter()
        .map(|document| document.document_id)
        .collect::<BTreeSet<_>>();
    if request
        .document_revisions
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != document_ids
        || request
            .document_revisions
            .values()
            .any(|revision| *revision < 1)
    {
        return Err(OrgError::invalid_input(
            "Snapshot document revisions must exactly match every document",
        ));
    }
    if request.mode == WorkspaceImportMode::Update && request.workspace.archived_at.is_some() {
        return Err(OrgError::new(
            OrgErrorCode::ArchivedWorkspace,
            "Archived Org workspace snapshots can only be restored in create mode",
            json!({"workspace_id": envelope.workspace_id}),
            false,
        ));
    }
    if request.workspace.archived_at.is_some() && request.workspace.revision < 2 {
        return Err(OrgError::invalid_input(
            "Archived snapshot workspace revision must be at least two",
        ));
    }
    if request.mode == WorkspaceImportMode::Create
        && (request.workspace.revision > MAX_SNAPSHOT_CREATE_REVISION
            || request
                .document_revisions
                .values()
                .any(|revision| *revision > MAX_SNAPSHOT_CREATE_REVISION))
    {
        return Err(OrgError::invalid_input(
            "Snapshot create revision exceeds the supported restoration limit",
        ));
    }
    validate_import_public_payload(
        &request.lease_proofs,
        &json!({
            "workspace": &request.workspace,
            "documents": &request.documents,
            "document_revisions": &request.document_revisions,
        }),
    )?;
    let mode = request.mode;
    let command_kind = match import_mode {
        DocumentImportMode::Online => IMPORT_WORKSPACE_SNAPSHOT,
        DocumentImportMode::Offline => IMPORT_OFFLINE_WORKSPACE_SNAPSHOT,
    };
    let request = request.clone();
    let fingerprint = workspace_snapshot_fingerprint(&request);
    match mode {
        WorkspaceImportMode::Create => {
            let command = envelope.clone();
            let workflow_context = context.clone();
            execute_idempotent_create(
                context,
                command_kind,
                envelope,
                &fingerprint,
                move |transaction, now| {
                    Box::pin(async move {
                        apply_workspace_snapshot(
                            transaction,
                            &workflow_context,
                            &command,
                            &request,
                            now,
                            import_mode,
                        )
                        .await
                    })
                },
            )
            .await
        }
        WorkspaceImportMode::Update => {
            let command = envelope.clone();
            let workflow_context = context.clone();
            execute_idempotent(
                context,
                command_kind,
                envelope,
                &fingerprint,
                move |transaction, now| {
                    Box::pin(async move {
                        apply_workspace_snapshot(
                            transaction,
                            &workflow_context,
                            &command,
                            &request,
                            now,
                            import_mode,
                        )
                        .await
                    })
                },
            )
            .await
        }
    }
}

async fn execute_import(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    command_kind: OrgCommandKind,
    request: ImportDocumentsRequest,
    import_mode: DocumentImportMode,
) -> Result<OrgCommandResult, OrgError> {
    validate_import_shape(&request)?;
    validate_import_public_payload(
        &request.lease_proofs,
        &json!({"documents": &request.documents}),
    )?;
    let fingerprint_request = import_fingerprint(&request);
    let command_envelope = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        command_kind,
        envelope,
        &fingerprint_request,
        move |transaction, now| {
            Box::pin(async move {
                let workspace = transaction
                    .get_org_workspace(command_envelope.workspace_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| not_found("workspace"))?;
                apply_document_import(
                    transaction,
                    &workflow_context,
                    &workspace,
                    &command_envelope,
                    &request,
                    now,
                    ApplyDocumentImportOptions {
                        event_ids: Vec::new(),
                        created_target_revisions: None,
                        import_mode,
                    },
                )
                .await
            })
        },
    )
    .await
}

fn validate_import_public_payload(
    lease_proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    public_payload: &serde_json::Value,
) -> Result<(), OrgError> {
    for proof in lease_proofs.values() {
        validate_lease_input(&proof.lease_id, &proof.fencing_token)?;
        validate_public_payload(&proof.fencing_token, public_payload)?;
    }
    Ok(())
}

async fn apply_workspace_snapshot(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportWorkspaceSnapshotRequest,
    now: i64,
    import_mode: DocumentImportMode,
) -> Result<OrgCommandResult, OrgError> {
    let current = transaction
        .get_org_workspace(envelope.workspace_id)
        .await
        .map_err(OrgError::storage)?;
    let event_ids = Vec::new();
    let mut workspace = match request.mode {
        WorkspaceImportMode::Create => {
            if current.is_some() {
                return Err(OrgError::new(
                    OrgErrorCode::InvalidTransition,
                    "Org workspace already exists",
                    json!({"workspace_id": envelope.workspace_id}),
                    false,
                ));
            }
            transaction
                .insert_org_workspace(NewOrgWorkspace {
                    id: envelope.workspace_id,
                    slug: &request.workspace.slug,
                    display_name: &request.workspace.display_name,
                    description: &request.workspace.description,
                    timezone: &request.workspace.timezone,
                    policy_schema_version: request.workspace.policy_schema_version,
                    policy: &request.workspace.policy,
                    now,
                })
                .await
                .map_err(OrgError::storage)?;
            let workspace = transaction
                .get_org_workspace(envelope.workspace_id)
                .await
                .map_err(OrgError::storage)?
                .ok_or_else(|| {
                    OrgError::new(
                        OrgErrorCode::StorageFailure,
                        "Inserted Org workspace could not be read",
                        json!({}),
                        false,
                    )
                })?;
            workspace
        }
        WorkspaceImportMode::Update => {
            let current = current.ok_or_else(|| not_found("workspace"))?;
            if current.revision != request.workspace.revision {
                return Err(OrgError::new(
                    OrgErrorCode::StaleRevision,
                    "Org workspace revision is stale",
                    json!({"current_revision": current.revision}),
                    true,
                ));
            }
            let current_items = transaction
                .list_org_workspace_projection(envelope.workspace_id)
                .await
                .map_err(OrgError::storage)?;
            super::validate_policy_role_compatibility(
                &current.policy,
                &request.workspace.policy,
                &current_items,
            )?;
            OrgWorkspace {
                id: current.id,
                slug: request.workspace.slug.clone(),
                display_name: request.workspace.display_name.clone(),
                description: request.workspace.description.clone(),
                timezone: request.workspace.timezone.clone(),
                policy_schema_version: request.workspace.policy_schema_version,
                policy: request.workspace.policy.clone(),
                revision: current.revision + 1,
                created_at: current.created_at,
                updated_at: now,
                archived_at: request.workspace.archived_at,
            }
        }
    };

    let existing_ids = transaction
        .list_org_documents(envelope.workspace_id)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .map(|document| document.id)
        .collect::<BTreeSet<_>>();
    let imported_ids = request
        .documents
        .iter()
        .map(|document| document.document_id)
        .collect::<BTreeSet<_>>();
    if request.mode == WorkspaceImportMode::Update && existing_ids != imported_ids {
        return Err(OrgError::invalid_input(
            "Workspace snapshot must include exactly every existing document",
        ));
    }
    let import = ImportDocumentsRequest {
        documents: request.documents.clone(),
        expected_revisions: match request.mode {
            WorkspaceImportMode::Create => BTreeMap::new(),
            WorkspaceImportMode::Update => request.document_revisions.clone(),
        },
        lease_proofs: request.lease_proofs.clone(),
    };
    let mut result = apply_document_import(
        transaction,
        context,
        &workspace,
        envelope,
        &import,
        now,
        ApplyDocumentImportOptions {
            event_ids,
            created_target_revisions: (request.mode == WorkspaceImportMode::Create)
                .then_some(&request.document_revisions),
            import_mode,
        },
    )
    .await?;

    let (event_type, summary) = match request.mode {
        WorkspaceImportMode::Create => {
            workspace = restore_created_workspace_revision(
                transaction,
                &workspace,
                &request.workspace,
                now,
            )
            .await?;
            (
                OrgEventType::Creation,
                "Created Org workspace from snapshot",
            )
        }
        WorkspaceImportMode::Update => {
            workspace = super::resolve_cas(
                transaction
                    .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                        id: envelope.workspace_id,
                        expected_revision: request.workspace.revision,
                        slug: &request.workspace.slug,
                        display_name: &request.workspace.display_name,
                        description: &request.workspace.description,
                        timezone: &request.workspace.timezone,
                        policy_schema_version: request.workspace.policy_schema_version,
                        policy: &request.workspace.policy,
                        archived_at: request.workspace.archived_at,
                        updated_at: now,
                    })
                    .await
                    .map_err(OrgError::storage)?,
                "workspace",
            )?;
            context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
            if request.workspace.archived_at.is_some() {
                (
                    OrgEventType::WorkspaceArchive,
                    "Imported archived Org workspace snapshot",
                )
            } else {
                (
                    OrgEventType::WorkspaceChange,
                    "Updated Org workspace from snapshot",
                )
            }
        }
    };
    result.event_ids.push(
        append_workspace_snapshot_event(
            transaction,
            envelope,
            &workspace,
            event_type,
            summary,
            now,
        )
        .await?,
    );
    result.workspace_revision = Some(workspace.revision);
    Ok(result)
}

async fn restore_created_workspace_revision(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    snapshot: &WorkspaceSnapshotMetadata,
    now: i64,
) -> Result<OrgWorkspace, OrgError> {
    let mut restored = workspace.clone();
    while restored.revision < snapshot.revision {
        let next_revision = restored.revision + 1;
        restored = super::resolve_cas(
            transaction
                .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                    id: restored.id,
                    expected_revision: restored.revision,
                    slug: &snapshot.slug,
                    display_name: &snapshot.display_name,
                    description: &snapshot.description,
                    timezone: &snapshot.timezone,
                    policy_schema_version: snapshot.policy_schema_version,
                    policy: &snapshot.policy,
                    archived_at: (next_revision == snapshot.revision)
                        .then_some(snapshot.archived_at)
                        .flatten(),
                    updated_at: now,
                })
                .await
                .map_err(OrgError::storage)?,
            "workspace",
        )?;
    }
    Ok(restored)
}

async fn append_workspace_snapshot_event(
    transaction: &dyn StorageTransaction,
    envelope: &CommandEnvelope,
    workspace: &OrgWorkspace,
    event_type: OrgEventType,
    summary: &'static str,
    now: i64,
) -> Result<String, OrgError> {
    let metadata = json!({
        "revision": workspace.revision,
        "slug": workspace.slug,
        "timezone": workspace.timezone,
        "policy_schema_version": workspace.policy_schema_version,
        "archived_at": workspace.archived_at,
    });
    transaction
        .append_org_event(NewOrgEvent {
            id: &uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace.id,
            subject_kind: "workspace",
            subject_id: &workspace.id.to_string(),
            actor_id: &envelope.actor_id,
            attempt_id: None,
            event_type,
            occurred_at: now,
            summary,
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .map(|event| event.id)
        .map_err(OrgError::storage)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentImportMode {
    Online,
    Offline,
}

struct ApplyDocumentImportOptions<'a> {
    event_ids: Vec<String>,
    created_target_revisions: Option<&'a BTreeMap<DocumentId, i64>>,
    import_mode: DocumentImportMode,
}

async fn apply_document_import(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    workspace: &OrgWorkspace,
    envelope: &CommandEnvelope,
    request: &ImportDocumentsRequest,
    now: i64,
    options: ApplyDocumentImportOptions<'_>,
) -> Result<OrgCommandResult, OrgError> {
    let prepared = prepare_import(
        transaction,
        workspace,
        &envelope.actor_id,
        request,
        now,
        options.import_mode,
    )
    .await?;
    let mut event_ids = options.event_ids;
    context.after_workflow_phase(OrgWorkflowPhase::Proof)?;
    validate_prepared_revisions(&prepared, &request.expected_revisions)?;
    apply_prepared_ownership_changes(
        transaction,
        context,
        workspace,
        &envelope.actor_id,
        &prepared,
        &request.lease_proofs,
        now,
    )
    .await?;
    let mut revisions = BTreeMap::new();
    for document in &prepared.documents {
        let revision = if let Some(existing) = &document.existing {
            super::resolve_cas(
                transaction
                    .compare_and_swap_org_document(OrgDocumentUpdate {
                        id: document.input.document_id,
                        expected_revision: existing.revision,
                        path: &document.input.path,
                        source: &document.input.source,
                        content_hash: &content_hash(&document.input.source),
                        updated_at: now,
                    })
                    .await
                    .map_err(OrgError::storage)?,
                "document",
            )?
            .revision
        } else {
            transaction
                .insert_org_document(NewOrgDocument {
                    id: document.input.document_id,
                    workspace_id: envelope.workspace_id,
                    path: &document.input.path,
                    source: &document.input.source,
                    content_hash: &content_hash(&document.input.source),
                    now,
                })
                .await
                .map_err(OrgError::storage)?;
            let mut revision = 1;
            let target_revision = options
                .created_target_revisions
                .and_then(|revisions| revisions.get(&document.input.document_id))
                .copied()
                .unwrap_or(1);
            while revision < target_revision {
                revision = super::resolve_cas(
                    transaction
                        .compare_and_swap_org_document(OrgDocumentUpdate {
                            id: document.input.document_id,
                            expected_revision: revision,
                            path: &document.input.path,
                            source: &document.input.source,
                            content_hash: &content_hash(&document.input.source),
                            updated_at: now,
                        })
                        .await
                        .map_err(OrgError::storage)?,
                    "document",
                )?
                .revision;
            }
            revision
        };
        revisions.insert(document.input.document_id.to_string(), revision);
        context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
    }
    transaction
        .rebuild_org_workspace_projection(envelope.workspace_id, &prepared.workspace_projection)
        .await
        .map_err(OrgError::storage)?;
    context.after_workflow_phase(OrgWorkflowPhase::ProjectionUpdate)?;

    for semantic in &prepared.semantic_events {
        let event = transaction
            .append_org_event(NewOrgEvent {
                id: &uuid::Uuid::new_v4().to_string(),
                workspace_id: workspace.id,
                subject_kind: "work_item",
                subject_id: &semantic.item_id.to_string(),
                actor_id: &envelope.actor_id,
                attempt_id: semantic.attempt_id.as_deref(),
                event_type: semantic.event_type.clone(),
                occurred_at: now,
                summary: semantic.summary,
                metadata: &semantic.metadata,
                previous_state: semantic.previous_state.as_deref(),
                resulting_state: semantic.resulting_state.as_deref(),
            })
            .await
            .map_err(OrgError::storage)?;
        event_ids.push(event.id);
        context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    }
    for document in &prepared.documents {
        let metadata = json!({
            "document_id": document.input.document_id,
            "path": document.input.path,
            "revision": revisions[&document.input.document_id.to_string()],
            "created": document.existing.is_none(),
        });
        let event = transaction
            .append_org_event(NewOrgEvent {
                id: &uuid::Uuid::new_v4().to_string(),
                workspace_id: envelope.workspace_id,
                subject_kind: "document",
                subject_id: &document.input.document_id.to_string(),
                actor_id: &envelope.actor_id,
                attempt_id: None,
                event_type: if document.existing.is_some() {
                    OrgEventType::DocumentImport
                } else {
                    OrgEventType::Creation
                },
                occurred_at: now,
                summary: "Imported Org document",
                metadata: &metadata,
                previous_state: None,
                resulting_state: None,
            })
            .await
            .map_err(OrgError::storage)?;
        event_ids.push(event.id);
        context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    }
    for moved_item in &prepared.moved_items {
        let metadata = json!({
            "source_document_id": moved_item.source_document_id,
            "target_document_id": moved_item.target_document_id,
            "source_parent_id": moved_item.source_parent_id,
            "target_parent_id": moved_item.target_parent_id,
        });
        let event = transaction
            .append_org_event(NewOrgEvent {
                id: &uuid::Uuid::new_v4().to_string(),
                workspace_id: envelope.workspace_id,
                subject_kind: "work_item",
                subject_id: &moved_item.item_id.to_string(),
                actor_id: &envelope.actor_id,
                attempt_id: moved_item.attempt_id.as_deref(),
                event_type: OrgEventType::ItemMove,
                occurred_at: now,
                summary: "Moved Org work item between documents",
                metadata: &metadata,
                previous_state: None,
                resulting_state: None,
            })
            .await
            .map_err(OrgError::storage)?;
        event_ids.push(event.id);
        context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    }
    Ok(OrgCommandResult {
        schema_version: ORG_COMMAND_SCHEMA_VERSION,
        workspace_id: envelope.workspace_id,
        operation_id: envelope.operation_id.clone(),
        event_ids,
        workspace_revision: Some(workspace.revision),
        document_revisions: revisions,
        data: json!({"document_count": prepared.documents.len()}),
    })
}

pub async fn move_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &MoveDocumentRequest,
) -> Result<OrgCommandResult, OrgError> {
    if request.target_workspace_id == envelope.workspace_id
        || request.expected_document_revision < 1
        || request.expected_source_workspace_revision < 1
        || request.expected_target_workspace_revision < 1
    {
        return Err(OrgError::invalid_input(
            "Document move requires distinct workspaces and positive expected revisions",
        ));
    }
    let fingerprint = move_fingerprint(request);
    let request = request.clone();
    let command_envelope = envelope.clone();
    let workflow_context = context.clone();
    execute_idempotent(
        context,
        MOVE_DOCUMENT,
        envelope,
        &fingerprint,
        move |transaction, now| {
            Box::pin(async move {
                let source_workspace = require_workspace(transaction, command_envelope.workspace_id).await?;
                let target_workspace = require_workspace(transaction, request.target_workspace_id).await?;
                if target_workspace.archived_at.is_some() {
                    return Err(OrgError::new(
                        OrgErrorCode::ArchivedWorkspace,
                        "Archived target Org workspaces are read-only",
                        json!({"workspace_id": request.target_workspace_id}),
                        false,
                    ));
                }
                let document = transaction
                    .get_org_document(request.document_id)
                    .await
                    .map_err(OrgError::storage)?
                    .ok_or_else(|| not_found("document"))?;
                if document.workspace_id != command_envelope.workspace_id {
                    return Err(not_found("document"));
                }
                if document.archived_at.is_some() {
                    return Err(OrgError::new(
                        OrgErrorCode::ArchivedDocument,
                        "Archived Org documents are read-only",
                        json!({"document_id": document.id}),
                        false,
                    ));
                }
                let current_projection = transaction
                    .list_org_document_projection(document.id)
                    .await
                    .map_err(OrgError::storage)?;
                let source_projection = projection_for_documents(
                    transaction,
                    &source_workspace,
                    Some(document.id),
                    None,
                    &BTreeMap::new(),
                    now,
                )
                .await?;
                let target_projection = projection_for_documents(
                    transaction,
                    &target_workspace,
                    None,
                    Some(&document),
                    &current_projection
                        .iter()
                        .map(|item| (item.id, item.created_at))
                        .collect(),
                    now,
                )
                .await?;
                super::validate_projected_items_for_policy(
                    &source_projection,
                    &source_workspace.policy,
                )?;
                super::validate_projected_items_for_policy(
                    &target_projection,
                    &target_workspace.policy,
                )?;
                super::validate_policy_role_compatibility(
                    &source_workspace.policy,
                    &target_workspace.policy,
                    &current_projection,
                )?;
                let target_by_id = target_projection
                    .iter()
                    .map(|item| (item.id, item))
                    .collect::<BTreeMap<_, _>>();
                for current in &current_projection {
                    let Some(candidate) = target_by_id.get(&current.id) else {
                        return Err(OrgError::new(
                            OrgErrorCode::InvalidInput,
                            "Target policy omitted an item contained by the moved document",
                            json!({"work_item_id": current.id}),
                            false,
                        ));
                    };
                    if !same_raw_import_semantics(current, candidate) {
                        return Err(OrgError::new(
                            OrgErrorCode::InvalidInput,
                            "Target policy would reinterpret moved Org work-item semantics",
                            json!({"work_item_id": current.id}),
                            false,
                        ));
                    }
                }

                let deferred = current_projection
                    .iter()
                    .map(|item| item.id)
                    .collect::<BTreeSet<_>>();
                let guard = validate_touched_lease_proofs_except(
                    transaction,
                    &source_workspace,
                    &command_envelope.actor_id,
                    &current_projection,
                    &request.lease_proofs,
                    &deferred,
                    now,
                )
                .await?;
                validate_active_move_target_compatibility(
                    transaction,
                    &target_workspace,
                    &target_by_id,
                    &guard.active,
                )
                .await?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::Proof)?;
                let applied = transaction
                    .compare_and_swap_org_document_ownership(OrgDocumentOwnershipMove {
                        document_id: document.id,
                        source_workspace_id: source_workspace.id,
                        target_workspace_id: target_workspace.id,
                        expected_document_revision: request.expected_document_revision,
                        expected_source_workspace_revision: request.expected_source_workspace_revision,
                        expected_target_workspace_revision: request.expected_target_workspace_revision,
                        updated_at: now,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                let moved = match applied {
                    OrgDocumentOwnershipMoveResult::Applied(value) => value,
                    OrgDocumentOwnershipMoveResult::NotFound => return Err(not_found("document")),
                    OrgDocumentOwnershipMoveResult::Conflict {
                        current_document_workspace_id,
                        current_document_revision,
                        current_source_workspace_revision,
                        current_target_workspace_revision,
                    } => {
                        return Err(OrgError::new(
                            OrgErrorCode::StaleRevision,
                            "Org document ownership move revisions are stale",
                            json!({
                                "current_document_workspace_id": current_document_workspace_id,
                                "current_document_revision": current_document_revision,
                                "current_source_workspace_revision": current_source_workspace_revision,
                                "current_target_workspace_revision": current_target_workspace_revision,
                            }),
                            true,
                        ));
                    }
                };
                workflow_context.after_workflow_phase(OrgWorkflowPhase::SourceEdit)?;
                transaction
                    .rebuild_org_workspace_projection(source_workspace.id, &source_projection)
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ProjectionUpdate)?;
                transaction
                    .rebuild_org_workspace_projection(target_workspace.id, &target_projection)
                    .await
                    .map_err(OrgError::storage)?;
                workflow_context.after_workflow_phase(OrgWorkflowPhase::ProjectionUpdate)?;
                for (item_id, lease) in &guard.active {
                    let proof = &request.lease_proofs[item_id];
                    let digest = token_hash(&proof.fencing_token);
                    let moved_lease = transaction
                        .move_org_lease_ownership(OrgLeaseOwnershipMove {
                            proof: OrgLeaseProof {
                                lease_id: &proof.lease_id,
                                workspace_id: source_workspace.id,
                                work_item_id: *item_id,
                                fencing_token_hash: &digest,
                                kind: storage_kind(proof.kind),
                                actor_id: &command_envelope.actor_id,
                                now,
                            },
                            target_workspace_id: target_workspace.id,
                            target_capacity: capacity(&target_workspace)?,
                        })
                        .await
                        .map_err(OrgError::storage)?;
                    match moved_lease {
                        ConditionalUpdate::Applied(moved)
                            if moved.id == lease.id
                                && moved.workspace_id == target_workspace.id => {}
                        ConditionalUpdate::Conflict => {
                            return Err(concurrency_limit(target_workspace.id));
                        }
                        ConditionalUpdate::Applied(_) | ConditionalUpdate::NotFound => {
                            return Err(stale_lease())
                        }
                    }
                    workflow_context.after_workflow_phase(OrgWorkflowPhase::LeaseUpdate)?;
                }

                let mut event_ids = Vec::new();
                append_move_event_pair(
                    transaction,
                    &workflow_context,
                    &command_envelope,
                    target_workspace.id,
                    "document",
                    &document.id.to_string(),
                    None,
                    now,
                    &mut event_ids,
                )
                .await?;
                for item in &current_projection {
                    append_move_event_pair(
                        transaction,
                        &workflow_context,
                        &command_envelope,
                        target_workspace.id,
                        "work_item",
                        &item.id.to_string(),
                        guard
                            .active
                            .get(&item.id)
                            .map(|lease| lease.attempt_id.as_str()),
                        now,
                        &mut event_ids,
                    )
                    .await?;
                }
                Ok(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: command_envelope.workspace_id,
                    operation_id: command_envelope.operation_id,
                    event_ids,
                    workspace_revision: Some(moved.source_workspace_revision),
                    document_revisions: BTreeMap::from([(
                        moved.document.id.to_string(),
                        moved.document.revision,
                    )]),
                    data: json!({
                        "document_id": moved.document.id,
                        "source_workspace_id": source_workspace.id,
                        "target_workspace_id": target_workspace.id,
                        "source_workspace_revision": moved.source_workspace_revision,
                        "target_workspace_revision": moved.target_workspace_revision,
                    }),
                })
            })
        },
    )
    .await
}

async fn validate_active_move_target_compatibility(
    transaction: &dyn StorageTransaction,
    target: &OrgWorkspace,
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
    active: &BTreeMap<note_org::WorkItemId, note_storage::SanitizedOrgLease>,
) -> Result<(), OrgError> {
    for (item_id, lease) in active {
        let item = candidates[item_id];
        let required_state = match lease.kind {
            OrgLeaseKind::Execution => target.policy.running_state.as_str(),
            OrgLeaseKind::Review => target.policy.review_state.as_str(),
        };
        if item.state.as_deref() != Some(required_state) {
            return Err(OrgError::new(
                OrgErrorCode::InvalidInput,
                "Target workspace policy is incompatible with the active lease state",
                json!({"work_item_id": item_id, "required_state": required_state}),
                false,
            ));
        }
        let actor_allowed = match target.policy.claim_policy {
            ClaimPolicy::Open => true,
            ClaimPolicy::AssignmentRestricted => item
                .assignee
                .as_deref()
                .is_none_or(|assignee| assignee == lease.actor_id),
            ClaimPolicy::ExplicitlyDispatched => {
                item.assignee.as_deref() == Some(lease.actor_id.as_str())
            }
        };
        if !actor_allowed {
            return Err(OrgError::new(
                OrgErrorCode::InvalidInput,
                "Target workspace assignment policy rejects the active lease actor",
                json!({"work_item_id": item_id}),
                false,
            ));
        }
        let attempts = transaction
            .list_org_attempts(*item_id)
            .await
            .map_err(OrgError::storage)?;
        if u32::try_from(attempts.len()).unwrap_or(u32::MAX) > target.policy.max_attempts() {
            return Err(OrgError::new(
                OrgErrorCode::RetryLimit,
                "Target workspace retry budget is lower than existing attempt history",
                json!({"work_item_id": item_id}),
                false,
            ));
        }
        let expected_status = match lease.kind {
            OrgLeaseKind::Execution => note_storage::OrgAttemptStatus::Running,
            OrgLeaseKind::Review => note_storage::OrgAttemptStatus::Submitted,
        };
        let valid_attempt = attempts.last().is_some_and(|attempt| {
            attempt.id == lease.attempt_id
                && attempt.work_item_id == *item_id
                && attempt.status == expected_status
                && attempt.ended_at.is_none()
        });
        if !valid_attempt {
            return Err(OrgError::new(
                OrgErrorCode::InvalidTransition,
                "Active lease attempt is incompatible with the target workspace",
                json!({"work_item_id": item_id}),
                false,
            ));
        }
    }
    Ok(())
}

struct PreparedDocument {
    input: DocumentImport,
    existing: Option<OrgDocument>,
    projection: Vec<OrgProjectedWorkItem>,
}

struct PreparedImport {
    documents: Vec<PreparedDocument>,
    workspace_projection: Vec<OrgProjectedWorkItem>,
    moved_items: Vec<PreparedItemMove>,
    semantic_events: Vec<PreparedSemanticEvent>,
    old_items: Vec<OrgProjectedWorkItem>,
    active_leases: BTreeMap<note_org::WorkItemId, note_storage::SanitizedOrgLease>,
    expired_leases: Vec<(OrgProjectedWorkItem, note_storage::OrgLease)>,
}

struct PreparedItemMove {
    item_id: note_org::WorkItemId,
    source_document_id: DocumentId,
    target_document_id: DocumentId,
    source_parent_id: Option<note_org::WorkItemId>,
    target_parent_id: Option<note_org::WorkItemId>,
    attempt_id: Option<String>,
}

struct PreparedSemanticEvent {
    item_id: note_org::WorkItemId,
    attempt_id: Option<String>,
    event_type: OrgEventType,
    summary: &'static str,
    metadata: serde_json::Value,
    previous_state: Option<String>,
    resulting_state: Option<String>,
}

async fn prepare_import(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    actor_id: &str,
    request: &ImportDocumentsRequest,
    now: i64,
    import_mode: DocumentImportMode,
) -> Result<PreparedImport, OrgError> {
    let old_workspace_projection = transaction
        .list_org_workspace_projection(workspace.id)
        .await
        .map_err(OrgError::storage)?;
    let created_at = old_workspace_projection
        .iter()
        .map(|item| (item.id, item.created_at))
        .collect::<BTreeMap<_, _>>();
    let affected_ids = request
        .documents
        .iter()
        .map(|document| document.document_id)
        .collect::<BTreeSet<_>>();
    let mut documents = Vec::new();
    let mut candidate_ids = BTreeSet::new();
    for input in &request.documents {
        let existing = transaction
            .get_org_document(input.document_id)
            .await
            .map_err(OrgError::storage)?;
        match &existing {
            Some(existing) => {
                if existing.workspace_id != workspace.id {
                    return Err(OrgError::new(
                        OrgErrorCode::InvalidTransition,
                        "Org document belongs to a different workspace",
                        json!({"document_id": input.document_id}),
                        false,
                    ));
                }
                if existing.archived_at.is_some() {
                    return Err(OrgError::new(
                        OrgErrorCode::ArchivedDocument,
                        "Archived Org documents are read-only",
                        json!({"document_id": existing.id}),
                        false,
                    ));
                }
                let Some(_expected) = request.expected_revisions.get(&input.document_id) else {
                    return Err(OrgError::invalid_input(
                        "Every existing document in an import requires an expected revision",
                    ));
                };
            }
            None if request.expected_revisions.contains_key(&input.document_id) => {
                return Err(not_found("document"));
            }
            None => {}
        }
        let parsed = parse_document(input.source.clone(), &workspace.policy.parse_options())
            .map_err(parse_error)?;
        let projection = super::project_document(
            &parsed,
            workspace.id,
            input.document_id,
            &workspace.timezone,
            &created_at,
            now,
        )?;
        for item in &projection {
            if !candidate_ids.insert(item.id) {
                return Err(OrgError::new(
                    OrgErrorCode::InvalidInput,
                    "Org import contains a duplicate work-item ID",
                    json!({"work_item_id": item.id}),
                    false,
                ));
            }
        }
        documents.push(PreparedDocument {
            input: input.clone(),
            existing,
            projection,
        });
    }
    let expected_ids = documents
        .iter()
        .filter(|document| document.existing.is_some())
        .map(|document| document.input.document_id)
        .collect::<BTreeSet<_>>();
    if request
        .expected_revisions
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != expected_ids
    {
        return Err(OrgError::invalid_input(
            "Import expected revisions must exactly match every existing affected document",
        ));
    }

    let affected_old = old_workspace_projection
        .iter()
        .filter(|item| affected_ids.contains(&item.document_id))
        .collect::<Vec<_>>();
    for old in &affected_old {
        if !candidate_ids.contains(&old.id) {
            return Err(OrgError::new(
                OrgErrorCode::UnsupportedSemanticEdit,
                "Raw Org import cannot implicitly delete a work item",
                json!({"work_item_id": old.id}),
                false,
            ));
        }
    }
    let unaffected_ids = old_workspace_projection
        .iter()
        .filter(|item| !affected_ids.contains(&item.document_id))
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    if let Some(duplicate) = candidate_ids.intersection(&unaffected_ids).next() {
        return Err(OrgError::new(
            OrgErrorCode::InvalidInput,
            "Org work-item ID is already owned by an unaffected document",
            json!({"work_item_id": duplicate}),
            false,
        ));
    }

    let candidates_by_id = documents
        .iter()
        .flat_map(|document| document.projection.iter())
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    validate_supported_raw_changes(&affected_old, &candidates_by_id)?;
    let mut workspace_projection = old_workspace_projection
        .iter()
        .filter(|item| !affected_ids.contains(&item.document_id))
        .cloned()
        .collect::<Vec<_>>();
    workspace_projection.extend(
        documents
            .iter()
            .flat_map(|document| document.projection.iter().cloned()),
    );
    super::validate_projected_items_for_policy(&workspace_projection, &workspace.policy)?;
    validate_raw_state_changes(
        workspace,
        &old_workspace_projection,
        &affected_old,
        &candidates_by_id,
    )?;
    if import_mode == DocumentImportMode::Online {
        validate_new_note_targets(
            transaction,
            &old_workspace_projection,
            &workspace_projection,
        )
        .await?;
    }

    let mut touched = affected_old
        .iter()
        .filter(|old| !same_raw_import_semantics(old, candidates_by_id[&old.id]))
        .map(|item| (*item).clone())
        .collect::<Vec<_>>();
    touched.extend(
        affected_old
            .iter()
            .filter(|old| !same_raw_import_semantics(old, candidates_by_id[&old.id]))
            .filter_map(|old| candidates_by_id.get(&old.id).copied().cloned()),
    );
    let deferred = raw_ownership_closing_candidates(&affected_old, &candidates_by_id, workspace);
    let guard = inspect_touched_lease_proofs_except(
        transaction,
        workspace,
        actor_id,
        &touched,
        &request.lease_proofs,
        &deferred,
        now,
    )
    .await?;
    let semantic_events = raw_semantic_events(
        &workspace.policy,
        &affected_old,
        &candidates_by_id,
        &guard.active,
    );
    let moved_items = affected_old
        .iter()
        .filter_map(|old| {
            let candidate = candidates_by_id.get(&old.id)?;
            (old.document_id != candidate.document_id || old.parent_id != candidate.parent_id)
                .then_some(PreparedItemMove {
                    item_id: old.id,
                    source_document_id: old.document_id,
                    target_document_id: candidate.document_id,
                    source_parent_id: old.parent_id,
                    target_parent_id: candidate.parent_id,
                    attempt_id: guard
                        .active
                        .get(&old.id)
                        .map(|lease| lease.attempt_id.clone()),
                })
        })
        .collect();
    Ok(PreparedImport {
        documents,
        workspace_projection,
        moved_items,
        semantic_events,
        old_items: affected_old.into_iter().cloned().collect(),
        active_leases: guard.active,
        expired_leases: guard.expired,
    })
}

async fn apply_prepared_ownership_changes(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    workspace: &OrgWorkspace,
    actor_id: &str,
    prepared: &PreparedImport,
    proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    now: i64,
) -> Result<bool, OrgError> {
    let mut updated = false;
    for (item, lease) in &prepared.expired_leases {
        super::bookkeep_expired_lease_with_phases(
            transaction,
            workspace,
            item,
            lease,
            now,
            |phase| context.after_workflow_phase(phase),
        )
        .await?;
        updated = true;
    }
    let candidates = prepared
        .workspace_projection
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    let old_items = prepared.old_items.iter().collect::<Vec<_>>();
    updated |= apply_raw_ownership_changes(
        transaction,
        context,
        workspace,
        actor_id,
        &old_items,
        &candidates,
        &prepared.active_leases,
        proofs,
        now,
    )
    .await?;
    Ok(updated)
}

fn validate_supported_raw_changes(
    old_items: &[&OrgProjectedWorkItem],
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
) -> Result<(), OrgError> {
    for old in old_items {
        let candidate = candidates[&old.id];
        if old.item_type != candidate.item_type
            || old.tags != candidate.tags
            || old.requires_review != candidate.requires_review
        {
            return Err(OrgError::new(
                OrgErrorCode::UnsupportedSemanticEdit,
                "Raw Org import cannot change work-item type, tags, or review requirement",
                json!({"work_item_id": old.id}),
                false,
            ));
        }
    }
    Ok(())
}

fn validate_raw_state_changes(
    workspace: &OrgWorkspace,
    old_projection: &[OrgProjectedWorkItem],
    old_items: &[&OrgProjectedWorkItem],
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
) -> Result<(), OrgError> {
    for old in old_items {
        let candidate = candidates[&old.id];
        if old.state == candidate.state {
            continue;
        }
        let from = old
            .state
            .as_deref()
            .ok_or_else(|| OrgError::invalid_input("Org work item has no workflow state"))?;
        let target = candidate
            .state
            .as_deref()
            .ok_or_else(|| OrgError::invalid_input("Org work item has no workflow state"))?;
        let lifecycle = classify_transition(&workspace.policy, from, target);
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
                json!({"work_item_id": old.id}),
                false,
            ));
        }
        validate_item_transition(
            &workspace.policy,
            old,
            old_projection,
            target,
            lifecycle == TransitionLifecycle::Approval,
        )?;
    }
    Ok(())
}

fn raw_ownership_closing_candidates(
    old_items: &[&OrgProjectedWorkItem],
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
    workspace: &OrgWorkspace,
) -> BTreeSet<note_org::WorkItemId> {
    old_items
        .iter()
        .filter_map(|old| {
            let candidate = candidates[&old.id];
            let state_closes = match (old.state.as_deref(), candidate.state.as_deref()) {
                (Some(from), Some(target)) if from != target => {
                    closes_lease(classify_transition(&workspace.policy, from, target))
                }
                _ => false,
            };
            (state_closes || old.assignee != candidate.assignee).then_some(old.id)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn apply_raw_ownership_changes(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    workspace: &OrgWorkspace,
    actor_id: &str,
    old_items: &[&OrgProjectedWorkItem],
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
    active: &BTreeMap<note_org::WorkItemId, note_storage::SanitizedOrgLease>,
    proofs: &BTreeMap<note_org::WorkItemId, LeaseProofInput>,
    now: i64,
) -> Result<bool, OrgError> {
    let old_by_id = old_items
        .iter()
        .map(|item| (item.id, *item))
        .collect::<BTreeMap<_, _>>();
    let mut updated = false;
    for (item_id, lease) in active {
        let old = old_by_id[item_id];
        let candidate = candidates[item_id];
        let lifecycle = match (old.state.as_deref(), candidate.state.as_deref()) {
            (Some(from), Some(target)) if from != target => {
                Some(classify_transition(&workspace.policy, from, target))
            }
            _ => None,
        };
        let assignment_changed = old.assignee != candidate.assignee;
        let ownership = if assignment_changed {
            let recovery_state = match lease.kind {
                OrgLeaseKind::Execution => &workspace.policy.release_state,
                OrgLeaseKind::Review => &workspace.policy.review_rejection_state,
            };
            if candidate.state.as_deref() != Some(recovery_state.as_str()) {
                return Err(OrgError::new(
                    OrgErrorCode::InvalidTransition,
                    "Active reassignment through raw import requires its configured recovery state",
                    json!({
                        "work_item_id": item_id,
                        "required_state": recovery_state,
                    }),
                    false,
                ));
            }
            Some((
                OrgLeaseEndReason::Reassignment,
                match lease.kind {
                    OrgLeaseKind::Execution => note_storage::OrgAttemptStatus::Running,
                    OrgLeaseKind::Review => note_storage::OrgAttemptStatus::Submitted,
                },
                note_storage::OrgAttemptStatus::Cancelled,
                (lease.kind == OrgLeaseKind::Review).then_some("reassigned"),
            ))
        } else if lifecycle.is_some_and(closes_lease) {
            let lifecycle = lifecycle.expect("closing lifecycle exists");
            let (expected, status, review_outcome) = lifecycle_attempt(
                lifecycle,
                old.state.as_deref() == Some(workspace.policy.review_state.as_str()),
            );
            Some((
                lifecycle_end_reason(lifecycle),
                expected,
                status,
                review_outcome,
            ))
        } else {
            None
        };
        let Some((end_reason, expected, status, review_outcome)) = ownership else {
            continue;
        };
        updated = true;

        let proof = proofs.get(item_id).ok_or_else(stale_lease)?;
        validate_lease_input(&proof.lease_id, &proof.fencing_token)?;
        let digest = token_hash(&proof.fencing_token);
        let closed = resolve_lease_update(
            transaction
                .close_org_lease(OrgLeaseClosure {
                    proof: OrgLeaseProof {
                        lease_id: &proof.lease_id,
                        workspace_id: workspace.id,
                        work_item_id: *item_id,
                        fencing_token_hash: &digest,
                        kind: storage_kind(proof.kind),
                        actor_id,
                        now,
                    },
                    ended_at: now,
                    end_reason,
                })
                .await
                .map_err(OrgError::storage)?,
        )?;
        if closed.id != lease.id {
            return Err(stale_lease());
        }
        context.after_workflow_phase(OrgWorkflowPhase::LeaseUpdate)?;
        let attempt = transaction
            .get_org_attempt(&lease.attempt_id)
            .await
            .map_err(OrgError::storage)?
            .ok_or_else(|| not_found("attempt"))?;
        resolve_update(
            transaction
                .update_org_attempt(note_storage::OrgAttemptUpdate {
                    id: &attempt.id,
                    expected_status: expected,
                    status,
                    ended_at: now,
                    error: attempt.error.as_deref(),
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
        context.after_workflow_phase(OrgWorkflowPhase::AttemptUpdate)?;
    }
    Ok(updated)
}

fn raw_semantic_events(
    policy: &note_org::WorkspacePolicy,
    old_items: &[&OrgProjectedWorkItem],
    candidates: &BTreeMap<note_org::WorkItemId, &OrgProjectedWorkItem>,
    active: &BTreeMap<note_org::WorkItemId, note_storage::SanitizedOrgLease>,
) -> Vec<PreparedSemanticEvent> {
    let mut events = Vec::new();
    for old in old_items {
        let candidate = candidates[&old.id];
        let attempt_id = active.get(&old.id).map(|lease| lease.attempt_id.clone());
        let mut push = |event_type, summary, metadata| {
            events.push(PreparedSemanticEvent {
                item_id: old.id,
                attempt_id: attempt_id.clone(),
                event_type,
                summary,
                metadata,
                previous_state: old.state.clone(),
                resulting_state: candidate.state.clone(),
            });
        };
        if old.state != candidate.state {
            if let (Some(from), Some(target)) = (old.state.as_deref(), candidate.state.as_deref()) {
                let lifecycle = classify_transition(policy, from, target);
                let assignment_recovery = active.get(&old.id).is_some_and(|lease| {
                    old.assignee != candidate.assignee
                        && target
                            == match lease.kind {
                                OrgLeaseKind::Execution => policy.release_state.as_str(),
                                OrgLeaseKind::Review => policy.review_rejection_state.as_str(),
                            }
                });
                if !assignment_recovery {
                    for event_type in lifecycle_events(lifecycle) {
                        push(
                            event_type.clone(),
                            lifecycle_summary(event_type),
                            json!({
                                "schema_version": 1,
                                "error": null,
                                "context": {},
                            }),
                        );
                    }
                }
            }
        }
        if old.title != candidate.title {
            push(
                OrgEventType::TitleChange,
                "Changed Org work-item title through raw import",
                json!({"previous_title": old.title, "resulting_title": candidate.title}),
            );
        }
        if old.priority != candidate.priority {
            push(
                OrgEventType::PriorityChange,
                "Changed Org work-item priority through raw import",
                json!({
                    "previous_priority": old.priority,
                    "resulting_priority": candidate.priority,
                }),
            );
        }
        if !same_local_timestamp(old.scheduled.as_ref(), candidate.scheduled.as_ref()) {
            push(
                OrgEventType::ScheduleChange,
                "Changed Org work-item schedule through raw import",
                json!({
                    "previous_scheduled": old.scheduled.as_ref().map(|value| &value.raw),
                    "resulting_scheduled": candidate.scheduled.as_ref().map(|value| &value.raw),
                }),
            );
        }
        if !same_local_timestamp(old.deadline.as_ref(), candidate.deadline.as_ref()) {
            push(
                OrgEventType::DeadlineChange,
                "Changed Org work-item deadline through raw import",
                json!({
                    "previous_deadline": old.deadline.as_ref().map(|value| &value.raw),
                    "resulting_deadline": candidate.deadline.as_ref().map(|value| &value.raw),
                }),
            );
        }
        if old.assignee != candidate.assignee {
            push(
                OrgEventType::Assignment,
                "Changed Org work-item assignment through raw import",
                json!({
                    "previous_assignee": old.assignee,
                    "resulting_assignee": candidate.assignee,
                }),
            );
        }
        if old.dependencies != candidate.dependencies {
            push(
                OrgEventType::DependencyChange,
                "Changed Org work-item dependencies through raw import",
                json!({
                    "previous_dependencies": old.dependencies,
                    "resulting_dependencies": candidate.dependencies,
                }),
            );
        }
        if old.note_links != candidate.note_links {
            push(
                OrgEventType::NoteLinkChange,
                "Changed Org work-item note links through raw import",
                json!({
                    "previous_note_links": old.note_links,
                    "resulting_note_links": candidate.note_links,
                }),
            );
        }
    }
    events
}

fn validate_prepared_revisions(
    prepared: &PreparedImport,
    expected_revisions: &BTreeMap<DocumentId, i64>,
) -> Result<(), OrgError> {
    for document in &prepared.documents {
        let Some(existing) = &document.existing else {
            continue;
        };
        if existing.revision != expected_revisions[&existing.id] {
            return Err(OrgError::new(
                OrgErrorCode::StaleRevision,
                "Org document revision is stale",
                json!({
                    "document_id": existing.id,
                    "current_revision": existing.revision,
                }),
                true,
            ));
        }
    }
    Ok(())
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
    let mut checked = BTreeSet::new();
    for item in candidate {
        for link in &item.note_links {
            let key = (item.id, link.purpose.clone(), link.note_id);
            if old_links.contains(&key) || !checked.insert(link.note_id) {
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

fn same_raw_import_semantics(old: &OrgProjectedWorkItem, candidate: &OrgProjectedWorkItem) -> bool {
    old.id == candidate.id
        && old.document_id == candidate.document_id
        && old.parent_id == candidate.parent_id
        && old.item_type == candidate.item_type
        && old.title == candidate.title
        && old.state == candidate.state
        && old.priority == candidate.priority
        && same_local_timestamp(old.scheduled.as_ref(), candidate.scheduled.as_ref())
        && same_local_timestamp(old.deadline.as_ref(), candidate.deadline.as_ref())
        && old.assignee == candidate.assignee
        && old.requires_review == candidate.requires_review
        && old.tags == candidate.tags
        && old.dependencies == candidate.dependencies
        && old.note_links == candidate.note_links
}

fn same_local_timestamp(
    old: Option<&note_storage::StoredOrgTimestamp>,
    candidate: Option<&note_storage::StoredOrgTimestamp>,
) -> bool {
    match (old, candidate) {
        (None, None) => true,
        (Some(old), Some(candidate)) => old.raw == candidate.raw && old.local == candidate.local,
        _ => false,
    }
}

async fn projection_for_documents(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    excluded_document: Option<DocumentId>,
    added_document: Option<&OrgDocument>,
    additional_created_at: &BTreeMap<note_org::WorkItemId, i64>,
    now: i64,
) -> Result<Vec<OrgProjectedWorkItem>, OrgError> {
    let old = transaction
        .list_org_workspace_projection(workspace.id)
        .await
        .map_err(OrgError::storage)?;
    let mut created_at = old
        .iter()
        .map(|item| (item.id, item.created_at))
        .collect::<BTreeMap<_, _>>();
    created_at.extend(additional_created_at);
    let mut result = old
        .into_iter()
        .filter(|item| Some(item.document_id) != excluded_document)
        .collect::<Vec<_>>();
    if let Some(document) = added_document {
        let parsed = parse_document(document.source.clone(), &workspace.policy.parse_options())
            .map_err(parse_error)?;
        result.extend(super::project_document(
            &parsed,
            workspace.id,
            document.id,
            &workspace.timezone,
            &created_at,
            now,
        )?);
    }
    Ok(result)
}

async fn append_move_event_pair(
    transaction: &dyn StorageTransaction,
    context: &OrgContext,
    envelope: &CommandEnvelope,
    target_workspace_id: WorkspaceId,
    subject_kind: &str,
    subject_id: &str,
    attempt_id: Option<&str>,
    now: i64,
    event_ids: &mut Vec<String>,
) -> Result<(), OrgError> {
    let source_metadata = json!({
        "source_workspace_id": envelope.workspace_id,
        "target_workspace_id": target_workspace_id,
        "direction": "out",
    });
    let source = transaction
        .append_org_event(NewOrgEvent {
            id: &uuid::Uuid::new_v4().to_string(),
            workspace_id: envelope.workspace_id,
            subject_kind,
            subject_id,
            actor_id: &envelope.actor_id,
            attempt_id,
            event_type: if subject_kind == "document" {
                OrgEventType::DocumentMove
            } else {
                OrgEventType::ItemMove
            },
            occurred_at: now,
            summary: if subject_kind == "document" {
                "Moved Org document out of workspace"
            } else {
                "Moved Org work item out of workspace"
            },
            metadata: &source_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .map_err(OrgError::storage)?;
    context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    let target_metadata = json!({
        "source_workspace_id": envelope.workspace_id,
        "target_workspace_id": target_workspace_id,
        "direction": "in",
        "lineage_previous_event_id": source.id,
    });
    let target = transaction
        .append_org_event(NewOrgEvent {
            id: &uuid::Uuid::new_v4().to_string(),
            workspace_id: target_workspace_id,
            subject_kind,
            subject_id,
            actor_id: &envelope.actor_id,
            // Attempts keep their workspace-at-start. The source-side lineage
            // event can reference it; the target-side event is linked through
            // metadata because storage requires event/attempt workspace parity.
            attempt_id: None,
            event_type: if subject_kind == "document" {
                OrgEventType::DocumentMove
            } else {
                OrgEventType::ItemMove
            },
            occurred_at: now,
            summary: if subject_kind == "document" {
                "Moved Org document into workspace"
            } else {
                "Moved Org work item into workspace"
            },
            metadata: &target_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .map_err(OrgError::storage)?;
    context.after_workflow_phase(OrgWorkflowPhase::Events)?;
    event_ids.extend([source.id, target.id]);
    Ok(())
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

fn validate_import_shape(request: &ImportDocumentsRequest) -> Result<(), OrgError> {
    validate_import_shape_inner(request, false)
}

fn validate_import_shape_inner(
    request: &ImportDocumentsRequest,
    allow_empty: bool,
) -> Result<(), OrgError> {
    if request.documents.is_empty() && !allow_empty {
        return Err(OrgError::invalid_input(
            "Org document import must contain at least one document",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for document in &request.documents {
        validate_document_path(&document.path).map_err(|_| {
            OrgError::new(
                OrgErrorCode::InvalidInput,
                "Org document path must be a portable relative lowercase .org path",
                json!({"field": "path", "path": document.path}),
                false,
            )
        })?;
        if !ids.insert(document.document_id) || !paths.insert(document.path.clone()) {
            return Err(OrgError::invalid_input(
                "Org import document IDs and paths must be unique",
            ));
        }
    }
    if request
        .expected_revisions
        .values()
        .any(|revision| *revision < 1)
    {
        return Err(OrgError::invalid_input(
            "Expected Org document revisions must be positive",
        ));
    }
    Ok(())
}

fn content_hash(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

fn import_fingerprint(request: &ImportDocumentsRequest) -> serde_json::Value {
    json!({
        "documents": request.documents,
        "expected_revisions": request.expected_revisions,
        "lease_proofs": request.lease_proofs.iter().map(|(item_id, proof)| {
            (item_id.to_string(), json!({
                "lease_id": proof.lease_id,
                "kind": proof.kind,
                "fencing_token_digest": super::token_hash(&proof.fencing_token),
            }))
        }).collect::<BTreeMap<_, _>>(),
    })
}

fn workspace_snapshot_fingerprint(request: &ImportWorkspaceSnapshotRequest) -> serde_json::Value {
    let documents = ImportDocumentsRequest {
        documents: request.documents.clone(),
        expected_revisions: request.document_revisions.clone(),
        lease_proofs: request.lease_proofs.clone(),
    };
    json!({
        "mode": request.mode,
        "workspace": request.workspace,
        "documents": import_fingerprint(&documents),
    })
}

fn move_fingerprint(request: &MoveDocumentRequest) -> serde_json::Value {
    json!({
        "document_id": request.document_id,
        "target_workspace_id": request.target_workspace_id,
        "expected_document_revision": request.expected_document_revision,
        "expected_source_workspace_revision": request.expected_source_workspace_revision,
        "expected_target_workspace_revision": request.expected_target_workspace_revision,
        "lease_proofs": request.lease_proofs.iter().map(|(item_id, proof)| {
            (item_id.to_string(), json!({
                "lease_id": proof.lease_id,
                "kind": proof.kind,
                "fencing_token_digest": token_hash(&proof.fencing_token),
            }))
        }).collect::<BTreeMap<_, _>>(),
    })
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
