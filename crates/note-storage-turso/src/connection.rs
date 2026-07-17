use note_storage::{StorageError, StorageErrorKind, StorageResult};
use std::time::Duration;

const SCHEMA: &str = include_str!("../schema.sql");
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

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

    pub(crate) async fn initialize(&self) -> StorageResult<()> {
        let result: turso::Result<()> = async {
            self.connection.execute("BEGIN IMMEDIATE", ()).await?;
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
        .await;

        if let Err(primary) = result {
            let rollback = self.connection.execute("ROLLBACK", ()).await.err();
            return Err(initialization_error(primary, rollback));
        }

        self.connection
            .cacheflush()
            .map_err(|error| map_turso_error("flush initialized database", error))?;
        checkpoint(&self.connection).await
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

async fn checkpoint(connection: &turso::Connection) -> StorageResult<()> {
    let mut rows = connection
        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .map_err(|error| map_turso_error("checkpoint initialized database", error))?;
    while rows
        .next()
        .await
        .map_err(|error| map_turso_error("checkpoint initialized database", error))?
        .is_some()
    {}
    Ok(())
}

pub(crate) fn map_turso_error(context: &str, error: turso::Error) -> StorageError {
    StorageError::with_source(turso_error_kind(&error), context, error)
}

fn turso_error_kind(error: &turso::Error) -> StorageErrorKind {
    match error {
        turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => StorageErrorKind::Unavailable,
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
}
