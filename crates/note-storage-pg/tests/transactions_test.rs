mod support;

use note_org::{WorkItemId, WorkspaceId, WorkspacePolicy};
use note_storage::{
    NewNote, NewOrgAttemptAllocation, NewOrgWorkspace, OrgRepository, StorageBackend,
    StorageErrorKind, StorageTransaction, TransactionMode, UpsertNoteChunk, EMBEDDING_DIMENSION,
};
use note_storage_pg::PgStorage;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr as _;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

async fn storage(test_name: &str) -> Option<(support::TestDatabase, Arc<PgStorage>)> {
    let database = support::TestDatabase::provision_with_vector(test_name).await?;
    let storage = Arc::new(PgStorage::connect(&database.url, 10).await.unwrap());
    Some((database, storage))
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

fn assert_pending<F: Future>(mut future: Pin<&mut F>, message: &str) {
    let mut context = Context::from_waker(Waker::noop());
    assert!(
        matches!(future.as_mut().poll(&mut context), Poll::Pending),
        "{message}"
    );
}

#[tokio::test]
async fn org_claim_races_preserve_uniqueness_capacity_and_rollback() {
    let Some((database, storage)) =
        storage("org_claim_races_preserve_uniqueness_capacity_and_rollback").await
    else {
        return;
    };
    let backend: Arc<dyn StorageBackend> = storage.clone();
    note_storage_contract_tests::run_org_claim_races(backend).await;
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn org_lease_proof_validation_is_conditional_and_does_not_mutate_the_lease() {
    let Some((database, storage)) =
        storage("org_lease_proof_validation_is_conditional_and_does_not_mutate_the_lease").await
    else {
        return;
    };
    let backend: Arc<dyn StorageBackend> = storage.clone();
    note_storage_contract_tests::run_org_lease_proof_validation(backend).await;
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn org_workflow_event_types_round_trip_through_postgresql_decoding() {
    let Some((database, storage)) =
        storage("org_workflow_event_types_round_trip_through_postgresql_decoding").await
    else {
        return;
    };
    let backend: Arc<dyn StorageBackend> = storage.clone();
    note_storage_contract_tests::run_org_workflow_event_decoding(backend).await;
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn dropping_pending_immediate_claim_waiter_does_not_consume_attempt_number() {
    let Some((database, storage)) =
        storage("dropping_pending_immediate_claim_waiter_does_not_consume_attempt_number").await
    else {
        return;
    };
    let workspace_id = WorkspaceId::from_str("7a000000-0000-0000-0000-000000000001").unwrap();
    let work_item_id = WorkItemId::from_str("7b000000-0000-0000-0000-000000000001").unwrap();
    let session = storage.connect_session().await.unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id,
            slug: "cancelled-claim-waiter",
            display_name: "Cancelled claim waiter",
            description: "observable pending acquisition contract",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &WorkspacePolicy::engineering_default(),
            now: 1,
        })
        .await
        .unwrap();
    drop(session);

    let blocker = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
    let mut waiter = Box::pin(async {
        let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
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
                metadata: &serde_json::json!({}),
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

    let retry = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
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
            metadata: &serde_json::json!({}),
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
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn deferred_transaction_commits_repository_writes() {
    let Some((database, storage)) = storage("deferred_transaction_commits_repository_writes").await
    else {
        return;
    };
    let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Deferred)
        .await
        .unwrap();
    insert_note(transaction.as_ref(), "deferred").await;

    transaction.commit().await.unwrap();

    let observer = StorageBackend::session(storage.as_ref()).await.unwrap();
    assert!(observer.note_exists("deferred").await.unwrap());
    drop(observer);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn immediate_transaction_commits_repository_writes() {
    let Some((database, storage)) =
        storage("immediate_transaction_commits_repository_writes").await
    else {
        return;
    };
    let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
    insert_note(transaction.as_ref(), "immediate").await;

    transaction.commit().await.unwrap();

    let observer = StorageBackend::session(storage.as_ref()).await.unwrap();
    assert!(observer.note_exists("immediate").await.unwrap());
    drop(observer);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn explicit_rollback_discards_repository_writes() {
    let Some((database, storage)) = storage("explicit_rollback_discards_repository_writes").await
    else {
        return;
    };
    let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
    insert_note(transaction.as_ref(), "rolled-back").await;

    transaction.rollback().await.unwrap();

    let observer = StorageBackend::session(storage.as_ref()).await.unwrap();
    assert!(!observer.note_exists("rolled-back").await.unwrap());
    drop(observer);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn dropping_unfinished_transaction_discards_repository_writes() {
    let Some((database, storage)) =
        storage("dropping_unfinished_transaction_discards_repository_writes").await
    else {
        return;
    };
    let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
    insert_note(transaction.as_ref(), "dropped").await;

    drop(transaction);

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate),
    )
    .await
    .expect("writer lock remained held after the transaction was dropped")
    .unwrap();
    insert_note(second.as_ref(), "after-drop").await;
    second.commit().await.unwrap();

    let observer = StorageBackend::session(storage.as_ref()).await.unwrap();
    assert!(!observer.note_exists("dropped").await.unwrap());
    assert!(observer.note_exists("after-drop").await.unwrap());
    drop(observer);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn immediate_transactions_serialize_writers_until_commit() {
    let Some((database, storage)) =
        storage("immediate_transactions_serialize_writers_until_commit").await
    else {
        return;
    };
    let first = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
    let second_storage = Arc::clone(&storage);
    let mut second = Box::pin(async move {
        StorageBackend::begin(second_storage.as_ref(), TransactionMode::Immediate)
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
        .expect("second immediate transaction did not proceed after commit");
    second.rollback().await.unwrap();
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn failed_multi_repository_write_rolls_back_all_postgresql_tables() {
    let Some((database, storage)) =
        storage("failed_multi_repository_write_rolls_back_all_postgresql_tables").await
    else {
        return;
    };
    let session = StorageBackend::session(storage.as_ref()).await.unwrap();
    session
        .insert_label_key("status", "Workflow status")
        .await
        .unwrap();
    drop(session);

    let transaction = StorageBackend::begin(storage.as_ref(), TransactionMode::Immediate)
        .await
        .unwrap();
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

    let pool = database.inspect_pool().await;
    for (table, query) in [
        ("notes", "SELECT count(*)::bigint FROM notes"),
        ("note_chunks", "SELECT count(*)::bigint FROM note_chunks"),
        (
            "embedding_jobs",
            "SELECT count(*)::bigint FROM embedding_jobs",
        ),
        (
            "note_chunk_embeddings",
            "SELECT count(*)::bigint FROM note_chunk_embeddings",
        ),
        ("note_labels", "SELECT count(*)::bigint FROM note_labels"),
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let label_key_count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM label_keys WHERE key = 'status'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(label_key_count, 1);
    pool.close().await;
    database.cleanup(Some(&storage)).await.unwrap();
}
