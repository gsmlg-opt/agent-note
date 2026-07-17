use note_storage::Storage;

async fn index_sql(storage: &Storage, name: &str) -> String {
    let conn = storage.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            libsql::params![name],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
}

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

    let vector_index = index_sql(&storage, "idx_note_chunk_embedding").await;
    assert!(vector_index.contains("'compress_neighbors=float8'"));
    assert!(vector_index.contains("'max_neighbors=20'"));
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

#[tokio::test]
async fn legacy_vector_index_is_rebuilt_only_by_explicit_optimization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-index.db");
    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    conn.execute("DROP INDEX idx_note_chunk_embedding", ())
        .await
        .unwrap();
    conn.execute(
        "CREATE INDEX idx_note_chunk_embedding
         ON note_chunk_embeddings(libsql_vector_idx(embedding))",
        (),
    )
    .await
    .unwrap();
    note_storage::insert_note(&conn, "note-1", "A", "content", 1, 1, 1)
        .await
        .unwrap();
    let embedding = vec![1.0f32; 1024];
    note_storage::insert_chunk_embedding(&conn, "note-1", 0, &embedding)
        .await
        .unwrap();
    drop(conn);
    drop(storage);

    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    assert!(!index_sql(&storage, "idx_note_chunk_embedding")
        .await
        .contains("compress_neighbors"));
    assert!(storage.optimize_chunk_vector_index().await.unwrap());
    assert!(!storage.optimize_chunk_vector_index().await.unwrap());

    let vector_index = index_sql(&storage, "idx_note_chunk_embedding").await;
    assert!(vector_index.contains("'compress_neighbors=float8'"));
    assert!(vector_index.contains("'max_neighbors=20'"));

    let conn = storage.connect().unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM note_chunk_embeddings", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    assert_eq!(
        note_storage::dense_ann_query(&conn, &embedding, 1)
            .await
            .unwrap(),
        vec!["note-1".to_string()]
    );
}
