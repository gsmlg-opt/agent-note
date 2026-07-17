use note_storage::StorageErrorKind;
use note_storage_turso::TursoStorage;
use std::path::Path;

const APPLICATION_ID: u32 = 0x414E4F54;
const SCHEMA_VERSION: u32 = 1;

fn database_header(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.len() >= 100);
    assert_eq!(&bytes[0..16], b"SQLite format 3\0");
    assert_eq!(
        u32::from_be_bytes(bytes[68..72].try_into().unwrap()),
        APPLICATION_ID
    );
    assert_eq!(
        u32::from_be_bytes(bytes[60..64].try_into().unwrap()),
        SCHEMA_VERSION
    );
    bytes
}

#[tokio::test]
async fn fresh_database_is_marked_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");

    drop(TursoStorage::open(&path).await.unwrap());
    database_header(&path);
    TursoStorage::open(&path).await.unwrap();
}

#[tokio::test]
async fn rollback_journal_unmarked_database_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute("CREATE TABLE legacy (id INTEGER)", ())
        .await
        .unwrap();
    connection.cacheflush().unwrap();
    drop(connection);
    drop(database);
    let mut before = std::fs::read(&path).unwrap();
    before[18] = 1;
    before[19] = 1;
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("unmarked database was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn zero_length_existing_file_is_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.db");
    std::fs::File::create(&path).unwrap();

    drop(TursoStorage::open(&path).await.unwrap());

    database_header(&path);
}

#[tokio::test]
async fn truncated_nonempty_file_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.db");
    let before = b"SQLite format 3\0truncated".to_vec();
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("truncated database was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::Corrupt);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn unsupported_marked_schema_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.db");
    drop(TursoStorage::open(&path).await.unwrap());
    let mut before = std::fs::read(&path).unwrap();
    before[60..64].copy_from_slice(&2_u32.to_be_bytes());
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("unsupported schema was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::UnsupportedSchema);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn fresh_database_contains_the_logical_schema_for_exact_linear_retrieval() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema.db");
    drop(TursoStorage::open(&path).await.unwrap());

    let database = turso::Builder::new_local(path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query(
            "SELECT type, name, sql
             FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
            (),
        )
        .await
        .unwrap();
    let mut tables = Vec::new();
    let mut indexes = Vec::new();
    let mut schema_sql = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let object_type = row.get::<String>(0).unwrap();
        let name = row.get::<String>(1).unwrap();
        if object_type == "table" {
            tables.push(name);
        } else if object_type == "index" {
            indexes.push(name);
        }
        if let Ok(sql) = row.get::<String>(2) {
            schema_sql.push(sql);
        }
    }

    for expected in [
        "notes",
        "label_keys",
        "note_labels",
        "note_chunks",
        "embedding_jobs",
        "note_chunk_embeddings",
        "note_chunk_sparse",
        "app_settings",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "missing table {expected}"
        );
    }
    for expected in [
        "idx_note_labels_key_value",
        "idx_note_chunks_hash",
        "idx_note_chunks_status",
        "idx_embedding_jobs_status",
        "idx_note_chunk_sparse_token",
    ] {
        assert!(
            indexes.iter().any(|index| index == expected),
            "missing index {expected}"
        );
    }

    let schema_sql = schema_sql.join("\n");
    assert!(include_str!("../schema.sql").contains("embedding F32_BLOB(1024) NOT NULL"));
    assert!(schema_sql.contains("embedding F32_BLOB (1024) NOT NULL"));
    assert!(!indexes
        .iter()
        .any(|index| index == "idx_note_chunk_embedding"));
}

#[tokio::test]
async fn repeated_open_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repeat.db");

    for _ in 0..3 {
        drop(TursoStorage::open(&path).await.unwrap());
    }

    database_header(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_open_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.db");

    let (first, second) = tokio::join!(TursoStorage::open(&path), TursoStorage::open(&path));

    drop(first.unwrap());
    drop(second.unwrap());
    database_header(&path);
    TursoStorage::open(&path).await.unwrap();
}

#[tokio::test]
async fn configured_sessions_and_initialization_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("configured.db");
    let storage = TursoStorage::open(&path).await.unwrap();
    let session = storage.connect().await.unwrap();
    drop(session);
    drop(storage);

    let reopened = TursoStorage::open(&path).await.unwrap();
    let session = reopened.connect().await.unwrap();
    drop(session);
    drop(reopened);

    database_header(&path);
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'notes'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}
