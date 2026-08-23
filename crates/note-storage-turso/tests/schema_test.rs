use note_org::{DocumentId, WorkspaceId};
use note_storage::{NewOrgEvent, NotesRepository, OrgEventType, OrgRepository, StorageErrorKind};
use note_storage_turso::TursoStorage;
use std::path::Path;
use std::str::FromStr as _;

const APPLICATION_ID: u32 = 0x414E4F54;
const SCHEMA_VERSION: u32 = 7;

fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",")
        .replace("CHECK (revision >= 1)", "CHECK(revision >= 1)")
        .replace(
            "REFERENCES org_workspaces (id)",
            "REFERENCES org_workspaces(id)",
        )
        .replace("length (", "length(")
        .replace("trim (", "trim(")
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

async fn sqlite_index_spec(
    connection: &turso::Connection,
    table: &str,
    index: &str,
) -> (bool, bool, Vec<String>, Option<String>) {
    let mut properties = connection
        .query(
            "SELECT \"unique\", partial
             FROM pragma_index_list(?1)
             WHERE name = ?2",
            turso::params![table, index],
        )
        .await
        .unwrap();
    let properties = properties
        .next()
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("missing index {index}"));

    let mut column_rows = connection
        .query(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            turso::params![index],
        )
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = column_rows.next().await.unwrap() {
        columns.push(row.get::<String>(0).unwrap());
    }

    let index_sql = connection
        .query(
            "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1",
            turso::params![index],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    let normalized = normalize_schema_sql(&index_sql);
    let predicate = normalized
        .split_once(" WHERE ")
        .map(|(_, predicate)| predicate.to_owned());

    (
        properties.get::<i64>(0).unwrap() == 1,
        properties.get::<i64>(1).unwrap() == 1,
        columns,
        predicate,
    )
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

async fn create_schema_v3_database(path: &Path) {
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema-v3.sql"))
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_workspaces (
                 id, slug, display_name, description, timezone,
                 policy_schema_version, policy, revision, last_event_sequence,
                 created_at, updated_at, archived_at
             ) VALUES (
                 '11111111-1111-4111-8111-111111111111', 'migration',
                 'Migration', '', 'UTC', 1, '{}', 1, 0, 1, 1, NULL
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_events (
                 id, workspace_id, sequence, subject_kind, subject_id,
                 actor_id, event_type, occurred_at, summary, metadata
             ) VALUES (
                 'event-before-v4', '11111111-1111-4111-8111-111111111111',
                 1, 'workspace', '11111111-1111-4111-8111-111111111111',
                 'migration-test', 'legacy_custom_event', 1, 'created', '{}'
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_events (
                 id, workspace_id, sequence, subject_kind, subject_id,
                 actor_id, event_type, occurred_at, summary, metadata
             ) VALUES (
                 'event-before-v4-blank', '11111111-1111-4111-8111-111111111111',
                 2, '', '   ', '', '', 2, ' ', '{}'
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute("PRAGMA application_id = 1095651156", ())
        .await
        .unwrap();
    connection
        .execute("PRAGMA user_version = 3", ())
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

async fn create_schema_v4_database(path: &Path) {
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema-v4.sql"))
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_workspaces (
                 id, slug, display_name, description, timezone,
                 policy_schema_version, policy, revision, last_event_sequence,
                 created_at, updated_at, archived_at
             ) VALUES (
                 '11111111-1111-4111-8111-111111111111', 'migration',
                 'Migration', '', 'UTC', 1, '{}', 1, 0, 1, 1, NULL
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_attempts (
                 id, workspace_id, work_item_id, attempt_number, actor_id,
                 status, started_at, ended_at, error, result_summary,
                 review_outcome, note_refs, artifacts, metadata
             ) VALUES (
                 'attempt-before-v5', '11111111-1111-4111-8111-111111111111',
                 '22222222-2222-4222-8222-222222222222', 1, 'migration-test',
                 'running', 1, NULL, NULL, NULL, NULL, '[]', '[]', '{}'
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute("PRAGMA application_id = 1095651156", ())
        .await
        .unwrap();
    connection
        .execute("PRAGMA user_version = 4", ())
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

async fn create_schema_v5_database(path: &Path) {
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema-v5.sql"))
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO notes (
                 id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
             ) VALUES ('migration-v5-survivor', 'Migration v5 survivor', 'preserved', '[]', 1, 1, 1, NULL)",
            (),
        )
        .await
        .unwrap();
    connection
        .execute("PRAGMA application_id = 1095651156", ())
        .await
        .unwrap();
    connection
        .execute("PRAGMA user_version = 5", ())
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

async fn create_schema_v6_database(path: &Path) {
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema-v6.sql"))
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_workspaces (
                 id, slug, display_name, description, timezone,
                 policy_schema_version, policy, revision, last_event_sequence,
                 created_at, updated_at, archived_at
             ) VALUES (
                 '11111111-1111-4111-8111-111111111111', 'migration-v6',
                 'Migration v6', '', 'UTC', 1, '{}', 1, 0, 1, 1, NULL
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO org_documents (
                 id, workspace_id, path, source, content_hash, revision, created_at, updated_at
             ) VALUES (
                 '22222222-2222-4222-8222-222222222222',
                 '11111111-1111-4111-8111-111111111111',
                 'docs/survivor.org', '* TODO Survive migration', 'v6-hash', 3, 1, 2
             )",
            (),
        )
        .await
        .unwrap();
    connection
        .execute("PRAGMA application_id = 1095651156", ())
        .await
        .unwrap();
    connection
        .execute("PRAGMA user_version = 6", ())
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
    before[60..64].copy_from_slice(&8_u32.to_be_bytes());
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
        "org_attempts",
        "org_leases",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }

    let migrated_schema = normalized_org_schema(&connection).await;
    drop(connection);
    drop(database);

    let fresh_path = dir.path().join("fresh-v5.db");
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
async fn schema_v3_is_migrated_without_rewriting_existing_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v3.db");
    create_schema_v3_database(&path).await;

    let storage = TursoStorage::open(&path).await.unwrap();
    database_header(&path);

    let session = storage.connect().await.unwrap();
    let events = session
        .list_org_events(
            WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap(),
            None,
            50,
        )
        .await
        .unwrap();
    assert_eq!(
        events[0].event_type,
        OrgEventType::Other("legacy_custom_event".into())
    );
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].subject_kind, "");
    assert_eq!(events[1].subject_id, "   ");
    assert_eq!(events[1].actor_id, "");
    assert_eq!(events[1].event_type, OrgEventType::Other(String::new()));
    assert_eq!(events[1].summary, " ");

    let metadata = serde_json::json!({"migrated": true});
    let appended = session
        .append_org_event(NewOrgEvent {
            id: "event-after-v4",
            workspace_id: WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap(),
            subject_kind: "workspace",
            subject_id: "11111111-1111-4111-8111-111111111111",
            actor_id: "migration-test",
            attempt_id: None,
            event_type: OrgEventType::WorkspaceChange,
            occurred_at: 3,
            summary: "migration completed",
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(appended.sequence, 3);

    let invalid = session
        .append_org_event(NewOrgEvent {
            id: "invalid-v4-event",
            workspace_id: appended.workspace_id,
            subject_kind: " ",
            subject_id: "workspace",
            actor_id: "migration-test",
            attempt_id: None,
            event_type: OrgEventType::Other(" ".into()),
            occurred_at: 4,
            summary: "invalid",
            metadata: &metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .expect_err("new v4 event fields must be nonblank");
    assert_eq!(invalid.kind(), StorageErrorKind::Constraint);
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
            "SELECT event_type, actor_id, attempt_id, previous_state, resulting_state
             FROM org_events WHERE id = 'event-before-v4'",
            (),
        )
        .await
        .unwrap();
    let event = rows.next().await.unwrap().unwrap();
    assert_eq!(event.get::<String>(0).unwrap(), "legacy_custom_event");
    assert_eq!(event.get::<String>(1).unwrap(), "migration-test");
    assert_eq!(event.get::<Option<String>>(2).unwrap(), None);
    assert_eq!(event.get::<Option<String>>(3).unwrap(), None);
    assert_eq!(event.get::<Option<String>>(4).unwrap(), None);

    let sequence: i64 = connection
        .query(
            "SELECT last_event_sequence FROM org_workspaces
             WHERE id = '11111111-1111-4111-8111-111111111111'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(sequence, 3);

    let migrated_schema = normalized_org_schema(&connection).await;
    drop(connection);
    drop(database);

    let fresh_path = dir.path().join("fresh-v5.db");
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
async fn schema_v4_is_migrated_without_rewriting_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v4.db");
    create_schema_v4_database(&path).await;

    drop(TursoStorage::open(&path).await.unwrap());
    database_header(&path);

    let database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let attempt_count: i64 = connection
        .query(
            "SELECT count(*) FROM org_attempts WHERE id = 'attempt-before-v5'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(attempt_count, 1);
    let migrated_schema = normalized_org_schema(&connection).await;
    drop(connection);
    drop(database);

    let fresh_path = dir.path().join("fresh-v5.db");
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
async fn schema_v5_is_migrated_without_losing_notes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v5.db");
    create_schema_v5_database(&path).await;

    let storage = TursoStorage::open(&path).await.unwrap();
    database_header(&path);
    let session = storage.connect().await.unwrap();
    assert_eq!(
        session
            .get_note_content("migration-v5-survivor")
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
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'attachment_operations'",
            (),
        )
        .await
        .unwrap();
    let migrated_schema = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert!(normalize_schema_sql(&migrated_schema).contains("CHECK (updated_at >= created_at)"));
}

#[tokio::test]
async fn schema_v6_is_migrated_with_active_org_documents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v6.db");
    create_schema_v6_database(&path).await;

    let storage = TursoStorage::open(&path).await.unwrap();
    database_header(&path);
    let session = storage.connect().await.unwrap();
    let document = session
        .get_org_document(DocumentId::from_str("22222222-2222-4222-8222-222222222222").unwrap())
        .await
        .unwrap()
        .expect("migrated Org document");

    assert_eq!(document.source, "* TODO Survive migration");
    assert_eq!(document.revision, 3);
    assert_eq!(document.archived_at, None);
    drop(session);
    drop(storage);

    let migrated_database = turso::Builder::new_local(path.to_str().unwrap())
        .experimental_index_method(true)
        .build()
        .await
        .unwrap();
    let migrated_connection = migrated_database.connect().unwrap();
    let migrated_schema = normalized_org_schema(&migrated_connection).await;

    let fresh_path = dir.path().join("fresh-v7.db");
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
        "attachment_operations",
        "org_workspaces",
        "org_documents",
        "org_work_items",
        "org_work_item_tags",
        "org_dependencies",
        "org_note_links",
        "org_events",
        "org_operations",
        "org_attempts",
        "org_leases",
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
        "idx_attachment_operations_note",
        "idx_attachment_operations_claim",
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
    assert!(indexes
        .iter()
        .any(|index| index == "idx_org_attempts_item_number"));
    assert!(indexes
        .iter()
        .any(|index| index == "idx_org_events_attempt"));
    for (table, index, unique, predicate, columns) in [
        (
            "org_leases",
            "idx_org_leases_one_open_per_item",
            true,
            Some("ended_at IS NULL"),
            &["work_item_id"][..],
        ),
        (
            "org_leases",
            "idx_org_leases_workspace_open_expiry",
            false,
            Some("ended_at IS NULL"),
            &["workspace_id", "expires_at", "work_item_id"][..],
        ),
        (
            "org_leases",
            "idx_org_leases_item_history",
            false,
            None,
            &["work_item_id", "acquired_at", "id"][..],
        ),
        (
            "org_leases",
            "idx_org_leases_actor_kind",
            false,
            Some("ended_at IS NULL"),
            &["actor_id", "kind", "expires_at"][..],
        ),
        (
            "org_work_items",
            "idx_org_work_items_operational_order",
            false,
            None,
            &[
                "workspace_id",
                "state",
                "priority",
                "deadline_utc",
                "scheduled_utc",
                "created_at",
                "id",
            ][..],
        ),
        (
            "org_work_items",
            "idx_org_work_items_assignment_order",
            false,
            None,
            &[
                "workspace_id",
                "assignee",
                "state",
                "priority",
                "deadline_utc",
                "scheduled_utc",
                "created_at",
                "id",
            ][..],
        ),
        (
            "org_work_items",
            "idx_org_work_items_schedule_order",
            false,
            None,
            &[
                "workspace_id",
                "scheduled_utc",
                "priority",
                "deadline_utc",
                "created_at",
                "id",
            ][..],
        ),
        (
            "org_work_items",
            "idx_org_work_items_deadline_order",
            false,
            None,
            &[
                "workspace_id",
                "deadline_utc",
                "priority",
                "scheduled_utc",
                "created_at",
                "id",
            ][..],
        ),
        (
            "org_dependencies",
            "idx_org_dependencies_target",
            false,
            None,
            &["depends_on_id", "work_item_id"][..],
        ),
    ] {
        let actual = sqlite_index_spec(&connection, table, index).await;
        assert_eq!(actual.0, unique, "unexpected uniqueness for {index}");
        assert_eq!(
            actual.1,
            predicate.is_some(),
            "unexpected partial flag for {index}"
        );
        assert_eq!(actual.2, columns, "unexpected column order for {index}");
        assert_eq!(
            actual.3,
            predicate.map(str::to_owned),
            "unexpected predicate for {index}"
        );
    }
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

    let attachment_schema = connection
        .query(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'attachment_operations'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    let attachment_schema = normalize_schema_sql(&attachment_schema);
    for expected in [
        "id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0)",
        "kind TEXT NOT NULL CHECK (kind = 'delete_object')",
        "note_id TEXT NOT NULL CHECK (length(trim(note_id)) > 0)",
        "attachment_id TEXT NOT NULL CHECK (length(trim(attachment_id)) > 0)",
        "storage_generation TEXT NOT NULL CHECK (length(trim(storage_generation)) > 0)",
        "object_key TEXT NOT NULL CHECK (length(trim(object_key)) > 0)",
        "status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'dead'))",
        "attempts INTEGER NOT NULL CHECK (attempts >= 0)",
        "CHECK (updated_at >= created_at)",
        "CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL))",
        "CHECK ((status = 'running') = (lease_owner IS NOT NULL))",
        "UNIQUE (kind, note_id, object_key)",
    ] {
        assert!(
            attachment_schema.contains(expected),
            "missing attachment operation schema constraint: {expected}"
        );
    }
    let note_index = sqlite_index_spec(
        &connection,
        "attachment_operations",
        "idx_attachment_operations_note",
    )
    .await;
    assert_eq!(note_index.0, false);
    assert_eq!(note_index.2, ["note_id", "created_at", "id"]);
    let claim_index = sqlite_index_spec(
        &connection,
        "attachment_operations",
        "idx_attachment_operations_claim",
    )
    .await;
    assert_eq!(claim_index.0, false);
    assert_eq!(
        claim_index.2,
        [
            "status",
            "next_attempt_at",
            "lease_expires_at",
            "created_at",
            "id",
        ]
    );
    let foreign_key_count: i64 = connection
        .query(
            "SELECT count(*) FROM pragma_foreign_key_list('attachment_operations')",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(foreign_key_count, 0);

    let mut columns = connection
        .query("PRAGMA table_info(org_leases)", ())
        .await
        .unwrap();
    let mut column_names = Vec::new();
    while let Some(row) = columns.next().await.unwrap() {
        column_names.push(row.get::<String>(1).unwrap());
    }
    assert!(column_names
        .iter()
        .any(|column| column == "fencing_token_hash"));
    assert!(!column_names.iter().any(|column| column == "fencing_token"));

    let mut foreign_keys = connection
        .query("PRAGMA foreign_key_list(org_leases)", ())
        .await
        .unwrap();
    let mut lease_foreign_keys = Vec::new();
    while let Some(row) = foreign_keys.next().await.unwrap() {
        lease_foreign_keys.push((row.get::<String>(3).unwrap(), row.get::<String>(2).unwrap()));
    }
    lease_foreign_keys.sort();
    assert_eq!(
        lease_foreign_keys,
        vec![
            ("attempt_id".into(), "org_attempts".into()),
            ("workspace_id".into(), "org_workspaces".into()),
        ]
    );
    let lease_table_sql = connection
        .query(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'table' AND name = 'org_leases'",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    let lease_table_sql = normalize_schema_sql(&lease_table_sql);
    assert_eq!(lease_table_sql.matches("CHECK (").count(), 10);
    for expected in [
        "CHECK (kind IN ('execution', 'review'))",
        "CHECK (length(trim(actor_id)) > 0)",
        "CHECK (length(fencing_token_hash) = 64)",
        "CHECK (acquired_at > 0)",
        "CHECK (last_heartbeat_at >= acquired_at)",
        "CHECK (expires_at >= last_heartbeat_at)",
        "CHECK (ended_at IS NULL OR ended_at >= acquired_at)",
        "CHECK (end_reason IS NULL OR end_reason IN ('release', 'completion', 'failure', 'block', 'cancellation', 'lease_expiry', 'review_request', 'approval', 'rejection', 'reassignment'))",
        "CHECK ((ended_at IS NULL) = (end_reason IS NULL))",
        "CHECK ((expiry_event_id IS NOT NULL) = (end_reason IS NOT NULL AND end_reason = 'lease_expiry'))",
    ] {
        assert!(
            lease_table_sql.contains(expected),
            "missing Org lease constraint: {expected}; actual schema: {lease_table_sql}"
        );
    }
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
