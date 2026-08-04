mod support;

use note_org::WorkspaceId;
use note_storage::{NewOrgEvent, NotesRepository, OrgEventType, OrgRepository, StorageErrorKind};
use note_storage_pg::PgStorage;
use std::str::FromStr as _;
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
        "org_attempts",
        "org_events",
        "org_leases",
        "org_note_links",
        "org_operations",
        "org_work_item_tags",
        "org_work_items",
        "org_workspaces",
    ] {
        assert!(tables.iter().any(|table| table == expected), "{expected}");
    }

    let indexes: Vec<(String, String, bool, Option<String>, Vec<String>)> = sqlx::query_as(
        "SELECT index_class.relname,
                access_method.amname,
                index_row.indisunique,
                pg_get_expr(index_row.indpred, index_row.indrelid, true),
                ARRAY(
                    SELECT attribute.attname
                    FROM unnest(index_row.indkey) WITH ORDINALITY AS key(attnum, position)
                    JOIN pg_attribute attribute
                      ON attribute.attrelid = index_row.indrelid
                     AND attribute.attnum = key.attnum
                    ORDER BY key.position
                )::text[]
         FROM pg_index index_row
         JOIN pg_class index_class ON index_class.oid = index_row.indexrelid
         JOIN pg_class table_class ON table_class.oid = index_row.indrelid
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
        ("idx_org_attempts_item_number", "btree"),
        ("idx_org_dependencies_target", "btree"),
        ("idx_org_events_attempt", "btree"),
        ("idx_org_leases_actor_kind", "btree"),
        ("idx_org_leases_item_history", "btree"),
        ("idx_org_leases_one_open_per_item", "btree"),
        ("idx_org_leases_workspace_open_expiry", "btree"),
        ("idx_org_work_items_assignment_order", "btree"),
        ("idx_org_work_items_deadline_order", "btree"),
        ("idx_org_work_items_operational_order", "btree"),
        ("idx_org_work_items_schedule_order", "btree"),
    ] {
        assert!(
            indexes
                .iter()
                .any(|(index, method, _, _, _)| index == expected && method == access_method),
            "{expected} must use {access_method}"
        );
    }
    assert!(
        indexes
            .iter()
            .all(|(_, method, _, _, _)| method != "hnsw" && method != "ivfflat"),
        "migration must not create an ANN index"
    );

    for (index, unique, predicate, columns) in [
        (
            "idx_org_leases_one_open_per_item",
            true,
            Some("ended_at IS NULL"),
            &["work_item_id"][..],
        ),
        (
            "idx_org_leases_workspace_open_expiry",
            false,
            Some("ended_at IS NULL"),
            &["workspace_id", "expires_at", "work_item_id"][..],
        ),
        (
            "idx_org_leases_item_history",
            false,
            None,
            &["work_item_id", "acquired_at", "id"][..],
        ),
        (
            "idx_org_leases_actor_kind",
            false,
            Some("ended_at IS NULL"),
            &["actor_id", "kind", "expires_at"][..],
        ),
        (
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
            "idx_org_dependencies_target",
            false,
            None,
            &["depends_on_id", "work_item_id"][..],
        ),
    ] {
        let actual = indexes
            .iter()
            .find(|(name, _, _, _, _)| name == index)
            .unwrap_or_else(|| panic!("missing index {index}"));
        assert_eq!(actual.2, unique, "unexpected uniqueness for {index}");
        assert_eq!(
            actual.3.as_deref(),
            predicate,
            "unexpected predicate for {index}"
        );
        assert_eq!(actual.4, columns, "unexpected column order for {index}");
    }

    let lease_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'org_leases'
         ORDER BY ordinal_position",
    )
    .fetch_all(&inspection)
    .await
    .expect("list authoritative Org lease columns");
    assert!(lease_columns
        .iter()
        .any(|column| column == "fencing_token_hash"));
    assert!(!lease_columns.iter().any(|column| column == "fencing_token"));

    let lease_foreign_keys: Vec<(String, String)> = sqlx::query_as(
        "SELECT source_column.attname, target_table.relname
         FROM pg_constraint constraint_row
         JOIN LATERAL unnest(constraint_row.conkey) AS source_key(attnum) ON true
         JOIN pg_attribute source_column
           ON source_column.attrelid = constraint_row.conrelid
          AND source_column.attnum = source_key.attnum
         JOIN pg_class target_table ON target_table.oid = constraint_row.confrelid
         WHERE constraint_row.conrelid = 'org_leases'::regclass
           AND constraint_row.contype = 'f'
         ORDER BY source_column.attname",
    )
    .fetch_all(&inspection)
    .await
    .expect("list authoritative Org lease foreign keys");
    assert_eq!(
        lease_foreign_keys,
        vec![
            ("attempt_id".into(), "org_attempts".into()),
            ("workspace_id".into(), "org_workspaces".into()),
        ]
    );

    let lease_checks: Vec<String> = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid, true)
         FROM pg_constraint
         WHERE conrelid = 'org_leases'::regclass AND contype = 'c'
         ORDER BY oid",
    )
    .fetch_all(&inspection)
    .await
    .expect("list authoritative Org lease checks");
    assert_eq!(lease_checks.len(), 10);
    for expected in [
        "CHECK (kind = ANY (ARRAY['execution'::text, 'review'::text]))",
        "CHECK (length(btrim(actor_id)) > 0)",
        "CHECK (length(fencing_token_hash) = 64)",
        "CHECK (acquired_at > 0)",
        "CHECK (last_heartbeat_at >= acquired_at)",
        "CHECK (expires_at >= last_heartbeat_at)",
        "CHECK (ended_at IS NULL OR ended_at >= acquired_at)",
        "CHECK (end_reason IS NULL OR (end_reason = ANY (ARRAY['release'::text, 'completion'::text, 'failure'::text, 'block'::text, 'cancellation'::text, 'lease_expiry'::text, 'review_request'::text, 'approval'::text, 'rejection'::text, 'reassignment'::text])))",
        "CHECK ((ended_at IS NULL) = (end_reason IS NULL))",
        "CHECK ((expiry_event_id IS NOT NULL) = (end_reason IS NOT NULL AND end_reason = 'lease_expiry'::text))",
    ] {
        assert!(
            lease_checks.iter().any(|check| check == expected),
            "missing Org lease constraint: {expected}"
        );
    }

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

    let seed_pool = database.inspect_pool().await;
    sqlx::migrate!("./migrations")
        .run_to(2, &seed_pool)
        .await
        .expect("provision PostgreSQL schema at migration 2");
    let seeded_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&seed_pool)
            .await
            .expect("read seeded migration version");
    assert_eq!(seeded_versions, vec![1, 2]);
    sqlx::query(
        "INSERT INTO notes (
             id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
         ) VALUES ($1, $2, $3, '[]'::jsonb, $4, $5, $6, NULL)",
    )
    .bind("migration-survivor")
    .bind("Migration survivor")
    .bind("preserved")
    .bind(1_i64)
    .bind(1_i64)
    .bind(1_i64)
    .execute(&seed_pool)
    .await
    .expect("seed note before migration 3");
    sqlx::query(
        "INSERT INTO org_workspaces (
             id, slug, display_name, description, timezone,
             policy_schema_version, policy, revision, last_event_sequence,
             created_at, updated_at, archived_at
         ) VALUES ($1, 'migration', 'Migration', '', 'UTC', 1, '{}'::jsonb,
                   1, 2, 1, 1, NULL)",
    )
    .bind("11111111-1111-4111-8111-111111111111")
    .execute(&seed_pool)
    .await
    .expect("seed Org workspace before migration 3");
    sqlx::query(
        "INSERT INTO org_events (
             id, workspace_id, sequence, subject_kind, subject_id, actor_id,
             event_type, occurred_at, summary, metadata
         ) VALUES
             ('event-before-v3', $1, 1, 'workspace', $1, 'migration-test',
              'legacy_custom_event', 1, 'created', '{}'::jsonb),
             ('event-before-v3-blank', $1, 2, '', '   ', '', '', 2, ' ', '{}'::jsonb)",
    )
    .bind("11111111-1111-4111-8111-111111111111")
    .execute(&seed_pool)
    .await
    .expect("seed schema-valid legacy Org events before migration 3");
    seed_pool.close().await;

    let migrated = PgStorage::connect(&database.url, 2)
        .await
        .expect("apply pending PostgreSQL migrations 3 and 4");
    let inspection = database.inspect_pool().await;
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&inspection)
            .await
            .expect("read ordered migration versions");
    assert_eq!(versions, vec![1, 2, 3, 4]);
    inspection.close().await;

    let session = migrated.connect_session().await.unwrap();
    assert_eq!(
        session
            .get_note_content("migration-survivor")
            .await
            .unwrap(),
        Some("preserved".to_owned())
    );
    let workspace_id = WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    let events = session
        .list_org_events(workspace_id, None, 50)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].event_type,
        OrgEventType::Other("legacy_custom_event".into())
    );
    assert_eq!(events[1].subject_kind, "");
    assert_eq!(events[1].subject_id, "   ");
    assert_eq!(events[1].actor_id, "");
    assert_eq!(events[1].event_type, OrgEventType::Other(String::new()));
    assert_eq!(events[1].summary, " ");
    assert_eq!(events[1].attempt_id, None);
    assert_eq!(events[1].previous_state, None);
    assert_eq!(events[1].resulting_state, None);

    let metadata = serde_json::json!({"migrated": true});
    let appended = session
        .append_org_event(NewOrgEvent {
            id: "event-after-v4",
            workspace_id,
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
            workspace_id,
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
        .expect_err("new PostgreSQL v4 event fields must be nonblank");
    assert_eq!(invalid.kind(), StorageErrorKind::Constraint);
    drop(session);

    let inspection = database.inspect_pool().await;
    let validation_versions: Vec<(String, i16)> = sqlx::query_as(
        "SELECT id, validation_version FROM org_events
         ORDER BY sequence",
    )
    .fetch_all(&inspection)
    .await
    .expect("read migrated Org event validation versions");
    assert_eq!(
        validation_versions,
        vec![
            ("event-before-v3".into(), 3),
            ("event-before-v3-blank".into(), 3),
            ("event-after-v4".into(), 4),
        ]
    );
    let last_sequence: i64 = sqlx::query_scalar(
        "SELECT last_event_sequence FROM org_workspaces
         WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(&inspection)
    .await
    .expect("read migrated Org workspace event sequence");
    assert_eq!(last_sequence, 3);
    inspection.close().await;
    migrated.close().await;
    drop(migrated);
    database
        .cleanup(None)
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
