use note_storage::{
    StorageError, StorageErrorKind, StorageResult, StorageTransaction, TransactionMode,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::preflight::{
    incompatible_database, preflight, unsupported_schema, Preflight, APPLICATION_ID,
    OLDEST_SCHEMA_VERSION, PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION,
};

const SCHEMA: &str = include_str!("../schema.sql");
const MIGRATION_2_TO_3: &str = include_str!("../migrations/0002_to_0003_org.sql");
const MIGRATION_3_TO_4: &str = include_str!("../migrations/0003_to_0004_org_workflow_audit.sql");
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenedState {
    Empty,
    Existing { version: u32 },
}

pub struct TursoSession {
    pub(crate) connection: turso::Connection,
    pub(crate) transaction_open: bool,
    operation_gate: Arc<Mutex<()>>,
}

impl TursoSession {
    pub(crate) async fn configured(database: &turso::Database) -> StorageResult<Self> {
        let connection = database
            .connect()
            .map_err(|error| map_turso_error("connect to local database", error))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|error| map_turso_error("configure database busy timeout", error))?;
        connection
            .execute("PRAGMA foreign_keys = ON", ())
            .await
            .map_err(|error| map_turso_error("enable foreign keys", error))?;

        let foreign_keys = pragma_i64(&connection, "PRAGMA foreign_keys").await?;
        if foreign_keys != 1 {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                "database connection did not enable foreign keys",
            ));
        }

        Ok(Self {
            connection,
            transaction_open: false,
            operation_gate: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) async fn operation_guard(&self) -> OwnedMutexGuard<()> {
        self.operation_gate.clone().lock_owned().await
    }

    pub(crate) async fn begin_transaction(&mut self, mode: TransactionMode) -> StorageResult<()> {
        let (sql, context) = match mode {
            TransactionMode::Deferred => ("BEGIN DEFERRED", "begin deferred storage transaction"),
            TransactionMode::Immediate => {
                ("BEGIN IMMEDIATE", "begin immediate storage transaction")
            }
        };
        self.connection
            .execute(sql, ())
            .await
            .map_err(|error| map_turso_error(context, error))?;
        self.transaction_open = true;
        Ok(())
    }

    async fn finalize_transaction(&mut self, sql: &str, context: &str) -> StorageResult<()> {
        let context = if self.transaction_open {
            context
        } else {
            "finalize storage session without an open transaction"
        };
        self.connection
            .execute(sql, ())
            .await
            .map_err(|error| map_transaction_error(context, error))?;
        self.transaction_open = false;
        Ok(())
    }

    pub(crate) async fn opened_state(&self, path: &Path) -> StorageResult<OpenedState> {
        let application_id = pragma_i64(&self.connection, "PRAGMA application_id").await?;
        let user_version = pragma_i64(&self.connection, "PRAGMA user_version").await?;
        let has_user_schema = has_user_schema(&self.connection).await?;

        if application_id == i64::from(APPLICATION_ID) {
            let version = user_version as u32;
            if !matches!(
                version,
                OLDEST_SCHEMA_VERSION | PREVIOUS_SCHEMA_VERSION | SCHEMA_VERSION
            ) {
                return Err(unsupported_schema(version));
            }
            return Ok(OpenedState::Existing { version });
        }
        if application_id == 0 && user_version == 0 && !has_user_schema {
            return Ok(OpenedState::Empty);
        }
        Err(incompatible_database(path))
    }

    pub(crate) async fn initialize(&self, path: &Path) -> StorageResult<()> {
        self.initialize_with_migrations(path, MIGRATION_2_TO_3, MIGRATION_3_TO_4)
            .await
    }

    async fn initialize_with_migrations(
        &self,
        path: &Path,
        migration_2_to_3: &str,
        migration_3_to_4: &str,
    ) -> StorageResult<()> {
        self.connection
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|error| initialization_error(error, None))?;

        let result = self
            .initialize_transaction_body(path, migration_2_to_3, migration_3_to_4)
            .await;
        if let Err(primary) = result {
            let rollback = self.connection.execute("ROLLBACK", ()).await.err();
            return Err(initialization_storage_error(primary, rollback));
        }

        self.ensure_durable(path).await
    }

    async fn initialize_transaction_body(
        &self,
        path: &Path,
        migration_2_to_3: &str,
        migration_3_to_4: &str,
    ) -> StorageResult<()> {
        match self.opened_state(path).await? {
            OpenedState::Empty => {
                self.connection
                    .execute_batch(SCHEMA)
                    .await
                    .map_err(|error| map_turso_error("initialize database schema", error))?;
                self.connection
                    .execute("PRAGMA application_id = 1095651156", ())
                    .await
                    .map_err(|error| map_turso_error("mark agent-note database", error))?;
                set_user_version(&self.connection, SCHEMA_VERSION).await?;
            }
            OpenedState::Existing {
                version: OLDEST_SCHEMA_VERSION,
            } => {
                self.connection
                    .execute_batch(migration_2_to_3)
                    .await
                    .map_err(|error| {
                        map_turso_error("migrate database schema from v2 to v3", error)
                    })?;
                self.connection
                    .execute_batch(migration_3_to_4)
                    .await
                    .map_err(|error| {
                        map_turso_error("migrate database schema from v3 to v4", error)
                    })?;
                set_user_version(&self.connection, SCHEMA_VERSION).await?;
            }
            OpenedState::Existing {
                version: PREVIOUS_SCHEMA_VERSION,
            } => {
                self.connection
                    .execute_batch(migration_3_to_4)
                    .await
                    .map_err(|error| {
                        map_turso_error("migrate database schema from v3 to v4", error)
                    })?;
                set_user_version(&self.connection, SCHEMA_VERSION).await?;
            }
            OpenedState::Existing {
                version: SCHEMA_VERSION,
            } => {}
            OpenedState::Existing { version } => return Err(unsupported_schema(version)),
        }

        self.connection
            .execute("COMMIT", ())
            .await
            .map_err(|error| map_transaction_error("commit database initialization", error))?;
        Ok(())
    }

    pub(crate) async fn ensure_durable(&self, path: &Path) -> StorageResult<()> {
        self.connection
            .cacheflush()
            .map_err(|error| map_turso_error("flush initialized database", error))?;
        checkpoint(&self.connection, path).await
    }
}

