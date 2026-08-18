use note_org::{DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_storage::{
    NewNote, NewOrgAttemptAllocation, NewOrgDocument, NewOrgEvent, NewOrgWorkspace,
    NoteFieldsUpdate, NoteMutationResult, NotesRepository, OrgDocumentOwnershipMove,
    OrgDocumentOwnershipMoveResult, OrgEventType, OrgRepository, StorageBackend, StorageErrorKind,
    StorageTransaction, TransactionMode, UpsertNoteChunk, EMBEDDING_DIMENSION,
};
use note_storage_turso::TursoStorage;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr as _;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

async fn storage() -> (tempfile::TempDir, TursoStorage) {
    let dir = tempfile::tempdir().unwrap();
    let storage = TursoStorage::open(dir.path().join("transactions.db"))
        .await
        .unwrap();
    (dir, storage)
}

async fn insert_note(transaction: &dyn StorageTransaction, id: &str) {
    transaction
        .insert_note(NewNote {
            id,
            title: id,
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn org_claim_races_preserve_uniqueness_capacity_and_rollback() {
    let (_dir, storage) = storage().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(storage);
    note_storage_contract_tests::run_org_claim_races(backend).await;
}

#[tokio::test]
async fn org_lease_proof_validation_is_conditional_and_does_not_mutate_the_lease() {
    let (_dir, storage) = storage().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(storage);
    note_storage_contract_tests::run_org_lease_proof_validation(backend).await;
}

#[tokio::test]
async fn org_workflow_event_types_round_trip_through_turso_decoding() {
    let (_dir, storage) = storage().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(storage);
    note_storage_contract_tests::run_org_workflow_event_decoding(backend).await;
}

#[tokio::test]
async fn dropping_pending_immediate_claim_waiter_does_not_consume_attempt_number() {
    let (_dir, storage) = storage().await;
    let workspace_id = WorkspaceId::from_str("7a000000-0000-0000-0000-000000000001").unwrap();
    let work_item_id = WorkItemId::from_str("7b000000-0000-0000-0000-000000000001").unwrap();
    seed_event_workspace(&storage, workspace_id, "cancelled-claim-waiter").await;

    let blocker = StorageBackend::begin(&storage, TransactionMode::Immediate)
        .await
        .unwrap();
    let mut waiter = Box::pin(async {
        let transaction = StorageBackend::begin(&storage, TransactionMode::Immediate)
            .await
            .unwrap();
        transaction
            .allocate_next_org_attempt(NewOrgAttemptAllocation {
                id: "cancelled-claim-waiter-attempt",
                workspace_id,
                work_item_id,
                actor_id: "cancelled-waiter",
                started_at: 1,
                note_refs: &[],
                artifacts: &[],
                metadata: &json!({}),
            })
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    });
    assert_pending(
        waiter.as_mut(),
        "claim waiter was not observably pending behind the immediate writer",
    );
    drop(waiter);
    blocker.rollback().await.unwrap();

    let retry = StorageBackend::begin(&storage, TransactionMode::Immediate)
        .await
        .unwrap();
    let attempt = retry
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "cancelled-claim-waiter-attempt",
            workspace_id,
            work_item_id,
            actor_id: "cancelled-waiter",
            started_at: 2,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    assert_eq!(attempt.attempt_number, 1);
    assert!(retry
        .get_open_org_lease_internal(work_item_id)
        .await
        .unwrap()
        .is_none());
    retry.rollback().await.unwrap();
}

async fn seed_event_workspace(storage: &TursoStorage, id: WorkspaceId, slug: &str) {
    storage
        .connect()
        .await
        .unwrap()
        .insert_org_workspace(NewOrgWorkspace {
            id,
            slug,
            display_name: "Event concurrency",
            description: "Turso event append transaction regression",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &WorkspacePolicy::engineering_default(),
            now: 1,
        })
        .await
        .unwrap();
}

fn assert_pending<F: Future>(mut future: Pin<&mut F>, message: &str) {
    let mut context = Context::from_waker(Waker::noop());
    assert!(
        matches!(future.as_mut().poll(&mut context), Poll::Pending),
        "{message}"
    );
}

#[tokio::test]
async fn same_session_ownership_moves_are_serialized_across_the_entire_savepoint() {
    let (_dir, storage) = storage().await;
    let source_workspace = WorkspaceId::from_str("74000000-0000-0000-0000-000000000001").unwrap();
    let target_workspace = WorkspaceId::from_str("74000000-0000-0000-0000-000000000002").unwrap();
    let document_id = DocumentId::from_str("75000000-0000-0000-0000-000000000001").unwrap();
    let policy = WorkspacePolicy::engineering_default();
    let session = storage.connect().await.unwrap();
    for (id, slug) in [
        (source_workspace, "move-race-source"),
        (target_workspace, "move-race-target"),
    ] {
        session
            .insert_org_workspace(NewOrgWorkspace {
                id,
                slug,
                display_name: slug,
                description: "same-session ownership race regression",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                now: 1,
            })
            .await
            .unwrap();
    }
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id: source_workspace,
            path: "move-race.org",
            source: "* TODO Move once",
            content_hash: "move-race-one",
            now: 1,
        })
        .await
        .unwrap();

    let external_writer = storage.begin(TransactionMode::Immediate).await.unwrap();
    let update = || OrgDocumentOwnershipMove {
        document_id,
        source_workspace_id: source_workspace,
        target_workspace_id: target_workspace,
        expected_document_revision: 1,
        expected_source_workspace_revision: 1,
        expected_target_workspace_revision: 1,
        updated_at: 2,
    };
    let mut first = Box::pin(session.compare_and_swap_org_document_ownership(update()));
    let mut second = Box::pin(session.compare_and_swap_org_document_ownership(update()));
    assert_pending(
        first.as_mut(),
        "first ownership move did not reach the external writer lock",
    );
    assert_pending(
        second.as_mut(),
        "second ownership move did not wait behind the first operation",
    );
    let mut unrelated_read = Box::pin(session.get_org_workspace(source_workspace));
    assert_pending(
        unrelated_read.as_mut(),
        "same-session read entered while ownership move held its savepoint",
    );

    external_writer.rollback().await.unwrap();
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first.as_mut(), second.as_mut())
    })
    .await
    .expect("serialized ownership moves did not finish after releasing the writer");
    let results = [first.unwrap(), second.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, OrgDocumentOwnershipMoveResult::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, OrgDocumentOwnershipMoveResult::Conflict { .. }))
            .count(),
        1
    );
    let source = unrelated_read.await.unwrap().unwrap();
    assert_eq!(source.revision, 2);
    let moved = session
        .get_org_document(document_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.workspace_id, target_workspace);
    assert_eq!(moved.revision, 2);
    assert_eq!(
        session
            .get_org_workspace(target_workspace)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        session.list_org_documents(target_workspace).await.unwrap(),
        vec![moved]
    );
}

