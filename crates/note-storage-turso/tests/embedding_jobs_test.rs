mod support;

use note_storage::{EmbeddingRepository, NewNote, NotesRepository, UpsertNoteChunk};
use note_storage_turso::TursoSession;
use support::{fixture, insert_test_note};

async fn insert_named_note(session: &TursoSession, id: &str, title: &str) {
    session
        .insert_note(NewNote {
            id,
            title,
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

async fn add_chunk(session: &TursoSession, note_id: &str, chunk_idx: i64, status: &str) {
    session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id,
            chunk_idx,
            content_hash: &format!("{note_id}-{chunk_idx}"),
            content: "Content",
            note_revision: 1,
            status,
            updated_at: 1000,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn dashboard_status_counts_complete_notes_and_reports_only_processing_jobs() {
    let fixture = fixture().await;
    for (id, title) in [
        ("embedded", "Embedded"),
        ("partial", "Partial"),
        ("failed", "Failed"),
        ("zero-chunks", "Zero chunks"),
        ("deleted", "Deleted"),
        ("processing", "Processing note"),
    ] {
        insert_named_note(&fixture.session, id, title).await;
    }

    add_chunk(&fixture.session, "embedded", 0, "embedded").await;
    add_chunk(&fixture.session, "embedded", 1, "embedded").await;
    add_chunk(&fixture.session, "partial", 0, "embedded").await;
    add_chunk(&fixture.session, "partial", 1, "pending").await;
    add_chunk(&fixture.session, "failed", 0, "failed").await;
    add_chunk(&fixture.session, "deleted", 0, "embedded").await;
    fixture
        .session
        .soft_delete_note("deleted", 1000)
        .await
        .unwrap();
    add_chunk(&fixture.session, "processing", 0, "pending").await;
    fixture
        .session
        .enqueue_embedding_job("processing", 0, "processing-0", "Content", 1, 1000)
        .await
        .unwrap();

    let queued = fixture.session.embedding_dashboard_status().await.unwrap();
    assert_eq!(queued.embedded_note_count, 1);
    assert!(queued.processing_note.is_none());

    assert_eq!(
        fixture
            .session
            .claim_pending_embedding_jobs(1, 1001)
            .await
            .unwrap()
            .len(),
        1
    );
    let processing = fixture.session.embedding_dashboard_status().await.unwrap();
    assert_eq!(processing.embedded_note_count, 1);
    let note = processing.processing_note.expect("processing note");
    assert_eq!(note.id, "processing");
    assert_eq!(note.title, "Processing note");
}

#[tokio::test]
async fn chunk_crud_preserves_ordering_and_conditional_status_updates() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    add_chunk(&fixture.session, "note-1", 1, "pending").await;
    add_chunk(&fixture.session, "note-1", 0, "embedded").await;

    let chunks = fixture.session.list_note_chunks("note-1").await.unwrap();
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.chunk_idx)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        fixture.session.get_note_chunk("note-1", 9).await.unwrap(),
        None
    );

    fixture
        .session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "note-1",
            chunk_idx: 1,
            content_hash: "updated-hash",
            content: "Updated content",
            note_revision: 2,
            status: "pending",
            updated_at: 2000,
        })
        .await
        .unwrap();
    let updated = fixture
        .session
        .get_note_chunk("note-1", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.content_hash, "updated-hash");
    assert_eq!(updated.content, "Updated content");
    assert_eq!(updated.note_revision, 2);
    assert_eq!(updated.updated_at, 2000);

    assert_eq!(
        fixture
            .session
            .mark_note_chunk_status("note-1", 1, "wrong-hash", 2, "embedded", 3000)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .session
            .mark_note_chunk_status("note-1", 1, "updated-hash", 1, "embedded", 3000)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .session
            .mark_note_chunk_status("note-1", 1, "updated-hash", 2, "embedded", 3000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .session
            .get_note_chunk("note-1", 1)
            .await
            .unwrap()
            .unwrap()
            .status,
        "embedded"
    );

    assert_eq!(
        fixture
            .session
            .delete_note_chunks_from("note-1", 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .session
            .list_note_chunks("note-1")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn processing_job_is_not_overwritten_and_requeue_increments_attempts() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Original", 1, 1)
        .await
        .unwrap();

    let first = fixture
        .session
        .claim_pending_embedding_jobs(1, 2)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.attempts, 1);
    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Replacement", 2, 3)
        .await
        .unwrap();
    assert!(fixture
        .session
        .claim_pending_embedding_jobs(1, 4)
        .await
        .unwrap()
        .is_empty());

    assert_eq!(
        fixture
            .session
            .requeue_processing_embedding_jobs(5)
            .await
            .unwrap(),
        1
    );
    let reclaimed = fixture
        .session
        .claim_pending_embedding_jobs(1, 6)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(reclaimed.id, first.id);
    assert_eq!(reclaimed.content, "Original");
    assert_eq!(reclaimed.note_revision, 1);
    assert_eq!(reclaimed.attempts, 2);
    assert_eq!(
        fixture
            .session
            .delete_embedding_job(reclaimed.id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .session
            .delete_embedding_job(reclaimed.id)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn stale_and_trailing_cleanup_preserve_processing_jobs() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    for (hash, now) in [("old-a", 1), ("old-b", 2), ("current", 3)] {
        fixture
            .session
            .enqueue_embedding_job("note-1", 0, hash, hash, 1, now)
            .await
            .unwrap();
    }

    let processing = fixture
        .session
        .claim_pending_embedding_jobs(1, 4)
        .await
        .unwrap();
    assert_eq!(processing[0].content_hash, "old-a");
    assert_eq!(
        fixture
            .session
            .delete_stale_embedding_jobs_for_chunk("note-1", 0, "current")
            .await
            .unwrap(),
        1
    );
    let remaining = fixture
        .session
        .claim_pending_embedding_jobs(10, 5)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].content_hash, "current");
    assert_eq!(
        fixture
            .session
            .requeue_processing_embedding_jobs(6)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        fixture
            .session
            .delete_embedding_jobs_from_chunk("note-1", 0)
            .await
            .unwrap(),
        2
    );
    assert!(fixture
        .session
        .claim_pending_embedding_jobs(10, 7)
        .await
        .unwrap()
        .is_empty());

    fixture
        .session
        .enqueue_embedding_job("note-1", 1, "one", "one", 1, 8)
        .await
        .unwrap();
    fixture
        .session
        .enqueue_embedding_job("note-1", 2, "two", "two", 1, 9)
        .await
        .unwrap();
    let processing = fixture
        .session
        .claim_pending_embedding_jobs(1, 10)
        .await
        .unwrap();
    assert_eq!(processing[0].chunk_idx, 1);
    assert_eq!(
        fixture
            .session
            .delete_embedding_jobs_from_chunk("note-1", 1)
            .await
            .unwrap(),
        1
    );
    fixture
        .session
        .requeue_processing_embedding_jobs(11)
        .await
        .unwrap();
    let remaining = fixture
        .session
        .claim_pending_embedding_jobs(10, 12)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].chunk_idx, 1);
}

