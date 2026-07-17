use note_storage::{
    NewNote, StorageBackend, StorageErrorKind, StorageTransaction, TransactionMode,
};
use note_storage_turso::TursoStorage;
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
