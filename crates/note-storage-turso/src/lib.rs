mod connection;
mod preflight;

pub use connection::TursoSession;

use connection::map_turso_error;
use note_storage::{StorageError, StorageErrorKind, StorageResult};
use preflight::{incompatible_database, preflight_and_reserve, PinnedIo, Preflight};
use std::path::Path;

pub struct TursoStorage {
    database: turso::Database,
}

impl TursoStorage {
    pub async fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        let path_string = database_path(path)?;
        let state = preflight_and_reserve(path)?;
        Self::open_preflighted(path, path_string, state).await
    }

    async fn open_preflighted(
        path: &Path,
        path_string: &str,
        state: Preflight,
    ) -> StorageResult<Self> {
        let (io, state) = PinnedIo::prepare(path, path_string, state)?;
        let database = turso::Builder::new_local(path_string)
            .with_io_impl(io.clone())
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

        io.verify_ready(path)?;
        Ok(storage)
    }

    pub async fn connect(&self) -> StorageResult<TursoSession> {
        TursoSession::configured(&self.database).await
    }
}

fn database_path(path: &Path) -> StorageResult<&str> {
    path.to_str().ok_or_else(|| {
        StorageError::new(
            StorageErrorKind::Operation,
            format!("database path is not valid UTF-8: {}", path.display()),
        )
    })
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

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_utf8_path_is_rejected_without_creating_a_file() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(OsString::from_vec(b"invalid-\xff.db".to_vec()));

        let error = match TursoStorage::open(&path).await {
            Ok(_) => panic!("invalid UTF-8 path was accepted"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), StorageErrorKind::Operation);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_fresh_preflight_does_not_convert_a_rollback_database_to_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.db");
        create_unmarked_database(&path).await;
        let mut before = std::fs::read(&path).unwrap();
        before[18] = 1;
        before[19] = 1;
        std::fs::write(&path, &before).unwrap();

        let error =
            match TursoStorage::open_preflighted(&path, path.to_str().unwrap(), Preflight::Fresh)
                .await
            {
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

        let error = match TursoStorage::open_preflighted(
            &path,
            path.to_str().unwrap(),
            Preflight::Existing,
        )
        .await
        {
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

        TursoStorage::open_preflighted(&path, path.to_str().unwrap(), Preflight::Fresh)
            .await
            .unwrap();
    }
}
