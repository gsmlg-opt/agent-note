use note_attachments::{
    AttachmentStore, DeleteObjectOutcome, FilesystemAttachmentStore, PutObjectRequest,
};
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

fn object_request(object_key: &str, bytes: &[u8], checksum_sha256: &str) -> PutObjectRequest {
    PutObjectRequest {
        object_key: object_key.to_string(),
        bytes: bytes.to_vec(),
        checksum_sha256: checksum_sha256.to_string(),
    }
}

#[tokio::test]
async fn immutable_object_lifecycle_reports_metadata_and_replays_delete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root);
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

    let stored = store
        .put_immutable(object_request("objects/01/file.bin", b"payload", checksum))
        .await
        .unwrap();

    assert_eq!(stored.object_key, "objects/01/file.bin");
    assert_eq!(stored.size_bytes, 7);
    assert_eq!(stored.checksum_sha256, checksum);
    assert_eq!(
        store.read_object("objects/01/file.bin").await.unwrap(),
        b"payload"
    );
    let metadata = store.head_object("objects/01/file.bin").await.unwrap();
    assert_eq!(metadata.size_bytes, 7);
    assert_eq!(metadata.checksum_sha256, checksum);
    assert_eq!(
        store.delete_object("objects/01/file.bin").await.unwrap(),
        DeleteObjectOutcome::Deleted
    );
    assert_eq!(
        store.delete_object("objects/01/file.bin").await.unwrap(),
        DeleteObjectOutcome::AlreadyAbsent
    );
}

#[tokio::test]
async fn immutable_object_put_refuses_collisions_without_changing_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let payload_checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
    let other_checksum = "d9298a10d1b0735837dc4bd85dac641b0f3cef27a47e5d53a54f2f3f5b2fcffa";
    store
        .put_immutable(object_request("objects/key", b"payload", payload_checksum))
        .await
        .unwrap();

    let error = store
        .put_immutable(object_request("objects/key", b"other", other_checksum))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("already exists"));
    assert_eq!(store.read_object("objects/key").await.unwrap(), b"payload");
}

