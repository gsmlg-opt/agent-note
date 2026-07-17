use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;

#[tokio::test]
async fn backend_info_reports_embed_database_location_and_size() {
    let dir = tempfile::Builder::new()
        .prefix("agent-note-backend-")
        .tempdir_in(".")
        .unwrap();
    let current_dir = std::env::current_dir().unwrap();
    let relative_path = dir
        .path()
        .strip_prefix(&current_dir)
        .unwrap()
        .join("backend.db");
    assert!(relative_path.is_relative());
    let storage = TursoStorage::open(&relative_path).await.unwrap();

    let info = storage.info().await.unwrap();

    assert_eq!(info.engine, "embed");
    let location = info.location.expect("embedded database has a location");
    assert!(location.is_absolute());
    assert_eq!(
        location.canonicalize().unwrap(),
        relative_path.canonicalize().unwrap()
    );
    assert!(info.size_bytes.expect("embedded database has a size") > 0);
}