async fn set_user_version(connection: &turso::Connection, version: u32) -> StorageResult<()> {
    let sql = format!("PRAGMA user_version = {version}");
    connection
        .execute(&sql, ())
        .await
        .map_err(|error| map_turso_error("mark database schema version", error))?;
    Ok(())
}

#[async_trait::async_trait]
impl StorageTransaction for TursoSession {
    async fn commit(mut self: Box<Self>) -> StorageResult<()> {
        self.finalize_transaction("COMMIT", "commit storage transaction")
            .await
    }

    async fn rollback(mut self: Box<Self>) -> StorageResult<()> {
        self.finalize_transaction("ROLLBACK", "rollback storage transaction")
            .await
    }
}

pub(crate) async fn pragma_i64(connection: &turso::Connection, pragma: &str) -> StorageResult<i64> {
    let mut rows = connection
        .query(pragma, ())
        .await
        .map_err(|error| map_turso_error("query database configuration", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read database configuration", error))?
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                format!("{pragma} returned no value"),
            )
        })?;
    row.get::<i64>(0)
        .map_err(|error| map_turso_error("decode database configuration", error))
}

async fn has_user_schema(connection: &turso::Connection) -> StorageResult<bool> {
    let mut rows = connection
        .query(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_schema
                WHERE name NOT LIKE 'sqlite_%'
                  AND name NOT LIKE '__turso_internal_%'
            )",
            (),
        )
        .await
        .map_err(|error| map_turso_error("inspect database schema", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| map_turso_error("inspect database schema", error))?
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                "database schema inspection returned no value",
            )
        })?;
    row.get::<i64>(0)
        .map(|value| value != 0)
        .map_err(|error| map_turso_error("decode database schema state", error))
}

