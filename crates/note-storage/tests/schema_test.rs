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
        "note_chunks",
        "embedding_jobs",
        "note_chunk_embeddings",
        "note_chunk_sparse",
    ] {
        assert!(
            tables.contains(&expected.to_string()),
            "missing table {expected}"
        );
    }

    let mut rows = conn
        .query("PRAGMA table_info(label_keys)", ())
        .await
        .unwrap();
    let mut columns = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(1).unwrap());
    }
    assert!(columns.contains(&"value_type".to_string()));

    let mut rows = conn.query("PRAGMA table_info(notes)", ()).await.unwrap();
    let mut note_columns = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        note_columns.push(row.get::<String>(1).unwrap());
    }
    assert!(note_columns.contains(&"note_revision".to_string()));
    assert!(note_columns.contains(&"attachments".to_string()));
    assert!(note_columns.contains(&"deleted_at".to_string()));

    let mut rows = conn
        .query("PRAGMA table_info(embedding_jobs)", ())
        .await
        .unwrap();
    let mut job_columns = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        job_columns.push(row.get::<String>(1).unwrap());
    }
    assert!(job_columns.contains(&"note_revision".to_string()));
}

#[tokio::test]
async fn schema_reapplies_to_an_existing_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let path = path.to_str().unwrap();

    Storage::open_local(path).await.unwrap();
    Storage::open_local(path).await.unwrap();
}

#[tokio::test]
async fn existing_notes_table_is_migrated_with_deleted_at() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE notes (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            attachments TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            note_revision INTEGER NOT NULL DEFAULT 1
        )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO notes (id, title, content, created_at, updated_at)
         VALUES ('legacy', 'Legacy', 'Content', 1, 1)",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(db);

    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();
    let mut rows = conn
        .query("SELECT deleted_at FROM notes WHERE id = 'legacy'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<Option<i64>>(0).unwrap(), None);
}
