use note_storage::{StorageError, StorageErrorKind};

pub(crate) fn map_connect_error(error: sqlx::Error) -> StorageError {
    let kind = match &error {
        sqlx::Error::Database(database)
            if database.code().as_deref().is_some_and(|code| {
                code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03")
            }) =>
        {
            StorageErrorKind::Unavailable
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        _ => StorageErrorKind::Operation,
    };

    StorageError::new(kind, "connect to PostgreSQL")
}

pub(crate) fn map_sqlx_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = match &error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        _ => StorageErrorKind::Operation,
    };

    StorageError::with_source(kind, context, error)
}