async fn checkpoint(connection: &turso::Connection, path: &Path) -> StorageResult<()> {
    let mut rows = connection
        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .map_err(|error| map_turso_error("checkpoint initialized database", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| map_turso_error("checkpoint initialized database", error))?
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                "database checkpoint returned no status",
            )
        })?;
    let busy = row
        .get::<i64>(0)
        .map_err(|error| map_turso_error("decode database checkpoint status", error))?;
    while rows
        .next()
        .await
        .map_err(|error| map_turso_error("checkpoint initialized database", error))?
        .is_some()
    {}
    verify_checkpoint_result(path, busy)
}

fn verify_checkpoint_result(path: &Path, busy: i64) -> StorageResult<()> {
    let durable = preflight(path);
    if matches!(
        durable,
        Ok(Preflight::Existing {
            version: SCHEMA_VERSION
        })
    ) {
        return Ok(());
    }
    if busy != 0 {
        let message = format!(
            "database checkpoint was busy before markers became durable: {}",
            path.display()
        );
        return Err(match durable {
            Err(source) => StorageError::with_source(StorageErrorKind::Conflict, message, source),
            Ok(_) => StorageError::new(StorageErrorKind::Conflict, message),
        });
    }
    match durable {
        Ok(Preflight::Missing | Preflight::Fresh) => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!(
                "database markers are not durable after checkpoint: {}",
                path.display()
            ),
        )),
        Ok(Preflight::Existing {
            version: SCHEMA_VERSION,
        }) => Ok(()),
        Ok(Preflight::Existing { version }) => Err(unsupported_schema(version)),
        Err(error) => Err(error),
    }
}

pub(crate) fn map_turso_error(context: &str, error: turso::Error) -> StorageError {
    let kind = match &error {
        turso::Error::Constraint(_) => StorageErrorKind::Constraint,
        turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => StorageErrorKind::Conflict,
        turso::Error::Corrupt(_) | turso::Error::NotAdb(_) => StorageErrorKind::Corrupt,
        turso::Error::IoError(_, _) | turso::Error::Readonly(_) | turso::Error::DatabaseFull(_) => {
            StorageErrorKind::Unavailable
        }
        _ => StorageErrorKind::Operation,
    };
    StorageError::with_source(kind, context, error)
}

fn map_transaction_error(context: &str, error: turso::Error) -> StorageError {
    StorageError::with_source(StorageErrorKind::Transaction, context, error)
}

fn initialization_error(primary: turso::Error, rollback: Option<turso::Error>) -> StorageError {
    let mut message = String::from("initialize database schema");
    if let Some(rollback) = rollback {
        message.push_str(&format!("; rollback also failed: {rollback}"));
    }
    map_turso_error(&message, primary)
}

fn initialization_storage_error(
    primary: StorageError,
    rollback: Option<turso::Error>,
) -> StorageError {
    if let Some(rollback) = rollback {
        StorageError::with_source(
            primary.kind(),
            format!("{primary}; rollback also failed: {rollback}"),
            primary,
        )
    } else {
        primary
    }
}

#[cfg(test)]
mod tests {
    use super::{
        map_turso_error, verify_checkpoint_result, StorageErrorKind, TursoSession,
        MIGRATION_2_TO_3, MIGRATION_3_TO_4,
    };
    use crate::TursoStorage;

