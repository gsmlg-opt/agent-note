mod support;

use note_storage::{NewNote, NotesRepository, StorageErrorKind};
use note_storage_pg::PgStorage;
use support::{configured_url_or_skip, TestDatabase};

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
    let Some(admin_url) =
        configured_url_or_skip("fresh_database_without_vector_extension_is_rejected")
    else {
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
    database
        .cleanup(None)
        .await
        .expect("explicit cleanup without leaked connections");
}

#[tokio::test]
async fn connect_runs_migrations_with_all_expected_tables_and_indexes() {
    let Some(admin_url) =
        configured_url_or_skip("connect_runs_migrations_with_all_expected_tables_and_indexes")
    else {
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
        "org_dependencies",
        "org_documents",
        "org_events",
        "org_note_links",
        "org_operations",
        "org_work_item_tags",
        "org_work_items",
        "org_workspaces",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }

    let indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT index_class.relname, access_method.amname
         FROM pg_index
         JOIN pg_class index_class ON index_class.oid = pg_index.indexrelid
         JOIN pg_class table_class ON table_class.oid = pg_index.indrelid
         JOIN pg_namespace ON pg_namespace.oid = table_class.relnamespace
         JOIN pg_am access_method ON access_method.oid = index_class.relam
         WHERE pg_namespace.nspname = 'public'
         ORDER BY index_class.relname",
    )
    .fetch_all(&inspection)
    .await
    .expect("list migrated indexes");
    for (expected, access_method) in [
        ("idx_embedding_jobs_status", "btree"),
        ("idx_note_chunks_hash", "btree"),
        ("idx_note_chunks_status", "btree"),
        ("idx_note_labels_key_value", "btree"),
        ("idx_notes_title_fts", "gin"),
    ] {
        assert!(
            indexes
                .iter()
                .any(|(index, method)| index == expected && method == access_method),
            "{expected} must use {access_method}"
        );
    }
    assert!(
        indexes
            .iter()
            .all(|(_, method)| method != "hnsw" && method != "ivfflat"),
        "migration must not create an ANN index"
    );

    inspection.close().await;
    database
        .cleanup(Some(&storage))
        .await
        .expect("explicit cleanup without leaked connections");
}

#[tokio::test]
async fn ordered_migrations_preserve_existing_notes() {
    let Some(admin_url) = configured_url_or_skip("ordered_migrations_preserve_existing_notes")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;

    let storage = PgStorage::connect(&database.url, 2)
        .await
        .expect("provision existing PostgreSQL fixture");
    let session = storage.connect_session().await.unwrap();
    session
        .insert_note(NewNote {
            id: "migration-survivor",
            title: "Migration survivor",
            content: "preserved",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    drop(session);
    storage.close().await;

    let reopened = PgStorage::connect(&database.url, 2)
        .await
        .expect("reconnect after ordered migrations");
    let inspection = database.inspect_pool().await;
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&inspection)
            .await
            .expect("read ordered migration versions");
    assert_eq!(versions, vec![1, 2]);
    inspection.close().await;

    let session = reopened.connect_session().await.unwrap();
    assert_eq!(
        session
            .get_note_content("migration-survivor")
            .await
            .unwrap(),
        Some("preserved".to_owned())
    );
    drop(session);
    database
        .cleanup(Some(&reopened))
        .await
        .expect("explicit cleanup without leaked connections");
}

#[tokio::test]
async fn repeated_connect_is_safe() {
    let Some(admin_url) = configured_url_or_skip("repeated_connect_is_safe") else {
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

    database
        .cleanup(Some(&second))
        .await
        .expect("explicit cleanup without leaked connections");
}

#[tokio::test]
async fn backend_info_is_credential_free() {
    let Some(admin_url) = configured_url_or_skip("backend_info_is_credential_free") else {
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

    database
        .cleanup(Some(&storage))
        .await
        .expect("explicit cleanup without leaked connections");
}

#[tokio::test]
async fn dropping_fixture_force_cleans_the_isolated_database() {
    let Some(admin_url) =
        configured_url_or_skip("dropping_fixture_force_cleans_the_isolated_database")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    let database_name = database.database_name().to_owned();

    drop(database);

    let exists = database_exists(&admin_url, &database_name).await;
    if exists {
        force_drop_for_failed_assertion(&admin_url, &database_name).await;
    }
    assert!(
        !exists,
        "fixture Drop must synchronously remove its database"
    );
}

#[tokio::test]
async fn cancelling_cleanup_future_runs_the_drop_guard() {
    let Some(admin_url) = configured_url_or_skip("cancelling_cleanup_future_runs_the_drop_guard")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    let database_name = database.database_name().to_owned();

    let cleanup = database.cleanup(None);
    drop(cleanup);

    let exists = database_exists(&admin_url, &database_name).await;
    if exists {
        force_drop_for_failed_assertion(&admin_url, &database_name).await;
    }
    assert!(
        !exists,
        "cancelling cleanup must synchronously force-remove its database"
    );
}

#[tokio::test]
async fn cleanup_reports_unknown_connections_after_removing_database() {
    let Some(admin_url) =
        configured_url_or_skip("cleanup_reports_unknown_connections_after_removing_database")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    let database_name = database.database_name().to_owned();
    let unknown_pool = database.inspect_pool().await;

    let cleanup = database.cleanup(None).await;
    assert!(
        cleanup
            .expect_err("explicit cleanup must report unknown live connections")
            .remaining_connections()
            >= 1
    );
    unknown_pool.close().await;
    assert!(
        !database_exists(&admin_url, &database_name).await,
        "explicit cleanup must remove the database after reporting leaked connections"
    );
}

async fn database_exists(admin_url: &str, database_name: &str) -> bool {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .unwrap_or_else(|_| panic!("connect to PostgreSQL administrative database"));
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(database_name)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| panic!("query isolated PostgreSQL test database"));
    pool.close().await;
    exists
}

async fn force_drop_for_failed_assertion(admin_url: &str, database_name: &str) {
    use sqlx::AssertSqlSafe;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .unwrap_or_else(|_| panic!("connect to PostgreSQL administrative database"));
    let _ = sqlx::query(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(&pool)
    .await;
    sqlx::query(AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{database_name}" WITH (FORCE)"#
    )))
    .execute(&pool)
    .await
    .unwrap_or_else(|_| panic!("drop leaked isolated PostgreSQL test database"));
    pool.close().await;
}
