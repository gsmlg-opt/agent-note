use crate::unit;
use note_storage::{NewNote, StorageBackend, UpsertNoteChunk};
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

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
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
        chunks
            .iter()
            .map(|chunk| chunk.chunk_idx)
            .collect::<Vec<_>>(),
        vec![0, 1]
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
    assert!(session
        .claim_pending_embedding_jobs(1, 14)
        .await
        .unwrap()
        .iter()
        .all(|job| job.id != first_claim.id));
    assert_eq!(
        session
            .fail_embedding_job(first_claim.id, first_claim.attempts, 2, "retry", 15)
            .await
            .unwrap(),
        1
    );
    let retry = session
        .claim_pending_embedding_jobs(10, 16)
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
        .claim_pending_embedding_jobs(10, 18)
        .await
        .unwrap()
        .iter()
        .all(|job| job.id != retry.id));

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
        .claim_pending_embedding_jobs(100, 22)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.note_id == "contract-embedding-idempotent")
        .unwrap();
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
        .claim_pending_embedding_jobs(100, 24)
        .await
        .unwrap()
        .iter()
        .all(|job| job.id != idempotent.id));
    session.requeue_processing_embedding_jobs(25).await.unwrap();
    let idempotent_reclaimed = session
        .claim_pending_embedding_jobs(100, 26)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.id == idempotent.id)
        .unwrap();
    assert_eq!(idempotent_reclaimed.content, "updated");
    assert_eq!(idempotent_reclaimed.note_revision, 2);
    session
        .delete_embedding_job(idempotent_reclaimed.id)
        .await
        .unwrap();

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
    session.requeue_processing_embedding_jobs(34).await.unwrap();
    assert_eq!(
        session
            .delete_stale_embedding_jobs_for_chunk("contract-embedding-stale", 0, "current")
            .await
            .unwrap(),
        1
    );
    let current = session
        .claim_pending_embedding_jobs(100, 35)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.note_id == "contract-embedding-stale" && job.content_hash == "current")
        .unwrap();
    session.delete_embedding_job(current.id).await.unwrap();

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
    session.requeue_processing_embedding_jobs(43).await.unwrap();
    let trailing_reclaimed = session
        .claim_pending_embedding_jobs(100, 44)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.id == trailing_processing.id)
        .unwrap();
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

    let dashboard_before = session.embedding_dashboard_status().await.unwrap();
    for (id, title) in [
        ("contract-embedding-dashboard-complete", "Complete"),
        ("contract-embedding-dashboard-partial", "Partial"),
        ("contract-embedding-dashboard-deleted", "Deleted"),
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
        .soft_delete_note("contract-embedding-dashboard-deleted", 40)
        .await
        .unwrap();
    let dashboard = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(
        dashboard.embedded_note_count,
        dashboard_before.embedded_note_count + 1
    );
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
    assert!(session
        .claim_pending_embedding_jobs(100, 100)
        .await
        .unwrap()
        .iter()
        .all(|job| job.note_id != "contract-embedding-clear"));
}
