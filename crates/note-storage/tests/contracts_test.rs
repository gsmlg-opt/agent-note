use note_storage::{
    StorageBackend, StorageError, StorageErrorKind, StorageSession, StorageTransaction,
};
use std::sync::Arc;

fn accepts_backend(_: Arc<dyn StorageBackend>) {}
fn accepts_session(_: Box<dyn StorageSession>) {}
fn accepts_transaction(_: Box<dyn StorageTransaction>) {}

#[test]
fn storage_contracts_are_dyn_compatible() {
    let _ = accepts_backend;
    let _ = accepts_session;
    let _ = accepts_transaction;
}

#[test]
fn storage_errors_expose_stable_categories() {
    let error = StorageError::new(
        StorageErrorKind::IncompatibleDatabase,
        "database is not an agent-note database",
    );
    assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
    assert!(error.to_string().contains("agent-note"));
}