#[tokio::test]
async fn cancelled_ownership_move_cleans_up_before_releasing_the_session_gate() {
    let (_dir, storage) = storage().await;
    let source_workspace = WorkspaceId::from_str("74000000-0000-0000-0000-000000000011").unwrap();
    let target_workspace = WorkspaceId::from_str("74000000-0000-0000-0000-000000000012").unwrap();
    let document_id = DocumentId::from_str("75000000-0000-0000-0000-000000000011").unwrap();
    let policy = WorkspacePolicy::engineering_default();
    let session = storage.connect().await.unwrap();
    for (id, slug) in [
        (source_workspace, "cancelled-move-source"),
        (target_workspace, "cancelled-move-target"),
    ] {
        session
            .insert_org_workspace(NewOrgWorkspace {
                id,
                slug,
                display_name: slug,
                description: "ownership cancellation cleanup regression",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                now: 1,
            })
            .await
            .unwrap();
    }
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id: source_workspace,
            path: "cancelled-move.org",
            source: "* TODO Cancel then retry",
            content_hash: "cancelled-move-one",
            now: 1,
        })
        .await
        .unwrap();

    let external_writer = storage.begin(TransactionMode::Immediate).await.unwrap();
    let update = || OrgDocumentOwnershipMove {
        document_id,
        source_workspace_id: source_workspace,
        target_workspace_id: target_workspace,
        expected_document_revision: 1,
        expected_source_workspace_revision: 1,
        expected_target_workspace_revision: 1,
        updated_at: 2,
    };
    let mut cancelled = Box::pin(session.compare_and_swap_org_document_ownership(update()));
    assert_pending(
        cancelled.as_mut(),
        "ownership move did not reach the external writer lock before cancellation",
    );
    drop(cancelled);

    let mut read_after_cancellation = Box::pin(session.get_org_workspace(source_workspace));
    assert_pending(
        read_after_cancellation.as_mut(),
        "cancelled ownership move released the session gate before savepoint cleanup",
    );
    external_writer.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), read_after_cancellation.as_mut())
        .await
        .expect("savepoint cleanup did not release the session gate")
        .unwrap()
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        session.compare_and_swap_org_document_ownership(update()),
    )
    .await
    .expect("session remained stuck after cancelled ownership move cleanup")
    .unwrap();
    assert!(matches!(result, OrgDocumentOwnershipMoveResult::Applied(_)));
    let observed = storage
        .connect()
        .await
        .unwrap()
        .get_org_document(document_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.workspace_id, target_workspace);
    assert_eq!(observed.revision, 2);
}

