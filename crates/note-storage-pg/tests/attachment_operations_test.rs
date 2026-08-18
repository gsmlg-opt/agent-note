#[allow(dead_code)]
mod support;

use note_storage::{
    AttachmentOperationKind, AttachmentOperationRepository, AttachmentOperationStatus,
    NewAttachmentOperation,
};
use note_storage_pg::PgStorage;
use support::{configured_url_or_skip, TestDatabase};

async fn storage_for(test_name: &str) -> Option<(TestDatabase, PgStorage)> {
    let admin_url = configured_url_or_skip(test_name)?;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    Some((database, storage))
}

fn operation(id: &str, note_id: &str, created_at: i64) -> NewAttachmentOperation {
    NewAttachmentOperation {
        id: id.into(),
        kind: AttachmentOperationKind::DeleteObject,
        note_id: note_id.into(),
        attachment_id: format!("attachment-{id}"),
        storage_generation: format!("generation-{id}"),
        object_key: format!("notes/{note_id}/{id}"),
        next_attempt_at: None,
        created_at,
    }
}

#[tokio::test]
async fn attachment_operations_round_trip_and_list_in_creation_order() {
    let Some((database, storage)) =
        storage_for("attachment_operations_round_trip_and_list_in_creation_order").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();

    let later = session
        .insert_attachment_operation(operation("later", "note-a", 20))
        .await
        .unwrap();
    session
        .insert_attachment_operation(operation("earlier", "note-a", 10))
        .await
        .unwrap();
    session
        .insert_attachment_operation(operation("other", "note-b", 5))
        .await
        .unwrap();

    assert_eq!(
        session.get_attachment_operation("later").await.unwrap(),
        Some(later)
    );
    assert_eq!(
        session
            .list_attachment_operations_for_note("note-a")
            .await
            .unwrap()
            .into_iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>(),
        vec!["earlier", "later"]
    );

    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn claims_ready_and_expired_operations_with_owner_fencing() {
    let Some((database, storage)) =
        storage_for("claims_ready_and_expired_operations_with_owner_fencing").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();

    for (owner, now, lease_expires_at) in [("", 100, 150), ("worker", 100, 100)] {
        let invalid = session
            .claim_attachment_operations(owner, now, lease_expires_at, 1)
            .await
            .expect_err("claims require an owner and future lease");
        assert_eq!(invalid.kind(), note_storage::StorageErrorKind::Constraint);
    }

    let mut delayed = operation("delayed", "note-a", 10);
    delayed.next_attempt_at = Some(200);
    session.insert_attachment_operation(delayed).await.unwrap();
    session
        .insert_attachment_operation(operation("ready", "note-a", 20))
        .await
        .unwrap();

    let claimed = session
        .claim_attachment_operations("worker-a", 100, 150, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "ready");
    assert_eq!(claimed[0].status, AttachmentOperationStatus::Running);
    assert_eq!(claimed[0].attempts, 1);
    assert_eq!(claimed[0].lease_owner.as_deref(), Some("worker-a"));
    assert_eq!(claimed[0].lease_expires_at, Some(150));

    assert!(session
        .claim_attachment_operations("worker-b", 149, 180, 10)
        .await
        .unwrap()
        .is_empty());
    let reclaimed = session
        .claim_attachment_operations("worker-b", 150, 250, 1)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, "ready");
    assert_eq!(reclaimed[0].attempts, 2);
    assert_eq!(reclaimed[0].lease_owner.as_deref(), Some("worker-b"));

    assert!(!session
        .complete_attachment_operation("ready", "worker-a", 1, 160)
        .await
        .unwrap());
    assert!(session
        .complete_attachment_operation("ready", "worker-b", 2, 160)
        .await
        .unwrap());
    assert!(session
        .complete_attachment_operation("ready", "worker-b", 2, 170)
        .await
        .unwrap());
    let completed = session
        .get_attachment_operation("ready")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, AttachmentOperationStatus::Completed);
    assert_eq!(completed.lease_owner, None);
    assert_eq!(completed.lease_expires_at, None);

    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn failure_requeues_or_dead_letters_only_the_owners_claim() {
    let Some((database, storage)) =
        storage_for("failure_requeues_or_dead_letters_only_the_owners_claim").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();
    session
        .insert_attachment_operation(operation("retry", "note-a", 10))
        .await
        .unwrap();
    session
        .claim_attachment_operations("worker", 20, 30, 1)
        .await
        .unwrap();

    assert!(!session
        .fail_attachment_operation(
            "retry",
            "other-worker",
            1,
            AttachmentOperationStatus::Pending,
            Some(50),
            "temporary",
            25,
        )
        .await
        .unwrap());
    assert!(session
        .fail_attachment_operation(
            "retry",
            "worker",
            1,
            AttachmentOperationStatus::Pending,
            Some(50),
            "temporary",
            25,
        )
        .await
        .unwrap());
    let pending = session
        .get_attachment_operation("retry")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, AttachmentOperationStatus::Pending);
    assert_eq!(pending.next_attempt_at, Some(50));
    assert_eq!(pending.last_error.as_deref(), Some("temporary"));
    assert_eq!(pending.lease_owner, None);

    let claimed = session
        .claim_attachment_operations("worker", 50, 60, 1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let invalid = session
        .fail_attachment_operation(
            "retry",
            "worker",
            2,
            AttachmentOperationStatus::Completed,
            None,
            "invalid transition",
            55,
        )
        .await
        .expect_err("failure transition must target pending or dead");
    assert_eq!(invalid.kind(), note_storage::StorageErrorKind::Constraint);
    let invalid = session
        .fail_attachment_operation(
            "retry",
            "worker",
            2,
            AttachmentOperationStatus::Pending,
            None,
            "missing retry time",
            55,
        )
        .await
        .expect_err("pending failure must schedule another attempt");
    assert_eq!(invalid.kind(), note_storage::StorageErrorKind::Constraint);
    assert!(session
        .fail_attachment_operation(
            "retry",
            "worker",
            2,
            AttachmentOperationStatus::Dead,
            None,
            "permanent",
            55,
        )
        .await
        .unwrap());
    let dead = session
        .get_attachment_operation("retry")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dead.status, AttachmentOperationStatus::Dead);
    assert_eq!(dead.next_attempt_at, None);
    assert_eq!(dead.last_error.as_deref(), Some("permanent"));

    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}