    async fn pragma_value(connection: &turso::Connection, pragma: &str) -> i64 {
        let mut rows = connection.query(pragma, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    #[tokio::test]
    async fn configured_session_enables_foreign_keys_and_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("configured.db");
        let storage = TursoStorage::open(&path).await.unwrap();

        let session = storage.connect().await.unwrap();

        assert_eq!(
            pragma_value(&session.connection, "PRAGMA foreign_keys").await,
            1
        );
        assert_eq!(
            pragma_value(&session.connection, "PRAGMA busy_timeout").await,
            30_000
        );
    }

    #[tokio::test]
    async fn failed_migration_rolls_back_schema_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("migration-rollback.db");
        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/schema-v2.sql"))
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
        drop(connection);
        drop(database);

        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let session = TursoSession::configured(&database).await.unwrap();
        let bad_migration = format!(
            "{MIGRATION_2_TO_3}\n\
             CREATE TABLE migration_probe (id INTEGER PRIMARY KEY);\n\
             INSERT INTO missing_migration_table(id) VALUES (1);"
        );

        session
            .initialize_with_migrations(&path, &bad_migration, MIGRATION_3_TO_4)
            .await
            .expect_err("bad migration tail must fail");

        assert_eq!(
            pragma_value(&session.connection, "PRAGMA user_version").await,
            2
        );
        let mut rows = session
            .connection
            .query(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'migration_probe'
                 )",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn failed_second_chained_migration_rolls_back_to_v2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("migration-chain-rollback.db");
        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/schema-v2.sql"))
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
        drop(connection);
        drop(database);

        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let session = TursoSession::configured(&database).await.unwrap();
        let bad_second_migration = format!(
            "{MIGRATION_3_TO_4}\n\
             CREATE TABLE migration_chain_probe (id INTEGER PRIMARY KEY);\n\
             INSERT INTO missing_migration_table(id) VALUES (1);"
        );

        session
            .initialize_with_migrations(&path, MIGRATION_2_TO_3, &bad_second_migration)
            .await
            .expect_err("failure in v3-to-v4 must roll the whole chain back");

        assert_eq!(
            pragma_value(&session.connection, "PRAGMA user_version").await,
            2
        );
        for table in ["org_workspaces", "org_attempts", "migration_chain_probe"] {
            let mut rows = session
                .connection
                .query(
                    "SELECT EXISTS(
                         SELECT 1 FROM sqlite_schema
                         WHERE type = 'table' AND name = ?1
                     )",
                    turso::params![table],
                )
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                0,
                "{table} must not survive the rolled-back migration chain"
            );
        }
    }

    #[tokio::test]
    async fn failed_v3_migration_rolls_back_schema_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("migration-v3-rollback.db");
        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/schema-v3.sql"))
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
        drop(connection);
        drop(database);

        let database = turso::Builder::new_local(path.to_str().unwrap())
            .experimental_index_method(true)
            .build()
            .await
            .unwrap();
        let session = TursoSession::configured(&database).await.unwrap();
        let bad_migration = format!(
            "{MIGRATION_3_TO_4}\n\
             CREATE TABLE migration_v3_probe (id INTEGER PRIMARY KEY);\n\
             INSERT INTO missing_migration_table(id) VALUES (1);"
        );

        session
            .initialize_with_migrations(&path, MIGRATION_2_TO_3, &bad_migration)
            .await
            .expect_err("bad v3 migration tail must fail");

        assert_eq!(
            pragma_value(&session.connection, "PRAGMA user_version").await,
            3
        );
        let mut rows = session
            .connection
            .query(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'migration_v3_probe'
                 )",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        let mut rows = session
            .connection
            .query(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'org_attempts'
                 )",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }

    #[test]
    fn busy_driver_errors_are_conflicts() {
        assert_eq!(
            map_turso_error("busy", turso::Error::Busy("locked".into())).kind(),
            StorageErrorKind::Conflict
        );
        assert_eq!(
            map_turso_error(
                "busy snapshot",
                turso::Error::BusySnapshot("stale snapshot".into())
            )
            .kind(),
            StorageErrorKind::Conflict
        );
    }

    #[test]
    fn checkpoint_busy_without_durable_markers_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.db");
        std::fs::File::create(&path).unwrap();

        let error = verify_checkpoint_result(&path, 1).unwrap_err();

        assert_eq!(error.kind(), StorageErrorKind::Conflict);
    }

    #[test]
    fn successful_checkpoint_requires_durable_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-durable.db");
        std::fs::File::create(&path).unwrap();

        let error = verify_checkpoint_result(&path, 0).unwrap_err();

        assert_eq!(error.kind(), StorageErrorKind::Corrupt);
    }

    #[tokio::test]
    async fn checkpoint_busy_accepts_already_durable_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("durable.db");
        drop(TursoStorage::open(&path).await.unwrap());

        verify_checkpoint_result(&path, 1).unwrap();
    }
}