#[tokio::test]
async fn cancelled_note_cas_rolls_back_before_releasing_the_session_gate() {
    let (_dir, storage) = storage().await;
    let session = storage.connect().await.unwrap();
    session
        .insert_note(NewNote {
            id: "cancelled-note-cas",
            title: "Before cancellation",
            content: "Before cancellation",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();

    let external_writer = storage.begin(TransactionMode::Immediate).await.unwrap();
    let update = || NoteFieldsUpdate {
        id: "cancelled-note-cas",
        expected_revision: 1,
        title: "Cancelled mutation",
        content: "Must not commit",
        updated_at: 2,
    };
    let mut cancelled = Box::pin(session.update_note_fields(update()));
    assert_pending(
        cancelled.as_mut(),
        "note CAS did not reach the external writer lock before cancellation",
    );
    drop(cancelled);

    let mut read_after_cancellation = Box::pin(session.get_note("cancelled-note-cas"));
    assert_pending(
        read_after_cancellation.as_mut(),
        "cancelled note CAS released the session gate before savepoint cleanup",
    );
    external_writer.rollback().await.unwrap();
    let unchanged = tokio::time::timeout(Duration::from_secs(2), read_after_cancellation.as_mut())
        .await
        .expect("note savepoint cleanup did not release the session gate")
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.title, "Before cancellation");
    assert_eq!(unchanged.revision, 1);

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), session.update_note_fields(update()))
            .await
            .expect("session remained stuck after cancelled note CAS cleanup")
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2,
        }
    );
    let observed = storage
        .connect()
        .await
        .unwrap()
        .get_note("cancelled-note-cas")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.title, "Cancelled mutation");
    assert_eq!(observed.revision, 2);
}

