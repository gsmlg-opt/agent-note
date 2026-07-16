use note_core::NoteAttachment;
use note_storage::{
    count_notes, delete_note, get_note, insert_note, insert_note_with_attachments,
    list_deleted_note_summaries, list_expired_deleted_note_ids, list_note_summaries, list_notes,
    note_exists, permanently_delete_note, restore_note, Storage,
};

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

#[tokio::test]
async fn deleted_notes_are_only_visible_in_trash_until_permanently_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();
    insert_note(&conn, "active", "Active", "Content", 1000, 1000, 1)
        .await
        .unwrap();
    insert_note(&conn, "deleted", "Deleted", "Content", 1001, 1001, 1)
        .await
        .unwrap();

    assert_eq!(delete_note(&conn, "deleted", 2000).await.unwrap(), 1);
    assert_eq!(delete_note(&conn, "deleted", 2001).await.unwrap(), 0);
    assert!(get_note(&conn, "deleted").await.unwrap().is_none());
    assert!(note_exists(&conn, "deleted").await.unwrap());

    let active = list_notes(&conn, &[], None, None).await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "active");
    let summaries = list_note_summaries(&conn, &[], None, None).await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(count_notes(&conn, &[]).await.unwrap(), 1);

    let deleted = list_deleted_note_summaries(&conn).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, "deleted");
    assert_eq!(deleted[0].deleted_at, Some(2000));

    assert_eq!(restore_note(&conn, "deleted", 2).await.unwrap(), 1);
    assert_eq!(restore_note(&conn, "deleted", 3).await.unwrap(), 0);
    assert!(get_note(&conn, "deleted").await.unwrap().is_some());
    assert_eq!(
        note_storage::get_note_revision(&conn, "deleted")
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(count_notes(&conn, &[]).await.unwrap(), 2);
    assert!(list_deleted_note_summaries(&conn).await.unwrap().is_empty());

    assert_eq!(delete_note(&conn, "deleted", 3000).await.unwrap(), 1);

    assert_eq!(permanently_delete_note(&conn, "deleted").await.unwrap(), 1);
    assert!(!note_exists(&conn, "deleted").await.unwrap());
    assert!(list_deleted_note_summaries(&conn).await.unwrap().is_empty());
    assert_eq!(permanently_delete_note(&conn, "active").await.unwrap(), 0);
}

#[tokio::test]
async fn expired_deleted_note_ids_include_the_ninety_day_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();
    for id in ["active", "older", "boundary", "recent"] {
        insert_note(&conn, id, id, "Content", 1000, 1000, 1)
            .await
            .unwrap();
    }
    delete_note(&conn, "older", 999).await.unwrap();
    delete_note(&conn, "boundary", 1000).await.unwrap();
    delete_note(&conn, "recent", 1001).await.unwrap();

    assert_eq!(
        list_expired_deleted_note_ids(&conn, 1000).await.unwrap(),
        vec!["older", "boundary"]
    );
}
