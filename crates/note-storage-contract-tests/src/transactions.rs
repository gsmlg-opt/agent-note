use crate::unit;
use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, ConditionalUpdate, NewNote, NewOrgAttempt, NewOrgAttemptAllocation,
    NewOrgDocument, NewOrgEvent, NewOrgLease, NewOrgWorkspace, OrgAttemptStatus,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEventType,
    OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseKind, OrgLeaseOwnershipMove, OrgLeaseProof,
    OrgProjectedWorkItem, OrgWorkspaceUpdate, StorageBackend, StorageErrorKind, StorageTransaction,
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
    let target_workspace_id =
        WorkspaceId::from_str("60000000-0000-0000-0000-000000000002").unwrap();
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
        .insert_org_workspace(NewOrgWorkspace {
            id: target_workspace_id,
            slug: "transaction-ledger-target",
            display_name: "Transaction ledger target",
            description: "shared Org ownership rollback target",
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
    let CompareAndSwap::Applied(updated_workspace) = org_atomic
        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
            id: workspace_id,
            expected_revision: 1,
            slug: "transaction-ledger",
            display_name: "Transaction ledger changed",
            description: "must roll back",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            archived_at: None,
            updated_at: 510,
        })
        .await
        .unwrap()
    else {
        panic!("Org rollback workspace compare-and-swap was not applied");
    };
    assert_eq!(updated_workspace.revision, 2);
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
    let OrgDocumentOwnershipMoveResult::Applied(moved) = org_atomic
        .compare_and_swap_org_document_ownership(OrgDocumentOwnershipMove {
            document_id,
            source_workspace_id: workspace_id,
            target_workspace_id,
            expected_document_revision: 3,
            expected_source_workspace_revision: 2,
            expected_target_workspace_revision: 1,
            updated_at: 511,
        })
        .await
        .unwrap()
    else {
        panic!("Org rollback ownership compare-and-swap was not applied");
    };
    assert_eq!(moved.document.workspace_id, target_workspace_id);
    org_atomic
        .insert_org_attempt(NewOrgAttempt {
            id: "rolled-back-attempt",
            workspace_id,
            work_item_id: original_item_id,
            attempt_number: 1,
            actor_id: "contract-rollback",
            status: OrgAttemptStatus::Running,
            started_at: 511,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({"rollback": true}),
        })
        .await
        .unwrap();
    assert!(matches!(
        org_atomic
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "rolled-back-lease",
                    workspace_id,
                    work_item_id: original_item_id,
                    attempt_id: "rolled-back-attempt",
                    kind: OrgLeaseKind::Execution,
                    actor_id: "contract-rollback",
                    fencing_token_hash:
                        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                    acquired_at: 511,
                    last_heartbeat_at: 511,
                    expires_at: 611,
                },
                1,
                511,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        org_atomic
            .append_org_event(NewOrgEvent {
                id: "63000000-0000-0000-0000-000000000003",
                workspace_id,
                subject_kind: "document",
                subject_id: "61000000-0000-0000-0000-000000000001",
                actor_id: "contract-rollback",
                attempt_id: Some("rolled-back-attempt"),
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
    assert_eq!(stored_revision_two.workspace_id, workspace_id);
    assert_eq!(stored_revision_two.source, "* TODO Revision two");
    assert_eq!(
        org_observer
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(
        org_observer
            .get_org_workspace(target_workspace_id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert!(org_observer
        .get_org_attempt("rolled-back-attempt")
        .await
        .unwrap()
        .is_none());
    assert!(org_observer
        .get_open_org_lease_internal(original_item_id)
        .await
        .unwrap()
        .is_none());
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

pub(crate) async fn run_org_claim_races(storage: Arc<dyn StorageBackend>) {
    let same_workspace = WorkspaceId::from_str("68000000-0000-0000-0000-000000000001").unwrap();
    let capacity_workspace = WorkspaceId::from_str("68000000-0000-0000-0000-000000000002").unwrap();
    let move_source = WorkspaceId::from_str("68000000-0000-0000-0000-000000000003").unwrap();
    let move_target = WorkspaceId::from_str("68000000-0000-0000-0000-000000000004").unwrap();
    let policy = WorkspacePolicy::engineering_default();
    let seed = storage.session().await.unwrap();
    for (id, slug) in [
        (same_workspace, "race-same-item"),
        (capacity_workspace, "race-capacity"),
        (move_source, "race-move-source"),
        (move_target, "race-move-target"),
    ] {
        seed.insert_org_workspace(NewOrgWorkspace {
            id,
            slug,
            display_name: slug,
            description: "barrier-driven lease race",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 1_000,
        })
        .await
        .unwrap();
    }
    drop(seed);

    let same_item = WorkItemId::from_str("69000000-0000-0000-0000-000000000001").unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first = tokio::spawn(race_claim(
        storage.clone(),
        barrier.clone(),
        same_workspace,
        same_item,
        "race-same-attempt-a",
        "race-same-lease-a",
        "1111111111111111111111111111111111111111111111111111111111111111",
        10,
    ));
    let second = tokio::spawn(race_claim(
        storage.clone(),
        barrier,
        same_workspace,
        same_item,
        "race-same-attempt-b",
        "race-same-lease-b",
        "2222222222222222222222222222222222222222222222222222222222222222",
        10,
    ));
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.into_iter().filter(|won| *won).count(), 1);
    let observer = storage.session().await.unwrap();
    assert_eq!(observer.count_org_attempts(same_item).await.unwrap(), 1);
    assert_eq!(
        observer
            .list_org_lease_history(same_item)
            .await
            .unwrap()
            .len(),
        1
    );
    drop(observer);

    let capacity_item_a = WorkItemId::from_str("69000000-0000-0000-0000-000000000002").unwrap();
    let capacity_item_b = WorkItemId::from_str("69000000-0000-0000-0000-000000000003").unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first = tokio::spawn(race_claim(
        storage.clone(),
        barrier.clone(),
        capacity_workspace,
        capacity_item_a,
        "race-capacity-attempt-a",
        "race-capacity-lease-a",
        "3333333333333333333333333333333333333333333333333333333333333333",
        1,
    ));
    let second = tokio::spawn(race_claim(
        storage.clone(),
        barrier,
        capacity_workspace,
        capacity_item_b,
        "race-capacity-attempt-b",
        "race-capacity-lease-b",
        "4444444444444444444444444444444444444444444444444444444444444444",
        1,
    ));
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.into_iter().filter(|won| *won).count(), 1);
    let observer = storage.session().await.unwrap();
    assert_eq!(
        observer
            .count_active_org_leases(capacity_workspace, 1_000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        observer.count_org_attempts(capacity_item_a).await.unwrap()
            + observer.count_org_attempts(capacity_item_b).await.unwrap(),
        1,
        "capacity loser must roll back its attempt allocation"
    );
    drop(observer);

    let moving_item = WorkItemId::from_str("69000000-0000-0000-0000-000000000004").unwrap();
    let target_claim_item = WorkItemId::from_str("69000000-0000-0000-0000-000000000005").unwrap();
    let setup = storage.begin(TransactionMode::Immediate).await.unwrap();
    let moving_attempt = setup
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "race-move-attempt",
            workspace_id: move_source,
            work_item_id: moving_item,
            actor_id: "race-mover",
            started_at: 1_000,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    assert!(matches!(
        setup
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "race-move-lease",
                    workspace_id: move_source,
                    work_item_id: moving_item,
                    attempt_id: &moving_attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "race-mover",
                    fencing_token_hash:
                        "5555555555555555555555555555555555555555555555555555555555555555",
                    acquired_at: 1_000,
                    last_heartbeat_at: 1_000,
                    expires_at: 1_100,
                },
                1,
                1_000,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    setup.commit().await.unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let move_storage = storage.clone();
    let move_barrier = barrier.clone();
    let mover = tokio::spawn(async move {
        move_barrier.wait().await;
        let transaction = move_storage
            .begin(TransactionMode::Immediate)
            .await
            .unwrap();
        let result = transaction
            .move_org_lease_ownership(OrgLeaseOwnershipMove {
                proof: OrgLeaseProof {
                    lease_id: "race-move-lease",
                    workspace_id: move_source,
                    work_item_id: moving_item,
                    fencing_token_hash:
                        "5555555555555555555555555555555555555555555555555555555555555555",
                    kind: OrgLeaseKind::Execution,
                    actor_id: "race-mover",
                    now: 1_000,
                },
                target_workspace_id: move_target,
                target_capacity: 1,
            })
            .await
            .unwrap();
        let won = matches!(result, ConditionalUpdate::Applied(_));
        if won {
            transaction.commit().await.unwrap();
        } else {
            transaction.rollback().await.unwrap();
        }
        won
    });
    let claimer = tokio::spawn(race_claim(
        storage.clone(),
        barrier,
        move_target,
        target_claim_item,
        "race-target-attempt",
        "race-target-lease",
        "6666666666666666666666666666666666666666666666666666666666666666",
        1,
    ));
    let move_won = mover.await.unwrap();
    let claim_won = claimer.await.unwrap();
    assert_ne!(
        move_won, claim_won,
        "exactly one target capacity action wins"
    );
    let observer = storage.session().await.unwrap();
    assert_eq!(
        observer
            .count_active_org_leases(move_target, 1_000)
            .await
            .unwrap(),
        1
    );
    let moving_lease = observer
        .get_open_org_lease_internal(moving_item)
        .await
        .unwrap()
        .unwrap();
    if move_won {
        assert_eq!(moving_lease.workspace_id, move_target);
        assert_eq!(
            observer
                .count_org_attempts(target_claim_item)
                .await
                .unwrap(),
            0
        );
    } else {
        assert_eq!(moving_lease.workspace_id, move_source);
        assert_eq!(
            observer
                .count_org_attempts(target_claim_item)
                .await
                .unwrap(),
            1
        );
    }
}

pub(crate) async fn run_org_lease_proof_validation(storage: Arc<dyn StorageBackend>) {
    let workspace_id = WorkspaceId::from_str("6a000000-0000-0000-0000-000000000001").unwrap();
    let other_workspace_id = WorkspaceId::from_str("6a000000-0000-0000-0000-000000000002").unwrap();
    let work_item_id = WorkItemId::from_str("6b000000-0000-0000-0000-000000000001").unwrap();
    let other_work_item_id = WorkItemId::from_str("6b000000-0000-0000-0000-000000000002").unwrap();
    let policy = WorkspacePolicy::engineering_default();
    let seed = storage.begin(TransactionMode::Immediate).await.unwrap();
    for (id, slug) in [
        (workspace_id, "lease-proof"),
        (other_workspace_id, "lease-proof-other"),
    ] {
        seed.insert_org_workspace(NewOrgWorkspace {
            id,
            slug,
            display_name: slug,
            description: "proof-only active lease validation",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 1_000,
        })
        .await
        .unwrap();
    }
    let attempt = seed
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "lease-proof-attempt",
            workspace_id,
            work_item_id,
            actor_id: "lease-proof-agent",
            started_at: 1_000,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    let ConditionalUpdate::Applied(inserted) = seed
        .insert_org_lease_if_capacity(
            NewOrgLease {
                id: "lease-proof-active",
                workspace_id,
                work_item_id,
                attempt_id: &attempt.id,
                kind: OrgLeaseKind::Execution,
                actor_id: "lease-proof-agent",
                fencing_token_hash:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                acquired_at: 1_000,
                last_heartbeat_at: 1_010,
                expires_at: 1_100,
            },
            1,
            1_000,
        )
        .await
        .unwrap()
    else {
        panic!("lease proof fixture should fit workspace capacity");
    };
    seed.commit().await.unwrap();

    let session = storage.session().await.unwrap();
    let proof = |now| OrgLeaseProof {
        lease_id: "lease-proof-active",
        workspace_id,
        work_item_id,
        fencing_token_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        kind: OrgLeaseKind::Execution,
        actor_id: "lease-proof-agent",
        now,
    };
    let ConditionalUpdate::Applied(validated) = session
        .validate_org_lease_proof(proof(1_050))
        .await
        .unwrap()
    else {
        panic!("the exact current lease proof should validate");
    };
    assert_eq!(validated, inserted);
    let after_validation = session
        .get_open_org_lease_internal(work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_validation.last_heartbeat_at, 1_010);
    assert_eq!(after_validation.expires_at, 1_100);
    assert_eq!(after_validation.ended_at, None);
    assert_eq!(after_validation.end_reason, None);

    for invalid_proof in [
        OrgLeaseProof {
            lease_id: "lease-proof-missing",
            ..proof(1_050)
        },
        OrgLeaseProof {
            workspace_id: other_workspace_id,
            ..proof(1_050)
        },
        OrgLeaseProof {
            work_item_id: other_work_item_id,
            ..proof(1_050)
        },
        OrgLeaseProof {
            fencing_token_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ..proof(1_050)
        },
        OrgLeaseProof {
            kind: OrgLeaseKind::Review,
            ..proof(1_050)
        },
        OrgLeaseProof {
            actor_id: "lease-proof-other-agent",
            ..proof(1_050)
        },
        proof(1_100),
    ] {
        assert_eq!(
            session
                .validate_org_lease_proof(invalid_proof)
                .await
                .unwrap(),
            ConditionalUpdate::Conflict,
            "every stale active-lease proof must have one generic result"
        );
    }

    let lease_before_rollback = session
        .get_open_org_lease_internal(work_item_id)
        .await
        .unwrap()
        .unwrap();
    let events_before_rollback = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap();
    drop(session);

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    assert!(matches!(
        rollback
            .validate_org_lease_proof(proof(1_055))
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    rollback
        .append_org_event(NewOrgEvent {
            id: "lease-proof-rolled-back-event",
            workspace_id,
            subject_kind: "work_item",
            subject_id: "6b000000-0000-0000-0000-000000000001",
            actor_id: "lease-proof-agent",
            attempt_id: Some(&attempt.id),
            event_type: OrgEventType::Progress,
            occurred_at: 1_055,
            summary: "This event must roll back",
            metadata: &json!({"contract": "lease-proof-rollback"}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(
        rollback
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap()
            .len(),
        events_before_rollback.len() + 1,
        "the transaction must observe its own event before rollback"
    );
    rollback.rollback().await.unwrap();

    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .get_open_org_lease_internal(work_item_id)
            .await
            .unwrap()
            .unwrap(),
        lease_before_rollback,
        "proof validation followed by rollback must preserve every lease field"
    );
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap(),
        events_before_rollback,
        "the event appended after proof validation must roll back completely"
    );

    assert!(matches!(
        session
            .close_org_lease(OrgLeaseClosure {
                proof: proof(1_060),
                ended_at: 1_060,
                end_reason: OrgLeaseEndReason::Release,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        session
            .validate_org_lease_proof(proof(1_061))
            .await
            .unwrap(),
        ConditionalUpdate::Conflict,
        "a released lease proof must stay stale"
    );
}

async fn race_claim(
    storage: Arc<dyn StorageBackend>,
    barrier: Arc<tokio::sync::Barrier>,
    workspace_id: WorkspaceId,
    work_item_id: WorkItemId,
    attempt_id: &'static str,
    lease_id: &'static str,
    fencing_token_hash: &'static str,
    capacity: i64,
) -> bool {
    barrier.wait().await;
    let transaction = storage.begin(TransactionMode::Immediate).await.unwrap();
    let attempt = transaction
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: attempt_id,
            workspace_id,
            work_item_id,
            actor_id: "race-claimer",
            started_at: 1_000,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    let won = matches!(
        transaction
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: lease_id,
                    workspace_id,
                    work_item_id,
                    attempt_id: &attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "race-claimer",
                    fencing_token_hash,
                    acquired_at: 1_000,
                    last_heartbeat_at: 1_000,
                    expires_at: 1_100,
                },
                capacity,
                1_000,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    );
    if won {
        transaction.commit().await.unwrap();
    } else {
        transaction.rollback().await.unwrap();
    }
    won
}