#[tokio::test]
async fn immutable_object_keys_and_checksums_are_validated_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

    for key in ["../escape", "/absolute", "./not-canonical", "C:/prefix"] {
        assert!(store
            .put_immutable(object_request(key, b"payload", checksum))
            .await
            .is_err());
    }
    assert!(store
        .put_immutable(object_request(
            "objects/bad-checksum",
            b"payload",
            "ed7002b439e9ac845f22357d822bac1444730fbdb6016d3ec9432297b9ec9f73"
        ))
        .await
        .is_err());
    assert!(!root.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn immutable_object_operations_reject_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("objects")).unwrap();
    let store = FilesystemAttachmentStore::new(root);
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

    assert!(store
        .put_immutable(object_request("objects/escape", b"payload", checksum))
        .await
        .is_err());
    assert!(store.read_object("objects/escape").await.is_err());
    assert!(store.head_object("objects/escape").await.is_err());
    assert!(store.delete_object("objects/escape").await.is_err());
    assert!(!outside.join("escape").exists());
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
async fn prepared_put_publishes_one_file_without_rewriting_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare(
            "note-1",
            &[
                attachment("./target.txt", b"old"),
                attachment("./sibling.txt", b"sibling"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    let sibling_metadata = std::fs::metadata(root.join("note-1/sibling.txt")).unwrap();
    #[cfg(unix)]
    let sibling_inode = {
        use std::os::unix::fs::MetadataExt;
        sibling_metadata.ino()
    };

    let prepared = store
        .prepare_put("note-1", &attachment("./target.txt", b"new"))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read(root.join("note-1/target.txt")).unwrap(),
        b"old"
    );
    prepared.publish().await.unwrap();

    assert_eq!(store.read("note-1", "./target.txt").await.unwrap(), b"new");
    assert_eq!(
        store.read("note-1", "./sibling.txt").await.unwrap(),
        b"sibling"
    );
    let published_sibling_metadata = std::fs::metadata(root.join("note-1/sibling.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(sibling_inode, published_sibling_metadata.ino());
    }
    assert_eq!(
        sibling_metadata.modified().unwrap(),
        published_sibling_metadata.modified().unwrap()
    );
}

#[tokio::test]
async fn aborted_put_leaves_previous_file_and_siblings_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare(
            "note-1",
            &[
                attachment("./target.txt", b"old"),
                attachment("./sibling.txt", b"sibling"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    store
        .prepare_put("note-1", &attachment("./target.txt", b"new"))
        .await
        .unwrap()
        .abort()
        .await
        .unwrap();

    assert_eq!(store.read("note-1", "./target.txt").await.unwrap(), b"old");
    assert_eq!(
        store.read("note-1", "./sibling.txt").await.unwrap(),
        b"sibling"
    );
    assert_eq!(directory_entries(&root), vec![root.join("note-1")]);
}

#[tokio::test]
async fn prepared_delete_removes_only_selected_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare(
            "note-1",
            &[
                attachment("./nested/target.txt", b"target"),
                attachment("./sibling.txt", b"sibling"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    let prepared = store
        .prepare_delete("note-1", "./nested/target.txt")
        .await
        .unwrap();
    assert!(root.join("note-1/nested/target.txt").exists());

    prepared.publish().await.unwrap();

    assert!(!root.join("note-1/nested/target.txt").exists());
    assert!(!root.join("note-1/nested").exists());
    assert_eq!(
        store.read("note-1", "./sibling.txt").await.unwrap(),
        b"sibling"
    );
}

#[tokio::test]
async fn aborted_delete_keeps_selected_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare("note-1", &[attachment("./target.txt", b"target")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    store
        .prepare_delete("note-1", "./target.txt")
        .await
        .unwrap()
        .abort()
        .await
        .unwrap();

    assert_eq!(
        store.read("note-1", "./target.txt").await.unwrap(),
        b"target"
    );
}

#[tokio::test]
async fn publishing_delete_for_a_missing_file_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare("note-1", &[attachment("./sibling.txt", b"sibling")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    store
        .prepare_delete("note-1", "./missing.txt")
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    assert_eq!(
        store.read("note-1", "./sibling.txt").await.unwrap(),
        b"sibling"
    );
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
    let store = FilesystemAttachmentStore::new(root.clone());

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

    for path in [
        r"C:\Windows\secret.txt",
        "C:/Windows/secret.txt",
        "C:Windows/secret.txt",
        "c:/Windows/secret.txt",
        r"\\server\share\secret.txt",
    ] {
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
async fn concurrent_prepare_for_the_same_note_waits_until_the_first_set_is_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let first = store
        .prepare("note-1", &[attachment("./first.txt", b"first")])
        .await
        .unwrap();
    let second_store = store.clone();
    let mut second = tokio::spawn(async move {
        second_store
            .prepare("note-1", &[attachment("./second.txt", b"second")])
            .await
    });

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut second)
        .await
        .is_err());

    first.abort().await.unwrap();
    second.await.unwrap().unwrap().abort().await.unwrap();
}

#[tokio::test]
async fn prepared_sets_for_different_notes_can_coexist() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let first = store
        .prepare("note-1", &[attachment("./first.txt", b"first")])
        .await
        .unwrap();

    let second = tokio::time::timeout(
        Duration::from_secs(1),
        store.prepare("note-2", &[attachment("./second.txt", b"second")]),
    )
    .await
    .expect("different note preparation must not block")
    .unwrap();

    first.abort().await.unwrap();
    second.abort().await.unwrap();
}

#[tokio::test]
async fn full_set_and_single_object_mutations_share_same_note_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let full_set = store
        .prepare("note-1", &[attachment("./full.txt", b"full")])
        .await
        .unwrap();
    let put_store = store.clone();
    let mut put = tokio::spawn(async move {
        put_store
            .prepare_put("note-1", &attachment("./single.txt", b"single"))
            .await
    });

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut put)
        .await
        .is_err());

    full_set.abort().await.unwrap();
    let put = put.await.unwrap().unwrap();
    let delete_store = store.clone();
    let mut delete =
        tokio::spawn(async move { delete_store.prepare_delete("note-1", "./single.txt").await });

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut delete)
        .await
        .is_err());

    put.abort().await.unwrap();
    delete.await.unwrap().unwrap().abort().await.unwrap();
}

#[tokio::test]
async fn different_note_single_mutations_can_coexist() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
    let put = store
        .prepare_put("note-1", &attachment("./file.txt", b"content"))
        .await
        .unwrap();

    let delete = tokio::time::timeout(
        Duration::from_secs(1),
        store.prepare_delete("note-2", "./file.txt"),
    )
    .await
    .expect("different-note single mutation must not block")
    .unwrap();

    put.abort().await.unwrap();
    delete.abort().await.unwrap();
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
async fn failed_put_promotion_restores_previous_file_and_keeps_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    store
        .prepare(
            "note-1",
            &[
                attachment("./target.txt", b"old"),
                attachment("./sibling.txt", b"sibling"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    let prepared = store
        .prepare_put("note-1", &attachment("./target.txt", b"new"))
        .await
        .unwrap();
    let staging_file = directory_entries(&root)
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".put.tmp"))
        })
        .expect("staged attachment file");
    std::fs::remove_file(staging_file).unwrap();

    assert!(prepared.publish().await.is_err());

    assert_eq!(store.read("note-1", "./target.txt").await.unwrap(), b"old");
    assert_eq!(
        store.read("note-1", "./sibling.txt").await.unwrap(),
        b"sibling"
    );
    assert_eq!(directory_entries(&root), vec![root.join("note-1")]);
}

