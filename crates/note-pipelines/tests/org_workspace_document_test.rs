mod support;

use note_org::{ClaimPolicy, DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    archive_document, archive_workspace, claim_item, create_document, create_workspace,
    export_document_by_id, export_workspace, get_document, import_documents,
    import_offline_document, import_offline_workspace_snapshot, import_workspace_snapshot,
    list_documents, move_document, put_document, release_claim, rename_document, restore_document,
    schedule_item, update_workspace, ArchiveWorkspaceRequest, CommandEnvelope,
    CreateDocumentRequest, CreateWorkspaceRequest, DocumentImport, DocumentLifecycleData,
    DocumentRevisionRequest, DocumentStatus, ImportDocumentsRequest,
    ImportWorkspaceSnapshotRequest, LeaseProofInput, MoveDocumentRequest, OrgClaimKind, OrgContext,
    OrgDocumentReadQuery, OrgErrorCode, OrgFieldPatch, OrgWorkflowPhase, PutDocumentRequest,
    ReleaseClaimRequest, RenameDocumentRequest, ScheduleItemRequest, StartClaimRequest,
    UpdateWorkspaceRequest, WorkspaceImportMode, WorkspaceSnapshotMetadata,
};
use note_storage::{
    NewNote, OrgAttemptStatus, OrgAttemptUpdate, OrgDocument, OrgEvent, OrgEventType,
    OrgProjectedWorkItem, OrgWorkspace, OrgWorkspaceUpdate, StorageBackend,
};
use note_storage_turso::TursoStorage;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{
    event_log, DeterministicTokenSource, EventStorageBackend, FailAtWorkflowPhaseOccurrence,
};

const NOW: i64 = 1_800_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceSnapshot {
    workspace: OrgWorkspace,
    documents: Vec<OrgDocument>,
    projection: Vec<OrgProjectedWorkItem>,
    events: Vec<OrgEvent>,
}

#[tokio::test]
async fn offline_document_import_accepts_note_links_without_touching_note_storage() {
    let (context, storage, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000099");
    create_test_workspace(&context, workspace, "offline-note-link-workspace", "UTC").await;
    let missing_note = "40000000-0000-4000-8000-000000000099";
    let document_id: DocumentId = "20000000-0000-4000-8000-000000000099".parse().unwrap();
    let request = PutDocumentRequest {
        document_id,
        path: "offline/note-links.org".into(),
        source: source(
            work_item_id("30000000-0000-4000-8000-000000000099"),
            "READY",
            &format!("[[agent-note:design:{missing_note}][Missing note]]\n"),
        ),
        expected_revision: None,
        lease_proofs: BTreeMap::new(),
    };

    let normal = put_document(
        &context,
        &envelope(workspace, "normal-note-link-import"),
        &request,
    )
    .await
    .unwrap_err();
    assert_eq!(normal.code, OrgErrorCode::NoteUnavailable);

    import_offline_document(
        &context,
        &envelope(workspace, "offline-note-link-import"),
        &request,
    )
    .await
    .unwrap();

    let session = storage.session().await.unwrap();
    assert!(session.list_all_notes().await.unwrap().is_empty());
    assert!(session
        .get_org_document(document_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn offline_workspace_snapshot_accepts_note_links_without_touching_note_storage() {
    let (context, storage, _dir) = empty_context(NOW).await;
    let workspace = "10000000-0000-4000-8000-000000000099".parse().unwrap();
    let document_id: DocumentId = "20000000-0000-4000-8000-000000000098".parse().unwrap();
    let missing_note = "40000000-0000-4000-8000-000000000098";
    let request = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "offline-snapshot-links".into(),
            display_name: "Offline snapshot links".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 1,
            archived_at: None,
        },
        documents: vec![DocumentImport {
            document_id,
            path: "snapshot/note-links.org".into(),
            source: source(
                work_item_id("30000000-0000-4000-8000-000000000098"),
                "READY",
                &format!("[[agent-note:design:{missing_note}][Missing note]]\n"),
            ),
            archived_at: None,
        }],
        document_revisions: BTreeMap::from([(document_id, 1)]),
        lease_proofs: BTreeMap::new(),
    };

    import_offline_workspace_snapshot(
        &context,
        &envelope(workspace, "offline-snapshot-note-links"),
        &request,
    )
    .await
    .unwrap();

    let session = storage.session().await.unwrap();
    assert!(session.list_all_notes().await.unwrap().is_empty());
    assert!(session
        .get_org_document(document_id)
        .await
        .unwrap()
        .is_some());
}

async fn workspace_snapshot(
    backend: &dyn StorageBackend,
    workspace_id: WorkspaceId,
) -> WorkspaceSnapshot {
    let session = backend.session().await.unwrap();
    WorkspaceSnapshot {
        workspace: session
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap(),
        documents: session.list_org_documents(workspace_id).await.unwrap(),
        projection: session
            .list_org_workspace_projection(workspace_id)
            .await
            .unwrap(),
        events: session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
    }
}

fn workspace_id(value: &str) -> WorkspaceId {
    WorkspaceId::from_str(value).unwrap()
}

fn document_id(value: &str) -> DocumentId {
    DocumentId::from_str(value).unwrap()
}

fn work_item_id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

fn envelope(workspace_id: WorkspaceId, operation_id: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id,
        actor_id: "agent-one".into(),
        operation_id: operation_id.into(),
    }
}

fn source(id: WorkItemId, state: &str, extra: &str) -> String {
    format!(
        "* {state} [#A] Ship parser :rust:\r\n:PROPERTIES:\r\n:ID: {id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n{extra}"
    )
}

fn source_with_metadata(
    id: WorkItemId,
    state: &str,
    planning: &str,
    properties: &str,
    body: &str,
) -> String {
    format!(
        "* {state} [#A] Ship parser :rust:\r\n{planning}:PROPERTIES:\r\n:ID: {id}\r\n:AGENT_NOTE_TYPE: task\r\n{properties}:END:\r\n{body}"
    )
}

async fn empty_context(now: i64) -> (OrgContext, Arc<dyn StorageBackend>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("org-task5.db"))
            .await
            .unwrap(),
    );
    (
        OrgContext::new(
            backend.clone(),
            Arc::new(note_pipelines::org::FixedOrgClock::new(now)),
        ),
        backend,
        dir,
    )
}

