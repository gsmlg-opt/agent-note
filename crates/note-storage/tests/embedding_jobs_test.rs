use note_storage::{
    claim_pending_embedding_jobs, delete_note, embedding_dashboard_status, enqueue_embedding_job,
    insert_note, upsert_note_chunk, Storage, UpsertNoteChunk,
};

async fn add_chunk(conn: &libsql::Connection, note_id: &str, chunk_idx: i64, status: &str) {
    upsert_note_chunk(
        conn,
        UpsertNoteChunk {
            note_id,
            chunk_idx,
            content_hash: &format!("{note_id}-{chunk_idx}"),
            content: "Content",
            note_revision: 1,
            status,
            updated_at: 1000,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn dashboard_status_counts_complete_notes_and_reports_only_processing_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();
    for (id, title) in [
        ("embedded", "Embedded"),
        ("partial", "Partial"),
        ("failed", "Failed"),
        ("zero-chunks", "Zero chunks"),
        ("deleted", "Deleted"),
        ("processing", "Processing note"),
    ] {
        insert_note(&conn, id, title, "Content", 1000, 1000, 1)
            .await
            .unwrap();
    }

    add_chunk(&conn, "embedded", 0, "embedded").await;
    add_chunk(&conn, "embedded", 1, "embedded").await;
    add_chunk(&conn, "partial", 0, "embedded").await;
    add_chunk(&conn, "partial", 1, "pending").await;
    add_chunk(&conn, "failed", 0, "failed").await;
    add_chunk(&conn, "deleted", 0, "embedded").await;
    delete_note(&conn, "deleted", 1000).await.unwrap();
    add_chunk(&conn, "processing", 0, "pending").await;
    enqueue_embedding_job(&conn, "processing", 0, "processing-0", "Content", 1, 1000)
        .await
        .unwrap();

    let queued = embedding_dashboard_status(&conn).await.unwrap();
    assert_eq!(queued.embedded_note_count, 1);
    assert!(queued.processing_note.is_none());

    assert_eq!(
        claim_pending_embedding_jobs(&conn, 1, 1001)
            .await
            .unwrap()
            .len(),
        1
    );
    let processing = embedding_dashboard_status(&conn).await.unwrap();
    assert_eq!(processing.embedded_note_count, 1);
    let note = processing.processing_note.expect("processing note");
    assert_eq!(note.id, "processing");
    assert_eq!(note.title, "Processing note");
}