#[tokio::test]
async fn failed_new_nested_put_promotion_removes_created_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let prepared = store
        .prepare_put("note-1", &attachment("./new/nested/target.txt", b"content"))
        .await
        .unwrap();
    let staging_file = directory_entries(&root)
        .into_iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".put.tmp"))
        })
        .expect("staged attachment file");
    std::fs::remove_file(staging_file).unwrap();

    assert!(prepared.publish().await.is_err());

    assert!(!root.join("note-1").exists());
    assert!(directory_entries(&root).is_empty());
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
async fn dropping_a_prepared_put_cleans_its_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let prepared = store
        .prepare_put("note-1", &attachment("./file.txt", b"content"))
        .await
        .unwrap();
    assert_eq!(directory_entries(&root).len(), 1);

    drop(prepared);

    assert!(directory_entries(&root).is_empty());
    let replacement = tokio::time::timeout(
        Duration::from_secs(1),
        store.prepare_put("note-1", &attachment("./file.txt", b"replacement")),
    )
    .await
    .expect("dropping prepared put must release note lock")
    .unwrap();
    replacement.abort().await.unwrap();
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

#[tokio::test]
async fn single_mutations_reject_unsafe_attachment_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));

    for path in ["../secret.txt", "/tmp/secret.txt", r"C:\Windows\secret.txt"] {
        let put_error = store
            .prepare_put("note-1", &attachment(path, b"sensitive payload"))
            .await
            .err()
            .expect("unsafe put path must fail");
        assert!(put_error.to_string().contains("invalid attachment path"));
        assert!(!put_error.to_string().contains("sensitive payload"));

        let delete_error = store
            .prepare_delete("note-1", path)
            .await
            .err()
            .expect("unsafe delete path must fail");
        assert!(delete_error.to_string().contains("invalid attachment path"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn single_mutations_reject_symlinked_paths() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    std::fs::create_dir_all(root.join("note-1")).unwrap();
    symlink(&outside, root.join("note-1/link")).unwrap();
    let store = FilesystemAttachmentStore::new(root.clone());

    let put_error = store
        .prepare_put("note-1", &attachment("./link/secret.txt", b"replacement"))
        .await
        .err()
        .expect("symlinked put path must fail");
    assert!(put_error.to_string().contains("symlink"));

    let delete_error = store
        .prepare_delete("note-1", "./link/secret.txt")
        .await
        .err()
        .expect("symlinked delete path must fail");
    assert!(delete_error.to_string().contains("symlink"));
    assert_eq!(
        std::fs::read(outside.join("secret.txt")).unwrap(),
        b"secret"
    );

    std::fs::remove_dir_all(root.join("note-1")).unwrap();
    symlink(&outside, root.join("note-1")).unwrap();

    let put_directory_error = store
        .prepare_put("note-1", &attachment("./secret.txt", b"replacement"))
        .await
        .err()
        .expect("symlinked note directory must reject put");
    assert!(put_directory_error.to_string().contains("symlink"));
    let delete_directory_error = store
        .prepare_delete("note-1", "./secret.txt")
        .await
        .err()
        .expect("symlinked note directory must reject delete");
    assert!(delete_directory_error.to_string().contains("symlink"));
}
