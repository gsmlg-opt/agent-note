mod connection;

pub use connection::PgSession;
use connection::{map_connect_error, map_sqlx_error};
use note_storage::{BackendInfo, StorageError, StorageErrorKind, StorageResult, TransactionMode};
use sqlx::postgres::PgPoolOptions;

#[derive(Debug)]
pub struct PgStorage {
    pool: sqlx::PgPool,
}

impl PgStorage {
    pub async fn connect(url: &str, max_connections: u32) -> StorageResult<Self> {
        if max_connections == 0 {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                "PostgreSQL max_connections must be greater than zero",
            ));
        }

        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(map_connect_error)?;

        let extension: Option<String> =
            sqlx::query_scalar("SELECT extversion FROM pg_extension WHERE extname = 'vector'")
                .fetch_optional(&pool)
                .await
                .map_err(|error| map_sqlx_error("verify PostgreSQL vector extension", error))?;

        if extension.is_none() {
            pool.close().await;
            return Err(StorageError::new(
                StorageErrorKind::UnsupportedSchema,
                "PostgreSQL extension vector is not installed; provision pgvector before starting agent-note",
            ));
        }

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(map_migrate_error)?;

        Ok(Self { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn connect_session(&self) -> StorageResult<PgSession> {
        PgSession::connect(&self.pool).await
    }

    pub async fn begin_session(&self, mode: TransactionMode) -> StorageResult<PgSession> {
        PgSession::begin(self.pool.clone(), mode).await
    }

    pub fn backend_info(&self) -> BackendInfo {
        BackendInfo {
            engine: "pg".into(),
            location: None,
            size_bytes: None,
        }
    }
}

fn map_migrate_error(error: sqlx::migrate::MigrateError) -> StorageError {
    use sqlx::migrate::MigrateError;

    let kind = match &error {
        MigrateError::VersionMissing(_)
        | MigrateError::VersionMismatch(_)
        | MigrateError::VersionNotPresent(_)
        | MigrateError::VersionTooOld(_, _)
        | MigrateError::VersionTooNew(_, _)
        | MigrateError::Dirty(_) => StorageErrorKind::UnsupportedSchema,
        _ => StorageErrorKind::Operation,
    };
    StorageError::with_source(kind, "migrate PostgreSQL schema", error)
}
