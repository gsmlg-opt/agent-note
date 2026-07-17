mod connection;
mod preflight;

pub use connection::TursoSession;

use connection::map_turso_error;
use note_storage::{StorageError, StorageErrorKind, StorageResult};
use preflight::{incompatible_database, preflight_and_reserve, Preflight};
use std::path::Path;

pub struct TursoStorage {
    database: turso::Database,
}

impl TursoStorage {
    pub async fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        let state = preflight_and_reserve(path)?;
        Self::open_preflighted(path, state).await
    }

    async fn open_preflighted(path: &Path, state: Preflight) -> StorageResult<Self> {
        let path_string = path.to_str().ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                format!("database path is not valid UTF-8: {}", path.display()),
            )
        })?;
        let database = turso::Builder::new_local(path_string)
            .build()
            .await
            .map_err(|error| map_turso_error("open local database", error))?;
        let storage = Self { database };
        let session = storage.connect().await?;

        match state {
            Preflight::Fresh => session.initialize(path).await?,
            Preflight::Existing => match session.opened_state(path).await? {
                connection::OpenedState::Existing => {}
                connection::OpenedState::Empty => return Err(incompatible_database(path)),
            },
            Preflight::Missing => return Err(incompatible_database(path)),
        }

        Ok(storage)
    }

    pub async fn connect(&self) -> StorageResult<TursoSession> {
        TursoSession::configured(&self.database).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_unmarked_database(path: &Path) {
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
        let mut rows = connection
            .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await
            .unwrap();
        while rows.next().await.unwrap().is_some() {}
    }

    #[tokio::test]
    async fn stale_fresh_preflight_rejects_actual_unmarked_database_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.db");
        create_unmarked_database(&path).await;
        let before = std::fs::read(&path).unwrap();

        let error = match TursoStorage::open_preflighted(&path, Preflight::Fresh).await {
            Ok(_) => panic!("stale fresh eligibility initialized an unmarked database"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn stale_existing_preflight_revalidates_actual_unmarked_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.db");
        create_unmarked_database(&path).await;
        let before = std::fs::read(&path).unwrap();

        let error = match TursoStorage::open_preflighted(&path, Preflight::Existing).await {
            Ok(_) => panic!("stale existing eligibility accepted an unmarked database"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn stale_fresh_preflight_accepts_database_initialized_by_another_opener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("initialized.db");
        drop(TursoStorage::open(&path).await.unwrap());

        TursoStorage::open_preflighted(&path, Preflight::Fresh)
            .await
            .unwrap();
    }
}