#[tokio::test]
async fn waiting_event_append_does_not_block_owning_outer_transaction() {
    let (_dir, storage) = storage().await;
    let workspace_id = WorkspaceId::from_str("70000000-0000-0000-0000-000000000001").unwrap();
    seed_event_workspace(&storage, workspace_id, "event-lock-order").await;
    let waiting_session = storage.connect().await.unwrap();
    let outer = storage.begin(TransactionMode::Immediate).await.unwrap();

    let waiting_metadata = json!({"writer": "waiting"});
    let mut waiting = Box::pin(waiting_session.append_org_event(NewOrgEvent {
        id: "71000000-0000-0000-0000-000000000002",
        workspace_id,
        subject_kind: "workspace",
        subject_id: "70000000-0000-0000-0000-000000000001",
        actor_id: "waiting-session",
        attempt_id: None,
        event_type: OrgEventType::Progress,
        occurred_at: 2,
        summary: "Waited for outer transaction",
        metadata: &waiting_metadata,
        previous_state: None,
        resulting_state: None,
    }));
    assert_pending(
        waiting.as_mut(),
        "ordinary append did not reach the blocked database statement",
    );

    let outer_metadata = json!({"writer": "outer"});
    let first = tokio::time::timeout(
        Duration::from_secs(2),
        outer.append_org_event(NewOrgEvent {
            id: "71000000-0000-0000-0000-000000000001",
            workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000001",
            actor_id: "outer-transaction",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 1,
            summary: "Outer transaction retained writer ownership",
            metadata: &outer_metadata,
            previous_state: None,
            resulting_state: None,
        }),
    )
    .await
    .expect("waiting append blocked the transaction that owns the database writer lock")
    .unwrap();
    assert_eq!(first.sequence, 1);
    outer.commit().await.unwrap();

    let second = tokio::time::timeout(Duration::from_secs(2), waiting.as_mut())
        .await
        .expect("waiting append did not resume after the outer transaction committed")
        .unwrap();
    assert_eq!(second.sequence, 2);
    assert_eq!(
        waiting_session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn waiting_event_append_does_not_serialize_an_independent_database() {
    let (_first_dir, first_storage) = storage().await;
    let (_second_dir, second_storage) = storage().await;
    let first_workspace_id = WorkspaceId::from_str("70000000-0000-0000-0000-000000000003").unwrap();
    let second_workspace_id =
        WorkspaceId::from_str("70000000-0000-0000-0000-000000000004").unwrap();
    seed_event_workspace(&first_storage, first_workspace_id, "blocked-database").await;
    seed_event_workspace(&second_storage, second_workspace_id, "independent-database").await;
    let first_session = first_storage.connect().await.unwrap();
    let first_outer = first_storage
        .begin(TransactionMode::Immediate)
        .await
        .unwrap();

    let waiting_metadata = json!({"database": "blocked"});
    let mut waiting = Box::pin(first_session.append_org_event(NewOrgEvent {
        id: "73000000-0000-0000-0000-000000000001",
        workspace_id: first_workspace_id,
        subject_kind: "workspace",
        subject_id: "70000000-0000-0000-0000-000000000003",
        actor_id: "blocked-database",
        attempt_id: None,
        event_type: OrgEventType::Progress,
        occurred_at: 1,
        summary: "Waited on first database",
        metadata: &waiting_metadata,
        previous_state: None,
        resulting_state: None,
    }));
    assert_pending(
        waiting.as_mut(),
        "first-database append did not reach the blocked database statement",
    );

    let second_session = second_storage.connect().await.unwrap();
    let metadata = json!({"database": "independent"});
    let independent = tokio::time::timeout(
        Duration::from_secs(2),
        second_session.append_org_event(NewOrgEvent {
            id: "73000000-0000-0000-0000-000000000002",
            workspace_id: second_workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000004",
            actor_id: "independent-database",
            attempt_id: None,
            event_type: OrgEventType::Completion,
            occurred_at: 1,
            summary: "Independent database was not serialized",
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        }),
    )
    .await
    .expect("append waiting on one database serialized an independent database")
    .unwrap();
    assert_eq!(independent.sequence, 1);

    drop(waiting);
    first_outer.rollback().await.unwrap();
}

#[tokio::test]
async fn cancelling_waiting_event_append_leaves_session_and_writer_lock_usable() {
    let (_dir, storage) = storage().await;
    let workspace_id = WorkspaceId::from_str("70000000-0000-0000-0000-000000000002").unwrap();
    seed_event_workspace(&storage, workspace_id, "event-cancellation").await;
    let waiting_session = storage.connect().await.unwrap();
    let outer = storage.begin(TransactionMode::Immediate).await.unwrap();

    let cancelled_metadata = json!({"cancelled": true});
    let mut cancelled = Box::pin(waiting_session.append_org_event(NewOrgEvent {
        id: "72000000-0000-0000-0000-000000000001",
        workspace_id,
        subject_kind: "workspace",
        subject_id: "70000000-0000-0000-0000-000000000002",
        actor_id: "cancelled-session",
        attempt_id: None,
        event_type: OrgEventType::Cancellation,
        occurred_at: 1,
        summary: "Cancelled while waiting for writer lock",
        metadata: &cancelled_metadata,
        previous_state: None,
        resulting_state: None,
    }));
    assert_pending(
        cancelled.as_mut(),
        "cancelled append did not reach the blocked database statement",
    );
    drop(cancelled);
    outer.rollback().await.unwrap();

    let observer_before = storage.connect().await.unwrap();
    assert!(observer_before
        .list_org_events(workspace_id, None, 200)
        .await
        .unwrap()
        .is_empty());
    let fresh_writer = tokio::time::timeout(
        Duration::from_secs(2),
        storage.begin(TransactionMode::Immediate),
    )
    .await
    .expect("cancelled append left the database writer lock held")
    .unwrap();
    let committed_metadata = json!({"committed_by": "fresh_writer"});
    let committed = fresh_writer
        .append_org_event(NewOrgEvent {
            id: "72000000-0000-0000-0000-000000000002",
            workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000002",
            actor_id: "fresh-writer",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 2,
            summary: "Fresh transaction committed after cancellation",
            metadata: &committed_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(committed.sequence, 1);
    fresh_writer.commit().await.unwrap();
    drop(observer_before);

    let observer_after_commit = storage.connect().await.unwrap();
    assert_eq!(
        observer_after_commit
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
        vec![committed.clone()],
        "fresh transaction commit must be visible outside the cancelled session"
    );
    drop(observer_after_commit);

    let metadata = json!({"after_cancellation": true});
    let stored = tokio::time::timeout(
        Duration::from_secs(2),
        waiting_session.append_org_event(NewOrgEvent {
            id: "72000000-0000-0000-0000-000000000003",
            workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000002",
            actor_id: "reused-session",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 3,
            summary: "Session remained usable after cancellation",
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        }),
    )
    .await
    .expect("cancelled append left the session or database writer lock unusable")
    .unwrap();
    assert_eq!(stored.sequence, 2);

    let observer_after = storage.connect().await.unwrap();
    assert_eq!(
        observer_after
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
        vec![committed, stored],
        "ordinary append after cancellation must commit and be visible outside its session"
    );
}

#[tokio::test]
async fn deferred_transaction_commits_repository_writes() {
    let (_dir, storage) = storage().await;
    let transaction = storage.begin(TransactionMode::Deferred).await.unwrap();
    insert_note(transaction.as_ref(), "deferred").await;

    transaction.commit().await.unwrap();

    let session = storage.session().await.unwrap();
    assert!(session.note_exists("deferred").await.unwrap());
}

#[tokio::test]
async fn bulk_update_selection_requires_an_immediate_transaction() {
    let (_dir, storage) = storage().await;
    let deferred = storage.begin(TransactionMode::Deferred).await.unwrap();

    let error = deferred
        .matching_note_ids_for_update(&[])
        .await
        .unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::Transaction);
    assert!(error.to_string().contains("immediate transaction"));
    deferred.rollback().await.unwrap();

    let immediate = storage.begin(TransactionMode::Immediate).await.unwrap();
    assert!(immediate
        .matching_note_ids_for_update(&[])
        .await
        .unwrap()
        .is_empty());
    immediate.rollback().await.unwrap();
}

#[tokio::test]
async fn immediate_transaction_commits_repository_writes() {
    let (_dir, storage) = storage().await;
    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(transaction.as_ref(), "immediate").await;

    transaction.commit().await.unwrap();

    let session = storage.session().await.unwrap();
    assert!(session.note_exists("immediate").await.unwrap());
}

#[tokio::test]
async fn explicit_rollback_discards_repository_writes() {
    let (_dir, storage) = storage().await;
    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(transaction.as_ref(), "rolled-back").await;

    transaction.rollback().await.unwrap();

    let session = storage.session().await.unwrap();
    assert!(!session.note_exists("rolled-back").await.unwrap());
}

#[tokio::test]
async fn dropping_unfinished_transaction_discards_repository_writes() {
    let (_dir, storage) = storage().await;
    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(transaction.as_ref(), "dropped").await;

    drop(transaction);

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        storage.begin(TransactionMode::Immediate),
    )
    .await
    .expect("writer lock remained held after the transaction was dropped")
    .unwrap();
    insert_note(second.as_ref(), "after-drop").await;
    second.commit().await.unwrap();

    let observer = storage.session().await.unwrap();
    assert!(!observer.note_exists("dropped").await.unwrap());
    assert!(observer.note_exists("after-drop").await.unwrap());
}

#[tokio::test]
async fn immediate_transactions_serialize_writers_without_waiting_for_timeout() {
    let (_dir, storage) = storage().await;
    let storage = Arc::new(storage);
    let first = storage.begin(TransactionMode::Immediate).await.unwrap();
    let second_storage = Arc::clone(&storage);
    let mut second = Box::pin(async move {
        second_storage
            .begin(TransactionMode::Immediate)
            .await
            .unwrap()
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(100), second.as_mut())
            .await
            .is_err(),
        "second immediate transaction unexpectedly acquired the writer lock"
    );

    first.commit().await.unwrap();

    let second = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("second immediate transaction did not proceed after the first committed");
    second.rollback().await.unwrap();
}

#[tokio::test]
async fn commit_failures_are_transaction_errors_with_turso_sources() {
    let (_dir, storage) = storage().await;
    let transaction: Box<dyn StorageTransaction> = Box::new(storage.connect().await.unwrap());

    let error = transaction.commit().await.unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::Transaction);
    let source = std::error::Error::source(&error).expect("Turso source is retained");
    assert!(source.downcast_ref::<turso::Error>().is_some());
}

#[tokio::test]
async fn rollback_failures_are_transaction_errors_with_turso_sources() {
    let (_dir, storage) = storage().await;
    let transaction: Box<dyn StorageTransaction> = Box::new(storage.connect().await.unwrap());

    let error = transaction.rollback().await.unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::Transaction);
    let source = std::error::Error::source(&error).expect("Turso source is retained");
    assert!(source.downcast_ref::<turso::Error>().is_some());
}

