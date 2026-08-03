use note_storage::{
    OrgRepository, StorageBackend, StorageError, StorageErrorKind, StorageSession,
    StorageTransaction,
};
use std::sync::Arc;

fn accepts_backend(_: Arc<dyn StorageBackend>) {}
fn accepts_session(_: Box<dyn StorageSession>) {}
fn accepts_transaction(_: Box<dyn StorageTransaction>) {}
fn accepts_org_repository(_: &dyn OrgRepository) {}
fn accepts_org_storage_session(_: Box<dyn StorageSession>) {}

#[test]
fn storage_contracts_are_dyn_compatible() {
    let _ = accepts_backend;
    let _ = accepts_session;
    let _ = accepts_transaction;
}

#[test]
fn storage_sessions_include_org_persistence() {
    let _ = accepts_org_repository;
    let _ = accepts_org_storage_session;
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
