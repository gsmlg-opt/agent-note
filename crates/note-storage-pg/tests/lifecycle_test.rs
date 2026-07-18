mod support;

use note_storage::StorageErrorKind;
use note_storage_pg::PgStorage;
use support::{configured_url, TestDatabase};

#[tokio::test]
async fn zero_max_connections_is_rejected_before_network_access() {
    let error = PgStorage::connect(
        "postgresql://agent_note:secret-that-must-not-leak@127.0.0.1:1/agent_note",
        0,
    )
    .await
    .expect_err("zero max_connections must be rejected");

    assert_eq!(error.kind(), StorageErrorKind::Operation);
    assert_eq!(
        error.to_string(),
        "PostgreSQL max_connections must be greater than zero"
    );
}

#[tokio::test]
async fn unreachable_database_is_unavailable_and_redacts_credentials() {
    const PASSWORD: &str = "secret-that-must-not-leak";
    let error = PgStorage::connect(
        &format!("postgresql://agent_note:{PASSWORD}@127.0.0.1:1/agent_note"),
        1,
    )
    .await
    .expect_err("unreachable database must fail");

    assert_eq!(error.kind(), StorageErrorKind::Unavailable);
    assert!(!error.to_string().contains(PASSWORD));
    assert!(!format!("{error:?}").contains(PASSWORD));
}

#[tokio::test]
async fn fresh_database_without_vector_extension_is_rejected() {
    let Some(admin_url) = configured_url() else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;

    let error = PgStorage::connect(&database.url, 1)
        .await
        .expect_err("missing vector extension must fail");

    assert_eq!(error.kind(), StorageErrorKind::UnsupportedSchema);
    assert!(error
        .to_string()
        .contains("PostgreSQL extension vector is not installed"));
    database.cleanup(None).await;
}

#[tokio::test]
async fn connect_runs_migrations_with_all_expected_tables_and_indexes() {
    let Some(admin_url) = configured_url() else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;

    let storage = PgStorage::connect(&database.url, 2)
        .await
        .expect("connect and migrate PostgreSQL storage");
    let inspection = database.inspect_pool().await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
         ORDER BY table_name",
    )
    .fetch_all(&inspection)
    .await
    .expect("list migrated tables");
    for expected in [
        "_sqlx_migrations",
        "app_settings",
        "embedding_jobs",
        "label_keys",
        "note_chunk_embeddings",
        "note_chunks",
        "note_labels",
        "notes",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname
         FROM pg_indexes
         WHERE schemaname = 'public'
         ORDER BY indexname",
    )
    .fetch_all(&inspection)
    .await
    .expect("list migrated indexes");
    for expected in [
        "idx_embedding_jobs_status",
        "idx_note_chunks_hash",
        "idx_note_chunks_status",
        "idx_note_labels_key_value",
        "idx_notes_title_fts",
    ] {
        assert!(indexes.iter().any(|index| index == expected), "{expected}");
    }
    assert!(
        indexes.iter().all(|index| {
            let lowered = index.to_ascii_lowercase();
            !lowered.contains("hnsw") && !lowered.contains("ivfflat")
        }),
        "migration must not create an ANN index"
    );

    inspection.close().await;
    database.cleanup(Some(&storage)).await;
}

#[tokio::test]
async fn repeated_connect_is_safe() {
    let Some(admin_url) = configured_url() else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;

    let first = PgStorage::connect(&database.url, 1)
        .await
        .expect("first connection");
    first.close().await;
    let second = PgStorage::connect(&database.url, 1)
        .await
        .expect("second connection");

    database.cleanup(Some(&second)).await;
}

#[tokio::test]
async fn backend_info_is_credential_free() {
    let Some(admin_url) = configured_url() else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 1)
        .await
        .expect("connect PostgreSQL storage");

    let info = storage.backend_info();
    assert_eq!(info.engine, "pg");
    assert_eq!(info.location, None);
    assert_eq!(info.size_bytes, None);

    database.cleanup(Some(&storage)).await;
}
