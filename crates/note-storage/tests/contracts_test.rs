use note_storage::{
    AttachmentOperation, AttachmentOperationKind, AttachmentOperationRepository,
    AttachmentOperationStatus, OrgRepository, StorageBackend, StorageError, StorageErrorKind,
    StorageSession, StorageTransaction,
};
use std::str::FromStr as _;
use std::sync::Arc;

fn accepts_backend(_: Arc<dyn StorageBackend>) {}
fn accepts_session(_: Box<dyn StorageSession>) {}
fn accepts_transaction(_: Box<dyn StorageTransaction>) {}
fn accepts_org_repository_object(_: &dyn OrgRepository) {}
fn accepts_attachment_operation_repository_object(_: &dyn AttachmentOperationRepository) {}

fn accepts_org_repository<T: OrgRepository + ?Sized>(_: &T) {}

fn accepts_org_storage_session(session: &dyn StorageSession) {
    accepts_org_repository(session);
    accepts_attachment_operation_repository_object(session);
}

#[test]
fn attachment_operation_values_have_stable_database_names() {
    assert_eq!(
        AttachmentOperationKind::DeleteObject.to_string(),
        "delete_object"
    );
    for (status, database_name) in [
        (AttachmentOperationStatus::Pending, "pending"),
        (AttachmentOperationStatus::Running, "running"),
        (AttachmentOperationStatus::Completed, "completed"),
        (AttachmentOperationStatus::Dead, "dead"),
    ] {
        assert_eq!(status.to_string(), database_name);
        assert_eq!(
            AttachmentOperationStatus::from_str(database_name).unwrap(),
            status
        );
    }

    let secret = "unknown-secret-value";
    let error = AttachmentOperationStatus::from_str(secret).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Corrupt);
    assert!(!error.to_string().contains(secret));

    let json_error =
        serde_json::from_str::<AttachmentOperationStatus>("\"unknown-json-secret-value\"")
            .unwrap_err();
    assert!(!json_error.to_string().contains("unknown-json-secret-value"));
}

#[test]
fn attachment_operation_debug_redacts_last_error() {
    let operation = AttachmentOperation {
        id: "operation-id".into(),
        kind: AttachmentOperationKind::DeleteObject,
        note_id: "note-id".into(),
        attachment_id: "attachment-id".into(),
        storage_generation: "generation-id".into(),
        object_key: "object-key".into(),
        status: AttachmentOperationStatus::Dead,
        attempts: 1,
        next_attempt_at: None,
        lease_owner: None,
        lease_expires_at: None,
        last_error: Some("ATTACHMENT-ERROR-SECRET".into()),
        created_at: 1,
        updated_at: 2,
    };

    let debug = format!("{operation:?}");
    assert!(!debug.contains("ATTACHMENT-ERROR-SECRET"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn storage_contracts_are_dyn_compatible() {
    let _ = accepts_backend;
    let _ = accepts_session;
    let _ = accepts_transaction;
}

#[test]
fn storage_sessions_include_org_persistence() {
    let _ = accepts_org_repository_object;
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
