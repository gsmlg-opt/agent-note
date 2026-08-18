use note_attachments::{
    AttachmentStore, DeleteObjectOutcome, FilesystemAttachmentStore, PutObjectRequest,
};

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
    assert_eq!(
        store
            .head_object("objects/01/file.bin")
            .await
            .unwrap()
            .size_bytes,
        7
    );
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
async fn legacy_read_and_delete_are_exact_contained_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    std::fs::create_dir_all(root.join("note-1/nested")).unwrap();
    std::fs::write(root.join("note-1/nested/legacy.txt"), b"legacy").unwrap();
    let store = FilesystemAttachmentStore::new(root);

    assert_eq!(
        store
            .read_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        b"legacy"
    );
    assert_eq!(
        store
            .delete_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        DeleteObjectOutcome::Deleted
    );
    assert_eq!(
        store
            .delete_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        DeleteObjectOutcome::AlreadyAbsent
    );
    assert!(store.delete_legacy("../escape", "file").await.is_err());
    assert!(store.delete_legacy("note-1", "../escape").await.is_err());
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

#[cfg(unix)]
#[tokio::test]
async fn immutable_put_rejects_a_symlinked_configured_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, &root).unwrap();
    let store = FilesystemAttachmentStore::new(root);
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

    let error = store
        .put_immutable(object_request("objects/escape", b"payload", checksum))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("root must not be a symlink"));
    assert!(!outside.join("objects/escape").exists());
}

#[tokio::test]
async fn immutable_put_rejects_a_non_directory_configured_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    std::fs::write(&root, b"not a directory").unwrap();
    let store = FilesystemAttachmentStore::new(root);
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

    let error = store
        .put_immutable(object_request("objects/file", b"payload", checksum))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("root path is not a directory"));
}

#[test]
fn info_reports_filesystem_engine_and_configured_root() {
    let root = std::path::PathBuf::from("configured/attachments");
    let store = FilesystemAttachmentStore::new(root.clone());
    let info = store.info();
    assert_eq!(info.engine, "filesystem");
    assert_eq!(info.location.as_deref(), root.to_str());
}