#[tokio::test]
async fn failed_jobs_requeue_below_the_attempt_limit_and_stop_at_the_limit() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Content", 1, 1)
        .await
        .unwrap();

    let first = fixture
        .session
        .claim_pending_embedding_jobs(1, 2)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.attempts, 1);
    assert_eq!(
        fixture
            .session
            .fail_embedding_job(first.id, first.attempts, 2, "retry", 3)
            .await
            .unwrap(),
        1
    );
    let second = fixture
        .session
        .claim_pending_embedding_jobs(1, 4)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(second.attempts, 2);
    fixture
        .session
        .fail_embedding_job(second.id, second.attempts, 2, "terminal", 5)
        .await
        .unwrap();
    assert!(fixture
        .session
        .claim_pending_embedding_jobs(1, 6)
        .await
        .unwrap()
        .is_empty());

    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Updated", 2, 7)
        .await
        .unwrap();
    let retried = fixture
        .session
        .claim_pending_embedding_jobs(1, 8)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(retried.attempts, 3);
    assert_eq!(retried.content, "Updated");
    assert_eq!(retried.note_revision, 2);
}

#[tokio::test]
async fn zero_limit_does_not_claim_a_pending_job() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Content", 1, 1)
        .await
        .unwrap();

    assert!(fixture
        .session
        .claim_pending_embedding_jobs(0, 2)
        .await
        .unwrap()
        .is_empty());
    let claimed = fixture
        .session
        .claim_pending_embedding_jobs(1, 3)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_claims_only_return_a_pending_job_once() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .enqueue_embedding_job("note-1", 0, "hash", "Content", 1, 1)
        .await
        .unwrap();
    let second = fixture.storage.connect().await.unwrap();

    let (first, second) = tokio::join!(
        fixture.session.claim_pending_embedding_jobs(1, 2),
        second.claim_pending_embedding_jobs(1, 2)
    );
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first.len() + second.len(), 1);
}
