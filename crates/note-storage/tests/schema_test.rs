use note_storage::Storage;

#[tokio::test]
async fn schema_applies_cleanly_to_a_fresh_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    let mut rows = conn
        .query("SELECT name FROM sqlite_master WHERE type='table'", ())
        .await
        .unwrap();
    let mut tables = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(0).unwrap();
        tables.push(name);
    }
    for expected in [
        "notes",
        "label_keys",
        "note_labels",
        "notes_embeddings",
        "notes_sparse_weights",
    ] {
        assert!(tables.contains(&expected.to_string()), "missing table {expected}");
    }
}
