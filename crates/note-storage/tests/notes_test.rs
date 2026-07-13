use note_core::NoteAttachment;
use note_storage::{get_note, insert_note, insert_note_with_attachments, Storage};

#[tokio::test]
async fn insert_then_get_returns_the_same_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000, 1)
        .await
        .unwrap();
    let fetched = get_note(&conn, "note-1")
        .await
        .unwrap()
        .expect("note should exist");

    assert_eq!(fetched.title, "Title");
    assert_eq!(fetched.content, "Content");
    assert!(fetched.attachments.is_empty());
}

#[tokio::test]
async fn insert_with_attachments_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note_with_attachments(
        &conn,
        "note-1",
        "Title",
        "Content",
        &[NoteAttachment {
            id: "meta".into(),
            path: "./meta.json".into(),
            mime: "application/json".into(),
            description: "metadata".into(),
            content: "{}".into(),
        }],
        1000,
        1000,
        1,
    )
    .await
    .unwrap();
    let fetched = get_note(&conn, "note-1")
        .await
        .unwrap()
        .expect("note should exist");

    assert_eq!(fetched.attachments.len(), 1);
    assert_eq!(fetched.attachments[0].id, "meta");
    assert_eq!(fetched.attachments[0].path, "./meta.json");
    assert_eq!(fetched.attachments[0].description, "metadata");
    assert_eq!(fetched.attachments[0].content, "");
}

#[tokio::test]
async fn get_returns_none_for_missing_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    let fetched = get_note(&conn, "does-not-exist").await.unwrap();
    assert!(fetched.is_none());
}
