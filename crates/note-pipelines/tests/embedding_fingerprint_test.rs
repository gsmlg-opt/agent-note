use note_pipelines::{reconcile_embedding_fingerprint, FingerprintReconciliation};
use note_storage::{EmbeddingJob, NewNote, StorageBackend, TransactionMode, UpsertNoteChunk};
use note_storage_turso::TursoStorage;

async fn storage() -> (tempfile::TempDir, TursoStorage) {
    let dir = tempfile::tempdir().unwrap();
    let storage = TursoStorage::open(dir.path().join("test.db"))
        .await
        .unwrap();
    (dir, storage)
}

async fn seed_embedded_chunk_with_job(storage: &TursoStorage) {
    let session = storage.session().await.unwrap();
    session
        .insert_note(NewNote {
            id: "note-1",
            title: "Note",
            content: "Content",
            attachments: &[],
            created_at: 900,
            updated_at: 900,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "note-1",
            chunk_idx: 0,
            content_hash: "hash-1",
            content: "Content",
            note_revision: 1,
            status: "embedded",
            updated_at: 900,
        })
        .await
        .unwrap();
    session
        .insert_chunk_embedding("note-1", 0, &vec![0.0; 1024])
        .await
        .unwrap();
    session
        .enqueue_embedding_job("note-1", 0, "hash-1", "Content", 1, 950)
        .await
        .unwrap();
}

async fn snapshot_pending_job(storage: &TursoStorage, now: i64) -> EmbeddingJob {
    let transaction = storage.begin(TransactionMode::Deferred).await.unwrap();
    let job = transaction
        .claim_pending_embedding_jobs(1, now)
        .await
        .unwrap()
        .pop()
        .unwrap();
    transaction.rollback().await.unwrap();
    job
}

#[tokio::test]
async fn missing_fingerprint_is_initialized_without_changing_vectors_or_jobs() {
    let (_dir, storage) = storage().await;
    seed_embedded_chunk_with_job(&storage).await;
    let expected_job = snapshot_pending_job(&storage, 999).await;

    assert_eq!(
        reconcile_embedding_fingerprint(&storage, "bge-m3:1024", 1000)
            .await
            .unwrap(),
        FingerprintReconciliation::Initialized
    );

    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
    assert!(session.chunk_embedding_exists("note-1", 0).await.unwrap());
    let jobs = session
        .claim_pending_embedding_jobs(10, 1001)
        .await
        .unwrap();
    assert_eq!(jobs, vec![expected_job]);
}

#[tokio::test]
async fn matching_fingerprint_preserves_exact_vectors_and_jobs() {
    let (_dir, storage) = storage().await;
    seed_embedded_chunk_with_job(&storage).await;
    let session = storage.session().await.unwrap();
    session
        .set_embedding_fingerprint("bge-m3:1024")
        .await
        .unwrap();
    drop(session);
    let expected_job = snapshot_pending_job(&storage, 1000).await;

    assert_eq!(
        reconcile_embedding_fingerprint(&storage, "bge-m3:1024", 1001)
            .await
            .unwrap(),
        FingerprintReconciliation::Unchanged
    );

    let session = storage.session().await.unwrap();
    assert!(session.chunk_embedding_exists("note-1", 0).await.unwrap());
    let jobs = session
        .claim_pending_embedding_jobs(10, 1002)
        .await
        .unwrap();
    assert_eq!(jobs, vec![expected_job]);
}

#[tokio::test]
async fn replacing_the_stub_with_bge_m3_resets_vectors_and_requeues_active_chunks() {
    let (_dir, storage) = storage().await;
    seed_embedded_chunk_with_job(&storage).await;
    let session = storage.session().await.unwrap();
    session
        .set_embedding_fingerprint("stub:1024")
        .await
        .unwrap();
    let old_job = session
        .claim_pending_embedding_jobs(1, 1000)
        .await
        .unwrap()
        .pop()
        .unwrap();
    drop(session);

    assert_eq!(
        reconcile_embedding_fingerprint(&storage, "bge-m3:1024", 1002)
            .await
            .unwrap(),
        FingerprintReconciliation::Regenerated { queued_jobs: 1 }
    );

    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
    assert!(!session.chunk_embedding_exists("note-1", 0).await.unwrap());
    let chunk = session.get_note_chunk("note-1", 0).await.unwrap().unwrap();
    assert_eq!(chunk.status, "pending");
    assert_eq!(chunk.updated_at, 1002);
    let jobs = session
        .claim_pending_embedding_jobs(10, 1003)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_ne!(jobs[0].id, old_job.id);
    assert_eq!(jobs[0].note_id, "note-1");
    assert_eq!(jobs[0].content_hash, "hash-1");
    assert_eq!(jobs[0].content, "Content");
    assert_eq!(jobs[0].note_revision, 1);
}

#[tokio::test]
async fn blank_fingerprint_is_rejected_without_starting_reconciliation() {
    let (_dir, storage) = storage().await;

    let error = reconcile_embedding_fingerprint(&storage, " \t\n", 1000)
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "embedding fingerprint must not be blank");
    let session = storage.session().await.unwrap();
    assert_eq!(session.get_embedding_fingerprint().await.unwrap(), None);
}
