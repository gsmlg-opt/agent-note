use crate::unit;
use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewNote, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, OrgDocumentUpdate,
    OrgEventType, OrgProjectedWorkItem, StorageBackend, StorageErrorKind, StorageTransaction,
    StoredOrgOperation, TransactionMode, UpsertNoteChunk,
};
use serde_json::json;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

async fn insert_note(transaction: &dyn StorageTransaction, id: &str) {
    transaction
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
    let deferred = storage.begin(TransactionMode::Deferred).await.unwrap();
    insert_note(deferred.as_ref(), "contract-transactions-deferred").await;
    deferred.commit().await.unwrap();
    assert!(storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-deferred")
        .await
        .unwrap());

    let immediate = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(immediate.as_ref(), "contract-transactions-immediate").await;
    immediate.commit().await.unwrap();
    assert!(storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-immediate")
        .await
        .unwrap());

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(rollback.as_ref(), "contract-transactions-rollback").await;
    rollback.rollback().await.unwrap();
    assert!(!storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-rollback")
        .await
        .unwrap());

    let dropped = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(dropped.as_ref(), "contract-transactions-dropped").await;
    drop(dropped);
    let after_drop = tokio::time::timeout(
        Duration::from_secs(2),
        storage.begin(TransactionMode::Immediate),
    )
    .await
    .expect("writer lock remained held after dropping an unfinished transaction")
    .unwrap();
    after_drop.rollback().await.unwrap();
    assert!(!storage
        .session()
        .await
        .unwrap()
        .note_exists("contract-transactions-dropped")
        .await
        .unwrap());

    let session = storage.session().await.unwrap();
    session
        .insert_label_key("contract-transactions-status", "Status")
        .await
        .unwrap();
    let atomic = storage.begin(TransactionMode::Immediate).await.unwrap();
    insert_note(atomic.as_ref(), "contract-transactions-atomic").await;
    atomic
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "contract-transactions-atomic",
            chunk_idx: 0,
            content_hash: "atomic-hash",
            content: "content",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    atomic
        .enqueue_embedding_job(
            "contract-transactions-atomic",
            0,
            "atomic-hash",
            "content",
            1,
            1,
        )
        .await
        .unwrap();
    atomic
        .insert_chunk_embedding("contract-transactions-atomic", 0, &unit(0))
        .await
        .unwrap();
    atomic
        .attach_label(
            "contract-transactions-atomic",
            "contract-transactions-status",
            "ready",
        )
        .await
        .unwrap();
    let duplicate = atomic
        .attach_label(
            "contract-transactions-atomic",
            "contract-transactions-status",
            "duplicate",
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.kind(), StorageErrorKind::Constraint);
    atomic.rollback().await.unwrap();

    let observer = storage.session().await.unwrap();
    assert!(!observer
        .note_exists("contract-transactions-atomic")
        .await
        .unwrap());
    assert!(observer
        .list_note_chunks("contract-transactions-atomic")
        .await
        .unwrap()
        .is_empty());
    assert!(!observer
        .chunk_embedding_exists("contract-transactions-atomic", 0)
        .await
        .unwrap());
    assert!(observer
        .labels_for_note("contract-transactions-atomic")
        .await
        .unwrap()
        .is_empty());
    assert!(observer
        .claim_pending_embedding_jobs(1, 2)
        .await
        .unwrap()
        .is_empty());

    let workspace_id = WorkspaceId::from_str("60000000-0000-0000-0000-000000000001").unwrap();
    let document_id = DocumentId::from_str("61000000-0000-0000-0000-000000000001").unwrap();
    let original_item_id = WorkItemId::from_str("62000000-0000-0000-0000-000000000001").unwrap();
    let replacement_item_id = WorkItemId::from_str("62000000-0000-0000-0000-000000000002").unwrap();
    let policy = WorkspacePolicy::engineering_default();
    let org_seed = storage.begin(TransactionMode::Immediate).await.unwrap();
    org_seed
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id,
            slug: "transaction-ledger",
            display_name: "Transaction ledger",
            description: "shared Org rollback contract",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 500,
        })
        .await
        .unwrap();
    org_seed
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "transaction.org",
            source: "* TODO Revision one",
            content_hash: "transaction-hash-one",
            now: 501,
        })
        .await
        .unwrap();
    let CompareAndSwap::Applied(revision_two) = org_seed
        .compare_and_swap_org_document(OrgDocumentUpdate {
            id: document_id,
            expected_revision: 1,
            path: "transaction.org",
            source: "* TODO Revision two",
            content_hash: "transaction-hash-two",
            updated_at: 502,
        })
        .await
        .unwrap()
    else {
        panic!("Org transaction seed compare-and-swap was not applied");
    };
    let original_projection = vec![OrgProjectedWorkItem {
        id: original_item_id,
        workspace_id,
        document_id,
        parent_id: None,
        source_order: 0,
        item_type: WorkItemType::Task,
        title: "Original projection".into(),
        state: Some("TODO".into()),
        priority: None,
        scheduled: None,
        deadline: None,
        assignee: None,
        requires_review: false,
        created_at: 503,
        tags: vec!["original".into()],
        dependencies: Vec::new(),
        note_links: Vec::new(),
    }];
    org_seed
        .replace_org_document_projection(document_id, &original_projection)
        .await
        .unwrap();
    for (id, occurred_at) in [
        ("63000000-0000-0000-0000-000000000001", 505),
        ("63000000-0000-0000-0000-000000000002", 504),
    ] {
        org_seed
            .append_org_event(NewOrgEvent {
                id,
                workspace_id,
                subject_kind: "document",
                subject_id: "61000000-0000-0000-0000-000000000001",
                actor_id: "contract-seed",
                attempt_id: None,
                event_type: OrgEventType::Creation,
                occurred_at,
                summary: "Seeded event",
                metadata: &json!({"seed": true}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    org_seed.commit().await.unwrap();
    assert_eq!(revision_two.revision, 2);

    let replacement_projection = vec![OrgProjectedWorkItem {
        id: replacement_item_id,
        workspace_id,
        document_id,
        parent_id: None,
        source_order: 1,
        item_type: WorkItemType::Task,
        title: "Replacement projection".into(),
        state: Some("ACTIVE".into()),
        priority: Some('B'),
        scheduled: None,
        deadline: None,
        assignee: Some("agent-rollback".into()),
        requires_review: true,
        created_at: 510,
        tags: vec!["replacement".into()],
        dependencies: Vec::new(),
        note_links: Vec::new(),
    }];
    let rolled_back_operation = StoredOrgOperation {
        workspace_id,
        operation_id: "rolled-back-operation".into(),
        request_fingerprint: "sha256:rolled-back".into(),
        result: json!({"document_revision": 3, "nested": {"atomic": true}}),
        created_at: 513,
    };
    let org_atomic = storage.begin(TransactionMode::Immediate).await.unwrap();
    let CompareAndSwap::Applied(revision_three) = org_atomic
        .compare_and_swap_org_document(OrgDocumentUpdate {
            id: document_id,
            expected_revision: 2,
            path: "transaction.org",
            source: "* ACTIVE Revision three",
            content_hash: "transaction-hash-three",
            updated_at: 511,
        })
        .await
        .unwrap()
    else {
        panic!("Org rollback compare-and-swap was not applied");
    };
    assert_eq!(revision_three.revision, 3);
    org_atomic
        .replace_org_document_projection(document_id, &replacement_projection)
        .await
        .unwrap();
    assert_eq!(
        org_atomic
            .append_org_event(NewOrgEvent {
                id: "63000000-0000-0000-0000-000000000003",
                workspace_id,
                subject_kind: "document",
                subject_id: "61000000-0000-0000-0000-000000000001",
                actor_id: "contract-rollback",
                attempt_id: None,
                event_type: OrgEventType::DocumentImport,
                occurred_at: 512,
                summary: "Rolled-back event",
                metadata: &json!({"revision": 3}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap()
            .sequence,
        3
    );
    org_atomic
        .insert_org_operation(&rolled_back_operation)
        .await
        .unwrap();
    let duplicate_operation = org_atomic
        .insert_org_operation(&rolled_back_operation)
        .await
        .unwrap_err();
    assert_eq!(duplicate_operation.kind(), StorageErrorKind::Constraint);
    org_atomic.rollback().await.unwrap();

    let org_observer = storage.session().await.unwrap();
    let stored_revision_two = org_observer
        .get_org_document(document_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_revision_two.revision, 2);
    assert_eq!(stored_revision_two.source, "* TODO Revision two");
    assert_eq!(
        org_observer
            .list_org_document_projection(document_id)
            .await
            .unwrap(),
        original_projection
    );
    assert!(org_observer
        .get_org_operation(workspace_id, "rolled-back-operation")
        .await
        .unwrap()
        .is_none());
    let after_rollback_metadata = json!({"revision": 2, "after_rollback": true});
    let next_event = org_observer
        .append_org_event(NewOrgEvent {
            id: "63000000-0000-0000-0000-000000000004",
            workspace_id,
            subject_kind: "document",
            subject_id: "61000000-0000-0000-0000-000000000001",
            actor_id: "contract-observer",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 514,
            summary: "Verified rollback",
            metadata: &after_rollback_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(next_event.sequence, 3);

    let first = storage.begin(TransactionMode::Immediate).await.unwrap();
    let second_storage = storage.clone();
    let mut second = Box::pin(async move {
        second_storage
            .begin(TransactionMode::Immediate)
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second.as_mut())
            .await
            .is_err()
    );
    first.commit().await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("second immediate transaction did not proceed after commit");
    second.rollback().await.unwrap();
}
