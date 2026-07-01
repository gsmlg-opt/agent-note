use note_storage::{Storage, insert_note, insert_sparse_weights, sparse_postings_query};

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
    insert_sparse_weights(&conn, "note-1", &[(42, 0.9)])
        .await
        .unwrap();
    insert_sparse_weights(&conn, "note-2", &[(42, 0.3)])
        .await
        .unwrap();

    let ranked = sparse_postings_query(&conn, &[42], 10).await.unwrap();
    assert_eq!(ranked, vec!["note-1".to_string(), "note-2".to_string()]);
}

use note_storage::{dense_ann_query, insert_embedding};

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

    insert_embedding(&conn, "note-1", &close).await.unwrap();
    insert_embedding(&conn, "note-2", &far).await.unwrap();

    let query = vec![1.0f32; 1024];
    let ranked = dense_ann_query(&conn, &query, 10).await.unwrap();
    assert_eq!(ranked[0], "note-1");
}
