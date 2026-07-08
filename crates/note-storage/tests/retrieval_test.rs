use note_storage::{insert_chunk_sparse_weights, insert_note, sparse_postings_query, Storage};

#[tokio::test]
async fn sparse_query_ranks_by_matching_token_weight() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000)
        .await
        .unwrap();
    insert_note(&conn, "note-2", "B", "content", 1000, 1000)
        .await
        .unwrap();
    insert_chunk_sparse_weights(&conn, "note-1", 0, &[(42, 0.9)])
        .await
        .unwrap();
    insert_chunk_sparse_weights(&conn, "note-2", 0, &[(42, 0.3)])
        .await
        .unwrap();

    let ranked = sparse_postings_query(&conn, &[42], 10).await.unwrap();
    assert_eq!(ranked, vec!["note-1".to_string(), "note-2".to_string()]);
}

#[tokio::test]
async fn sparse_query_ranks_by_best_matching_chunk_per_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000)
        .await
        .unwrap();
    insert_note(&conn, "note-2", "B", "content", 1000, 1000)
        .await
        .unwrap();
    insert_chunk_sparse_weights(&conn, "note-1", 0, &[(42, 0.2)])
        .await
        .unwrap();
    insert_chunk_sparse_weights(&conn, "note-1", 1, &[(42, 0.8)])
        .await
        .unwrap();
    insert_chunk_sparse_weights(&conn, "note-2", 0, &[(42, 0.6), (7, 0.3)])
        .await
        .unwrap();

    let ranked = sparse_postings_query(&conn, &[42, 7], 10).await.unwrap();
    assert_eq!(ranked, vec!["note-2".to_string(), "note-1".to_string()]);
}

use note_storage::{dense_ann_query, insert_chunk_embedding};

#[tokio::test]
async fn dense_query_returns_nearest_neighbors() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000)
        .await
        .unwrap();
    insert_note(&conn, "note-2", "B", "content", 1000, 1000)
        .await
        .unwrap();

    let mut close = vec![1.0f32; 1024];
    close[0] = 1.01;
    let mut far = vec![0.0f32; 1024];
    far[0] = 1.0;

    insert_chunk_embedding(&conn, "note-1", 0, &close)
        .await
        .unwrap();
    insert_chunk_embedding(&conn, "note-2", 0, &far)
        .await
        .unwrap();

    let query = vec![1.0f32; 1024];
    let ranked = dense_ann_query(&conn, &query, 10).await.unwrap();
    assert_eq!(ranked[0], "note-1");
}

#[tokio::test]
async fn dense_query_deduplicates_chunks_to_note_ids() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000)
        .await
        .unwrap();

    let query = vec![1.0f32; 1024];
    let close = vec![1.0f32; 1024];
    let mut also_close = vec![1.0f32; 1024];
    also_close[0] = 1.01;

    insert_chunk_embedding(&conn, "note-1", 0, &close)
        .await
        .unwrap();
    insert_chunk_embedding(&conn, "note-1", 1, &also_close)
        .await
        .unwrap();

    let ranked = dense_ann_query(&conn, &query, 10).await.unwrap();
    assert_eq!(ranked, vec!["note-1".to_string()]);
}
