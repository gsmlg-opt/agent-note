#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    IncompatibleDatabase,
    UnsupportedSchema,
    Constraint,
    Conflict,
    Transaction,
    Unavailable,
    Corrupt,
    Operation,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StorageError {
    kind: StorageErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl StorageError {
    pub fn new(kind: StorageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        kind: StorageErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn kind(&self) -> StorageErrorKind {
        self.kind
    }
}

pub type StorageResult<T> = Result<T, StorageError>;
