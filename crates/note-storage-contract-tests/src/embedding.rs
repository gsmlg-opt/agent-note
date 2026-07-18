use crate::unit;
use note_storage::{NewNote, NoteChunk, NoteUpdate, StorageBackend, UpsertNoteChunk};
use std::sync::Arc;

async fn insert_note(storage: &dyn note_storage::StorageSession, id: &str, title: &str) {
    storage
        .insert_note(NewNote {
            id,
            title,
            content: "Content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

async fn upsert_chunk(
    storage: &dyn note_storage::StorageSession,
    note_id: &str,
    chunk_idx: i64,
    hash: &str,
    status: &str,
) {
    storage
        .upsert_note_chunk(UpsertNoteChunk {
            note_id,
            chunk_idx,
            content_hash: hash,
            content: &format!("content-{chunk_idx}"),
            note_revision: 1,
            status,
            updated_at: 10 + chunk_idx,
        })
        .await
        .unwrap();
}

async fn assert_job_queue_empty(storage: &dyn note_storage::StorageSession, now: i64) {
    assert_eq!(
        storage
            .requeue_processing_embedding_jobs(now)
            .await
            .unwrap(),
        0
    );
    assert!(storage
        .claim_pending_embedding_jobs(1, now)
        .await
        .unwrap()
        .is_empty());
}

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    assert_job_queue_empty(session.as_ref(), 1).await;
    insert_note(session.as_ref(), "contract-embedding-chunks", "Chunks").await;
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-chunks",
        1,
        "hash-1",
        "pending",
    )
    .await;
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-chunks",
        0,
        "hash-0",
        "embedded",
    )
    .await;
    let chunks = session
        .list_note_chunks("contract-embedding-chunks")
        .await
        .unwrap();
    assert_eq!(
        chunks,
        vec![
            NoteChunk {
                note_id: "contract-embedding-chunks".into(),
                chunk_idx: 0,
                content_hash: "hash-0".into(),
                content: "content-0".into(),
                note_revision: 1,
                status: "embedded".into(),
                updated_at: 10,
            },
            NoteChunk {
                note_id: "contract-embedding-chunks".into(),
                chunk_idx: 1,
                content_hash: "hash-1".into(),
                content: "content-1".into(),
                note_revision: 1,
                status: "pending".into(),
                updated_at: 11,
            },
        ]
    );
    let chunk = session
        .get_note_chunk("contract-embedding-chunks", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chunk.note_id, "contract-embedding-chunks");
    assert_eq!(chunk.content_hash, "hash-1");
    assert_eq!(chunk.content, "content-1");
    assert_eq!(chunk.note_revision, 1);
    assert_eq!(chunk.status, "pending");
    assert_eq!(chunk.updated_at, 11);
    assert_eq!(
        session
            .mark_note_chunk_status("contract-embedding-chunks", 1, "stale", 1, "embedded", 20,)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .mark_note_chunk_status("contract-embedding-chunks", 1, "hash-1", 2, "embedded", 20,)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .mark_note_chunk_status("contract-embedding-chunks", 1, "hash-1", 1, "embedded", 20,)
            .await
            .unwrap(),
        1
    );

    insert_note(session.as_ref(), "contract-embedding-jobs", "Jobs").await;
    session
        .enqueue_embedding_job("contract-embedding-jobs", 0, "hash-a", "first", 1, 10)
        .await
        .unwrap();
    session
        .enqueue_embedding_job("contract-embedding-jobs", 0, "hash-a", "replacement", 2, 11)
        .await
        .unwrap();
    session
        .enqueue_embedding_job("contract-embedding-jobs", 1, "hash-b", "second", 1, 9)
        .await
        .unwrap();
    assert!(session
        .claim_pending_embedding_jobs(0, 12)
        .await
        .unwrap()
        .is_empty());
    let first_claim = session
        .claim_pending_embedding_jobs(1, 13)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first_claim.chunk_idx, 1);
    assert_eq!(first_claim.attempts, 1);
    let second_claim = session
        .claim_pending_embedding_jobs(1, 14)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_ne!(second_claim.id, first_claim.id);
    assert_eq!(second_claim.note_id, "contract-embedding-jobs");
    assert_eq!(second_claim.chunk_idx, 0);
    assert_eq!(second_claim.content_hash, "hash-a");
    assert_eq!(second_claim.content, "replacement");
    assert_eq!(second_claim.note_revision, 2);
    assert_eq!(second_claim.attempts, 1);
    assert_eq!(
        session.delete_embedding_job(second_claim.id).await.unwrap(),
        1
    );
    assert_eq!(
        session
            .fail_embedding_job(first_claim.id, first_claim.attempts, 2, "retry", 15)
            .await
            .unwrap(),
        1
    );
    let retry = session
        .claim_pending_embedding_jobs(1, 16)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.id == first_claim.id)
        .unwrap();
    assert_eq!(retry.attempts, 2);
    assert_eq!(
        session
            .fail_embedding_job(retry.id, retry.attempts, 2, "terminal", 17)
            .await
            .unwrap(),
        1
    );
    assert!(session
        .claim_pending_embedding_jobs(1, 18)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(session.delete_embedding_job(retry.id).await.unwrap(), 1);
    assert_job_queue_empty(session.as_ref(), 18).await;

    insert_note(
        session.as_ref(),
        "contract-embedding-claim-tie",
        "Claim tie",
    )
    .await;
    for chunk_idx in 0..2 {
        session
            .enqueue_embedding_job(
                "contract-embedding-claim-tie",
                chunk_idx,
                &format!("tie-{chunk_idx}"),
                "tie",
                1,
                19,
            )
            .await
            .unwrap();
    }
    let tied = session.claim_pending_embedding_jobs(2, 20).await.unwrap();
    assert_eq!(tied.len(), 2);
    assert!(tied[0].id < tied[1].id);
    assert_eq!(
        tied.iter().map(|job| job.chunk_idx).collect::<Vec<_>>(),
        vec![0, 1]
    );
    for job in tied {
        session.delete_embedding_job(job.id).await.unwrap();
    }
    assert_job_queue_empty(session.as_ref(), 20).await;

    insert_note(
        session.as_ref(),
        "contract-embedding-idempotent",
        "Idempotent",
    )
    .await;
    session
        .enqueue_embedding_job(
            "contract-embedding-idempotent",
            0,
            "same-hash",
            "original",
            1,
            20,
        )
        .await
        .unwrap();
    session
        .enqueue_embedding_job(
            "contract-embedding-idempotent",
            0,
            "same-hash",
            "updated",
            2,
            21,
        )
        .await
        .unwrap();
    let idempotent = session
        .claim_pending_embedding_jobs(1, 22)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(idempotent.note_id, "contract-embedding-idempotent");
    assert_eq!(idempotent.chunk_idx, 0);
    assert_eq!(idempotent.content_hash, "same-hash");
    assert_eq!(idempotent.content, "updated");
    assert_eq!(idempotent.note_revision, 2);
    session
        .enqueue_embedding_job(
            "contract-embedding-idempotent",
            0,
            "same-hash",
            "must not replace processing",
            3,
            23,
        )
        .await
        .unwrap();
    assert!(session
        .claim_pending_embedding_jobs(1, 24)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session.requeue_processing_embedding_jobs(25).await.unwrap(),
        1
    );
    let mut matching_reclaimed = session.claim_pending_embedding_jobs(2, 26).await.unwrap();
    assert_eq!(matching_reclaimed.len(), 1);
    let idempotent_reclaimed = matching_reclaimed.pop().unwrap();
    assert_eq!(idempotent_reclaimed.id, idempotent.id);
    assert_eq!(
        idempotent_reclaimed.note_id,
        "contract-embedding-idempotent"
    );
    assert_eq!(idempotent_reclaimed.chunk_idx, 0);
    assert_eq!(idempotent_reclaimed.content_hash, "same-hash");
    assert_eq!(idempotent_reclaimed.content, "updated");
    assert_eq!(idempotent_reclaimed.note_revision, 2);
    session
        .delete_embedding_job(idempotent_reclaimed.id)
        .await
        .unwrap();
    assert_job_queue_empty(session.as_ref(), 26).await;

    insert_note(session.as_ref(), "contract-embedding-stale", "Stale").await;
    for (hash, now) in [("old", 30), ("current", 31)] {
        session
            .enqueue_embedding_job("contract-embedding-stale", 0, hash, hash, 1, now)
            .await
            .unwrap();
    }
    let processing = session
        .claim_pending_embedding_jobs(1, 33)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(processing.note_id, "contract-embedding-stale");
    assert_eq!(processing.content_hash, "old");
    assert_eq!(
        session
            .delete_stale_embedding_jobs_for_chunk("contract-embedding-stale", 0, "current")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session.requeue_processing_embedding_jobs(34).await.unwrap(),
        1
    );
    assert_eq!(
        session
            .delete_stale_embedding_jobs_for_chunk("contract-embedding-stale", 0, "current")
            .await
            .unwrap(),
        1
    );
    let current = session
        .claim_pending_embedding_jobs(1, 35)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(current.note_id, "contract-embedding-stale");
    assert_eq!(current.content_hash, "current");
    session.delete_embedding_job(current.id).await.unwrap();
    assert_job_queue_empty(session.as_ref(), 35).await;

    insert_note(session.as_ref(), "contract-embedding-trailing", "Trailing").await;
    for (chunk_idx, now) in [(1, 40), (2, 41)] {
        session
            .enqueue_embedding_job(
                "contract-embedding-trailing",
                chunk_idx,
                &format!("trailing-{chunk_idx}"),
                "content",
                1,
                now,
            )
            .await
            .unwrap();
    }
    let trailing_processing = session
        .claim_pending_embedding_jobs(1, 42)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(trailing_processing.note_id, "contract-embedding-trailing");
    assert_eq!(trailing_processing.chunk_idx, 1);
    assert_eq!(
        session
            .delete_embedding_jobs_from_chunk("contract-embedding-trailing", 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session.requeue_processing_embedding_jobs(43).await.unwrap(),
        1
    );
    let trailing_reclaimed = session
        .claim_pending_embedding_jobs(1, 44)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(trailing_reclaimed.id, trailing_processing.id);
    assert_eq!(
        session
            .delete_embedding_job(trailing_reclaimed.id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .delete_embedding_job(trailing_reclaimed.id)
            .await
            .unwrap(),
        0
    );
    assert_job_queue_empty(session.as_ref(), 44).await;

    let dashboard_before = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(dashboard_before.processing_note, None);
    for (id, title) in [
        ("contract-embedding-dashboard-complete", "Complete"),
        ("contract-embedding-dashboard-partial", "Partial"),
        ("contract-embedding-dashboard-deleted", "Deleted"),
        ("contract-embedding-dashboard-stale", "Stale revision"),
        (
            "contract-embedding-dashboard-processing",
            "Processing contract",
        ),
    ] {
        insert_note(session.as_ref(), id, title).await;
    }
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-complete",
        0,
        "complete",
        "embedded",
    )
    .await;
    session
        .insert_chunk_embedding("contract-embedding-dashboard-complete", 0, &unit(0))
        .await
        .unwrap();
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-partial",
        0,
        "partial-a",
        "embedded",
    )
    .await;
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-partial",
        1,
        "partial-b",
        "pending",
    )
    .await;
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-deleted",
        0,
        "deleted",
        "embedded",
    )
    .await;
    session
        .update_note(NoteUpdate {
            id: "contract-embedding-dashboard-stale",
            title: "Stale revision",
            content: "Content",
            attachments: &[],
            updated_at: 2,
            note_revision: 2,
        })
        .await
        .unwrap();
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-stale",
        0,
        "stale-revision",
        "embedded",
    )
    .await;
    upsert_chunk(
        session.as_ref(),
        "contract-embedding-dashboard-processing",
        0,
        "processing",
        "pending",
    )
    .await;
    session
        .soft_delete_note("contract-embedding-dashboard-deleted", 40)
        .await
        .unwrap();
    let dashboard = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(
        dashboard.embedded_note_count,
        dashboard_before.embedded_note_count + 1
    );
    assert_eq!(dashboard.processing_note, None);
    session
        .enqueue_embedding_job(
            "contract-embedding-dashboard-processing",
            0,
            "processing",
            "content-0",
            1,
            45,
        )
        .await
        .unwrap();
    let dashboard_job = session
        .claim_pending_embedding_jobs(1, 46)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        dashboard_job.note_id,
        "contract-embedding-dashboard-processing"
    );
    let dashboard_processing = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(
        dashboard_processing.embedded_note_count,
        dashboard.embedded_note_count
    );
    let processing_note = dashboard_processing.processing_note.unwrap();
    assert_eq!(
        processing_note.id,
        "contract-embedding-dashboard-processing"
    );
    assert_eq!(processing_note.title, "Processing contract");
    assert_eq!(
        session
            .delete_embedding_job(dashboard_job.id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .embedding_dashboard_status()
            .await
            .unwrap()
            .processing_note,
        None
    );
    assert_job_queue_empty(session.as_ref(), 46).await;
    session
        .clear_note_search_data("contract-embedding-dashboard-complete")
        .await
        .unwrap();

    insert_note(session.as_ref(), "contract-embedding-clear", "Clear").await;
    for chunk_idx in 0..3 {
        let hash = format!("clear-{chunk_idx}");
        upsert_chunk(
            session.as_ref(),
            "contract-embedding-clear",
            chunk_idx,
            &hash,
            "embedded",
        )
        .await;
        session
            .insert_chunk_embedding(
                "contract-embedding-clear",
                chunk_idx,
                &unit(chunk_idx as usize),
            )
            .await
            .unwrap();
        session
            .enqueue_embedding_job(
                "contract-embedding-clear",
                chunk_idx,
                &hash,
                "content",
                1,
                50 + chunk_idx,
            )
            .await
            .unwrap();
    }
    session
        .clear_note_chunk_derived("contract-embedding-clear", 0)
        .await
        .unwrap();
    assert!(!session
        .chunk_embedding_exists("contract-embedding-clear", 0)
        .await
        .unwrap());
    session
        .clear_note_chunks_from_derived("contract-embedding-clear", 2)
        .await
        .unwrap();
    assert!(session
        .chunk_embedding_exists("contract-embedding-clear", 1)
        .await
        .unwrap());
    assert!(!session
        .chunk_embedding_exists("contract-embedding-clear", 2)
        .await
        .unwrap());
    session
        .clear_note_search_data("contract-embedding-clear")
        .await
        .unwrap();
    assert!(session
        .list_note_chunks("contract-embedding-clear")
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .dense_search(&unit(1), 20)
        .await
        .unwrap()
        .iter()
        .all(|id| id != "contract-embedding-clear"));
    assert_job_queue_empty(session.as_ref(), 100).await;
}
