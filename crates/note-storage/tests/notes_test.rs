use note_storage::{Storage, insert_note, get_note};

#[tokio::test]
async fn insert_then_get_returns_the_same_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000).await.unwrap();
    let fetched = get_note(&conn, "note-1").await.unwrap().expect("note should exist");

    assert_eq!(fetched.title, "Title");
    assert_eq!(fetched.content, "Content");
}

#[tokio::test]
async fn get_returns_none_for_missing_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    let fetched = get_note(&conn, "does-not-exist").await.unwrap();
    assert!(fetched.is_none());
}
