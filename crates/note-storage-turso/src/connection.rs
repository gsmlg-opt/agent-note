use note_storage::{StorageError, StorageErrorKind, StorageResult};
use std::path::Path;
use std::time::Duration;

use crate::preflight::{
    incompatible_database, preflight, unsupported_schema, Preflight, APPLICATION_ID, SCHEMA_VERSION,
};

const SCHEMA: &str = include_str!("../schema.sql");
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenedState {
    Empty,
    Existing,
}

pub struct TursoSession {
    connection: turso::Connection,
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

        Ok(Self { connection })
    }

    pub(crate) async fn opened_state(&self, path: &Path) -> StorageResult<OpenedState> {
        let application_id = pragma_i64(&self.connection, "PRAGMA application_id").await?;
        let user_version = pragma_i64(&self.connection, "PRAGMA user_version").await?;
        let has_user_schema = has_user_schema(&self.connection).await?;

        if application_id == i64::from(APPLICATION_ID) {
            if user_version != i64::from(SCHEMA_VERSION) {
                return Err(unsupported_schema(user_version as u32));
            }
            return Ok(OpenedState::Existing);
        }
        if application_id == 0 && user_version == 0 && !has_user_schema {
            return Ok(OpenedState::Empty);
        }
        Err(incompatible_database(path))
    }

    pub(crate) async fn initialize(&self, path: &Path) -> StorageResult<()> {
        if let Err(primary) = self.connection.execute("BEGIN IMMEDIATE", ()).await {
            return Err(initialization_error(primary, None));
        }

        let state = match self.opened_state(path).await {
            Ok(state) => state,
            Err(primary) => {
                let _ = self.connection.execute("ROLLBACK", ()).await;
                return Err(primary);
            }
        };

        let result = match state {
            OpenedState::Existing => self.connection.execute("COMMIT", ()).await.map(|_| ()),
            OpenedState::Empty => {
                async {
                    self.connection.execute_batch(SCHEMA).await?;
                    self.connection
                        .execute("PRAGMA application_id = 1095651156", ())
                        .await?;
                    self.connection
                        .execute("PRAGMA user_version = 1", ())
                        .await?;
                    self.connection.execute("COMMIT", ()).await?;
                    Ok(())
                }
                .await
            }
        };

        if let Err(primary) = result {
            let rollback = self.connection.execute("ROLLBACK", ()).await.err();
            return Err(initialization_error(primary, rollback));
        }

        self.ensure_durable(path).await
    }

    pub(crate) async fn ensure_durable(&self, path: &Path) -> StorageResult<()> {
        self.connection
            .cacheflush()
            .map_err(|error| map_turso_error("flush initialized database", error))?;
        checkpoint(&self.connection, path).await
    }
}

async fn pragma_i64(connection: &turso::Connection, pragma: &str) -> StorageResult<i64> {
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
    if matches!(durable, Ok(Preflight::Existing)) {
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
        Ok(Preflight::Existing) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn map_turso_error(context: &str, error: turso::Error) -> StorageError {
    StorageError::with_source(turso_error_kind(&error), context, error)
}

fn turso_error_kind(error: &turso::Error) -> StorageErrorKind {
    match error {
        turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => StorageErrorKind::Conflict,
        turso::Error::Constraint(_) => StorageErrorKind::Constraint,
        turso::Error::Corrupt(_) | turso::Error::NotAdb(_) => StorageErrorKind::Corrupt,
        turso::Error::Readonly(_) | turso::Error::DatabaseFull(_) | turso::Error::IoError(..) => {
            StorageErrorKind::Unavailable
        }
        _ => StorageErrorKind::Operation,
    }
}

fn initialization_error(primary: turso::Error, rollback: Option<turso::Error>) -> StorageError {
    let mut message = String::from("initialize database schema");
    if let Some(rollback) = rollback {
        message.push_str(&format!("; rollback also failed: {rollback}"));
    }
    StorageError::with_source(turso_error_kind(&primary), message, primary)
}

#[cfg(test)]
mod tests {
    use super::{turso_error_kind, verify_checkpoint_result, StorageErrorKind};
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

    #[test]
    fn busy_driver_errors_are_conflicts() {
        assert_eq!(
            turso_error_kind(&turso::Error::Busy("locked".into())),
            StorageErrorKind::Conflict
        );
        assert_eq!(
            turso_error_kind(&turso::Error::BusySnapshot("stale snapshot".into())),
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
