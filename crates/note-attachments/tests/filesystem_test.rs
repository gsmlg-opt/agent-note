use note_attachments::{AttachmentStore, FilesystemAttachmentStore};
use note_core::NoteAttachment;
use std::{path::Path, time::Duration};

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

#[tokio::test]
async fn concurrent_prepare_waits_until_the_first_set_is_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let first = store
        .prepare("note-1", &[attachment("./first.txt", b"first")])
        .await
        .unwrap();
    let second_store = store.clone();
    let mut second = tokio::spawn(async move {
        second_store
            .prepare("note-2", &[attachment("./second.txt", b"second")])
            .await
    });

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut second)
        .await
        .is_err());

    first.abort().await.unwrap();
    second.await.unwrap().unwrap().abort().await.unwrap();
}

#[tokio::test]
async fn failed_promotion_restores_the_previous_attachment_set() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare("note-1", &[attachment("./file.txt", b"old")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    let prepared = store
        .prepare("note-1", &[attachment("./file.txt", b"new")])
        .await
        .unwrap();
    let staging_dir = directory_entries(&root)
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".tmp"))
        })
        .expect("staging directory");
    std::fs::remove_dir_all(staging_dir).unwrap();

    assert!(prepared.publish().await.is_err());

    assert_eq!(store.read("note-1", "./file.txt").await.unwrap(), b"old");
    assert_eq!(directory_entries(&root), vec![root.join("note-1")]);
}

#[tokio::test]
async fn dropping_a_prepared_set_cleans_its_staging_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let prepared = store
        .prepare("note-1", &[attachment("./file.txt", b"content")])
        .await
        .unwrap();
    assert_eq!(directory_entries(&root).len(), 1);

    drop(prepared);

    assert!(directory_entries(&root).is_empty());
}

#[tokio::test]
async fn cancelling_prepare_cleans_its_staging_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let payload = vec![7; 1024 * 1024];
    let attachments = (0..16)
        .map(|index| attachment(&format!("./{index}.bin"), &payload))
        .collect::<Vec<_>>();
    let task = tokio::spawn(async move { store.prepare("note-1", &attachments).await });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !directory_entries(&root).is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prepare must create staging data");
    task.abort();
    let join_error = task.await.err().expect("prepare task must be cancelled");
    assert!(join_error.is_cancelled());

    assert!(directory_entries(&root).is_empty());
}

#[tokio::test]
async fn hydrate_observes_one_complete_published_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    store
        .prepare(
            "note-1",
            &[
                attachment("./first.txt", b"old-first"),
                attachment("./second.txt", b"old-second"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    let replacement = store
        .prepare(
            "note-1",
            &[
                attachment("./first.txt", b"new-first"),
                attachment("./second.txt", b"new-second"),
            ],
        )
        .await
        .unwrap();
    let hydrate_store = store.clone();
    let mut hydration = tokio::spawn(async move {
        let mut metadata = vec![
            attachment("./first.txt", b""),
            attachment("./second.txt", b""),
        ];
        hydrate_store
            .hydrate("note-1", &mut metadata)
            .await
            .unwrap();
        metadata
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut hydration)
            .await
            .is_err()
    );
    replacement.publish().await.unwrap();
    let hydrated = hydration.await.unwrap();

    assert_eq!(hydrated[0].content, b"new-first");
    assert_eq!(hydrated[1].content, b"new-second");
}

#[cfg(unix)]
#[tokio::test]
async fn read_rejects_symlinked_note_directories_and_targets() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    std::fs::create_dir_all(root.join("note-1")).unwrap();
    symlink(outside.join("secret.txt"), root.join("note-1/file.txt")).unwrap();
    let store = FilesystemAttachmentStore::new(root.clone());

    let target_error = store.read("note-1", "./file.txt").await.unwrap_err();
    assert!(target_error.to_string().contains("symlink"));

    std::fs::remove_dir_all(root.join("note-1")).unwrap();
    symlink(&outside, root.join("note-1")).unwrap();
    let directory_error = store.read("note-1", "./secret.txt").await.unwrap_err();
    assert!(directory_error.to_string().contains("symlink"));
}
