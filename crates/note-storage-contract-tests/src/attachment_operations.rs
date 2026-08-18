use note_storage::{
    AttachmentOperationKind, AttachmentOperationStatus, NewAttachmentOperation, NewNote,
    StorageBackend, TransactionMode,
};
use std::sync::Arc;

fn operation(id: &str, note_id: &str, object_key: &str, now: i64) -> NewAttachmentOperation {
    NewAttachmentOperation {
        id: id.into(),
        kind: AttachmentOperationKind::DeleteObject,
        note_id: note_id.into(),
        attachment_id: "attachment-1".into(),
        storage_generation: "generation-1".into(),
        object_key: object_key.into(),
        status: AttachmentOperationStatus::Pending,
        attempts: 0,
        next_attempt_at: Some(now),
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    }
}

async fn insert_note(session: &dyn note_storage::StorageSession, id: &str) {
    session
        .insert_note(NewNote {
            id,
            title: id,
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    insert_note(session.as_ref(), "contract-attachment-operations-note").await;

    let invalid = operation(
        "",
        "contract-attachment-operations-note",
        "objects/ATTACHMENT-OBJECT-SECRET",
        1,
    );
    let invalid_error = session
        .insert_attachment_operation(invalid)
        .await
        .unwrap_err();
    assert!(!format!("{invalid_error:?}").contains("ATTACHMENT-OBJECT-SECRET"));

    let first = session
        .insert_attachment_operation(operation(
            "attachment-operation-2",
            "contract-attachment-operations-note",
            "objects/generation-1",
            20,
        ))
        .await
        .unwrap();
    let second = session
        .insert_attachment_operation(operation(
            "attachment-operation-1",
            "contract-attachment-operations-note",
            "objects/generation-0",
            10,
        ))
        .await
        .unwrap();
    assert_eq!(
        session.get_attachment_operation(&first.id).await.unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        session
            .list_attachment_operations_for_note("contract-attachment-operations-note")
            .await
            .unwrap()
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<Vec<_>>(),
        vec![second.id.as_str(), first.id.as_str()]
    );

    let duplicate = session
        .insert_attachment_operation(operation(
            "attachment-operation-duplicate",
            "contract-attachment-operations-note",
            "objects/generation-1",
            30,
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.id, first.id);

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    rollback
        .insert_attachment_operation(operation(
            "attachment-operation-rollback",
            "contract-attachment-operations-note",
            "objects/rollback",
            30,
        ))
        .await
        .unwrap();
    rollback.rollback().await.unwrap();
    assert!(session
        .get_attachment_operation("attachment-operation-rollback")
        .await
        .unwrap()
        .is_none());

    let claimed = session
        .claim_attachment_operations("worker-a", 20, 30, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 2);
    assert!(claimed.iter().all(|operation| {
        operation.status == AttachmentOperationStatus::Running
            && operation.attempts == 1
            && operation.lease_owner.as_deref() == Some("worker-a")
    }));
    assert!(session
        .claim_attachment_operations("worker-b", 25, 35, 10)
        .await
        .unwrap()
        .is_empty());

    let first_id = first.id.as_str();
    assert!(!session
        .complete_attachment_operation(first_id, "worker-b", 26)
        .await
        .unwrap());
    assert!(session
        .complete_attachment_operation(first_id, "worker-a", 27)
        .await
        .unwrap());
    assert!(session
        .complete_attachment_operation(first_id, "worker-a", 28)
        .await
        .unwrap());

    let reclaimed = session
        .claim_attachment_operations("worker-b", 31, 40, 10)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, second.id);
    assert_eq!(reclaimed[0].attempts, 2);
    assert!(!session
        .fail_attachment_operation(
            &second.id,
            "worker-a",
            AttachmentOperationStatus::Pending,
            Some(50),
            "wrong owner",
            32,
        )
        .await
        .unwrap());
    assert!(session
        .fail_attachment_operation(
            &second.id,
            "worker-b",
            AttachmentOperationStatus::Pending,
            Some(50),
            "temporary failure",
            33,
        )
        .await
        .unwrap());

    let fresh = storage.session().await.unwrap();
    let retry = fresh
        .get_attachment_operation(&second.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.next_attempt_at, Some(50));
    assert_eq!(retry.last_error.as_deref(), Some("temporary failure"));
    let claimed = fresh
        .claim_attachment_operations("worker-c", 50, 60, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(fresh
        .fail_attachment_operation(
            &second.id,
            "worker-c",
            AttachmentOperationStatus::Dead,
            None,
            "permanent failure",
            51,
        )
        .await
        .unwrap());

    assert!(matches!(
        fresh
            .soft_delete_note("contract-attachment-operations-note", 1, 52)
            .await
            .unwrap(),
        note_storage::NoteMutationResult::Applied { revision: 2, .. }
    ));
    assert!(matches!(
        fresh
            .permanently_delete_note("contract-attachment-operations-note", 2)
            .await
            .unwrap(),
        note_storage::NoteMutationResult::Applied { .. }
    ));
    assert_eq!(
        fresh
            .list_attachment_operations_for_note("contract-attachment-operations-note")
            .await
            .unwrap()
            .len(),
        2
    );
}
