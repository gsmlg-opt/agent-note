use super::{
    execute_idempotent, CommandEnvelope, ConservativeLeaseGuard, LeaseGuard, LeaseGuardOperation,
    OrgCommandKind, OrgCommandResult, OrgContext, OrgError, OrgErrorCode,
    ORG_COMMAND_SCHEMA_VERSION,
};
use note_org::{parse_document, DocumentId, WorkspaceId};
use note_storage::{
    NewOrgDocument, NewOrgEvent, OrgDocument, OrgDocumentOwnershipMove,
    OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEventType, OrgProjectedWorkItem,
    OrgWorkspace, StorageTransaction,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const PUT_DOCUMENT: OrgCommandKind = OrgCommandKind::new("put_document", 1);
const IMPORT_DOCUMENTS: OrgCommandKind = OrgCommandKind::new("import_documents", 1);
const MOVE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("move_document", 1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutDocumentRequest {
    pub document_id: DocumentId,
    pub path: String,
    pub source: String,
    pub expected_revision: Option<i64>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveDocumentRequest {
    pub document_id: DocumentId,
    pub target_workspace_id: WorkspaceId,
    pub expected_document_revision: i64,
    pub expected_source_workspace_revision: i64,
    pub expected_target_workspace_revision: i64,
}

pub async fn put_document(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &PutDocumentRequest,
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
    };
    execute_import(context, envelope, PUT_DOCUMENT, batch).await
}

pub async fn import_documents(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    request: &ImportDocumentsRequest,
) -> Result<OrgCommandResult, OrgError> {
    execute_import(context, envelope, IMPORT_DOCUMENTS, request.clone()).await
}

async fn execute_import(
    context: &OrgContext,
    envelope: &CommandEnvelope,
    command_kind: OrgCommandKind,
    request: ImportDocumentsRequest,
) -> Result<OrgCommandResult, OrgError> {
    validate_import_shape(&request)?;
    let fingerprint_request = request.clone();
    let command_envelope = envelope.clone();
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
                let prepared = prepare_import(transaction, &workspace, &request, now).await?;
                let mut revisions = BTreeMap::new();
                for document in &prepared.documents {
                    let revision = if let Some(existing) = &document.existing {
                        let updated = super::resolve_cas(
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
                        )?;
                        updated.revision
                    } else {
                        transaction
                            .insert_org_document(NewOrgDocument {
                                id: document.input.document_id,
                                workspace_id: command_envelope.workspace_id,
                                path: &document.input.path,
                                source: &document.input.source,
                                content_hash: &content_hash(&document.input.source),
                                now,
                            })
                            .await
                            .map_err(OrgError::storage)?;
                        1
                    };
                    revisions.insert(document.input.document_id.to_string(), revision);
                }
                transaction
                    .rebuild_org_workspace_projection(
                        command_envelope.workspace_id,
                        &prepared.workspace_projection,
                    )
                    .await
                    .map_err(OrgError::storage)?;

                let mut event_ids = Vec::new();
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
                            workspace_id: command_envelope.workspace_id,
                            subject_kind: "document",
                            subject_id: &document.input.document_id.to_string(),
                            actor_id: &command_envelope.actor_id,
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
                }
                for moved_item in &prepared.moved_items {
                    let metadata = json!({
                        "source_document_id": moved_item.source_document_id,
                        "target_document_id": moved_item.target_document_id,
                    });
                    let event = transaction
                        .append_org_event(NewOrgEvent {
                            id: &uuid::Uuid::new_v4().to_string(),
                            workspace_id: command_envelope.workspace_id,
                            subject_kind: "work_item",
                            subject_id: &moved_item.item_id.to_string(),
                            actor_id: &command_envelope.actor_id,
                            attempt_id: None,
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
                }
                Ok(OrgCommandResult {
                    schema_version: ORG_COMMAND_SCHEMA_VERSION,
                    workspace_id: command_envelope.workspace_id,
                    operation_id: command_envelope.operation_id,
                    event_ids,
                    workspace_revision: Some(workspace.revision),
                    document_revisions: revisions,
                    data: json!({"document_count": prepared.documents.len()}),
                })
            })
        },
    )
    .await
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
    let request = request.clone();
    let command_envelope = envelope.clone();
    execute_idempotent(
        context,
        MOVE_DOCUMENT,
        envelope,
        &request,
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
                let current_projection = transaction
                    .list_org_document_projection(document.id)
                    .await
                    .map_err(OrgError::storage)?;
                ConservativeLeaseGuard.validate(
                    LeaseGuardOperation::DocumentMove,
                    &source_workspace.policy,
                    &current_projection,
                )?;

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
                transaction
                    .rebuild_org_workspace_projection(source_workspace.id, &source_projection)
                    .await
                    .map_err(OrgError::storage)?;
                transaction
                    .rebuild_org_workspace_projection(target_workspace.id, &target_projection)
                    .await
                    .map_err(OrgError::storage)?;

                let mut event_ids = Vec::new();
                append_move_event_pair(
                    transaction,
                    &command_envelope,
                    target_workspace.id,
                    "document",
                    &document.id.to_string(),
                    now,
                    &mut event_ids,
                )
                .await?;
                for item in &current_projection {
                    append_move_event_pair(
                        transaction,
                        &command_envelope,
                        target_workspace.id,
                        "work_item",
                        &item.id.to_string(),
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

struct PreparedDocument {
    input: DocumentImport,
    existing: Option<OrgDocument>,
    projection: Vec<OrgProjectedWorkItem>,
}

struct PreparedImport {
    documents: Vec<PreparedDocument>,
    workspace_projection: Vec<OrgProjectedWorkItem>,
    moved_items: Vec<PreparedItemMove>,
}

struct PreparedItemMove {
    item_id: note_org::WorkItemId,
    source_document_id: DocumentId,
    target_document_id: DocumentId,
}

async fn prepare_import(
    transaction: &dyn StorageTransaction,
    workspace: &OrgWorkspace,
    request: &ImportDocumentsRequest,
    now: i64,
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
                let Some(expected) = request.expected_revisions.get(&input.document_id) else {
                    return Err(OrgError::invalid_input(
                        "Every existing document in an import requires an expected revision",
                    ));
                };
                if *expected != existing.revision {
                    return Err(OrgError::new(
                        OrgErrorCode::StaleRevision,
                        "Org document revision is stale",
                        json!({
                            "document_id": input.document_id,
                            "current_revision": existing.revision,
                        }),
                        true,
                    ));
                }
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
    for old in &affected_old {
        let candidate = candidates_by_id[&old.id];
        if !same_raw_import_semantics(old, candidate) {
            return Err(OrgError::new(
                OrgErrorCode::UnsupportedSemanticEdit,
                "Raw Org replacement contains an ownership-sensitive semantic change",
                json!({"work_item_id": old.id}),
                false,
            ));
        }
    }
    let mut touched = affected_old
        .iter()
        .map(|item| (*item).clone())
        .collect::<Vec<_>>();
    touched.extend(
        affected_old
            .iter()
            .filter_map(|old| candidates_by_id.get(&old.id).copied().cloned()),
    );
    ConservativeLeaseGuard.validate(LeaseGuardOperation::RawImport, &workspace.policy, &touched)?;

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
    validate_new_note_targets(
        transaction,
        &old_workspace_projection,
        &workspace_projection,
    )
    .await?;
    let moved_items = affected_old
        .iter()
        .filter_map(|old| {
            let candidate = candidates_by_id.get(&old.id)?;
            (old.document_id != candidate.document_id).then_some(PreparedItemMove {
                item_id: old.id,
                source_document_id: old.document_id,
                target_document_id: candidate.document_id,
            })
        })
        .collect();
    Ok(PreparedImport {
        documents,
        workspace_projection,
        moved_items,
    })
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
            if !transaction
                .note_exists(&link.note_id.to_string())
                .await
                .map_err(OrgError::storage)?
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
    envelope: &CommandEnvelope,
    target_workspace_id: WorkspaceId,
    subject_kind: &str,
    subject_id: &str,
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
            attempt_id: None,
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
    if request.documents.is_empty() {
        return Err(OrgError::invalid_input(
            "Org document import must contain at least one document",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for document in &request.documents {
        if document.path.trim().is_empty() || document.path != document.path.trim() {
            return Err(OrgError::invalid_input(
                "Org document path must be nonblank and trimmed",
            ));
        }
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

fn parse_error(error: note_org::OrgError) -> OrgError {
    OrgError::new(
        OrgErrorCode::InvalidInput,
        "Org document is invalid",
        json!({"reason": error.to_string()}),
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
