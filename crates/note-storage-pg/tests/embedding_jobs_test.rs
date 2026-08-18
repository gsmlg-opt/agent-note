#[allow(dead_code)]
mod support;

use note_storage::{
    EmbeddingRepository, NewNote, NotesRepository, RetrievalRepository, SettingsRepository,
    StorageBackend, TransactionMode, UpsertNoteChunk, EMBEDDING_DIMENSION,
};
use note_storage_pg::{PgSession, PgStorage};
use support::{configured_url_or_skip, TestDatabase};

async fn storage_for(test_name: &str) -> Option<(TestDatabase, PgStorage)> {
    let admin_url = configured_url_or_skip(test_name)?;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    Some((database, storage))
}

fn unit(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; EMBEDDING_DIMENSION];
    vector[index] = 1.0;
    vector
}

async fn insert_test_note(session: &PgSession, id: &str) {
    session
        .insert_note(NewNote {
            id,
            title: id,
            content: "Content",
            attachments: &[],
            created_at: 1000,
            updated_at: 1000,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

async fn add_chunk(session: &PgSession, note_id: &str, status: &str) {
    session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id,
            chunk_idx: 0,
            content_hash: &format!("{note_id}-0"),
            content: "Content",
            note_revision: 1,
            status,
            updated_at: 1000,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn reset_embeddings_requeues_only_active_chunks() {
    let Some((database, storage)) =
        storage_for("reset_embeddings_requeues_only_active_chunks").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();
    insert_test_note(&session, "active").await;
    insert_test_note(&session, "deleted").await;
    add_chunk(&session, "active", "embedded").await;
    add_chunk(&session, "deleted", "embedded").await;
    session.soft_delete_note("deleted", 1, 1500).await.unwrap();
    session
        .insert_chunk_embedding("active", 0, &unit(0))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("deleted", 0, &unit(1))
        .await
        .unwrap();
    for note_id in ["active", "deleted"] {
        session
            .enqueue_embedding_job(
                note_id,
                0,
                &format!("{note_id}-0"),
                "stale content",
                1,
                1600,
            )
            .await
            .unwrap();
    }
    let old_jobs = session
        .claim_pending_embedding_jobs(10, 1700)
        .await
        .unwrap();
    assert_eq!(old_jobs.len(), 2);
    assert!(old_jobs.iter().all(|job| job.attempts == 1));
    let old_job_ids = old_jobs.iter().map(|job| job.id).collect::<Vec<_>>();

    assert_eq!(
        session
            .reset_embeddings_for_regeneration(2000)
            .await
            .unwrap(),
        1
    );

    assert!(!session.chunk_embedding_exists("active", 0).await.unwrap());
    assert!(!session.chunk_embedding_exists("deleted", 0).await.unwrap());
    let active_chunk = session.get_note_chunk("active", 0).await.unwrap().unwrap();
    assert_eq!(active_chunk.status, "pending");
    assert_eq!(active_chunk.updated_at, 2000);
    let deleted_chunk = session.get_note_chunk("deleted", 0).await.unwrap().unwrap();
    assert_eq!(deleted_chunk.status, "embedded");
    assert_eq!(deleted_chunk.updated_at, 1000);

    let jobs = session
        .claim_pending_embedding_jobs(10, 2001)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].note_id, "active");
    assert_eq!(jobs[0].attempts, 1);
    assert!(!old_job_ids.contains(&jobs[0].id));
    assert!(session
        .claim_pending_embedding_jobs(10, 2002)
        .await
        .unwrap()
        .is_empty());

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up embedding reset test database");
}

#[tokio::test]
async fn rolled_back_embedding_reset_preserves_fingerprint_vectors_and_jobs() {
    let Some((database, storage)) =
        storage_for("rolled_back_embedding_reset_preserves_fingerprint_vectors_and_jobs").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();
    insert_test_note(&session, "active").await;
    add_chunk(&session, "active", "embedded").await;
    session
        .insert_chunk_embedding("active", 0, &unit(0))
        .await
        .unwrap();
    session
        .enqueue_embedding_job("active", 0, "active-0", "old content", 1, 1000)
        .await
        .unwrap();
    let old_job = session
        .claim_pending_embedding_jobs(1, 1100)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(old_job.attempts, 1);
    session
        .set_embedding_fingerprint("old-model:1024")
        .await
        .unwrap();

    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    assert_eq!(
        transaction
            .reset_embeddings_for_regeneration(2000)
            .await
            .unwrap(),
        1
    );
    transaction
        .set_embedding_fingerprint("bge-m3:1024")
        .await
        .unwrap();
    assert_eq!(
        transaction
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
    assert!(!transaction
        .chunk_embedding_exists("active", 0)
        .await
        .unwrap());
    transaction.rollback().await.unwrap();

    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("old-model:1024")
    );
    assert!(session.chunk_embedding_exists("active", 0).await.unwrap());
    let preserved_chunk = session.get_note_chunk("active", 0).await.unwrap().unwrap();
    assert_eq!(preserved_chunk.status, "embedded");
    assert_eq!(preserved_chunk.updated_at, 1000);
    assert_eq!(
        session
            .requeue_processing_embedding_jobs(2100)
            .await
            .unwrap(),
        1
    );
    let preserved_job = session
        .claim_pending_embedding_jobs(10, 2200)
        .await
        .unwrap();
    assert_eq!(preserved_job.len(), 1);
    assert_eq!(preserved_job[0].id, old_job.id);
    assert_eq!(preserved_job[0].attempts, 2);
    assert_eq!(preserved_job[0].content, "old content");

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up embedding reset rollback test database");
}
