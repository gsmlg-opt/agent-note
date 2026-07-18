use note_attachments::{AttachmentStore, FilesystemAttachmentStore};
use note_core::NoteAttachment;
use std::path::Path;

fn attachment(path: &str, content: &[u8]) -> NoteAttachment {
    NoteAttachment {
        id: path.to_string(),
        path: path.to_string(),
        mime: "text/plain".to_string(),
        description: "test attachment".to_string(),
        content: content.to_vec(),
    }
}

fn directory_entries(path: &Path) -> Vec<std::path::PathBuf> {
    if !path.exists() {
        return Vec::new();
    }

    std::fs::read_dir(path)
        .expect("read attachment root")
        .map(|entry| entry.expect("read directory entry").path())
        .collect()
}

#[tokio::test]
async fn prepare_stages_files_and_publish_makes_them_readable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let attachments = vec![attachment("./file.txt", b"content")];

    let prepared = store.prepare("note-1", &attachments).await.unwrap();

    assert!(!root.join("note-1/file.txt").exists());
    assert_eq!(prepared.metadata()[0].content, Vec::<u8>::new());

    prepared.publish().await.unwrap();

    assert_eq!(
        store.read("note-1", "./file.txt").await.unwrap(),
        b"content"
    );
}

#[tokio::test]
async fn abort_removes_staged_data_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let attachments = vec![attachment("./file.txt", b"content")];

    let prepared = store.prepare("note-1", &attachments).await.unwrap();
    assert_eq!(directory_entries(&root).len(), 1);

    prepared.abort().await.unwrap();

    assert!(directory_entries(&root).is_empty());
    assert!(!root.join("note-1").exists());
}

#[tokio::test]
async fn replacement_publication_removes_obsolete_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());

    store
        .prepare("note-1", &[attachment("./old.txt", b"old")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    store
        .prepare("note-1", &[attachment("./new.txt", b"new")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    assert!(!root.join("note-1/old.txt").exists());
    assert_eq!(store.read("note-1", "./new.txt").await.unwrap(), b"new");
}

#[tokio::test]
async fn remove_note_removes_the_whole_note_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare("note-1", &[attachment("./nested/file.txt", b"content")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    store.remove_note("note-1").await.unwrap();

    assert!(!root.join("note-1").exists());
}

#[tokio::test]
async fn traversal_and_absolute_attachment_paths_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root);

    for path in ["../secret.txt", "/tmp/secret.txt"] {
        let error = store
            .prepare("note-1", &[attachment(path, b"sensitive payload")])
            .await
            .err()
            .expect("invalid path must fail");
        assert!(error.to_string().contains("invalid attachment path"));
        assert!(!error.to_string().contains("sensitive payload"));
    }
}

#[tokio::test]
async fn platform_prefix_attachment_paths_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));

    for path in [r"C:\Windows\secret.txt", r"\\server\share\secret.txt"] {
        let error = store
            .prepare("note-1", &[attachment(path, b"content")])
            .await
            .err()
            .expect("platform prefix must fail");
        assert!(error.to_string().contains("invalid attachment path"));
    }
}

#[tokio::test]
async fn invalid_note_ids_cannot_escape_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root);

    for note_id in ["../outside", "/tmp/outside", ".", "nested/note"] {
        let error = store
            .prepare(note_id, &[attachment("./file.txt", b"content")])
            .await
            .err()
            .expect("invalid note id must fail");
        assert!(error.to_string().contains("invalid note id"));
    }
}

#[tokio::test]
async fn failed_prepare_cleans_up_partial_staging_data() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let attachments = vec![
        attachment("./written-first.txt", b"first"),
        attachment("../invalid.txt", b"must not leak"),
    ];

    let error = store
        .prepare("note-1", &attachments)
        .await
        .err()
        .expect("invalid attachment path must fail");

    assert!(error.to_string().contains("invalid attachment path"));
    assert!(!error.to_string().contains("must not leak"));
    assert!(directory_entries(&root).is_empty());
}

#[test]
fn info_reports_filesystem_engine_and_configured_root() {
    let root = std::path::PathBuf::from("configured/attachments");
    let store = FilesystemAttachmentStore::new(root.clone());

    let info = store.info();

    assert_eq!(info.engine, "filesystem");
    assert_eq!(info.location.as_deref(), root.to_str());
}

#[tokio::test]
async fn empty_attachment_set_replaces_old_set_with_no_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare("note-1", &[attachment("./old.txt", b"old")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    let prepared = store.prepare("note-1", &[]).await.unwrap();
    assert!(prepared.metadata().is_empty());
    prepared.publish().await.unwrap();

    assert!(!root.join("note-1").exists());
}