async fn create_test_workspace(
    context: &OrgContext,
    id: WorkspaceId,
    operation_id: &str,
    timezone: &str,
) {
    create_workspace(
        context,
        &envelope(id, operation_id),
        &CreateWorkspaceRequest {
            slug: format!("workspace-{id}"),
            display_name: "Engineering".into(),
            description: "Task 5 workspace".into(),
            timezone: timezone.into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn document_reads_filter_status_before_paging_and_bind_cursors_to_status() {
    let (context, _backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000205");
    create_test_workspace(&context, workspace, "status-read-workspace", "UTC").await;
    let documents = [
        document_id("20000000-0000-4000-8000-000000000211"),
        document_id("20000000-0000-4000-8000-000000000212"),
        document_id("20000000-0000-4000-8000-000000000213"),
        document_id("20000000-0000-4000-8000-000000000214"),
    ];
    for (index, document) in documents.iter().enumerate() {
        create_document(
            &context,
            &envelope(workspace, &format!("create-status-document-{index}")),
            &CreateDocumentRequest {
                document_id: *document,
                path: format!("status/{index}.org"),
            },
        )
        .await
        .unwrap();
    }
    for (index, document) in [documents[1], documents[3]].into_iter().enumerate() {
        archive_document(
            &context,
            &envelope(workspace, &format!("archive-status-document-{index}")),
            &DocumentRevisionRequest {
                document_id: document,
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
    }

    let mut archived_query = OrgDocumentReadQuery {
        cursor: None,
        limit: Some(1),
        status: DocumentStatus::Archived,
    };
    let first_archived = list_documents(&context, workspace, &archived_query)
        .await
        .unwrap();
    assert_eq!(first_archived.items.len(), 1);
    assert_eq!(first_archived.items[0].id, documents[1]);
    assert_eq!(first_archived.items[0].path, "status/1.org");
    assert_eq!(first_archived.items[0].revision, 2);
    assert_eq!(first_archived.items[0].archived_at, Some(NOW));
    assert!(first_archived.next_cursor.is_some());
    archived_query.cursor = first_archived.next_cursor.clone();
    let second_archived = list_documents(&context, workspace, &archived_query)
        .await
        .unwrap();
    assert_eq!(second_archived.items.len(), 1);
    assert_eq!(second_archived.items[0].id, documents[3]);
    assert_eq!(second_archived.items[0].archived_at, Some(NOW));
    assert!(second_archived.next_cursor.is_none());

    let mut active_query = OrgDocumentReadQuery {
        cursor: None,
        limit: Some(1),
        status: DocumentStatus::Active,
    };
    let first_active = list_documents(&context, workspace, &active_query)
        .await
        .unwrap();
    assert_eq!(first_active.items[0].id, documents[0]);
    assert_eq!(first_active.items[0].archived_at, None);
    let active_cursor = first_active.next_cursor.clone().unwrap();
    active_query.cursor = Some(active_cursor.clone());
    assert_eq!(
        list_documents(&context, workspace, &active_query)
            .await
            .unwrap()
            .items[0]
            .id,
        documents[2]
    );

    for status in [DocumentStatus::Archived, DocumentStatus::All] {
        let error = list_documents(
            &context,
            workspace,
            &OrgDocumentReadQuery {
                cursor: Some(active_cursor.clone()),
                limit: Some(1),
                status,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
    }

    let all = list_documents(
        &context,
        workspace,
        &OrgDocumentReadQuery {
            cursor: None,
            limit: None,
            status: DocumentStatus::All,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        all.items
            .iter()
            .map(|document| document.id)
            .collect::<Vec<_>>(),
        documents
    );
}

#[tokio::test]
async fn archived_workspace_keeps_status_aware_document_reads_and_exports_inclusive() {
    let (context, _backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000206");
    let active_document = document_id("20000000-0000-4000-8000-000000000215");
    let archived_document = document_id("20000000-0000-4000-8000-000000000216");
    create_test_workspace(&context, workspace, "archived-workspace-reads", "UTC").await;
    for (operation, document, path) in [
        (
            "create-active-read-document",
            active_document,
            "read/active.org",
        ),
        (
            "create-archived-read-document",
            archived_document,
            "read/archived.org",
        ),
    ] {
        create_document(
            &context,
            &envelope(workspace, operation),
            &CreateDocumentRequest {
                document_id: document,
                path: path.into(),
            },
        )
        .await
        .unwrap();
    }
    archive_document(
        &context,
        &envelope(workspace, "archive-read-document"),
        &DocumentRevisionRequest {
            document_id: archived_document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    archive_workspace(
        &context,
        &envelope(workspace, "archive-read-workspace"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap();

    let default_page = list_documents(&context, workspace, &OrgDocumentReadQuery::default())
        .await
        .unwrap();
    assert_eq!(default_page.items.len(), 1);
    assert_eq!(default_page.items[0].id, active_document);
    assert_eq!(default_page.items[0].archived_at, None);

    let archived_page = list_documents(
        &context,
        workspace,
        &OrgDocumentReadQuery {
            status: DocumentStatus::Archived,
            ..OrgDocumentReadQuery::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(archived_page.items.len(), 1);
    assert_eq!(archived_page.items[0].id, archived_document);
    assert_eq!(archived_page.items[0].archived_at, Some(NOW));

    let direct = get_document(&context, workspace, archived_document)
        .await
        .unwrap();
    assert_eq!(direct.archived_at, Some(NOW));
    let export = export_workspace(&context, workspace).await.unwrap();
    assert_eq!(export.documents.len(), 2);
    assert_eq!(
        export
            .documents
            .iter()
            .find(|document| document.id == archived_document)
            .unwrap()
            .archived_at,
        Some(NOW)
    );
}

#[tokio::test]
async fn document_create_is_empty_idempotent_and_records_one_creation_event() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000201");
    let document = document_id("20000000-0000-4000-8000-000000000201");
    create_test_workspace(&context, workspace, "create-lifecycle-workspace", "UTC").await;
    let request = CreateDocumentRequest {
        document_id: document,
        path: "lifecycle/empty.org".into(),
    };
    let command = envelope(workspace, "create-empty-document");

    let created = create_document(&context, &command, &request).await.unwrap();
    let replay = create_document(&context, &command, &request).await.unwrap();
    assert_eq!(replay, created);
    assert_eq!(created.workspace_revision, None);
    assert_eq!(created.document_revisions[&document.to_string()], 1);
    assert_eq!(created.event_ids.len(), 1);
    assert_eq!(
        serde_json::from_value::<DocumentLifecycleData>(created.data.clone()).unwrap(),
        DocumentLifecycleData {
            document_id: document,
            path: "lifecycle/empty.org".into(),
            archived_at: None,
        }
    );

    let session = backend.session().await.unwrap();
    let stored = session.get_org_document(document).await.unwrap().unwrap();
    assert_eq!(stored.source, "");
    assert_eq!(
        stored.content_hash,
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.archived_at, None);
    assert!(session
        .list_org_document_projection(document)
        .await
        .unwrap()
        .is_empty());
    let events = session.list_org_events(workspace, None, 50).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.subject_kind == "document"
                && event.subject_id == document.to_string())
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .find(|event| event.id == created.event_ids[0])
            .unwrap()
            .event_type,
        OrgEventType::Creation
    );
    assert!(session
        .get_org_operation(workspace, "create-empty-document")
        .await
        .unwrap()
        .is_some());
    drop(session);

    let divergent = create_document(
        &context,
        &command,
        &CreateDocumentRequest {
            document_id: document,
            path: "lifecycle/divergent.org".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(divergent.code, OrgErrorCode::IdempotencyConflict);
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_org_events(workspace, None, 50)
            .await
            .unwrap(),
        events
    );
}

#[tokio::test]
async fn document_create_rejects_duplicate_identity_reserved_paths_and_invalid_paths_atomically() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000202");
    let first = document_id("20000000-0000-4000-8000-000000000202");
    let second = document_id("20000000-0000-4000-8000-000000000203");
    create_test_workspace(&context, workspace, "create-conflict-workspace", "UTC").await;
    create_document(
        &context,
        &envelope(workspace, "create-conflict-first"),
        &CreateDocumentRequest {
            document_id: first,
            path: "reserved.org".into(),
        },
    )
    .await
    .unwrap();
    archive_document(
        &context,
        &envelope(workspace, "archive-conflict-first"),
        &DocumentRevisionRequest {
            document_id: first,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    let before = workspace_snapshot(backend.as_ref(), workspace).await;

    let duplicate = create_document(
        &context,
        &envelope(workspace, "duplicate-document-id"),
        &CreateDocumentRequest {
            document_id: first,
            path: "different.org".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(duplicate.code, OrgErrorCode::InvalidTransition);

    let reserved = create_document(
        &context,
        &envelope(workspace, "reserved-archived-path"),
        &CreateDocumentRequest {
            document_id: second,
            path: "reserved.org".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(reserved.code, OrgErrorCode::DocumentPathConflict);

    let invalid = create_document(
        &context,
        &envelope(workspace, "invalid-document-path"),
        &CreateDocumentRequest {
            document_id: second,
            path: "../Invalid.ORG".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.code, OrgErrorCode::InvalidInput);
    assert_eq!(
        invalid.message,
        "Org document path must be a portable relative lowercase .org path"
    );
    assert_eq!(
        invalid.details,
        serde_json::json!({"field": "path", "path": "../Invalid.ORG"})
    );
    assert_eq!(
        workspace_snapshot(backend.as_ref(), workspace).await,
        before
    );
}

#[tokio::test]
async fn document_rename_preserves_payload_and_works_before_and_after_archive() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000203");
    let document = document_id("20000000-0000-4000-8000-000000000204");
    let replacement = document_id("20000000-0000-4000-8000-000000000205");
    create_test_workspace(&context, workspace, "rename-lifecycle-workspace", "UTC").await;
    create_document(
        &context,
        &envelope(workspace, "rename-create"),
        &CreateDocumentRequest {
            document_id: document,
            path: "rename/first.org".into(),
        },
    )
    .await
    .unwrap();
    let original = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();

    let active = rename_document(
        &context,
        &envelope(workspace, "rename-active"),
        &RenameDocumentRequest {
            document_id: document,
            new_path: "rename/active.org".into(),
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(active.document_revisions[&document.to_string()], 2);
    archive_document(
        &context,
        &envelope(workspace, "rename-archive"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 2,
        },
    )
    .await
    .unwrap();
    let archived = rename_document(
        &context,
        &envelope(workspace, "rename-archived"),
        &RenameDocumentRequest {
            document_id: document,
            new_path: "rename/archived.org".into(),
            expected_revision: 3,
        },
    )
    .await
    .unwrap();
    assert_eq!(archived.document_revisions[&document.to_string()], 4);
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.path, "rename/archived.org");
    assert_eq!(stored.source, original.source);
    assert_eq!(stored.content_hash, original.content_hash);
    assert_eq!(stored.created_at, original.created_at);
    assert_eq!(stored.archived_at, Some(NOW));

    create_document(
        &context,
        &envelope(workspace, "reuse-renamed-archived-path"),
        &CreateDocumentRequest {
            document_id: replacement,
            path: "rename/active.org".into(),
        },
    )
    .await
    .unwrap();
    let conflict = rename_document(
        &context,
        &envelope(workspace, "rename-reserved-target"),
        &RenameDocumentRequest {
            document_id: document,
            new_path: "rename/active.org".into(),
            expected_revision: 4,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, OrgErrorCode::DocumentPathConflict);
    let stale = rename_document(
        &context,
        &envelope(workspace, "rename-stale"),
        &RenameDocumentRequest {
            document_id: document,
            new_path: "rename/active.org".into(),
            expected_revision: 3,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleRevision);
}

#[tokio::test]
async fn document_archive_and_restore_preserve_content_and_replay_state_changes_once() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000204");
    let document = document_id("20000000-0000-4000-8000-000000000206");
    create_test_workspace(&context, workspace, "archive-restore-workspace", "UTC").await;
    create_document(
        &context,
        &envelope(workspace, "archive-restore-create"),
        &CreateDocumentRequest {
            document_id: document,
            path: "archive/restore.org".into(),
        },
    )
    .await
    .unwrap();
    let before = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    let archive_request = DocumentRevisionRequest {
        document_id: document,
        expected_revision: 1,
    };
    let archive_command = envelope(workspace, "archive-document");
    let archived = archive_document(&context, &archive_command, &archive_request)
        .await
        .unwrap();
    assert_eq!(
        archive_document(&context, &archive_command, &archive_request)
            .await
            .unwrap(),
        archived
    );
    let archived_record = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(archived_record.revision, 2);
    assert_eq!(archived_record.archived_at, Some(NOW));
    assert_eq!(archived_record.source, before.source);
    assert_eq!(archived_record.content_hash, before.content_hash);

    let stale_archive = archive_document(
        &context,
        &envelope(workspace, "archive-stale-wrong-state"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale_archive.code, OrgErrorCode::StaleRevision);
    assert_eq!(stale_archive.details["current_revision"], 2);

    let wrong_state = archive_document(
        &context,
        &envelope(workspace, "archive-wrong-state"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 2,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_state.code, OrgErrorCode::InvalidTransition);
    let stale_restore = restore_document(
        &context,
        &envelope(workspace, "restore-stale"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale_restore.code, OrgErrorCode::StaleRevision);

    let restored = restore_document(
        &context,
        &envelope(workspace, "restore-document"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(restored.document_revisions[&document.to_string()], 3);
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.archived_at, None);
    assert_eq!(stored.source, before.source);

    let stale_restore = restore_document(
        &context,
        &envelope(workspace, "restore-stale-wrong-state"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 2,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale_restore.code, OrgErrorCode::StaleRevision);
    assert_eq!(stale_restore.details["current_revision"], 3);
    let wrong_restore_state = restore_document(
        &context,
        &envelope(workspace, "restore-wrong-state"),
        &DocumentRevisionRequest {
            document_id: document,
            expected_revision: 3,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_restore_state.code, OrgErrorCode::InvalidTransition);

    let events = backend
        .session()
        .await
        .unwrap()
        .list_org_events(workspace, None, 50)
        .await
        .unwrap();
    let archive_events = events
        .iter()
        .filter(|event| event.event_type == OrgEventType::DocumentArchive)
        .collect::<Vec<_>>();
    assert_eq!(archive_events.len(), 1);
    assert_eq!(
        archive_events[0].metadata,
        serde_json::json!({
            "path": "archive/restore.org",
            "previous_archived_at": null,
            "resulting_archived_at": NOW,
            "previous_revision": 1,
            "resulting_revision": 2,
        })
    );
    let restore_events = events
        .iter()
        .filter(|event| event.event_type == OrgEventType::DocumentRestore)
        .collect::<Vec<_>>();
    assert_eq!(restore_events.len(), 1);
    assert_eq!(
        restore_events[0].metadata,
        serde_json::json!({
            "path": "archive/restore.org",
            "previous_archived_at": NOW,
            "resulting_archived_at": null,
            "previous_revision": 2,
            "resulting_revision": 3,
        })
    );
    for event in [archive_events[0], restore_events[0]] {
        let metadata = serde_json::to_string(&event.metadata).unwrap();
        for forbidden in ["source", "hash", "token", "lease"] {
            assert!(!metadata.contains(forbidden), "{forbidden}: {metadata}");
        }
    }
}

#[tokio::test]
async fn workspace_create_update_validates_timezone_and_policy_and_replays_once() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let id = workspace_id("10000000-0000-4000-8000-000000000001");
    let request = CreateWorkspaceRequest {
        slug: "engineering".into(),
        display_name: "Engineering".into(),
        description: "Initial".into(),
        timezone: "Asia/Shanghai".into(),
        policy_schema_version: 1,
        policy: WorkspacePolicy::engineering_default(),
    };

    let created = create_workspace(&context, &envelope(id, "create-workspace"), &request)
        .await
        .unwrap();
    let replay = create_workspace(&context, &envelope(id, "create-workspace"), &request)
        .await
        .unwrap();
    assert_eq!(created, replay);

    let session = backend.session().await.unwrap();
    let workspace = session.get_org_workspace(id).await.unwrap().unwrap();
    assert_eq!(workspace.revision, 1);
    assert_eq!(workspace.created_at, NOW);
    assert_eq!(workspace.timezone, "Asia/Shanghai");
    assert_eq!(
        session.list_org_events(id, None, 50).await.unwrap().len(),
        1
    );

    let update = UpdateWorkspaceRequest {
        expected_revision: 1,
        slug: "engineering".into(),
        display_name: "Platform Engineering".into(),
        description: "Updated".into(),
        timezone: "America/New_York".into(),
        policy_schema_version: 1,
        policy: WorkspacePolicy::engineering_default(),
    };
    let updated = update_workspace(&context, &envelope(id, "update-workspace"), &update)
        .await
        .unwrap();
    assert_eq!(updated.workspace_revision, Some(2));
    let workspace = session.get_org_workspace(id).await.unwrap().unwrap();
    assert_eq!(workspace.created_at, NOW);
    assert_eq!(workspace.updated_at, NOW);

    let before_unsupported_update = workspace.clone();
    let before_unsupported_events = session.list_org_events(id, None, 50).await.unwrap();
    let mut unsupported_update = update.clone();
    unsupported_update.expected_revision = workspace.revision;
    unsupported_update.policy_schema_version = 2;
    let error = update_workspace(
        &context,
        &envelope(id, "unsupported-policy-update"),
        &unsupported_update,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
    assert_eq!(
        session.get_org_workspace(id).await.unwrap().unwrap(),
        before_unsupported_update
    );
    assert_eq!(
        session.list_org_events(id, None, 50).await.unwrap(),
        before_unsupported_events
    );
    assert!(session
        .get_org_operation(id, "unsupported-policy-update")
        .await
        .unwrap()
        .is_none());

    for (operation, timezone) in [("bad-zone", "Mars/Olympus"), ("blank-zone", "")] {
        let mut invalid = request.clone();
        invalid.timezone = timezone.into();
        let error = create_workspace(
            &context,
            &envelope(
                workspace_id(if operation == "bad-zone" {
                    "10000000-0000-4000-8000-000000000002"
                } else {
                    "10000000-0000-4000-8000-000000000003"
                }),
                operation,
            ),
            &invalid,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
    }
    let unsupported_id = workspace_id("10000000-0000-4000-8000-000000000005");
    let mut unsupported_create = request.clone();
    unsupported_create.policy_schema_version = 2;
    let error = create_workspace(
        &context,
        &envelope(unsupported_id, "unsupported-policy-create"),
        &unsupported_create,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
    assert!(session
        .get_org_workspace(unsupported_id)
        .await
        .unwrap()
        .is_none());
    let mut invalid_policy = request;
    invalid_policy.policy.concurrency_limit = 0;
    let error = create_workspace(
        &context,
        &envelope(
            workspace_id("10000000-0000-4000-8000-000000000004"),
            "bad-policy",
        ),
        &invalid_policy,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
}

#[tokio::test]
async fn policy_update_rejects_existing_items_it_would_invalidate_without_side_effects() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000010");
    create_test_workspace(&context, workspace, "create-policy-workspace", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000010");
    let item = work_item_id("30000000-0000-4000-8000-000000000010");
    put_document(
        &context,
        &envelope(workspace, "seed-policy-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "tasks.org".into(),
            source: source(item, "DONE", "Body.\r\n"),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    put_document(
        &context,
        &envelope(workspace, "seed-policy-cancelled-document"),
        &PutDocumentRequest {
            document_id: document_id("20000000-0000-4000-8000-000000000011"),
            path: "cancelled.org".into(),
            source: source(
                work_item_id("30000000-0000-4000-8000-000000000011"),
                "CANCELLED",
                "Cancelled policy role.\r\n",
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    put_document(
        &context,
        &envelope(workspace, "seed-policy-ready-document"),
        &PutDocumentRequest {
            document_id: document_id("20000000-0000-4000-8000-000000000012"),
            path: "ready.org".into(),
            source: source(
                work_item_id("30000000-0000-4000-8000-000000000012"),
                "READY",
                "Recovery policy roles.\r\n",
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let before = session.get_org_workspace(workspace).await.unwrap().unwrap();
    let before_events = session.list_org_events(workspace, None, 50).await.unwrap();
    let baseline = workspace_snapshot(backend.as_ref(), workspace).await;
    let mut removes_type = before.policy.clone();
    removes_type
        .allowed_types
        .remove(&note_org::WorkItemType::Task);
    let mut removes_state = before.policy.clone();
    removes_state.states.remove("DONE");
    removes_state.successful_terminal_states.remove("DONE");
    removes_state.terminal_states.remove("DONE");
    removes_state
        .transitions
        .retain(|(from, to)| from != "DONE" && to != "DONE");
    let mut changes_transition_role = before.policy.clone();
    changes_transition_role
        .successful_terminal_states
        .remove("DONE");
    let mut rejects_tag = before.policy.clone();
    rejects_tag.tag_rules.insert(
        note_org::WorkItemType::Task,
        note_org::TagRule {
            allowed: ["go".to_string()].into_iter().collect(),
            required: Default::default(),
        },
    );
    let mut adds_review_requirement = before.policy.clone();
    adds_review_requirement
        .review_required_types
        .insert(note_org::WorkItemType::Task);
    let mut changes_cancelled_role = before.policy.clone();
    changes_cancelled_role.cancelled_state = "BACKLOG".into();
    let mut changes_release_role = before.policy.clone();
    changes_release_role.release_state = "BACKLOG".into();
    changes_release_role
        .executable_states
        .insert("BACKLOG".into());
    let mut changes_review_rejection_role = before.policy.clone();
    changes_review_rejection_role.review_rejection_state = "BACKLOG".into();
    changes_review_rejection_role
        .executable_states
        .insert("BACKLOG".into());
    let mut changes_lease_expiry_role = before.policy.clone();
    changes_lease_expiry_role.lease_expiry_recovery_state = "BACKLOG".into();
    changes_lease_expiry_role
        .executable_states
        .insert("BACKLOG".into());

    for (case, policy) in [
        ("type", removes_type),
        ("state", removes_state),
        ("successful-role", changes_transition_role),
        ("tag", rejects_tag),
        ("review", adds_review_requirement),
        ("cancelled-role", changes_cancelled_role),
        ("release-role", changes_release_role),
        ("review-rejection-role", changes_review_rejection_role),
        ("lease-expiry-role", changes_lease_expiry_role),
    ] {
        let operation_id = format!("invalidating-policy-{case}");
        let error = update_workspace(
            &context,
            &envelope(workspace, &operation_id),
            &UpdateWorkspaceRequest {
                expected_revision: before.revision,
                slug: before.slug.clone(),
                display_name: before.display_name.clone(),
                description: before.description.clone(),
                timezone: before.timezone.clone(),
                policy_schema_version: 1,
                policy,
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.code,
                OrgErrorCode::InvalidInput | OrgErrorCode::ReviewRequired
            ),
            "case {case} returned {error:?}"
        );
        assert_eq!(
            session.get_org_workspace(workspace).await.unwrap().unwrap(),
            before
        );
        assert_eq!(
            session.list_org_events(workspace, None, 50).await.unwrap(),
            before_events
        );
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            baseline
        );
        assert!(session
            .get_org_operation(workspace, &operation_id)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn document_import_preserves_bytes_created_at_and_resolves_workspace_time() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000020");
    create_test_workspace(
        &context,
        workspace,
        "create-time-workspace",
        "Asia/Shanghai",
    )
    .await;
    let document = document_id("20000000-0000-4000-8000-000000000020");
    let item = work_item_id("30000000-0000-4000-8000-000000000020");
    let raw = source_with_metadata(
        item,
        "READY",
        "SCHEDULED: <2026-08-04 Tue 15:30>\r\nDEADLINE: <2026-08-05 Wed 09:00>\r\n",
        "",
        "Opaque  bytes.\r\n",
    );
    let inserted = put_document(
        &context,
        &envelope(workspace, "put-time-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "time.org".into(),
            source: raw.clone(),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(inserted.document_revisions[&document.to_string()], 1);

    let session = backend.session().await.unwrap();
    let stored = session.get_org_document(document).await.unwrap().unwrap();
    assert_eq!(stored.source.as_bytes(), raw.as_bytes());
    let projection = session
        .list_org_document_projection(document)
        .await
        .unwrap();
    assert_eq!(projection[0].created_at, NOW);
    let scheduled = projection[0].scheduled.as_ref().unwrap();
    assert_eq!(scheduled.raw, "<2026-08-04 Tue 15:30>");
    assert_eq!(scheduled.timezone, "Asia/Shanghai");
    assert_eq!(scheduled.utc_timestamp, 1_785_828_600);
    let creation_events = session
        .list_org_events(workspace, None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.subject_id == document.to_string())
        .collect::<Vec<_>>();
    assert_eq!(creation_events.len(), 1);
    assert_eq!(creation_events[0].subject_kind, "document");
    assert_eq!(creation_events[0].event_type, OrgEventType::Creation);

    let opaque_only = raw.replace("Opaque  bytes.", "Opaque\tbytes.");
    let updated = put_document(
        &context,
        &envelope(workspace, "opaque-update"),
        &PutDocumentRequest {
            document_id: document,
            path: "time.org".into(),
            source: opaque_only,
            expected_revision: Some(1),
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let updated_event = session
        .list_org_events(workspace, None, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|event| updated.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(updated_event.subject_kind, "document");
    assert_eq!(updated_event.event_type, OrgEventType::DocumentImport);
    let projection = session
        .list_org_document_projection(document)
        .await
        .unwrap();
    assert_eq!(projection[0].created_at, NOW);

    let stored_workspace = session.get_org_workspace(workspace).await.unwrap().unwrap();
    update_workspace(
        &context,
        &envelope(workspace, "change-workspace-timezone"),
        &UpdateWorkspaceRequest {
            expected_revision: stored_workspace.revision,
            slug: stored_workspace.slug,
            display_name: stored_workspace.display_name,
            description: stored_workspace.description,
            timezone: "America/New_York".into(),
            policy_schema_version: stored_workspace.policy_schema_version,
            policy: stored_workspace.policy,
        },
    )
    .await
    .unwrap();
    let reprojected = session
        .list_org_document_projection(document)
        .await
        .unwrap();
    let scheduled = reprojected[0].scheduled.as_ref().unwrap();
    assert_eq!(scheduled.raw, "<2026-08-04 Tue 15:30>");
    assert_eq!(scheduled.timezone, "America/New_York");
    assert_eq!(scheduled.utc_timestamp, 1_785_871_800);
    assert_eq!(reprojected[0].created_at, NOW);
}

#[tokio::test]
async fn ambiguous_and_nonexistent_dst_timestamps_reject_the_entire_import() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000030");
    create_test_workspace(
        &context,
        workspace,
        "create-dst-workspace",
        "America/New_York",
    )
    .await;
    for (operation, timestamp) in [
        ("ambiguous", "<2026-11-01 Sun 01:30>"),
        ("nonexistent", "<2026-03-08 Sun 02:30>"),
    ] {
        let document = document_id(if operation == "ambiguous" {
            "20000000-0000-4000-8000-000000000031"
        } else {
            "20000000-0000-4000-8000-000000000032"
        });
        let item = work_item_id(if operation == "ambiguous" {
            "30000000-0000-4000-8000-000000000031"
        } else {
            "30000000-0000-4000-8000-000000000032"
        });
        let error = put_document(
            &context,
            &envelope(workspace, operation),
            &PutDocumentRequest {
                document_id: document,
                path: format!("{operation}.org"),
                source: source_with_metadata(
                    item,
                    "READY",
                    &format!("SCHEDULED: {timestamp}\r\n"),
                    "",
                    "",
                ),
                expected_revision: None,
                lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_document(document)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn raw_replacement_rejects_invalid_changes_but_allows_opaque_active_state_bytes() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000040");
    create_test_workspace(&context, workspace, "create-guard-workspace", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000040");
    let item = work_item_id("30000000-0000-4000-8000-000000000040");
    let original = source(item, "READY", "Keep.\r\n");
    put_document(
        &context,
        &envelope(workspace, "seed-guard-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "guard.org".into(),
            source: original.clone(),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let baseline = workspace_snapshot(backend.as_ref(), workspace).await;

    for (operation, candidate) in [
        ("state-change", source(item, "DONE", "Keep.\r\n")),
        ("omission", "No work items.\r\n".into()),
    ] {
        let error = put_document(
            &context,
            &envelope(workspace, operation),
            &PutDocumentRequest {
                document_id: document,
                path: "guard.org".into(),
                source: candidate,
                expected_revision: Some(1),
                lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.code,
            OrgErrorCode::InvalidTransition | OrgErrorCode::UnsupportedSemanticEdit
        ));
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            baseline
        );
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace, operation)
            .await
            .unwrap()
            .is_none());
    }
    let session = backend.session().await.unwrap();
    let stale = put_document(
        &context,
        &envelope(workspace, "stale-raw-replacement"),
        &PutDocumentRequest {
            document_id: document,
            path: "guard.org".into(),
            source: original.replace("Keep.", "Keep opaque change."),
            expected_revision: Some(99),
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleRevision);
    assert_eq!(stale.details["current_revision"], 1);
    assert!(session
        .get_org_operation(workspace, "stale-raw-replacement")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        workspace_snapshot(backend.as_ref(), workspace).await,
        baseline
    );
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .source,
        original
    );
    assert_eq!(
        session
            .list_org_document_projection(document)
            .await
            .unwrap()
            .len(),
        1
    );

    for (index, state) in ["RUNNING", "REVIEW"].into_iter().enumerate() {
        let new_document = document_id(if index == 0 {
            "20000000-0000-4000-8000-000000000041"
        } else {
            "20000000-0000-4000-8000-000000000042"
        });
        let new_item = work_item_id(if index == 0 {
            "30000000-0000-4000-8000-000000000041"
        } else {
            "30000000-0000-4000-8000-000000000042"
        });
        let initial = source(new_item, state, "Existing active state.\r\n");
        put_document(
            &context,
            &envelope(workspace, &format!("new-active-{index}")),
            &PutDocumentRequest {
                document_id: new_document,
                path: format!("active-{index}.org"),
                source: initial.clone(),
                expected_revision: None,
                lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        put_document(
            &context,
            &envelope(workspace, &format!("replace-active-{index}")),
            &PutDocumentRequest {
                document_id: new_document,
                path: format!("active-{index}.org"),
                source: initial.replace("Existing active state.", "Opaque active change."),
                expected_revision: Some(1),
                lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_open_org_lease_internal(new_item)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn multi_document_import_requires_complete_revisions_and_moves_ids_atomically() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000050");
    create_test_workspace(&context, workspace, "create-batch-workspace", "UTC").await;
    let source_document = document_id("20000000-0000-4000-8000-000000000051");
    let target_document = document_id("20000000-0000-4000-8000-000000000052");
    let moved_item = work_item_id("30000000-0000-4000-8000-000000000051");
    import_documents(
        &context,
        &envelope(workspace, "seed-batch"),
        &ImportDocumentsRequest {
            documents: vec![
                DocumentImport {
                    document_id: source_document,
                    path: "source.org".into(),
                    source: source(moved_item, "READY", "Moved body.\r\n"),
                    archived_at: None,
                },
                DocumentImport {
                    document_id: target_document,
                    path: "target.org".into(),
                    source: "Target prelude.\r\n".into(),
                    archived_at: None,
                },
            ],
            expected_revisions: BTreeMap::new(),
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();

    let incomplete = ImportDocumentsRequest {
        documents: vec![
            DocumentImport {
                document_id: source_document,
                path: "source.org".into(),
                source: "Source now empty.\r\n".into(),
                archived_at: None,
            },
            DocumentImport {
                document_id: target_document,
                path: "target.org".into(),
                source: source(moved_item, "READY", "Moved body.\r\n"),
                archived_at: None,
            },
        ],
        expected_revisions: BTreeMap::from([(source_document, 1)]),
        lease_proofs: std::collections::BTreeMap::new(),
    };
    let error = import_documents(
        &context,
        &envelope(workspace, "incomplete-batch"),
        &incomplete,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);

    let mut complete = incomplete;
    complete.expected_revisions.insert(target_document, 1);
    let moved_result =
        import_documents(&context, &envelope(workspace, "complete-batch"), &complete)
            .await
            .unwrap();
    let session = backend.session().await.unwrap();
    let moved = session
        .get_org_work_item(moved_item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.document_id, target_document);
    assert_eq!(moved.created_at, NOW);
    assert!(session
        .list_org_document_projection(source_document)
        .await
        .unwrap()
        .is_empty());
    let moved_events = session
        .list_org_events(workspace, None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| moved_result.event_ids.contains(&event.id))
        .collect::<Vec<_>>();
    assert_eq!(
        moved_events
            .iter()
            .filter(|event| event.subject_kind == "document"
                && event.event_type == OrgEventType::DocumentImport)
            .count(),
        2
    );
    let item_move = moved_events
        .iter()
        .filter(|event| event.subject_kind == "work_item")
        .collect::<Vec<_>>();
    assert_eq!(item_move.len(), 1);
    assert_eq!(item_move[0].event_type, OrgEventType::ItemMove);
    assert_eq!(item_move[0].subject_id, moved_item.to_string());
}

#[tokio::test]
async fn complete_dependency_graph_and_new_note_targets_are_validated_before_writes() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000060");
    create_test_workspace(&context, workspace, "create-validation-workspace", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000060");
    let item = work_item_id("30000000-0000-4000-8000-000000000060");
    let missing = work_item_id("30000000-0000-4000-8000-000000000061");
    let missing_note = "40000000-0000-4000-8000-000000000060";
    let baseline = workspace_snapshot(backend.as_ref(), workspace).await;
    for (operation, candidate) in [
        (
            "parse-failure",
            "* READY Broken\r\n:PROPERTIES:\r\n:ID: not-a-uuid\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
                .to_string(),
        ),
        (
            "missing-dependency",
            source_with_metadata(
                item,
                "READY",
                "",
                &format!(":DEPENDS_ON: {missing}\r\n"),
                "",
            ),
        ),
        (
            "missing-note",
            source(
                item,
                "READY",
                &format!("[[agent-note:design:{missing_note}][Missing]]\r\n"),
            ),
        ),
    ] {
        let error = put_document(
            &context,
            &envelope(workspace, operation),
            &PutDocumentRequest {
                document_id: document,
                path: "invalid.org".into(),
                source: candidate,
                expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.code,
            OrgErrorCode::InvalidInput | OrgErrorCode::NoteUnavailable
        ));
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_document(document)
            .await
            .unwrap()
            .is_none());
        assert_eq!(workspace_snapshot(backend.as_ref(), workspace).await, baseline);
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace, operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn document_parse_errors_do_not_expose_untrusted_source_or_parser_text() {
    const ATTACKER_VALUE: &str = "private-secret-token-value";
    const PARSER_TEXT: &str = "unsupported work item type";

    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000062");
    create_test_workspace(
        &context,
        workspace,
        "create-sanitized-error-workspace",
        "UTC",
    )
    .await;
    let document = document_id("20000000-0000-4000-8000-000000000062");
    let before = workspace_snapshot(backend.as_ref(), workspace).await;
    let candidate = format!(
        "* READY Sensitive parser input\r\n:PROPERTIES:\r\n:ID: 30000000-0000-4000-8000-000000000062\r\n:AGENT_NOTE_TYPE: {ATTACKER_VALUE}\r\n:END:\r\n"
    );

    let error = put_document(
        &context,
        &envelope(workspace, "reject-sensitive-parser-input"),
        &PutDocumentRequest {
            document_id: document,
            path: "sensitive-invalid.org".into(),
            source: candidate,
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, OrgErrorCode::InvalidInput);
    assert_eq!(error.message, "Org document is invalid");
    assert_eq!(
        error.details,
        serde_json::json!({"reason": "invalid Org input", "line": 4})
    );
    assert!(!error.retryable);

    for exposed_surface in [
        error.message.clone(),
        error.details.to_string(),
        format!("{error:?}"),
        serde_json::to_string(&error).unwrap(),
    ] {
        assert!(!exposed_surface.contains(ATTACKER_VALUE));
        assert!(!exposed_surface.contains(PARSER_TEXT));
    }

    let session = backend.session().await.unwrap();
    assert!(session.get_org_document(document).await.unwrap().is_none());
    assert!(session
        .get_org_operation(workspace, "reject-sensitive-parser-input")
        .await
        .unwrap()
        .is_none());
    let after = workspace_snapshot(backend.as_ref(), workspace).await;
    assert_eq!(after.events.len(), before.events.len());
    assert_eq!(after, before);
}

#[tokio::test]
async fn raw_import_keeps_existing_weak_links_but_rejects_new_links_to_soft_deleted_notes() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-000000000065");
    create_test_workspace(&context, workspace, "create-weak-link-workspace", "UTC").await;
    let note_id = "40000000-0000-4000-8000-000000000065";
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Weak target",
            content: "context",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let existing_document = document_id("20000000-0000-4000-8000-000000000065");
    let existing_item = work_item_id("30000000-0000-4000-8000-000000000065");
    let linked_source = source(
        existing_item,
        "READY",
        &format!("[[agent-note:design:{note_id}][Context]]\r\n"),
    );
    put_document(
        &context,
        &envelope(workspace, "seed-active-weak-link"),
        &PutDocumentRequest {
            document_id: existing_document,
            path: "existing-link.org".into(),
            source: linked_source.clone(),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    backend
        .session()
        .await
        .unwrap()
        .soft_delete_note(note_id, 1, NOW + 1)
        .await
        .unwrap();

    put_document(
        &context,
        &envelope(workspace, "preserve-existing-weak-link"),
        &PutDocumentRequest {
            document_id: existing_document,
            path: "existing-link.org".into(),
            source: linked_source.replace("Context]]", "Context]]\r\nOpaque change."),
            expected_revision: Some(1),
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_org_work_item(existing_item)
            .await
            .unwrap()
            .unwrap()
            .note_links
            .len(),
        1
    );

    let baseline = workspace_snapshot(backend.as_ref(), workspace).await;
    let new_document = document_id("20000000-0000-4000-8000-000000000066");
    let error = put_document(
        &context,
        &envelope(workspace, "new-link-to-soft-deleted-note"),
        &PutDocumentRequest {
            document_id: new_document,
            path: "new-link.org".into(),
            source: source(
                work_item_id("30000000-0000-4000-8000-000000000066"),
                "READY",
                &format!("[[agent-note:design:{note_id}][Deleted]]\r\n"),
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::NoteUnavailable);
    assert_eq!(
        workspace_snapshot(backend.as_ref(), workspace).await,
        baseline
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace, "new-link-to-soft-deleted-note")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cross_workspace_document_move_rebuilds_both_workspaces_and_pairs_events_atomically() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "closed-before-move-token",
        "move-document-token",
        "move-document-second-token",
    ])));
    let source_workspace = workspace_id("10000000-0000-4000-8000-000000000070");
    let target_workspace = workspace_id("10000000-0000-4000-8000-000000000071");
    create_test_workspace(&context, source_workspace, "create-source-workspace", "UTC").await;
    create_test_workspace(&context, target_workspace, "create-target-workspace", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000070");
    let item = work_item_id("30000000-0000-4000-8000-000000000070");
    let second_item = work_item_id("30000000-0000-4000-8000-000000000079");
    put_document(
        &context,
        &envelope(source_workspace, "seed-move-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "move.org".into(),
            source: format!(
                "{}{}",
                source(item, "READY", "Move intact.\r\n"),
                source(second_item, "READY", "Move together.\r\n")
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let closed_claim = claim_item(
        &context,
        &envelope(source_workspace, "claim-to-close-before-document-move"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    release_claim(
        &context,
        &envelope(source_workspace, "close-before-document-move"),
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: 2,
            lease_id: closed_claim.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: closed_claim.fencing_token.clone(),
            target_state: Some("READY".into()),
        },
    )
    .await
    .unwrap();
    let claim = claim_item(
        &context,
        &envelope(source_workspace, "claim-before-document-move"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 3,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let second_claim = claim_item(
        &context,
        &envelope(source_workspace, "claim-second-before-document-move"),
        &StartClaimRequest {
            work_item_id: second_item,
            document_id: document,
            expected_document_revision: 4,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let lease_before = session
        .get_open_org_lease_internal(item)
        .await
        .unwrap()
        .unwrap();
    let attempt_before = session
        .list_org_attempts(item)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let source_revision = session
        .get_org_workspace(source_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let target_revision = session
        .get_org_workspace(target_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let request = MoveDocumentRequest {
        document_id: document,
        target_workspace_id: target_workspace,
        expected_document_revision: 5,
        expected_source_workspace_revision: source_revision,
        expected_target_workspace_revision: target_revision,
        lease_proofs: BTreeMap::from([
            (
                item,
                LeaseProofInput {
                    lease_id: claim.lease_id.clone(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token.clone(),
                },
            ),
            (
                second_item,
                LeaseProofInput {
                    lease_id: second_claim.lease_id.clone(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: second_claim.fencing_token.clone(),
                },
            ),
        ]),
    };
    let later_context = OrgContext::new(
        backend.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW + 1)),
    );
    let mut incomplete = request.clone();
    incomplete.lease_proofs.remove(&second_item);
    let missing = move_document(
        &later_context,
        &envelope(source_workspace, "move-document-missing-contained-proof"),
        &incomplete,
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::StaleLease);
    let moved = move_document(
        &later_context,
        &envelope(source_workspace, "move-document"),
        &request,
    )
    .await
    .unwrap();
    let replay = move_document(
        &later_context,
        &envelope(source_workspace, "move-document"),
        &request,
    )
    .await
    .unwrap();
    assert_eq!(moved, replay);
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        target_workspace
    );
    assert!(session
        .list_org_workspace_projection(source_workspace)
        .await
        .unwrap()
        .is_empty());
    let moved_item = session.get_org_work_item(item).await.unwrap().unwrap();
    assert_eq!(moved_item.workspace_id, target_workspace);
    assert_eq!(moved_item.created_at, NOW);
    assert_eq!(
        session
            .get_org_work_item(second_item)
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        target_workspace
    );
    let lease_after = session
        .get_open_org_lease_internal(item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease_after.workspace_id, target_workspace);
    assert_eq!(lease_after.id, lease_before.id);
    assert_eq!(lease_after.attempt_id, lease_before.attempt_id);
    assert_eq!(lease_after.actor_id, lease_before.actor_id);
    assert_eq!(lease_after.kind, lease_before.kind);
    assert_eq!(lease_after.expires_at, lease_before.expires_at);
    let attempt_after = session
        .list_org_attempts(item)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt_after, attempt_before);
    assert_eq!(attempt_after.workspace_id, source_workspace);
    assert_eq!(
        session
            .get_open_org_lease_internal(second_item)
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        target_workspace
    );

    let source_events = session
        .list_org_events(source_workspace, None, 50)
        .await
        .unwrap();
    let target_events = session
        .list_org_events(target_workspace, None, 50)
        .await
        .unwrap();
    let source_move = source_events
        .iter()
        .find(|event| event.event_type == OrgEventType::DocumentMove)
        .unwrap();
    let target_move = target_events
        .iter()
        .find(|event| event.event_type == OrgEventType::DocumentMove)
        .unwrap();
    assert_eq!(
        target_move.metadata["lineage_previous_event_id"],
        source_move.id
    );
    let source_item_move = source_events
        .iter()
        .find(|event| {
            event.subject_kind == "work_item"
                && event.subject_id == item.to_string()
                && event.event_type == OrgEventType::ItemMove
        })
        .unwrap();
    let target_item_move = target_events
        .iter()
        .find(|event| {
            event.subject_kind == "work_item"
                && event.subject_id == item.to_string()
                && event.event_type == OrgEventType::ItemMove
        })
        .unwrap();
    assert_eq!(
        target_item_move.metadata["lineage_previous_event_id"],
        source_item_move.id
    );
    assert_eq!(
        source_item_move.summary,
        "Moved Org work item out of workspace"
    );
    assert_eq!(
        target_item_move.summary,
        "Moved Org work item into workspace"
    );
    assert_eq!(
        source_item_move.attempt_id.as_deref(),
        Some(attempt_after.id.as_str())
    );
    assert_eq!(target_item_move.attempt_id, None);

    drop(session);
    let stale = schedule_item(
        &later_context,
        &envelope(
            target_workspace,
            "prove-closed-token-stays-stale-after-move",
        ),
        &ScheduleItemRequest {
            item_id: item,
            document_id: document,
            scheduled: OrgFieldPatch::Set("<2029-01-01 Mon 09:00>".into()),
            deadline: OrgFieldPatch::Unchanged,
            expected_revisions: BTreeMap::from([(document, 6)]),
            lease: Some(LeaseProofInput {
                lease_id: closed_claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: closed_claim.fencing_token,
            }),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleLease);
    schedule_item(
        &later_context,
        &envelope(target_workspace, "prove-moved-token-current"),
        &ScheduleItemRequest {
            item_id: item,
            document_id: document,
            scheduled: OrgFieldPatch::Set("<2030-01-01 Tue 09:00>".into()),
            deadline: OrgFieldPatch::Unchanged,
            expected_revisions: BTreeMap::from([(document, 6)]),
            lease: Some(LeaseProofInput {
                lease_id: claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token,
            }),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cross_workspace_active_move_rejects_incompatible_target_role_and_dispatch_policy() {
    for (suffix, mutate_policy) in [
        (
            "role",
            (|policy: &mut WorkspacePolicy| {
                policy.states.insert("ACTIVE".into());
                policy.running_state = "ACTIVE".into();
                policy.transitions.extend([
                    ("READY".into(), "ACTIVE".into()),
                    ("ACTIVE".into(), "READY".into()),
                    ("FAILED".into(), "ACTIVE".into()),
                ]);
            }) as fn(&mut WorkspacePolicy),
        ),
        (
            "dispatch",
            (|policy: &mut WorkspacePolicy| {
                policy.claim_policy = ClaimPolicy::ExplicitlyDispatched;
            }) as fn(&mut WorkspacePolicy),
        ),
    ] {
        let (context, backend, _dir) = empty_context(NOW).await;
        let context =
            context.with_token_source(Arc::new(DeterministicTokenSource::new([format!(
                "target-policy-{suffix}-token"
            )])));
        let source_workspace = workspace_id(if suffix == "role" {
            "10000000-0000-4000-8000-000000000074"
        } else {
            "10000000-0000-4000-8000-000000000076"
        });
        let target_workspace = workspace_id(if suffix == "role" {
            "10000000-0000-4000-8000-000000000075"
        } else {
            "10000000-0000-4000-8000-000000000077"
        });
        create_test_workspace(
            &context,
            source_workspace,
            &format!("create-policy-source-{suffix}"),
            "UTC",
        )
        .await;
        create_test_workspace(
            &context,
            target_workspace,
            &format!("create-policy-target-{suffix}"),
            "UTC",
        )
        .await;
        let mut policy = WorkspacePolicy::engineering_default();
        mutate_policy(&mut policy);
        update_workspace(
            &context,
            &envelope(target_workspace, &format!("update-policy-target-{suffix}")),
            &UpdateWorkspaceRequest {
                expected_revision: 1,
                slug: format!("workspace-{target_workspace}"),
                display_name: "Policy target".into(),
                description: "Incompatible active lease target".into(),
                timezone: "UTC".into(),
                policy_schema_version: 1,
                policy,
            },
        )
        .await
        .unwrap();
        let document = document_id(if suffix == "role" {
            "20000000-0000-4000-8000-000000000074"
        } else {
            "20000000-0000-4000-8000-000000000076"
        });
        let item = work_item_id(if suffix == "role" {
            "30000000-0000-4000-8000-000000000074"
        } else {
            "30000000-0000-4000-8000-000000000076"
        });
        put_document(
            &context,
            &envelope(source_workspace, &format!("seed-policy-move-{suffix}")),
            &PutDocumentRequest {
                document_id: document,
                path: format!("policy-{suffix}.org"),
                source: source(item, "READY", ""),
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let claim = claim_item(
            &context,
            &envelope(source_workspace, &format!("claim-policy-move-{suffix}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap();
        let before_source = workspace_snapshot(backend.as_ref(), source_workspace).await;
        let before_target = workspace_snapshot(backend.as_ref(), target_workspace).await;
        let error = move_document(
            &context,
            &envelope(source_workspace, &format!("move-policy-{suffix}")),
            &MoveDocumentRequest {
                document_id: document,
                target_workspace_id: target_workspace,
                expected_document_revision: 2,
                expected_source_workspace_revision: before_source.workspace.revision,
                expected_target_workspace_revision: before_target.workspace.revision,
                lease_proofs: BTreeMap::from([(
                    item,
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: OrgClaimKind::Execution,
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput, "{suffix}");
        assert_eq!(
            workspace_snapshot(backend.as_ref(), source_workspace).await,
            before_source,
            "{suffix}"
        );
        assert_eq!(
            workspace_snapshot(backend.as_ref(), target_workspace).await,
            before_target,
            "{suffix}"
        );
    }
}

#[tokio::test]
async fn cross_workspace_second_projection_failure_rolls_back_both_workspaces_and_retries() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let source_workspace = workspace_id("10000000-0000-4000-8000-000000000098");
    let target_workspace = workspace_id("10000000-0000-4000-8000-000000000099");
    create_test_workspace(
        &context,
        source_workspace,
        "create-second-projection-source",
        "UTC",
    )
    .await;
    create_test_workspace(
        &context,
        target_workspace,
        "create-second-projection-target",
        "UTC",
    )
    .await;
    let document = document_id("20000000-0000-4000-8000-000000000098");
    let item = work_item_id("30000000-0000-4000-8000-000000000098");
    put_document(
        &context,
        &envelope(source_workspace, "seed-second-projection-move"),
        &PutDocumentRequest {
            document_id: document,
            path: "second-projection.org".into(),
            source: source(item, "READY", ""),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let source_before = workspace_snapshot(backend.as_ref(), source_workspace).await;
    let target_before = workspace_snapshot(backend.as_ref(), target_workspace).await;
    let operation_id = "move-fail-second-projection";
    let request = MoveDocumentRequest {
        document_id: document,
        target_workspace_id: target_workspace,
        expected_document_revision: 1,
        expected_source_workspace_revision: source_before.workspace.revision,
        expected_target_workspace_revision: target_before.workspace.revision,
        lease_proofs: BTreeMap::new(),
    };
    let fault_context =
        context
            .clone()
            .with_workflow_test_hook(Arc::new(FailAtWorkflowPhaseOccurrence::new(
                OrgWorkflowPhase::ProjectionUpdate,
                2,
            )));
    let error = move_document(
        &fault_context,
        &envelope(source_workspace, operation_id),
        &request,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StorageFailure);
    assert_eq!(
        workspace_snapshot(backend.as_ref(), source_workspace).await,
        source_before
    );
    assert_eq!(
        workspace_snapshot(backend.as_ref(), target_workspace).await,
        target_before
    );
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        source_workspace
    );
    assert!(session
        .get_org_operation(source_workspace, operation_id)
        .await
        .unwrap()
        .is_none());
    drop(session);
    move_document(
        &context,
        &envelope(source_workspace, operation_id),
        &request,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cross_workspace_active_move_rejects_target_retry_and_attempt_incompatibility() {
    for case in ["retry", "attempt"] {
        let (context, backend, _dir) = empty_context(NOW).await;
        let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
            format!("{case}-first-token"),
            format!("{case}-second-token"),
        ])));
        let source_workspace = workspace_id(if case == "retry" {
            "10000000-0000-4000-8000-000000000094"
        } else {
            "10000000-0000-4000-8000-000000000096"
        });
        let target_workspace = workspace_id(if case == "retry" {
            "10000000-0000-4000-8000-000000000095"
        } else {
            "10000000-0000-4000-8000-000000000097"
        });
        create_test_workspace(
            &context,
            source_workspace,
            &format!("create-{case}-source"),
            "UTC",
        )
        .await;
        create_test_workspace(
            &context,
            target_workspace,
            &format!("create-{case}-target"),
            "UTC",
        )
        .await;
        if case == "retry" {
            let mut policy = WorkspacePolicy::engineering_default();
            policy.retry_limit = 0;
            update_workspace(
                &context,
                &envelope(target_workspace, "limit-target-retries"),
                &UpdateWorkspaceRequest {
                    expected_revision: 1,
                    slug: format!("workspace-{target_workspace}"),
                    display_name: "Retry target".into(),
                    description: "No retries".into(),
                    timezone: "UTC".into(),
                    policy_schema_version: 1,
                    policy,
                },
            )
            .await
            .unwrap();
        }
        let document = document_id(if case == "retry" {
            "20000000-0000-4000-8000-000000000094"
        } else {
            "20000000-0000-4000-8000-000000000096"
        });
        let item = work_item_id(if case == "retry" {
            "30000000-0000-4000-8000-000000000094"
        } else {
            "30000000-0000-4000-8000-000000000096"
        });
        put_document(
            &context,
            &envelope(source_workspace, &format!("seed-{case}-move")),
            &PutDocumentRequest {
                document_id: document,
                path: format!("{case}-move.org"),
                source: source(item, "READY", ""),
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let first = claim_item(
            &context,
            &envelope(source_workspace, &format!("claim-{case}-first")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap();
        let (claim, revision) = if case == "retry" {
            release_claim(
                &context,
                &envelope(source_workspace, "release-before-retry-move"),
                &ReleaseClaimRequest {
                    schema_version: 1,
                    work_item_id: item,
                    document_id: document,
                    expected_document_revision: 2,
                    lease_id: first.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: first.fencing_token,
                    target_state: Some("READY".into()),
                },
            )
            .await
            .unwrap();
            (
                claim_item(
                    &context,
                    &envelope(source_workspace, "claim-before-retry-move"),
                    &StartClaimRequest {
                        work_item_id: item,
                        document_id: document,
                        expected_document_revision: 3,
                        kind: OrgClaimKind::Execution,
                    },
                )
                .await
                .unwrap(),
                4,
            )
        } else {
            let attempt = backend
                .session()
                .await
                .unwrap()
                .list_org_attempts(item)
                .await
                .unwrap()
                .pop()
                .unwrap();
            backend
                .session()
                .await
                .unwrap()
                .update_org_attempt(OrgAttemptUpdate {
                    id: &attempt.id,
                    expected_status: OrgAttemptStatus::Running,
                    status: OrgAttemptStatus::Failed,
                    ended_at: NOW,
                    error: Some("injected inconsistency"),
                    result_summary: None,
                    review_outcome: None,
                    note_refs: &attempt.note_refs,
                    artifacts: &attempt.artifacts,
                    metadata: &attempt.metadata,
                })
                .await
                .unwrap();
            (first, 2)
        };
        let before_source = workspace_snapshot(backend.as_ref(), source_workspace).await;
        let before_target = workspace_snapshot(backend.as_ref(), target_workspace).await;
        let error = move_document(
            &context,
            &envelope(source_workspace, &format!("reject-{case}-move")),
            &MoveDocumentRequest {
                document_id: document,
                target_workspace_id: target_workspace,
                expected_document_revision: revision,
                expected_source_workspace_revision: before_source.workspace.revision,
                expected_target_workspace_revision: before_target.workspace.revision,
                lease_proofs: BTreeMap::from([(
                    item,
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: OrgClaimKind::Execution,
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            if case == "retry" {
                OrgErrorCode::RetryLimit
            } else {
                OrgErrorCode::InvalidTransition
            },
            "{case}"
        );
        assert_eq!(
            workspace_snapshot(backend.as_ref(), source_workspace).await,
            before_source,
            "{case}"
        );
        assert_eq!(
            workspace_snapshot(backend.as_ref(), target_workspace).await,
            before_target,
            "{case}"
        );
    }
}

#[tokio::test]
async fn cross_workspace_move_and_target_claim_share_one_capacity_boundary() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "capacity-source-token",
        "capacity-target-token",
    ])));
    let source_workspace = workspace_id("10000000-0000-4000-8000-000000000072");
    let target_workspace = workspace_id("10000000-0000-4000-8000-000000000073");
    create_test_workspace(
        &context,
        source_workspace,
        "capacity-source-workspace",
        "UTC",
    )
    .await;
    create_test_workspace(
        &context,
        target_workspace,
        "capacity-target-workspace",
        "UTC",
    )
    .await;
    let mut target_policy = WorkspacePolicy::engineering_default();
    target_policy.concurrency_limit = 1;
    update_workspace(
        &context,
        &envelope(target_workspace, "limit-target-capacity"),
        &UpdateWorkspaceRequest {
            slug: format!("workspace-{target_workspace}"),
            display_name: "Capacity target".into(),
            description: "One active lease".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: target_policy,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();

    let source_document = document_id("20000000-0000-4000-8000-000000000072");
    let source_item = work_item_id("30000000-0000-4000-8000-000000000072");
    let target_document = document_id("20000000-0000-4000-8000-000000000073");
    let target_item = work_item_id("30000000-0000-4000-8000-000000000073");
    for (workspace, document, item, path, operation) in [
        (
            source_workspace,
            source_document,
            source_item,
            "capacity-source.org",
            "seed-capacity-source",
        ),
        (
            target_workspace,
            target_document,
            target_item,
            "capacity-target.org",
            "seed-capacity-target",
        ),
    ] {
        put_document(
            &context,
            &envelope(workspace, operation),
            &PutDocumentRequest {
                document_id: document,
                path: path.into(),
                source: source(item, "READY", ""),
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    }
    let source_claim = claim_item(
        &context,
        &envelope(source_workspace, "claim-capacity-source"),
        &StartClaimRequest {
            work_item_id: source_item,
            document_id: source_document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let source_revision = session
        .get_org_workspace(source_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let target_revision = session
        .get_org_workspace(target_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    drop(session);
    let move_request = MoveDocumentRequest {
        document_id: source_document,
        target_workspace_id: target_workspace,
        expected_document_revision: 2,
        expected_source_workspace_revision: source_revision,
        expected_target_workspace_revision: target_revision,
        lease_proofs: BTreeMap::from([(
            source_item,
            LeaseProofInput {
                lease_id: source_claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: source_claim.fencing_token,
            },
        )]),
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let move_task = {
        let context = context.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            move_document(
                &context,
                &envelope(source_workspace, "race-capacity-move"),
                &move_request,
            )
            .await
        })
    };
    let claim_task = {
        let context = context.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            claim_item(
                &context,
                &envelope(target_workspace, "race-capacity-claim"),
                &StartClaimRequest {
                    work_item_id: target_item,
                    document_id: target_document,
                    expected_document_revision: 1,
                    kind: OrgClaimKind::Execution,
                },
            )
            .await
        })
    };
    barrier.wait().await;
    let moved = move_task.await.unwrap();
    let claimed = claim_task.await.unwrap();
    assert_eq!(usize::from(moved.is_ok()) + usize::from(claimed.is_ok()), 1);
    let loser = moved
        .as_ref()
        .err()
        .or_else(|| claimed.as_ref().err())
        .unwrap();
    assert_eq!(loser.code, OrgErrorCode::ConcurrencyLimit);

    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .count_active_org_leases(target_workspace, NOW)
            .await
            .unwrap(),
        1
    );
    let stored_document = session
        .get_org_document(source_document)
        .await
        .unwrap()
        .unwrap();
    let source_lease = session
        .get_open_org_lease_internal(source_item)
        .await
        .unwrap()
        .unwrap();
    if moved.is_ok() {
        assert_eq!(stored_document.workspace_id, target_workspace);
        assert_eq!(source_lease.workspace_id, target_workspace);
        assert!(session
            .get_org_operation(target_workspace, "race-capacity-claim")
            .await
            .unwrap()
            .is_none());
    } else {
        assert_eq!(stored_document.workspace_id, source_workspace);
        assert_eq!(source_lease.workspace_id, source_workspace);
        assert!(session
            .get_org_operation(source_workspace, "race-capacity-move")
            .await
            .unwrap()
            .is_none());
        assert!(session
            .list_org_workspace_projection(source_workspace)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == source_item));
        assert!(session
            .list_org_workspace_projection(target_workspace)
            .await
            .unwrap()
            .iter()
            .all(|item| item.id != source_item));
    }
}

#[tokio::test]
async fn archived_rejection_and_commit_failure_leave_document_state_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("rollback.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let wrapped = Arc::new(EventStorageBackend::new(inner.clone(), events));
    let context = OrgContext::new(
        wrapped.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW)),
    );
    let workspace = workspace_id("10000000-0000-4000-8000-000000000080");
    create_test_workspace(&context, workspace, "create-rollback-workspace", "UTC").await;
    wrapped.fail_next_commit();
    let document = document_id("20000000-0000-4000-8000-000000000080");
    let item = work_item_id("30000000-0000-4000-8000-000000000080");
    let error = put_document(
        &context,
        &envelope(workspace, "failed-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "failed.org".into(),
            source: source(item, "READY", "No partial data.\r\n"),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StorageFailure);
    let session = inner.session().await.unwrap();
    assert!(session.get_org_document(document).await.unwrap().is_none());
    assert!(session
        .get_org_operation(workspace, "failed-document")
        .await
        .unwrap()
        .is_none());
    assert!(session
        .list_org_events(workspace, None, 50)
        .await
        .unwrap()
        .iter()
        .all(|event| event.subject_id != document.to_string()));

    let replay_document = document_id("20000000-0000-4000-8000-000000000081");
    let replay_item = work_item_id("30000000-0000-4000-8000-000000000081");
    let replay_request = PutDocumentRequest {
        document_id: replay_document,
        path: "replay.org".into(),
        source: source(replay_item, "READY", "Replay after archive.\r\n"),
        expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
    };
    let first = put_document(
        &context,
        &envelope(workspace, "replay-document"),
        &replay_request,
    )
    .await
    .unwrap();
    let current = session.get_org_workspace(workspace).await.unwrap().unwrap();
    session
        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
            id: workspace,
            expected_revision: current.revision,
            slug: &current.slug,
            display_name: &current.display_name,
            description: &current.description,
            timezone: &current.timezone,
            policy_schema_version: current.policy_schema_version,
            policy: &current.policy,
            archived_at: Some(NOW + 1),
            updated_at: NOW + 1,
        })
        .await
        .unwrap();
    let replay = put_document(
        &context,
        &envelope(workspace, "replay-document"),
        &replay_request,
    )
    .await
    .unwrap();
    assert_eq!(replay, first);
    let rejected = put_document(
        &context,
        &envelope(workspace, "new-after-archive"),
        &PutDocumentRequest {
            document_id: document_id("20000000-0000-4000-8000-000000000082"),
            path: "rejected.org".into(),
            source: source(
                work_item_id("30000000-0000-4000-8000-000000000082"),
                "READY",
                "Rejected.\r\n",
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.code, OrgErrorCode::ArchivedWorkspace);
}

#[tokio::test]
async fn repository_failpoints_rollback_source_projection_event_and_operation_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("call-failures.db"))
            .await
            .unwrap(),
    );
    let wrapped = Arc::new(EventStorageBackend::new(inner.clone(), event_log()));
    let context = OrgContext::new(
        wrapped.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW)),
    );
    let workspace = workspace_id("10000000-0000-4000-8000-000000000090");
    create_test_workspace(&context, workspace, "create-call-failure-workspace", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000090");
    let item = work_item_id("30000000-0000-4000-8000-000000000090");
    let initial = source(item, "READY", "Original opaque body.\r\n");
    put_document(
        &context,
        &envelope(workspace, "seed-call-failure-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "failpoints.org".into(),
            source: initial.clone(),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let baseline = workspace_snapshot(inner.as_ref(), workspace).await;

    for (index, failpoint) in [
        "compare_and_swap_org_document",
        "rebuild_org_workspace_projection",
        "append_org_event",
        "insert_org_operation",
    ]
    .into_iter()
    .enumerate()
    {
        let operation_id = format!("call-failure-{index}");
        wrapped.fail_next_repository_call(failpoint);
        let error = put_document(
            &context,
            &envelope(workspace, &operation_id),
            &PutDocumentRequest {
                document_id: document,
                path: "failpoints.org".into(),
                source: initial.replace("Original opaque body.", "Changed opaque body."),
                expected_revision: Some(1),
                lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure, "{failpoint}");
        assert_eq!(
            workspace_snapshot(inner.as_ref(), workspace).await,
            baseline
        );
        assert!(inner
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace, &operation_id)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn failure_after_ownership_cas_rolls_back_both_workspaces_completely() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("ownership-failure.db"))
            .await
            .unwrap(),
    );
    let wrapped = Arc::new(EventStorageBackend::new(inner.clone(), event_log()));
    let context = OrgContext::new(
        wrapped.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW)),
    );
    let source_workspace = workspace_id("10000000-0000-4000-8000-000000000091");
    let target_workspace = workspace_id("10000000-0000-4000-8000-000000000092");
    create_test_workspace(&context, source_workspace, "create-failure-source", "UTC").await;
    create_test_workspace(&context, target_workspace, "create-failure-target", "UTC").await;
    let document = document_id("20000000-0000-4000-8000-000000000091");
    let item = work_item_id("30000000-0000-4000-8000-000000000091");
    put_document(
        &context,
        &envelope(source_workspace, "seed-ownership-failure"),
        &PutDocumentRequest {
            document_id: document,
            path: "ownership.org".into(),
            source: source(item, "READY", "Ownership rollback.\r\n"),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let source_before = workspace_snapshot(inner.as_ref(), source_workspace).await;
    let target_before = workspace_snapshot(inner.as_ref(), target_workspace).await;
    let source_revision = source_before.workspace.revision;
    let target_revision = target_before.workspace.revision;

    wrapped.fail_next_repository_call("rebuild_org_workspace_projection");
    let error = move_document(
        &context,
        &envelope(source_workspace, "ownership-failure"),
        &MoveDocumentRequest {
            document_id: document,
            target_workspace_id: target_workspace,
            expected_document_revision: 1,
            expected_source_workspace_revision: source_revision,
            expected_target_workspace_revision: target_revision,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StorageFailure);
    assert_eq!(
        workspace_snapshot(inner.as_ref(), source_workspace).await,
        source_before
    );
    assert_eq!(
        workspace_snapshot(inner.as_ref(), target_workspace).await,
        target_before
    );
    assert!(inner
        .session()
        .await
        .unwrap()
        .get_org_operation(source_workspace, "ownership-failure")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn workspace_snapshot_create_is_atomic_and_document_export_resolves_ownership() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-0000000000a1");
    let valid_document = document_id("20000000-0000-4000-8000-0000000000a1");
    let invalid_document = document_id("20000000-0000-4000-8000-0000000000a2");
    let item = work_item_id("30000000-0000-4000-8000-0000000000a1");
    let request = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "atomic-snapshot".into(),
            display_name: "Atomic snapshot".into(),
            description: "Must not partially restore".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 3,
            archived_at: Some(NOW - 10),
        },
        documents: vec![
            DocumentImport {
                document_id: valid_document,
                path: "valid.org".into(),
                source: source(item, "READY", "Exact bytes.\r\n"),
                archived_at: None,
            },
            DocumentImport {
                document_id: invalid_document,
                path: "invalid.org".into(),
                source: source(item, "READY", "Duplicate identity.\r\n"),
                archived_at: None,
            },
        ],
        document_revisions: BTreeMap::from([(valid_document, 4), (invalid_document, 1)]),
        lease_proofs: BTreeMap::new(),
    };

    let error = import_workspace_snapshot(
        &context,
        &envelope(workspace, "atomic-create-failure"),
        &request,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_workspace(workspace)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .get_org_document(valid_document)
        .await
        .unwrap()
        .is_none());

    let mut valid = request;
    valid.documents.pop();
    valid.document_revisions.remove(&invalid_document);
    let created = import_workspace_snapshot(
        &context,
        &envelope(workspace, "atomic-create-success"),
        &valid,
    )
    .await
    .unwrap();
    assert_eq!(created.workspace_revision, Some(3));
    assert_eq!(created.document_revisions[&valid_document.to_string()], 4);

    let exported = export_document_by_id(&context, valid_document)
        .await
        .unwrap();
    assert_eq!(exported.workspace_id, workspace);
    assert_eq!(exported.id, valid_document);
    assert_eq!(exported.path, "valid.org");
    assert_eq!(exported.source, source(item, "READY", "Exact bytes.\r\n"));
    assert_eq!(exported.revision, 4);
    let restored_workspace = backend
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored_workspace.revision, 3);
    assert_eq!(restored_workspace.archived_at, Some(NOW - 10));
    assert!(export_document_by_id(
        &context,
        document_id("20000000-0000-4000-8000-0000000000ff")
    )
    .await
    .is_err());

    let excessive_workspace = workspace_id("10000000-0000-4000-8000-0000000000af");
    let excessive = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "excessive-revision".into(),
            display_name: "Excessive revision".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 1_001,
            archived_at: None,
        },
        documents: Vec::new(),
        document_revisions: BTreeMap::new(),
        lease_proofs: BTreeMap::new(),
    };
    let error = import_workspace_snapshot(
        &context,
        &envelope(excessive_workspace, "excessive-revision"),
        &excessive,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_workspace(excessive_workspace)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn workspace_snapshot_round_trips_active_and_archived_document_lifecycle() {
    let (source_context, source_backend, _source_dir) = empty_context(NOW).await;
    let source_workspace = workspace_id("10000000-0000-4000-8000-0000000000c1");
    create_test_workspace(&source_context, source_workspace, "lifecycle-source", "UTC").await;
    let active_document = document_id("20000000-0000-4000-8000-0000000000c1");
    let archived_document = document_id("20000000-0000-4000-8000-0000000000c2");
    let advanced_archived_document = document_id("20000000-0000-4000-8000-0000000000c3");
    let active_item = work_item_id("30000000-0000-4000-8000-0000000000c1");
    let archived_item = work_item_id("30000000-0000-4000-8000-0000000000c2");
    let advanced_archived_item = work_item_id("30000000-0000-4000-8000-0000000000c3");
    for (operation, document_id, path, source) in [
        (
            "seed-active-snapshot-document",
            active_document,
            "active.org",
            source(active_item, "READY", "Active source.\r\n"),
        ),
        (
            "seed-archived-snapshot-document",
            archived_document,
            "archive/history.org",
            source(archived_item, "READY", "Archived source.\r\n"),
        ),
        (
            "seed-advanced-archived-snapshot-document",
            advanced_archived_document,
            "archive/advanced-history.org",
            source(
                advanced_archived_item,
                "READY",
                "Advanced archived source.\r\n",
            ),
        ),
    ] {
        put_document(
            &source_context,
            &envelope(source_workspace, operation),
            &PutDocumentRequest {
                document_id,
                path: path.into(),
                source,
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    }
    archive_document(
        &source_context,
        &envelope(source_workspace, "archive-snapshot-document"),
        &DocumentRevisionRequest {
            document_id: archived_document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    archive_document(
        &source_context,
        &envelope(source_workspace, "archive-advanced-snapshot-document"),
        &DocumentRevisionRequest {
            document_id: advanced_archived_document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    for (operation, expected_revision, path) in [
        (
            "advance-archived-document-three",
            2,
            "archive/advanced-3.org",
        ),
        (
            "advance-archived-document-four",
            3,
            "archive/advanced-4.org",
        ),
        (
            "advance-archived-document-five",
            4,
            "archive/advanced-history.org",
        ),
    ] {
        rename_document(
            &source_context,
            &envelope(source_workspace, operation),
            &RenameDocumentRequest {
                document_id: advanced_archived_document,
                new_path: path.into(),
                expected_revision,
            },
        )
        .await
        .unwrap();
    }

    let export = export_workspace(&source_context, source_workspace)
        .await
        .unwrap();
    let source_projection = source_backend
        .session()
        .await
        .unwrap()
        .list_org_workspace_projection(source_workspace)
        .await
        .unwrap();
    let target_workspace = workspace_id("10000000-0000-4000-8000-0000000000c2");
    let (target_context, target_backend, _target_dir) = empty_context(NOW + 10).await;
    let request = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "lifecycle-target".into(),
            display_name: export.workspace.display_name,
            description: export.workspace.description,
            timezone: export.workspace.timezone,
            policy_schema_version: export.workspace.policy_schema_version,
            policy: export.workspace.policy,
            revision: export.workspace.revision,
            archived_at: export.workspace.archived_at,
        },
        documents: export
            .documents
            .iter()
            .map(|document| DocumentImport {
                document_id: document.id,
                path: document.path.clone(),
                source: document.source.clone(),
                archived_at: document.archived_at,
            })
            .collect(),
        document_revisions: export
            .documents
            .iter()
            .map(|document| (document.id, document.revision))
            .collect(),
        lease_proofs: BTreeMap::new(),
    };
    let command = envelope(target_workspace, "restore-lifecycle-snapshot");
    let created = import_workspace_snapshot(&target_context, &command, &request)
        .await
        .unwrap();
    let replay = import_workspace_snapshot(&target_context, &command, &request)
        .await
        .unwrap();
    assert_eq!(replay, created);

    let target_session = target_backend.session().await.unwrap();
    let restored = target_session
        .list_org_documents(target_workspace)
        .await
        .unwrap();
    assert_eq!(restored.len(), 3);
    for expected in &export.documents {
        let actual = restored
            .iter()
            .find(|document| document.id == expected.id)
            .unwrap();
        assert_eq!(actual.path, expected.path);
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.content_hash, expected.content_hash);
        assert_eq!(actual.revision, expected.revision);
        assert_eq!(actual.archived_at, expected.archived_at);
    }
    let archived_events = target_session
        .list_org_events(target_workspace, None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.subject_kind == "document" && event.subject_id == archived_document.to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        archived_events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![OrgEventType::Creation, OrgEventType::DocumentArchive]
    );
    assert!(archived_events[0].sequence < archived_events[1].sequence);
    assert!(archived_events
        .iter()
        .all(|event| created.event_ids.contains(&event.id)));
    assert_eq!(
        archived_events[0].metadata,
        serde_json::json!({
            "document_id": archived_document,
            "path": "archive/history.org",
            "revision": 1,
            "archived_at": null,
            "previous_archived_at": null,
            "resulting_archived_at": null,
            "previous_revision": null,
            "resulting_revision": 1,
            "created": true,
        })
    );
    let archive_metadata = &archived_events[1].metadata;
    assert_eq!(
        archive_metadata,
        &serde_json::json!({
            "path": "archive/history.org",
            "previous_archived_at": null,
            "resulting_archived_at": NOW,
            "previous_revision": 1,
            "resulting_revision": 2,
        })
    );
    for forbidden in ["source", "hash", "secret", "token", "lease"] {
        for metadata in [&archived_events[0].metadata, archive_metadata] {
            assert!(
                !metadata.to_string().contains(forbidden),
                "{forbidden}: {metadata}"
            );
        }
    }
    let advanced_archived_events = target_session
        .list_org_events(target_workspace, None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.subject_kind == "document"
                && event.subject_id == advanced_archived_document.to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        advanced_archived_events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![OrgEventType::Creation, OrgEventType::DocumentArchive]
    );
    assert!(advanced_archived_events[0].sequence < advanced_archived_events[1].sequence);
    assert!(advanced_archived_events
        .iter()
        .all(|event| created.event_ids.contains(&event.id)));
    assert_eq!(
        advanced_archived_events[0].metadata,
        serde_json::json!({
            "document_id": advanced_archived_document,
            "path": "archive/advanced-history.org",
            "revision": 4,
            "archived_at": null,
            "previous_archived_at": null,
            "resulting_archived_at": null,
            "previous_revision": null,
            "resulting_revision": 4,
            "created": true,
        })
    );
    assert_eq!(
        advanced_archived_events[1].metadata,
        serde_json::json!({
            "path": "archive/advanced-history.org",
            "previous_archived_at": null,
            "resulting_archived_at": NOW,
            "previous_revision": 4,
            "resulting_revision": 5,
        })
    );
    for forbidden in ["source", "hash", "secret", "token", "lease"] {
        for event in &advanced_archived_events {
            let metadata = event.metadata.to_string();
            assert!(!metadata.contains(forbidden), "{forbidden}: {metadata}");
        }
    }
    let restored_projection = target_session
        .list_org_workspace_projection(target_workspace)
        .await
        .unwrap();
    assert_eq!(
        restored_projection
            .iter()
            .map(|item| item.id)
            .collect::<BTreeSet<_>>(),
        source_projection
            .iter()
            .map(|item| item.id)
            .collect::<BTreeSet<_>>()
    );
    drop(target_session);
    let active = list_documents(
        &target_context,
        target_workspace,
        &OrgDocumentReadQuery {
            status: DocumentStatus::Active,
            cursor: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();
    assert_eq!(active.items.len(), 1);
    assert_eq!(active.items[0].id, active_document);
}

#[tokio::test]
async fn workspace_snapshot_update_applies_document_lifecycle_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("snapshot-lifecycle-update.db"))
            .await
            .unwrap(),
    );
    let context = OrgContext::new(
        inner.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW)),
    );
    let workspace = workspace_id("10000000-0000-4000-8000-0000000000d1");
    let document = document_id("20000000-0000-4000-8000-0000000000d1");
    let item = work_item_id("30000000-0000-4000-8000-0000000000d1");
    create_test_workspace(&context, workspace, "snapshot-lifecycle-update", "UTC").await;
    let original = source(item, "READY", "Lifecycle source.\r\n");
    put_document(
        &context,
        &envelope(workspace, "seed-snapshot-lifecycle-update"),
        &PutDocumentRequest {
            document_id: document,
            path: "lifecycle.org".into(),
            source: original.clone(),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let workspace_record = inner
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace)
        .await
        .unwrap()
        .unwrap();
    let update =
        |workspace_revision, document_revision, archived_at| ImportWorkspaceSnapshotRequest {
            mode: WorkspaceImportMode::Update,
            workspace: WorkspaceSnapshotMetadata {
                slug: workspace_record.slug.clone(),
                display_name: workspace_record.display_name.clone(),
                description: workspace_record.description.clone(),
                timezone: workspace_record.timezone.clone(),
                policy_schema_version: workspace_record.policy_schema_version,
                policy: workspace_record.policy.clone(),
                revision: workspace_revision,
                archived_at: None,
            },
            documents: vec![DocumentImport {
                document_id: document,
                path: "lifecycle.org".into(),
                source: original.clone(),
                archived_at,
            }],
            document_revisions: BTreeMap::from([(document, document_revision)]),
            lease_proofs: BTreeMap::new(),
        };

    let before = workspace_snapshot(inner.as_ref(), workspace).await;
    let fault_context =
        context
            .clone()
            .with_workflow_test_hook(Arc::new(FailAtWorkflowPhaseOccurrence::new(
                OrgWorkflowPhase::SourceEdit,
                1,
            )));
    let failed = import_workspace_snapshot(
        &fault_context,
        &envelope(workspace, "snapshot-archive-rollback"),
        &update(1, 1, Some(NOW)),
    )
    .await
    .unwrap_err();
    assert_eq!(failed.code, OrgErrorCode::StorageFailure, "{failed:?}");
    assert_eq!(workspace_snapshot(inner.as_ref(), workspace).await, before);
    assert!(inner
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace, "snapshot-archive-rollback")
        .await
        .unwrap()
        .is_none());

    let archived = import_workspace_snapshot(
        &context,
        &envelope(workspace, "snapshot-archive-success"),
        &update(1, 1, Some(NOW)),
    )
    .await
    .unwrap();
    assert_eq!(archived.document_revisions[&document.to_string()], 3);
    let archived_record = inner
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(archived_record.source, original);
    assert_eq!(archived_record.archived_at, Some(NOW));
    let archive_event = workspace_snapshot(inner.as_ref(), workspace)
        .await
        .events
        .into_iter()
        .find(|event| event.event_type == OrgEventType::DocumentArchive)
        .unwrap();
    assert_eq!(archive_event.metadata["previous_revision"], 1);
    assert_eq!(archive_event.metadata["resulting_revision"], 3);
    assert!(!archive_event.metadata.to_string().contains("source"));
    assert!(!archive_event.metadata.to_string().contains("hash"));
    assert!(!archive_event.metadata.to_string().contains("lease"));

    let restored = import_workspace_snapshot(
        &context,
        &envelope(workspace, "snapshot-restore-success"),
        &update(2, 3, None),
    )
    .await
    .unwrap();
    assert_eq!(restored.document_revisions[&document.to_string()], 5);
    let restored_record = inner
        .session()
        .await
        .unwrap()
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored_record.id, document);
    assert_eq!(restored_record.source, original);
    assert_eq!(restored_record.archived_at, None);
    assert!(workspace_snapshot(inner.as_ref(), workspace)
        .await
        .events
        .iter()
        .any(|event| event.event_type == OrgEventType::DocumentRestore));
}

#[tokio::test]
async fn snapshot_and_ordinary_import_validate_document_archive_state() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let invalid_workspace = workspace_id("10000000-0000-4000-8000-0000000000e1");
    let invalid_document = document_id("20000000-0000-4000-8000-0000000000e1");
    let invalid_item = work_item_id("30000000-0000-4000-8000-0000000000e1");
    let invalid_snapshot = |revision, archived_at| ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "invalid-document-lifecycle".into(),
            display_name: "Invalid document lifecycle".into(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 1,
            archived_at: None,
        },
        documents: vec![DocumentImport {
            document_id: invalid_document,
            path: "invalid.org".into(),
            source: source(invalid_item, "READY", "Invalid lifecycle.\r\n"),
            archived_at,
        }],
        document_revisions: BTreeMap::from([(invalid_document, revision)]),
        lease_proofs: BTreeMap::new(),
    };
    for (operation, request) in [
        ("archived-revision-one", invalid_snapshot(1, Some(NOW))),
        ("archived-timestamp-zero", invalid_snapshot(2, Some(0))),
    ] {
        let error =
            import_workspace_snapshot(&context, &envelope(invalid_workspace, operation), &request)
                .await
                .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_workspace(invalid_workspace)
            .await
            .unwrap()
            .is_none());
    }

    let workspace = workspace_id("10000000-0000-4000-8000-0000000000e2");
    create_test_workspace(&context, workspace, "ordinary-archive-state", "UTC").await;
    let error = import_documents(
        &context,
        &envelope(workspace, "ordinary-import-archive-state"),
        &ImportDocumentsRequest {
            documents: vec![DocumentImport {
                document_id: document_id("20000000-0000-4000-8000-0000000000e2"),
                path: "ordinary.org".into(),
                source: source(
                    work_item_id("30000000-0000-4000-8000-0000000000e2"),
                    "READY",
                    "Ordinary import.\r\n",
                ),
                archived_at: Some(NOW),
            }],
            expected_revisions: BTreeMap::new(),
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);
}

#[tokio::test]
async fn workspace_snapshot_update_checks_workspace_and_every_document_revision_atomically() {
    let (context, backend, _dir) = empty_context(NOW).await;
    let workspace = workspace_id("10000000-0000-4000-8000-0000000000b1");
    let first = document_id("20000000-0000-4000-8000-0000000000b1");
    let second = document_id("20000000-0000-4000-8000-0000000000b2");
    let first_item = work_item_id("30000000-0000-4000-8000-0000000000b1");
    let second_item = work_item_id("30000000-0000-4000-8000-0000000000b2");
    let create = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Create,
        workspace: WorkspaceSnapshotMetadata {
            slug: "revision-snapshot".into(),
            display_name: "Revision snapshot".into(),
            description: "Before".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 1,
            archived_at: None,
        },
        documents: vec![
            DocumentImport {
                document_id: first,
                path: "first.org".into(),
                source: source(first_item, "READY", "First.\r\n"),
                archived_at: None,
            },
            DocumentImport {
                document_id: second,
                path: "second.org".into(),
                source: source(second_item, "READY", "Second.\r\n"),
                archived_at: None,
            },
        ],
        document_revisions: BTreeMap::from([(first, 1), (second, 1)]),
        lease_proofs: BTreeMap::new(),
    };
    import_workspace_snapshot(&context, &envelope(workspace, "snapshot-create"), &create)
        .await
        .unwrap();
    let before = workspace_snapshot(backend.as_ref(), workspace).await;

    let mut update = create;
    update.mode = WorkspaceImportMode::Update;
    update.workspace.description = "After".into();
    update.documents[0].source = source(first_item, "READY", "Changed first.\r\n");
    update.document_revisions = BTreeMap::from([(first, 1), (second, 99)]);
    let stale =
        import_workspace_snapshot(&context, &envelope(workspace, "snapshot-stale"), &update)
            .await
            .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleRevision);
    assert_eq!(
        workspace_snapshot(backend.as_ref(), workspace).await,
        before
    );

    update.document_revisions.insert(second, 1);
    let mut archived_update = update.clone();
    archived_update.workspace.archived_at = Some(NOW);
    let archived_error = import_workspace_snapshot(
        &context,
        &envelope(workspace, "snapshot-archive-update"),
        &archived_update,
    )
    .await
    .unwrap_err();
    assert_eq!(archived_error.code, OrgErrorCode::ArchivedWorkspace);
    assert_eq!(
        workspace_snapshot(backend.as_ref(), workspace).await,
        before
    );

    let updated =
        import_workspace_snapshot(&context, &envelope(workspace, "snapshot-update"), &update)
            .await
            .unwrap();
    assert_eq!(updated.workspace_revision, Some(2));
    assert_eq!(updated.document_revisions[&first.to_string()], 2);
    assert_eq!(updated.document_revisions[&second.to_string()], 2);
    let stored = workspace_snapshot(backend.as_ref(), workspace).await;
    assert_eq!(stored.workspace.description, "After");
}

#[tokio::test]
async fn workspace_snapshot_command_kinds_replay_once_and_reject_divergent_payloads() {
    let (context, backend, _dir) = empty_context(NOW).await;

    for (index, offline) in [false, true].into_iter().enumerate() {
        let workspace = workspace_id(&format!("10000000-0000-4000-8000-0000000001{index:02x}"));
        let document = document_id(&format!("20000000-0000-4000-8000-0000000001{index:02x}"));
        let item = work_item_id(&format!("30000000-0000-4000-8000-0000000001{index:02x}"));
        let request = ImportWorkspaceSnapshotRequest {
            mode: WorkspaceImportMode::Create,
            workspace: WorkspaceSnapshotMetadata {
                slug: format!("idempotent-create-{index}"),
                display_name: "Idempotent create".into(),
                description: "Original".into(),
                timezone: "UTC".into(),
                policy_schema_version: 1,
                policy: WorkspacePolicy::engineering_default(),
                revision: 1,
                archived_at: None,
            },
            documents: vec![DocumentImport {
                document_id: document,
                path: "idempotent.org".into(),
                source: source(item, "READY", "Original.\r\n"),
                archived_at: None,
            }],
            document_revisions: BTreeMap::from([(document, 1)]),
            lease_proofs: BTreeMap::new(),
        };
        let command = envelope(workspace, "snapshot-idempotent-create");
        let first = if offline {
            import_offline_workspace_snapshot(&context, &command, &request).await
        } else {
            import_workspace_snapshot(&context, &command, &request).await
        }
        .unwrap();
        let after_first = workspace_snapshot(backend.as_ref(), workspace).await;
        let replay = if offline {
            import_offline_workspace_snapshot(&context, &command, &request).await
        } else {
            import_workspace_snapshot(&context, &command, &request).await
        }
        .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            after_first
        );

        let mut divergent = request.clone();
        divergent.workspace.description = "Divergent".into();
        let error = if offline {
            import_offline_workspace_snapshot(&context, &command, &divergent).await
        } else {
            import_workspace_snapshot(&context, &command, &divergent).await
        }
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::IdempotencyConflict);
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            after_first
        );

        let mut update = request;
        update.mode = WorkspaceImportMode::Update;
        update.workspace.description = "Updated".into();
        update.documents[0].source = source(item, "READY", "Updated.\r\n");
        let update_command = envelope(workspace, "snapshot-idempotent-update");
        let updated = if offline {
            import_offline_workspace_snapshot(&context, &update_command, &update).await
        } else {
            import_workspace_snapshot(&context, &update_command, &update).await
        }
        .unwrap();
        let after_update = workspace_snapshot(backend.as_ref(), workspace).await;
        let update_replay = if offline {
            import_offline_workspace_snapshot(&context, &update_command, &update).await
        } else {
            import_workspace_snapshot(&context, &update_command, &update).await
        }
        .unwrap();
        assert_eq!(update_replay, updated);
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            after_update
        );

        let mut divergent_update = update;
        divergent_update.workspace.display_name = "Divergent update".into();
        let error = if offline {
            import_offline_workspace_snapshot(&context, &update_command, &divergent_update).await
        } else {
            import_workspace_snapshot(&context, &update_command, &divergent_update).await
        }
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::IdempotencyConflict);
        assert_eq!(
            workspace_snapshot(backend.as_ref(), workspace).await,
            after_update
        );
    }
}
