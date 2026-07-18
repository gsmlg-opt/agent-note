use crate::unit;
use note_storage::{
    NewNote, StorageBackend, StorageErrorKind, StorageTransaction, TransactionMode, UpsertNoteChunk,
};
use std::sync::Arc;
use std::time::Duration;

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

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let deferred = storage.begin(TransactionMode::Deferred).await.unwrap();
    insert_note(deferred.as_ref(), "contract-transactions-deferred").await;
    deferred.commit().await.unwrap();
    assert!(storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-deferred")
        .await
        .unwrap());

    let immediate = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(immediate.as_ref(), "contract-transactions-immediate").await;
    immediate.commit().await.unwrap();
    assert!(storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-immediate")
        .await
        .unwrap());

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(rollback.as_ref(), "contract-transactions-rollback").await;
    rollback.rollback().await.unwrap();
    assert!(!storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-rollback")
        .await
        .unwrap());

    let dropped = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(dropped.as_ref(), "contract-transactions-dropped").await;
    drop(dropped);
    let after_drop = tokio::time::timeout(
        Duration::from_secs(2),
        storage.begin(TransactionMode::Immediate),
    )
    .await
    .expect("writer lock remained held after dropping an unfinished transaction")
    .unwrap();
    after_drop.rollback().await.unwrap();
    assert!(!storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-dropped")
        .await
        .unwrap());

    let session = storage.session().await.unwrap();
    session
        .insert_label_key("contract-transactions-status", "Status")
        .await
        .unwrap();
    let atomic = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(atomic.as_ref(), "contract-transactions-atomic").await;
    atomic
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "contract-transactions-atomic",
            chunk_idx: 0,
            content_hash: "atomic-hash",
            content: "content",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    atomic
        .enqueue_embedding_job(
            "contract-transactions-atomic",
            0,
            "atomic-hash",
            "content",
            1,
            1,
        )
        .await
        .unwrap();
    atomic
        .insert_chunk_embedding("contract-transactions-atomic", 0, &unit(0))
        .await
        .unwrap();
    atomic
        .attach_label(
            "contract-transactions-atomic",
            "contract-transactions-status",
            "ready",
        )
        .await
        .unwrap();
    let duplicate = atomic
        .attach_label(
            "contract-transactions-atomic",
            "contract-transactions-status",
            "duplicate",
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.kind(), StorageErrorKind::Constraint);
    atomic.rollback().await.unwrap();

    let observer = storage.session().await.unwrap();
    assert!(!observer
        .note_exists("contract-transactions-atomic")
        .await
        .unwrap());
    assert!(observer
        .list_note_chunks("contract-transactions-atomic")
        .await
        .unwrap()
        .is_empty());
    assert!(!observer
        .chunk_embedding_exists("contract-transactions-atomic", 0)
        .await
        .unwrap());
    assert!(observer
        .labels_for_note("contract-transactions-atomic")
        .await
        .unwrap()
        .is_empty());
    assert!(observer
        .claim_pending_embedding_jobs(100, 2)
        .await
        .unwrap()
        .iter()
        .all(|job| job.note_id != "contract-transactions-atomic"));

    let first = storage.begin(TransactionMode::Immediate).await.unwrap();
    let second_storage = storage.clone();
    let mut second = Box::pin(async move {
        second_storage
            .begin(TransactionMode::Immediate)
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second.as_mut())
            .await
            .is_err()
    );
    first.commit().await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("second immediate transaction did not proceed after commit");
    second.rollback().await.unwrap();
}
