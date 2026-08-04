use note_storage::{StorageError, StorageErrorKind};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgErrorCode {
    InvalidInput,
    NotFound,
    ArchivedWorkspace,
    StaleRevision,
    IdempotencyConflict,
    InvalidTransition,
    DependencyBlocked,
    ReviewRequired,
    ActiveLease,
    StaleLease,
    ConcurrencyLimit,
    RetryLimit,
    UnsupportedSemanticEdit,
    NoteUnavailable,
    StorageFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgError {
    pub code: OrgErrorCode,
    pub message: String,
    pub details: serde_json::Value,
    pub retryable: bool,
}

impl OrgError {
    pub fn new(
        code: OrgErrorCode,
        message: impl Into<String>,
        details: serde_json::Value,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            retryable,
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(
            OrgErrorCode::InvalidInput,
            message,
            serde_json::Value::Object(Default::default()),
            false,
        )
    }

    pub fn storage(error: StorageError) -> Self {
        let retryable = matches!(
            error.kind(),
            StorageErrorKind::Conflict
                | StorageErrorKind::Transaction
                | StorageErrorKind::Unavailable
        );
        Self::new(
            OrgErrorCode::StorageFailure,
            "Org storage operation failed",
            serde_json::json!({"storage_kind": storage_kind_name(error.kind())}),
            retryable,
        )
    }
}

impl fmt::Display for OrgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OrgError {}

fn storage_kind_name(kind: StorageErrorKind) -> &'static str {
    match kind {
        StorageErrorKind::IncompatibleDatabase => "incompatible_database",
        StorageErrorKind::Constraint => "constraint",
        StorageErrorKind::Conflict => "conflict",
        StorageErrorKind::Corrupt => "corrupt",
        StorageErrorKind::Operation => "operation",
        StorageErrorKind::Transaction => "transaction",
        StorageErrorKind::Unavailable => "unavailable",
        StorageErrorKind::UnsupportedSchema => "unsupported_schema",
    }
}
