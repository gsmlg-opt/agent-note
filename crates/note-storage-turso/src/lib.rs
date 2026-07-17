mod connection;
mod preflight;

pub use connection::TursoSession;

use connection::map_turso_error;
use note_storage::{StorageError, StorageErrorKind, StorageResult};
use preflight::{preflight, Preflight};
use std::path::Path;

pub struct TursoStorage {
    database: turso::Database,
}

impl TursoStorage {
    pub async fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        let state = preflight(path)?;
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

        if state == Preflight::Fresh {
            session.initialize().await?;
        }

        Ok(storage)
    }

    pub async fn connect(&self) -> StorageResult<TursoSession> {
        TursoSession::configured(&self.database).await
    }
}
