mod support;

use note_storage::{
    NewNote, StorageBackend, StorageErrorKind, StorageTransaction, TransactionMode,
    UpsertNoteChunk, EMBEDDING_DIMENSION,
};
use note_storage_pg::PgStorage;
use std::sync::Arc;
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
