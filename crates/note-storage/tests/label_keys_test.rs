use note_storage::{Storage, insert_label_key, list_label_keys};

#[tokio::test]
async fn insert_then_list_returns_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_label_key(&conn, "status", "Workflow status of the note").await.unwrap();
    let keys = list_label_keys(&conn).await.unwrap();

    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].description, "Workflow status of the note");
}

#[tokio::test]
async fn duplicate_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_label_key(&conn, "status", "first").await.unwrap();
    let result = insert_label_key(&conn, "status", "second").await;
    assert!(result.is_err());
}
