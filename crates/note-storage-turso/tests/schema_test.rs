use note_storage::{NotesRepository, StorageErrorKind};
use note_storage_turso::TursoStorage;
use std::path::Path;

const APPLICATION_ID: u32 = 0x414E4F54;
const SCHEMA_VERSION: u32 = 3;

fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",")
}

async fn normalized_org_schema(connection: &turso::Connection) -> Vec<(String, String, String)> {
    let mut rows = connection
        .query(
            "SELECT type, name, sql
             FROM sqlite_schema
             WHERE type IN ('table', 'index', 'trigger')
               AND (name GLOB 'org_*' OR name GLOB 'idx_org_*')
               AND name NOT GLOB 'sqlite_autoindex_*'
               AND sql IS NOT NULL
             ORDER BY type, name",
            (),
        )
        .await
        .unwrap();
    let mut schema = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        schema.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            normalize_schema_sql(&row.get::<String>(2).unwrap()),
        ));
    }
    schema
}

async fn create_schema_v2_database(path: &Path) {
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema-v2.sql"))
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO notes (
                 id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
             ) VALUES ('migration-survivor', 'Migration survivor', 'preserved', '[]', 1, 1, 1, NULL)",
            (),
        )
        .await
        .unwrap();
    connection
        .execute("PRAGMA application_id = 1095651156", ())
        .await
        .unwrap();
    connection
        .execute("PRAGMA user_version = 2", ())
        .await
        .unwrap();
    connection.execute("COMMIT", ()).await.unwrap();
    connection.cacheflush().unwrap();
    let mut rows = connection
        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .unwrap();
    while rows.next().await.unwrap().is_some() {}
}

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
    before[60..64].copy_from_slice(&4_u32.to_be_bytes());
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("unsupported schema was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::UnsupportedSchema);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn schema_v2_is_migrated_without_losing_notes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    create_schema_v2_database(&path).await;

    let storage = TursoStorage::open(&path).await.unwrap();

    database_header(&path);
    let session = storage.connect().await.unwrap();
    assert_eq!(
        session
            .get_note_content("migration-survivor")
            .await
            .unwrap(),
        Some("preserved".to_owned())
    );
    drop(session);
    drop(storage);

    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query(
            "SELECT name
             FROM sqlite_schema
             WHERE type = 'table' AND name LIKE 'org_%'
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        tables.push(row.get::<String>(0).unwrap());
    }
    for expected in [
        "org_workspaces",
        "org_documents",
        "org_work_items",
        "org_work_item_tags",
        "org_dependencies",
        "org_note_links",
        "org_events",
        "org_operations",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }

    let migrated_schema = normalized_org_schema(&connection).await;
    drop(connection);
    drop(database);

    let fresh_path = dir.path().join("fresh-v3.db");
    drop(TursoStorage::open(&fresh_path).await.unwrap());
    let fresh_database = turso::Builder::new_local(fresh_path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let fresh_connection = fresh_database.connect().unwrap();
    let fresh_schema = normalized_org_schema(&fresh_connection).await;

    assert_eq!(migrated_schema, fresh_schema);
}

#[tokio::test]
async fn schema_v1_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    drop(TursoStorage::open(&path).await.unwrap());
    let mut before = std::fs::read(&path).unwrap();
    before[60..64].copy_from_slice(&1_u32.to_be_bytes());
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("schema v1 was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::UnsupportedSchema);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn fresh_database_contains_current_schema_objects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema.db");
    drop(TursoStorage::open(&path).await.unwrap());

    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
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
    let mut triggers = Vec::new();
    let mut schema_sql = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let object_type = row.get::<String>(0).unwrap();
        let name = row.get::<String>(1).unwrap();
        if object_type == "table" {
            tables.push(name);
        } else if object_type == "index" {
            indexes.push(name);
        } else if object_type == "trigger" {
            triggers.push(name);
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
        "app_settings",
        "org_workspaces",
        "org_documents",
        "org_work_items",
        "org_work_item_tags",
        "org_dependencies",
        "org_note_links",
        "org_events",
        "org_operations",
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
    ] {
        assert!(
            indexes.iter().any(|index| index == expected),
            "missing index {expected}"
        );
    }
    assert!(indexes.iter().any(|index| index == "idx_notes_title_fts"));
    assert!(triggers
        .iter()
        .any(|trigger| trigger == "org_events_advance_workspace_sequence"));
    assert!(!tables.iter().any(|table| table == "note_chunk_sparse"));
    assert!(!indexes
        .iter()
        .any(|index| index == "idx_note_chunk_sparse_token"));

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
        .experimental_index_method(true)
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