#[tokio::test]
async fn failed_multi_repository_write_rolls_back_all_turso_tables() {
    let (dir, storage) = storage().await;
    let session = storage.session().await.unwrap();
    session
        .insert_label_key("status", "Workflow status")
        .await
        .unwrap();

    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(transaction.as_ref(), "atomic").await;
    transaction
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "atomic",
            chunk_idx: 0,
            content_hash: "hash",
            content: "content",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    transaction
        .enqueue_embedding_job("atomic", 0, "hash", "content", 1, 1)
        .await
        .unwrap();
    transaction
        .insert_chunk_embedding("atomic", 0, &vec![0.0; EMBEDDING_DIMENSION])
        .await
        .unwrap();
    transaction
        .attach_label("atomic", "status", "done")
        .await
        .unwrap();
    let error = transaction
        .attach_label("atomic", "status", "wip")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    transaction.rollback().await.unwrap();

    let database = turso::Builder::new_local(
        dir.path()
            .join("transactions.db")
            .to_str()
            .expect("temporary path is UTF-8"),
    )
    .experimental_index_method(true)
    .build()
    .await
    .unwrap();
    let connection = database.connect().unwrap();
    for table in [
        "notes",
        "note_chunks",
        "embedding_jobs",
        "note_chunk_embeddings",
        "note_labels",
    ] {
        assert_eq!(table_row_count(&connection, table).await, 0, "{table}");
    }
    assert_eq!(table_row_count(&connection, "label_keys").await, 1);
}

async fn table_row_count(connection: &turso::Connection, table: &str) -> i64 {
    let mut rows = connection
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}
