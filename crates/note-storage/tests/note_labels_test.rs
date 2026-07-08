use note_storage::{attach_label, insert_label_key, insert_note, labels_for_note, Storage};

#[tokio::test]
async fn attach_and_fetch_labels_for_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000)
        .await
        .unwrap();
    insert_label_key(&conn, "status", "Workflow status")
        .await
        .unwrap();
    attach_label(&conn, "note-1", "status", "done")
        .await
        .unwrap();

    let labels = labels_for_note(&conn, "note-1").await.unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].key, "status");
    assert_eq!(labels[0].value, "done");
    assert_eq!(labels[0].description, "Workflow status");
}

#[tokio::test]
async fn attaching_unknown_key_fails() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000)
        .await
        .unwrap();
    let result = attach_label(&conn, "note-1", "does-not-exist", "value").await;
    assert!(result.is_err());
}
