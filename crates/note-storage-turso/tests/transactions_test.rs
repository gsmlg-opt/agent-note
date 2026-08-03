use note_org::{WorkspaceId, WorkspacePolicy};
use note_storage::{
    NewNote, NewOrgEvent, NewOrgWorkspace, OrgRepository, StorageBackend, StorageErrorKind,
    StorageTransaction, TransactionMode, UpsertNoteChunk, EMBEDDING_DIMENSION,
};
use note_storage_turso::TursoStorage;
use serde_json::json;
use std::str::FromStr as _;
use std::sync::Arc;
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

#[tokio::test]
async fn waiting_event_append_does_not_block_owning_outer_transaction() {
    let (_dir, storage) = storage().await;
    let workspace_id = WorkspaceId::from_str("70000000-0000-0000-0000-000000000001").unwrap();
    seed_event_workspace(&storage, workspace_id, "event-lock-order").await;
    let waiting_session = Arc::new(storage.connect().await.unwrap());
    let outer = storage.begin(TransactionMode::Immediate).await.unwrap();

    let waiting = {
        let session = Arc::clone(&waiting_session);
        tokio::spawn(async move {
            let metadata = json!({"writer": "waiting"});
            session
                .append_org_event(NewOrgEvent {
                    id: "71000000-0000-0000-0000-000000000002",
                    workspace_id,
                    subject_kind: "workspace",
                    subject_id: "70000000-0000-0000-0000-000000000001",
                    actor_id: "waiting-session",
                    event_type: "waited",
                    occurred_at: 2,
                    summary: "Waited for outer transaction",
                    metadata: &metadata,
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    let outer_metadata = json!({"writer": "outer"});
    let first = tokio::time::timeout(
        Duration::from_secs(2),
        outer.append_org_event(NewOrgEvent {
            id: "71000000-0000-0000-0000-000000000001",
            workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000001",
            actor_id: "outer-transaction",
            event_type: "owned_lock",
            occurred_at: 1,
            summary: "Outer transaction retained writer ownership",
            metadata: &outer_metadata,
        }),
    )
    .await
    .expect("waiting append blocked the transaction that owns the database writer lock")
    .unwrap();
    assert_eq!(first.sequence, 1);
    outer.commit().await.unwrap();

    let second = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("waiting append did not resume after the outer transaction committed")
        .unwrap()
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
    let first_session = Arc::new(first_storage.connect().await.unwrap());
    let first_outer = first_storage
        .begin(TransactionMode::Immediate)
        .await
        .unwrap();

    let waiting = {
        let session = Arc::clone(&first_session);
        tokio::spawn(async move {
            let metadata = json!({"database": "blocked"});
            session
                .append_org_event(NewOrgEvent {
                    id: "73000000-0000-0000-0000-000000000001",
                    workspace_id: first_workspace_id,
                    subject_kind: "workspace",
                    subject_id: "70000000-0000-0000-0000-000000000003",
                    actor_id: "blocked-database",
                    event_type: "waited",
                    occurred_at: 1,
                    summary: "Waited on first database",
                    metadata: &metadata,
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

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
            event_type: "completed",
            occurred_at: 1,
            summary: "Independent database was not serialized",
            metadata: &metadata,
        }),
    )
    .await
    .expect("append waiting on one database serialized an independent database")
    .unwrap();
    assert_eq!(independent.sequence, 1);

    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    first_outer.rollback().await.unwrap();
}

#[tokio::test]
async fn cancelling_waiting_event_append_leaves_session_and_writer_lock_usable() {
    let (_dir, storage) = storage().await;
    let workspace_id = WorkspaceId::from_str("70000000-0000-0000-0000-000000000002").unwrap();
    seed_event_workspace(&storage, workspace_id, "event-cancellation").await;
    let waiting_session = Arc::new(storage.connect().await.unwrap());
    let outer = storage.begin(TransactionMode::Immediate).await.unwrap();

    let cancelled = {
        let session = Arc::clone(&waiting_session);
        tokio::spawn(async move {
            let metadata = json!({"cancelled": true});
            session
                .append_org_event(NewOrgEvent {
                    id: "72000000-0000-0000-0000-000000000001",
                    workspace_id,
                    subject_kind: "workspace",
                    subject_id: "70000000-0000-0000-0000-000000000002",
                    actor_id: "cancelled-session",
                    event_type: "cancelled",
                    occurred_at: 1,
                    summary: "Cancelled while waiting for writer lock",
                    metadata: &metadata,
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    outer.rollback().await.unwrap();

    let metadata = json!({"after_cancellation": true});
    let stored = tokio::time::timeout(
        Duration::from_secs(2),
        waiting_session.append_org_event(NewOrgEvent {
            id: "72000000-0000-0000-0000-000000000002",
            workspace_id,
            subject_kind: "workspace",
            subject_id: "70000000-0000-0000-0000-000000000002",
            actor_id: "reused-session",
            event_type: "reused",
            occurred_at: 2,
            summary: "Session remained usable after cancellation",
            metadata: &metadata,
        }),
    )
    .await
    .expect("cancelled append left the session or database writer lock unusable")
    .unwrap();
    assert_eq!(stored.sequence, 1);
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
