use note_org::{
    ClaimPolicy, DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy,
};
use note_storage::{
    CompareAndSwap, ConditionalUpdate, ExpiredOrgLeaseClosure, NewOrgAttempt,
    NewOrgAttemptAllocation, NewOrgDocument, NewOrgEvent, NewOrgLease, NewOrgWorkspace,
    OrgArtifactReference, OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEventType,
    OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseHeartbeat, OrgLeaseKind, OrgLeaseOwnershipMove,
    OrgLeaseProof, OrgOperationalQuery, OrgOperationalView, OrgProjectedWorkItem, OrgReadyMarker,
    OrgReviewLeaseMarker, OrgWorkspaceUpdate, StorageBackend, StorageErrorKind, StoredOrgOperation,
    StoredOrgTimestamp, TransactionMode,
};
use serde_json::json;
use std::str::FromStr as _;
use std::sync::Arc;

fn workspace_id(value: &str) -> WorkspaceId {
    WorkspaceId::from_str(value).unwrap()
}

fn document_id(value: &str) -> DocumentId {
    DocumentId::from_str(value).unwrap()
}

fn work_item_id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

fn scheduled(raw: &str, local: &str, utc_timestamp: i64) -> StoredOrgTimestamp {
    StoredOrgTimestamp {
        raw: raw.into(),
        local: local.into(),
        timezone: "Asia/Shanghai".into(),
        utc_timestamp,
    }
}

fn projected_item(
    id: WorkItemId,
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    parent_id: Option<WorkItemId>,
    source_order: i64,
    title: &str,
) -> OrgProjectedWorkItem {
    OrgProjectedWorkItem {
        id,
        workspace_id,
        document_id,
        parent_id,
        source_order,
        item_type: WorkItemType::Task,
        title: title.into(),
        state: Some("TODO".into()),
        priority: Some('A'),
        scheduled: None,
        deadline: None,
        assignee: Some("agent-one".into()),
        requires_review: false,
        created_at: 60,
        tags: Vec::new(),
        dependencies: Vec::new(),
        note_links: Vec::new(),
    }
}

fn operational_query<'a>(
    view: OrgOperationalView,
    workspace_ids: &'a [WorkspaceId],
    now: i64,
) -> OrgOperationalQuery<'a> {
    OrgOperationalQuery {
        view,
        workspace_ids,
        item_type: None,
        state: None,
        priority: None,
        tags: &[],
        assignee: None,
        scheduled_from: None,
        scheduled_to: None,
        deadline_from: None,
        deadline_to: None,
        completed_from: None,
        completed_to: None,
        include_archived: false,
        now,
        after: None,
        limit: 50,
    }
}

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    let alpha_id = workspace_id("10000000-0000-0000-0000-000000000001");
    let beta_id = workspace_id("10000000-0000-0000-0000-000000000002");
    let missing_workspace_id = workspace_id("10000000-0000-0000-0000-000000000099");
    let policy = WorkspacePolicy::engineering_default();

    session
        .insert_org_workspace(NewOrgWorkspace {
            id: beta_id,
            slug: "beta",
            display_name: "Beta",
            description: "second workspace",
            timezone: "Asia/Shanghai",
            policy_schema_version: 1,
            policy: &policy,
            now: 10,
        })
        .await
        .unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: alpha_id,
            slug: "alpha",
            display_name: "Alpha",
            description: "first workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 20,
        })
        .await
        .unwrap();

    let alpha = session.get_org_workspace(alpha_id).await.unwrap().unwrap();
    assert_eq!(alpha.slug, "alpha");
    assert_eq!(alpha.revision, 1);
    assert_eq!(alpha.created_at, 20);
    assert_eq!(alpha.updated_at, 20);
    assert_eq!(alpha.policy, policy);
    assert_eq!(
        session
            .get_org_workspace_by_slug("beta")
            .await
            .unwrap()
            .unwrap()
            .id,
        beta_id
    );

    let duplicate_slug = session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id("10000000-0000-0000-0000-000000000003"),
            slug: "alpha",
            display_name: "Duplicate",
            description: "duplicate slug",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 21,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_slug.kind(), StorageErrorKind::Constraint);

    let source = "* TODO Unicode 任务\r\n#+begin_src opaque\r\n\\0 not parsed\r\n#+end_src";
    let revision_two_source =
        "* DONE Unicode 任务\r\n#+begin_src opaque\r\n\\0 still not parsed\r\n#+end_src";
    let first_document_id = document_id("20000000-0000-0000-0000-000000000001");
    let second_document_id = document_id("20000000-0000-0000-0000-000000000002");
    session
        .insert_org_document(NewOrgDocument {
            id: first_document_id,
            workspace_id: alpha_id,
            path: "zeta.org",
            source,
            content_hash: "hash-one",
            now: 30,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: second_document_id,
            workspace_id: alpha_id,
            path: "alpha.org",
            source: "* TODO Other",
            content_hash: "hash-other",
            now: 31,
        })
        .await
        .unwrap();

    let stored = session
        .get_org_document(first_document_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.source.as_bytes(), source.as_bytes());
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.created_at, 30);
    assert_eq!(stored.updated_at, 30);
    let documents = session.list_org_documents(alpha_id).await.unwrap();
    assert_eq!(
        documents
            .iter()
            .map(|document| (document.path.as_str(), document.id))
            .collect::<Vec<_>>(),
        vec![
            ("alpha.org", second_document_id),
            ("zeta.org", first_document_id)
        ]
    );

    let duplicate_path = session
        .insert_org_document(NewOrgDocument {
            id: document_id("20000000-0000-0000-0000-000000000003"),
            workspace_id: alpha_id,
            path: "zeta.org",
            source: "duplicate",
            content_hash: "duplicate-hash",
            now: 32,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_path.kind(), StorageErrorKind::Constraint);

    let document_update = OrgDocumentUpdate {
        id: first_document_id,
        expected_revision: 1,
        path: "zeta.org",
        source: revision_two_source,
        content_hash: "hash-two",
        updated_at: 40,
    };
    let CompareAndSwap::Applied(updated_document) = session
        .compare_and_swap_org_document(document_update)
        .await
        .unwrap()
    else {
        panic!("document compare-and-swap was not applied");
    };
    assert_eq!(updated_document.revision, 2);
    assert_eq!(
        updated_document.source.as_bytes(),
        revision_two_source.as_bytes()
    );
    assert_eq!(updated_document.created_at, 30);
    assert_eq!(updated_document.updated_at, 40);

    assert_eq!(
        session
            .compare_and_swap_org_document(OrgDocumentUpdate {
                id: first_document_id,
                expected_revision: 1,
                path: "stale.org",
                source: "stale",
                content_hash: "stale-hash",
                updated_at: 41,
            })
            .await
            .unwrap(),
        CompareAndSwap::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(
        session
            .get_org_document(first_document_id)
            .await
            .unwrap()
            .unwrap()
            .source
            .as_bytes(),
        revision_two_source.as_bytes()
    );
    assert_eq!(
        session
            .compare_and_swap_org_document(OrgDocumentUpdate {
                id: document_id("20000000-0000-0000-0000-000000000099"),
                expected_revision: 1,
                path: "missing.org",
                source: "missing",
                content_hash: "missing-hash",
                updated_at: 42,
            })
            .await
            .unwrap(),
        CompareAndSwap::NotFound
    );

    let parent_id = work_item_id("30000000-0000-0000-0000-000000000001");
    let child_id = work_item_id("30000000-0000-0000-0000-000000000002");
    let mut parent = projected_item(
        parent_id,
        alpha_id,
        first_document_id,
        None,
        0,
        "Release service",
    );
    parent.item_type = WorkItemType::Epic;
    parent.state = Some("ACTIVE".into());
    parent.priority = None;
    parent.assignee = None;
    let mut child = projected_item(
        child_id,
        alpha_id,
        first_document_id,
        Some(parent_id),
        1,
        "Ship backend",
    );
    child.tags = vec!["backend".into(), "release".into()];
    child.dependencies = vec![parent_id];
    child.scheduled = Some(scheduled(
        "<2026-08-04 Tue 15:30>",
        "2026-08-04T15:30:00",
        1_785_828_600,
    ));
    child.note_links = vec![
        NoteLink {
            purpose: "context".into(),
            note_id: "40000000-0000-0000-0000-000000000001".parse().unwrap(),
            description: "available note".into(),
        },
        NoteLink {
            purpose: "evidence".into(),
            note_id: "40000000-0000-0000-0000-000000000099".parse().unwrap(),
            description: "unavailable note".into(),
        },
    ];
    let initial_projection = vec![parent.clone(), child.clone()];

    let projection = storage.begin(TransactionMode::Immediate).await.unwrap();
    projection
        .replace_org_document_projection(first_document_id, &initial_projection)
        .await
        .unwrap();
    projection.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        initial_projection
    );

    let mut changed_parent = parent.clone();
    changed_parent.title = "Release service safely".into();
    let mut changed_child = child.clone();
    changed_child.title = "Ship backend safely".into();
    changed_child.tags = vec!["release".into()];
    changed_child.dependencies.clear();
    changed_child.note_links = vec![NoteLink {
        purpose: "context".into(),
        note_id: "40000000-0000-0000-0000-000000000001".parse().unwrap(),
        description: "updated available note".into(),
    }];
    let changed_projection = vec![changed_parent.clone(), changed_child.clone()];
    let replacement = storage.begin(TransactionMode::Immediate).await.unwrap();
    replacement
        .replace_org_document_projection(first_document_id, &changed_projection)
        .await
        .unwrap();
    replacement.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection
    );

    let mut reordered_parent = changed_parent.clone();
    reordered_parent.source_order = 1;
    let mut reordered_child = changed_child.clone();
    reordered_child.source_order = 0;
    let reordered_projection = vec![reordered_child, reordered_parent];
    let reorder = storage.begin(TransactionMode::Immediate).await.unwrap();
    reorder
        .replace_org_document_projection(first_document_id, &reordered_projection)
        .await
        .unwrap();
    reorder.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        reordered_projection,
        "stable work item IDs must be able to swap source order"
    );
    let restore_order = storage.begin(TransactionMode::Immediate).await.unwrap();
    restore_order
        .replace_org_document_projection(first_document_id, &changed_projection)
        .await
        .unwrap();
    restore_order.commit().await.unwrap();

    let omitted = storage.begin(TransactionMode::Immediate).await.unwrap();
    omitted
        .replace_org_document_projection(first_document_id, &[changed_parent.clone()])
        .await
        .unwrap();
    omitted.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection,
        "replacement must not delete a work item omitted by the import candidate"
    );

    let mut omitted_order_conflict = changed_parent.clone();
    omitted_order_conflict.source_order = changed_child.source_order;
    let conflict = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = conflict
        .replace_org_document_projection(first_document_id, &[omitted_order_conflict])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    conflict.rollback().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection
    );

    let duplicate_id = work_item_id("30000000-0000-0000-0000-000000000003");
    let duplicate = projected_item(
        duplicate_id,
        alpha_id,
        first_document_id,
        None,
        0,
        "Duplicate source order",
    );
    let rollback_item_id = work_item_id("30000000-0000-0000-0000-000000000004");
    let rollback_item = projected_item(
        rollback_item_id,
        alpha_id,
        second_document_id,
        None,
        0,
        "Must roll back",
    );
    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    rollback
        .replace_org_document_projection(second_document_id, &[rollback_item])
        .await
        .unwrap();
    let error = rollback
        .replace_org_document_projection(first_document_id, &[changed_parent.clone(), duplicate])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    rollback.rollback().await.unwrap();
    assert!(session
        .list_org_document_projection(second_document_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection
    );

    let stale_id = work_item_id("30000000-0000-0000-0000-000000000005");
    let stale = projected_item(
        stale_id,
        alpha_id,
        second_document_id,
        None,
        0,
        "Stale projection",
    );
    let seed_stale = storage.begin(TransactionMode::Immediate).await.unwrap();
    seed_stale
        .replace_org_document_projection(second_document_id, &[stale])
        .await
        .unwrap();
    seed_stale.commit().await.unwrap();

    let rebuilt_id = work_item_id("30000000-0000-0000-0000-000000000006");
    let rebuilt = projected_item(
        rebuilt_id,
        alpha_id,
        second_document_id,
        None,
        0,
        "Rebuilt projection",
    );
    let mut all_rebuilt = changed_projection.clone();
    all_rebuilt.push(rebuilt.clone());
    let rebuild = storage.begin(TransactionMode::Immediate).await.unwrap();
    rebuild
        .rebuild_org_workspace_projection(alpha_id, &all_rebuilt)
        .await
        .unwrap();
    rebuild.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection
    );
    assert_eq!(
        session
            .list_org_document_projection(second_document_id)
            .await
            .unwrap(),
        vec![rebuilt.clone()]
    );

    let missing_parent_id = work_item_id("30000000-0000-0000-0000-000000000099");
    let mut missing_parent = changed_child.clone();
    missing_parent.parent_id = Some(missing_parent_id);
    let invalid_parent = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = invalid_parent
        .replace_org_document_projection(first_document_id, &[missing_parent])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    invalid_parent.rollback().await.unwrap();

    let mut cyclic_parent = changed_parent.clone();
    cyclic_parent.parent_id = Some(child_id);
    let cycle = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = cycle
        .replace_org_document_projection(first_document_id, &[cyclic_parent, changed_child.clone()])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    cycle.rollback().await.unwrap();

    let beta_document_id = document_id("20000000-0000-0000-0000-000000000010");
    session
        .insert_org_document(NewOrgDocument {
            id: beta_document_id,
            workspace_id: beta_id,
            path: "beta.org",
            source: "* TODO Beta",
            content_hash: "hash-beta",
            now: 70,
        })
        .await
        .unwrap();
    let beta_item_id = work_item_id("30000000-0000-0000-0000-000000000010");
    let beta_item = projected_item(
        beta_item_id,
        beta_id,
        beta_document_id,
        None,
        0,
        "Beta item",
    );
    let seed_beta = storage.begin(TransactionMode::Immediate).await.unwrap();
    seed_beta
        .replace_org_document_projection(beta_document_id, &[beta_item])
        .await
        .unwrap();
    seed_beta.commit().await.unwrap();
    let mut cross_workspace_dependency = changed_child.clone();
    cross_workspace_dependency.dependencies = vec![beta_item_id];
    let cross_workspace = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = cross_workspace
        .replace_org_document_projection(first_document_id, &[cross_workspace_dependency])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    cross_workspace.rollback().await.unwrap();

    let mut mismatched_workspace = changed_parent.clone();
    mismatched_workspace.workspace_id = beta_id;
    let mismatch = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = mismatch
        .replace_org_document_projection(first_document_id, &[mismatched_workspace])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    mismatch.rollback().await.unwrap();

    let mut candidate_cycle_parent = changed_parent.clone();
    candidate_cycle_parent.dependencies = vec![child_id];
    let mut candidate_cycle_child = changed_child.clone();
    candidate_cycle_child.dependencies = vec![parent_id];
    let candidate_cycle = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = candidate_cycle
        .replace_org_document_projection(
            first_document_id,
            &[candidate_cycle_parent, candidate_cycle_child],
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    candidate_cycle.rollback().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        changed_projection
    );

    let mut omitted_cycle_child = changed_child.clone();
    omitted_cycle_child.dependencies = vec![parent_id];
    let omitted_cycle_base = vec![changed_parent.clone(), omitted_cycle_child.clone()];
    let seed_omitted_cycle = storage.begin(TransactionMode::Immediate).await.unwrap();
    seed_omitted_cycle
        .replace_org_document_projection(first_document_id, &omitted_cycle_base)
        .await
        .unwrap();
    seed_omitted_cycle.commit().await.unwrap();
    let mut closing_candidate = changed_parent.clone();
    closing_candidate.dependencies = vec![child_id];
    let omitted_cycle = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = omitted_cycle
        .replace_org_document_projection(first_document_id, &[closing_candidate])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    omitted_cycle.rollback().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(first_document_id)
            .await
            .unwrap(),
        omitted_cycle_base,
        "dependency-cycle rejection must leave the prior projection unchanged"
    );

    let scale_workspace_id = workspace_id("10000000-0000-0000-0000-000000000020");
    let scale_document_id = document_id("20000000-0000-0000-0000-000000000020");
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: scale_workspace_id,
            slug: "scale",
            display_name: "Scale",
            description: "projection validation stress workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 80,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: scale_document_id,
            workspace_id: scale_workspace_id,
            path: "deep.org",
            source: "* TODO Deep projection",
            content_hash: "hash-deep",
            now: 81,
        })
        .await
        .unwrap();
    let mut deep_projection = Vec::new();
    let mut previous_id = None;
    for index in 0..257 {
        let id = work_item_id(&format!("60000000-0000-0000-0000-{index:012x}"));
        deep_projection.push(projected_item(
            id,
            scale_workspace_id,
            scale_document_id,
            previous_id,
            index,
            &format!("Deep item {index}"),
        ));
        previous_id = Some(id);
    }
    let persist_deep = storage.begin(TransactionMode::Immediate).await.unwrap();
    persist_deep
        .replace_org_document_projection(scale_document_id, &deep_projection)
        .await
        .unwrap();
    persist_deep.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(scale_document_id)
            .await
            .unwrap(),
        deep_projection
    );

    let mut deep_cycle = deep_projection.clone();
    let last_id = deep_cycle.last().unwrap().id;
    deep_cycle.last_mut().unwrap().parent_id = Some(last_id);
    let reject_deep_cycle = storage.begin(TransactionMode::Immediate).await.unwrap();
    let error = reject_deep_cycle
        .replace_org_document_projection(scale_document_id, &deep_cycle)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    reject_deep_cycle.rollback().await.unwrap();
    assert_eq!(
        session
            .list_org_document_projection(scale_document_id)
            .await
            .unwrap(),
        deep_projection
    );

    let alpha_first_metadata = json!({
        "change": {"from": "TODO", "to": "ACTIVE"},
        "labels": ["backend", "release"]
    });
    let alpha_first = session
        .append_org_event(NewOrgEvent {
            id: "50000000-0000-0000-0000-000000000001",
            workspace_id: alpha_id,
            subject_kind: "work_item",
            subject_id: "30000000-0000-0000-0000-000000000001",
            actor_id: "agent-one",
            attempt_id: None,
            event_type: OrgEventType::Start,
            occurred_at: 200,
            summary: "Activated release",
            metadata: &alpha_first_metadata,
            previous_state: Some("TODO"),
            resulting_state: Some("ACTIVE"),
        })
        .await
        .unwrap();
    let alpha_second_metadata = json!({"revision": 2, "nested": {"verified": true}});
    let alpha_second = session
        .append_org_event(NewOrgEvent {
            id: "50000000-0000-0000-0000-000000000002",
            workspace_id: alpha_id,
            subject_kind: "document",
            subject_id: "20000000-0000-0000-0000-000000000001",
            actor_id: "agent-two",
            attempt_id: None,
            event_type: OrgEventType::DocumentImport,
            occurred_at: 100,
            summary: "Updated canonical source",
            metadata: &alpha_second_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    let beta_metadata = json!({"queue": "triage"});
    let beta_first = session
        .append_org_event(NewOrgEvent {
            id: "50000000-0000-0000-0000-000000000003",
            workspace_id: beta_id,
            subject_kind: "workspace",
            subject_id: "10000000-0000-0000-0000-000000000002",
            actor_id: "agent-three",
            attempt_id: None,
            event_type: OrgEventType::WorkspaceChange,
            occurred_at: 150,
            summary: "Checked beta workspace",
            metadata: &beta_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(alpha_first.sequence, 1);
    assert_eq!(alpha_second.sequence, 2);
    assert_eq!(beta_first.sequence, 1);

    let alpha_events = session.list_org_events(alpha_id, None, 200).await.unwrap();
    assert_eq!(
        alpha_events,
        vec![alpha_first.clone(), alpha_second.clone()]
    );
    assert_eq!(
        alpha_events
            .iter()
            .map(|event| event.occurred_at)
            .collect::<Vec<_>>(),
        vec![200, 100],
        "Org events must be ordered by sequence rather than occurrence time"
    );
    assert_eq!(
        session
            .list_org_events(alpha_id, Some(1), 200)
            .await
            .unwrap(),
        vec![alpha_second.clone()],
        "after_sequence must be exclusive"
    );
    assert_eq!(
        session.list_org_events(alpha_id, None, 1).await.unwrap(),
        vec![alpha_first.clone()],
        "Org event list limit must be honored"
    );
    assert_eq!(
        session.list_org_events(beta_id, None, 200).await.unwrap(),
        vec![beta_first]
    );
    for invalid_limit in [0, 201] {
        let error = session
            .list_org_events(alpha_id, None, invalid_limit)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
    }

    let duplicate_event_metadata = json!({"duplicate": true});
    let duplicate_event = session
        .append_org_event(NewOrgEvent {
            id: "50000000-0000-0000-0000-000000000001",
            workspace_id: alpha_id,
            subject_kind: "work_item",
            subject_id: "30000000-0000-0000-0000-000000000001",
            actor_id: "agent-duplicate",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 250,
            summary: "Duplicate event id",
            metadata: &duplicate_event_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_event.kind(), StorageErrorKind::Constraint);
    let alpha_third_metadata = json!({"after_failed_append": true});
    let alpha_third = session
        .append_org_event(NewOrgEvent {
            id: "50000000-0000-0000-0000-000000000004",
            workspace_id: alpha_id,
            subject_kind: "work_item",
            subject_id: "30000000-0000-0000-0000-000000000002",
            actor_id: "agent-four",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 260,
            summary: "Appended after failed duplicate",
            metadata: &alpha_third_metadata,
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(
        alpha_third.sequence, 3,
        "a failed Org event insert must not consume its allocated sequence"
    );
    assert_eq!(
        session.list_org_events(alpha_id, None, 200).await.unwrap(),
        vec![alpha_first.clone(), alpha_second.clone(), alpha_third],
        "failed Org event inserts must not leave sequence gaps"
    );

    let operation = StoredOrgOperation {
        workspace_id: alpha_id,
        operation_id: "operation-alpha-1".into(),
        request_fingerprint: "sha256:alpha-1".into(),
        result: json!({
            "status": "applied",
            "document": {"id": first_document_id.to_string(), "revision": 2},
            "warnings": [{"code": "none", "details": {"count": 0}}]
        }),
        created_at: 300,
    };
    session.insert_org_operation(&operation).await.unwrap();
    assert_eq!(
        session
            .get_org_operation(alpha_id, "operation-alpha-1")
            .await
            .unwrap(),
        Some(operation.clone())
    );
    let duplicate_operation = session.insert_org_operation(&operation).await.unwrap_err();
    assert_eq!(duplicate_operation.kind(), StorageErrorKind::Constraint);

    let events_before_rebuild = session.list_org_events(alpha_id, None, 200).await.unwrap();
    let operation_before_rebuild = session
        .get_org_operation(alpha_id, "operation-alpha-1")
        .await
        .unwrap();
    let mut preserved_rebuild = omitted_cycle_base.clone();
    preserved_rebuild.push(rebuilt.clone());
    let preserve_ledger = storage.begin(TransactionMode::Immediate).await.unwrap();
    preserve_ledger
        .rebuild_org_workspace_projection(alpha_id, &preserved_rebuild)
        .await
        .unwrap();
    preserve_ledger.commit().await.unwrap();
    assert_eq!(
        session.list_org_events(alpha_id, None, 200).await.unwrap(),
        events_before_rebuild,
        "projection rebuild must preserve Org event sequence, content, and metadata"
    );
    assert_eq!(
        session
            .get_org_operation(alpha_id, "operation-alpha-1")
            .await
            .unwrap(),
        operation_before_rebuild,
        "projection rebuild must preserve Org operation results"
    );

    let CompareAndSwap::Applied(archived_alpha) = session
        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
            id: alpha_id,
            expected_revision: 1,
            slug: "alpha",
            display_name: "Alpha archived",
            description: "first workspace archived",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            archived_at: Some(50),
            updated_at: 50,
        })
        .await
        .unwrap()
    else {
        panic!("workspace compare-and-swap was not applied");
    };
    assert_eq!(archived_alpha.revision, 2);
    assert_eq!(archived_alpha.created_at, 20);
    assert_eq!(archived_alpha.updated_at, 50);
    assert_eq!(archived_alpha.archived_at, Some(50));

    assert_eq!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: alpha_id,
                expected_revision: 1,
                slug: "alpha-stale",
                display_name: "Stale",
                description: "stale",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                archived_at: None,
                updated_at: 51,
            })
            .await
            .unwrap(),
        CompareAndSwap::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: missing_workspace_id,
                expected_revision: 1,
                slug: "missing",
                display_name: "Missing",
                description: "missing",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                archived_at: None,
                updated_at: 52,
            })
            .await
            .unwrap(),
        CompareAndSwap::NotFound
    );

    let active = session.list_org_workspaces(false).await.unwrap();
    assert_eq!(
        active
            .iter()
            .map(|workspace| (workspace.slug.as_str(), workspace.id))
            .collect::<Vec<_>>(),
        vec![("beta", beta_id), ("scale", scale_workspace_id)]
    );
    let all = session.list_org_workspaces(true).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|workspace| (workspace.slug.as_str(), workspace.id))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", alpha_id),
            ("beta", beta_id),
            ("scale", scale_workspace_id)
        ]
    );

    run_workflow_audit_contracts(storage).await;
}

async fn run_workflow_audit_contracts(storage: Arc<dyn StorageBackend>) {
    let source_workspace = workspace_id("70000000-0000-0000-0000-000000000002");
    let target_workspace = workspace_id("70000000-0000-0000-0000-000000000001");
    let document = document_id("71000000-0000-0000-0000-000000000001");
    let parent = work_item_id("72000000-0000-0000-0000-000000000001");
    let child = work_item_id("72000000-0000-0000-0000-000000000002");
    let linked_note = "73000000-0000-0000-0000-000000000001";
    let policy = WorkspacePolicy::engineering_default();
    let session = storage.session().await.unwrap();

    for (id, slug) in [
        (source_workspace, "workflow-source"),
        (target_workspace, "workflow-target"),
    ] {
        session
            .insert_org_workspace(NewOrgWorkspace {
                id,
                slug,
                display_name: slug,
                description: "workflow/audit shared contract",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                now: 1_000,
            })
            .await
            .unwrap();
    }
    session
        .insert_org_document(NewOrgDocument {
            id: document,
            workspace_id: source_workspace,
            path: "workflow.org",
            source: "* TODO Parent\n** TODO Child",
            content_hash: "workflow-one",
            now: 1_001,
        })
        .await
        .unwrap();

    let parent_projection = projected_item(
        parent,
        source_workspace,
        document,
        None,
        0,
        "Workflow parent",
    );
    let mut child_projection = projected_item(
        child,
        source_workspace,
        document,
        Some(parent),
        1,
        "Workflow child",
    );
    child_projection.note_links.push(NoteLink {
        purpose: "evidence".into(),
        note_id: linked_note.parse().unwrap(),
        description: "Linked evidence".into(),
    });
    let source_projection = vec![parent_projection.clone(), child_projection.clone()];
    let seed_projection = storage.begin(TransactionMode::Immediate).await.unwrap();
    seed_projection
        .replace_org_document_projection(document, &source_projection)
        .await
        .unwrap();
    seed_projection.commit().await.unwrap();

    assert_eq!(
        session.get_org_work_item(parent).await.unwrap(),
        Some(parent_projection.clone())
    );
    assert_eq!(
        session
            .list_org_workspace_projection(source_workspace)
            .await
            .unwrap(),
        source_projection
    );
    assert_eq!(
        session.list_org_work_item_children(parent).await.unwrap(),
        vec![child_projection.clone()]
    );
    assert_eq!(
        session
            .list_org_work_items_linking_note(linked_note)
            .await
            .unwrap(),
        vec![child_projection.clone()]
    );

    let ordering_workspace = workspace_id("70000000-0000-0000-0000-000000000010");
    let zeta_document = document_id("71000000-0000-0000-0000-000000000010");
    let alpha_document = document_id("71000000-0000-0000-0000-000000000020");
    let ordering_parent = work_item_id("72000000-0000-0000-0000-000000000010");
    let zeta_child = work_item_id("72000000-0000-0000-0000-000000000011");
    let alpha_child = work_item_id("72000000-0000-0000-0000-000000000012");
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: ordering_workspace,
            slug: "projection-ordering",
            display_name: "Projection ordering",
            description: "path ordering shared contract",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 1_005,
        })
        .await
        .unwrap();
    for (id, path) in [(zeta_document, "zeta.org"), (alpha_document, "alpha.org")] {
        session
            .insert_org_document(NewOrgDocument {
                id,
                workspace_id: ordering_workspace,
                path,
                source: "* TODO Ordering fixture",
                content_hash: path,
                now: 1_006,
            })
            .await
            .unwrap();
    }
    let ordering_parent_projection = projected_item(
        ordering_parent,
        ordering_workspace,
        zeta_document,
        None,
        0,
        "Ordering parent",
    );
    let zeta_child_projection = projected_item(
        zeta_child,
        ordering_workspace,
        zeta_document,
        Some(ordering_parent),
        1,
        "Zeta child",
    );
    let alpha_child_projection = projected_item(
        alpha_child,
        ordering_workspace,
        alpha_document,
        Some(ordering_parent),
        0,
        "Alpha child",
    );
    let seed_ordering = storage.begin(TransactionMode::Immediate).await.unwrap();
    seed_ordering
        .replace_org_document_projection(
            zeta_document,
            &[
                ordering_parent_projection.clone(),
                zeta_child_projection.clone(),
            ],
        )
        .await
        .unwrap();
    seed_ordering
        .replace_org_document_projection(alpha_document, &[alpha_child_projection.clone()])
        .await
        .unwrap();
    seed_ordering.commit().await.unwrap();
    assert_eq!(
        session
            .list_org_workspace_projection(ordering_workspace)
            .await
            .unwrap(),
        vec![
            alpha_child_projection.clone(),
            ordering_parent_projection,
            zeta_child_projection.clone(),
        ],
        "workspace projection must order documents by path before source order"
    );
    assert_eq!(
        session
            .list_org_work_item_children(ordering_parent)
            .await
            .unwrap(),
        vec![alpha_child_projection, zeta_child_projection],
        "children spanning documents must inherit workspace path ordering"
    );

    let note_refs = vec![OrgAttemptNoteReference {
        purpose: "context".into(),
        note_id: linked_note.into(),
        description: "Attempt context snapshot".into(),
    }];
    let artifacts = vec![OrgArtifactReference {
        uri: "artifact://build/one".into(),
        media_type: "application/json".into(),
        name: "build".into(),
        description: "Build result".into(),
    }];
    let attempt_metadata = json!({"lease": {"fence": 1}});
    let first_attempt = session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-workflow-1",
            workspace_id: source_workspace,
            work_item_id: child,
            attempt_number: 1,
            actor_id: "agent-workflow",
            status: OrgAttemptStatus::Running,
            started_at: 1_010,
            note_refs: &note_refs,
            artifacts: &artifacts,
            metadata: &attempt_metadata,
        })
        .await
        .unwrap();
    assert_eq!(
        session.get_org_attempt("attempt-workflow-1").await.unwrap(),
        Some(first_attempt.clone())
    );

    let event_metadata = json!({"percent": 50});
    let attempt_event = session
        .append_org_event(NewOrgEvent {
            id: "event-workflow-source-1",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "agent-workflow",
            attempt_id: Some("attempt-workflow-1"),
            event_type: OrgEventType::Progress,
            occurred_at: 1_040,
            summary: "Halfway",
            metadata: &event_metadata,
            previous_state: Some("ACTIVE"),
            resulting_state: Some("ACTIVE"),
        })
        .await
        .unwrap();
    assert_eq!(
        attempt_event.attempt_id.as_deref(),
        Some("attempt-workflow-1")
    );
    assert_eq!(attempt_event.previous_state.as_deref(), Some("ACTIVE"));
    assert_eq!(attempt_event.resulting_state.as_deref(), Some("ACTIVE"));
    assert_eq!(
        session
            .list_org_subject_events(source_workspace, "work_item", &child.to_string(), None, 200,)
            .await
            .unwrap(),
        vec![attempt_event.clone()]
    );

    let invalid_state_pair = session
        .append_org_event(NewOrgEvent {
            id: "event-invalid-state-pair",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "agent-workflow",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: 1_041,
            summary: "Invalid state pair",
            metadata: &json!({}),
            previous_state: Some("ACTIVE"),
            resulting_state: None,
        })
        .await
        .unwrap_err();
    assert_eq!(invalid_state_pair.kind(), StorageErrorKind::Constraint);
    let unknown_event_type = session
        .append_org_event(NewOrgEvent {
            id: "event-unknown-type",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "agent-workflow",
            attempt_id: None,
            event_type: OrgEventType::Other("future_type".into()),
            occurred_at: 1_042,
            summary: "Unknown client event",
            metadata: &json!({}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap_err();
    assert_eq!(unknown_event_type.kind(), StorageErrorKind::Constraint);
    let reserved_actor = session
        .append_org_event(NewOrgEvent {
            id: "event-reserved-actor",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "system",
            attempt_id: None,
            event_type: OrgEventType::LeaseExpiry,
            occurred_at: 1_043,
            summary: "Client cannot impersonate system",
            metadata: &json!({}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap_err();
    assert_eq!(reserved_actor.kind(), StorageErrorKind::Operation);
    let internal_event = session
        .append_internal_org_event(NewOrgEvent {
            id: "event-internal-expiry",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "system",
            attempt_id: Some("attempt-workflow-1"),
            event_type: OrgEventType::LeaseExpiry,
            occurred_at: 1_044,
            summary: "Expired lease",
            metadata: &json!({"recovery": true}),
            previous_state: Some("ACTIVE"),
            resulting_state: Some("TODO"),
        })
        .await
        .unwrap();
    assert_eq!(internal_event.sequence, attempt_event.sequence + 1);

    let closed_metadata = json!({"lease": {"fence": 1}, "closed": true});
    let ConditionalUpdate::Applied(closed_attempt) = session
        .update_org_attempt(OrgAttemptUpdate {
            id: "attempt-workflow-1",
            expected_status: OrgAttemptStatus::Running,
            status: OrgAttemptStatus::Expired,
            ended_at: 1_045,
            error: Some("lease expired"),
            result_summary: None,
            review_outcome: None,
            note_refs: &note_refs,
            artifacts: &artifacts,
            metadata: &closed_metadata,
        })
        .await
        .unwrap()
    else {
        panic!("running attempt should close once");
    };
    assert_eq!(closed_attempt.status, OrgAttemptStatus::Expired);
    assert_eq!(closed_attempt.ended_at, Some(1_045));
    assert_eq!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: "attempt-workflow-1",
                expected_status: OrgAttemptStatus::Running,
                status: OrgAttemptStatus::Failed,
                ended_at: 1_046,
                error: Some("must not overwrite"),
                result_summary: None,
                review_outcome: None,
                note_refs: &[],
                artifacts: &[],
                metadata: &json!({}),
            })
            .await
            .unwrap(),
        ConditionalUpdate::Conflict
    );

    let operation = StoredOrgOperation {
        workspace_id: source_workspace,
        operation_id: "workflow-preserved".into(),
        request_fingerprint: "sha256:workflow-preserved".into(),
        result: json!({"preserved": true}),
        created_at: 1_050,
    };
    session.insert_org_operation(&operation).await.unwrap();

    for (document_revision, source_revision, target_revision) in [(2, 1, 1), (1, 2, 1), (1, 1, 2)] {
        let stale_move = session
            .compare_and_swap_org_document_ownership(OrgDocumentOwnershipMove {
                document_id: document,
                source_workspace_id: source_workspace,
                target_workspace_id: target_workspace,
                expected_document_revision: document_revision,
                expected_source_workspace_revision: source_revision,
                expected_target_workspace_revision: target_revision,
                updated_at: 1_060,
            })
            .await
            .unwrap();
        assert!(matches!(
            stale_move,
            OrgDocumentOwnershipMoveResult::Conflict { .. }
        ));
        let unchanged_document = session.get_org_document(document).await.unwrap().unwrap();
        assert_eq!(unchanged_document.workspace_id, source_workspace);
        assert_eq!(unchanged_document.revision, 1);
        assert_eq!(
            session
                .get_org_workspace(source_workspace)
                .await
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            session
                .get_org_workspace(target_workspace)
                .await
                .unwrap()
                .unwrap()
                .revision,
            1
        );
    }
    assert_eq!(
        session
            .compare_and_swap_org_document_ownership(OrgDocumentOwnershipMove {
                document_id: document_id("71000000-0000-0000-0000-000000000099"),
                source_workspace_id: source_workspace,
                target_workspace_id: target_workspace,
                expected_document_revision: 1,
                expected_source_workspace_revision: 1,
                expected_target_workspace_revision: 1,
                updated_at: 1_060,
            })
            .await
            .unwrap(),
        OrgDocumentOwnershipMoveResult::NotFound
    );

    let OrgDocumentOwnershipMoveResult::Applied(moved) = session
        .compare_and_swap_org_document_ownership(OrgDocumentOwnershipMove {
            document_id: document,
            source_workspace_id: source_workspace,
            target_workspace_id: target_workspace,
            expected_document_revision: 1,
            expected_source_workspace_revision: 1,
            expected_target_workspace_revision: 1,
            updated_at: 1_061,
        })
        .await
        .unwrap()
    else {
        panic!("document ownership move should apply");
    };
    assert_eq!(moved.document.workspace_id, target_workspace);
    assert_eq!(moved.document.revision, 2);
    assert_eq!(moved.source_workspace_revision, 2);
    assert_eq!(moved.target_workspace_revision, 2);
    assert_eq!(
        session
            .list_org_workspace_projection(source_workspace)
            .await
            .unwrap(),
        source_projection,
        "ownership CAS must not rewrite projection workspace ownership before caller rebuild"
    );
    assert!(
        session
            .list_org_workspace_projection(target_workspace)
            .await
            .unwrap()
            .is_empty(),
        "target projection must remain empty until caller rebuild"
    );

    let target_projection = source_projection
        .iter()
        .cloned()
        .map(|mut item| {
            item.workspace_id = target_workspace;
            item
        })
        .collect::<Vec<_>>();
    let rebuild_after_move = storage.begin(TransactionMode::Immediate).await.unwrap();
    rebuild_after_move
        .rebuild_org_workspace_projection(source_workspace, &[])
        .await
        .unwrap();
    rebuild_after_move
        .rebuild_org_workspace_projection(target_workspace, &target_projection)
        .await
        .unwrap();
    rebuild_after_move.commit().await.unwrap();

    let second_attempt = session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-workflow-2",
            workspace_id: target_workspace,
            work_item_id: child,
            attempt_number: 2,
            actor_id: "agent-target",
            status: OrgAttemptStatus::Running,
            started_at: 1_070,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({"lease": {"fence": 2}}),
        })
        .await
        .unwrap();
    let duplicate_global_number = session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-workflow-duplicate-number",
            workspace_id: target_workspace,
            work_item_id: child,
            attempt_number: 1,
            actor_id: "agent-target",
            status: OrgAttemptStatus::Running,
            started_at: 1_071,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate_global_number.kind(), StorageErrorKind::Constraint);
    assert_eq!(
        session.list_org_attempts(child).await.unwrap(),
        vec![closed_attempt.clone(), second_attempt]
    );
    assert_eq!(closed_attempt.workspace_id, source_workspace);

    let target_event = session
        .append_org_event(NewOrgEvent {
            id: "event-workflow-target-1",
            workspace_id: target_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "agent-target",
            attempt_id: Some("attempt-workflow-2"),
            event_type: OrgEventType::Start,
            occurred_at: 900,
            summary: "Started after move",
            metadata: &json!({"lineage_previous_event_id": internal_event.id}),
            previous_state: Some("TODO"),
            resulting_state: Some("ACTIVE"),
        })
        .await
        .unwrap();
    assert_eq!(target_event.sequence, 1);
    let return_event = session
        .append_org_event(NewOrgEvent {
            id: "event-workflow-source-return",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &child.to_string(),
            actor_id: "agent-workflow",
            attempt_id: None,
            event_type: OrgEventType::DocumentMove,
            occurred_at: 800,
            summary: "Returned after target segment",
            metadata: &json!({"lineage_previous_event_id": target_event.id}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    let global_history = session
        .list_org_global_subject_events("work_item", &child.to_string())
        .await
        .unwrap();
    assert_eq!(
        global_history
            .iter()
            .map(|event| (event.workspace_id, event.sequence))
            .collect::<Vec<_>>(),
        vec![
            (source_workspace, attempt_event.sequence),
            (source_workspace, internal_event.sequence),
            (target_workspace, target_event.sequence),
            (source_workspace, return_event.sequence),
        ],
        "global history must support repeated workspace segments without sorting by time or UUID"
    );

    for (id, workspace_id) in [
        ("event-lineage-missing-a", source_workspace),
        ("event-lineage-missing-b", target_workspace),
    ] {
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id,
                subject_kind: "work_item",
                subject_id: "lineage-missing",
                actor_id: "lineage-test",
                attempt_id: None,
                event_type: OrgEventType::DocumentMove,
                occurred_at: 700,
                summary: "Missing lineage",
                metadata: &json!({}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        session
            .list_org_global_subject_events("work_item", "lineage-missing")
            .await
            .unwrap_err()
            .kind(),
        StorageErrorKind::Corrupt
    );

    for (id, metadata) in [
        ("event-lineage-early", json!({})),
        ("event-lineage-boundary", json!({})),
    ] {
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id: source_workspace,
                subject_kind: "work_item",
                subject_id: "lineage-non-boundary",
                actor_id: "lineage-test",
                attempt_id: None,
                event_type: OrgEventType::DocumentMove,
                occurred_at: 600,
                summary: "Source lineage segment",
                metadata: &metadata,
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    session
        .append_org_event(NewOrgEvent {
            id: "event-lineage-target",
            workspace_id: target_workspace,
            subject_kind: "work_item",
            subject_id: "lineage-non-boundary",
            actor_id: "lineage-test",
            attempt_id: None,
            event_type: OrgEventType::DocumentMove,
            occurred_at: 500,
            summary: "Links before source boundary",
            metadata: &json!({"lineage_previous_event_id": "event-lineage-early"}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .list_org_global_subject_events("work_item", "lineage-non-boundary")
            .await
            .unwrap_err()
            .kind(),
        StorageErrorKind::Corrupt
    );

    for (id, workspace_id, previous_id) in [
        (
            "event-lineage-cycle-a",
            source_workspace,
            "event-lineage-cycle-b",
        ),
        (
            "event-lineage-cycle-b",
            target_workspace,
            "event-lineage-cycle-a",
        ),
    ] {
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id,
                subject_kind: "work_item",
                subject_id: "lineage-cycle",
                actor_id: "lineage-test",
                attempt_id: None,
                event_type: OrgEventType::DocumentMove,
                occurred_at: 400,
                summary: "Cyclic lineage",
                metadata: &json!({"lineage_previous_event_id": previous_id}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        session
            .list_org_global_subject_events("work_item", "lineage-cycle")
            .await
            .unwrap_err()
            .kind(),
        StorageErrorKind::Corrupt
    );
    assert_eq!(
        session
            .get_org_operation(source_workspace, "workflow-preserved")
            .await
            .unwrap(),
        Some(operation)
    );
    assert_eq!(
        session
            .list_org_work_items_linking_note(linked_note)
            .await
            .unwrap(),
        vec![target_projection[1].clone()]
    );

    run_lease_and_attempt_contracts(storage.clone()).await;
    run_operational_ready_contract(storage).await;
}

async fn run_operational_ready_contract(storage: Arc<dyn StorageBackend>) {
    let workspace = workspace_id("1a000000-0000-0000-0000-000000000001");
    let document = document_id("1b000000-0000-0000-0000-000000000001");
    let ready_id = work_item_id("1c000000-0000-0000-0000-000000000001");
    let assigned_id = work_item_id("1c000000-0000-0000-0000-000000000002");
    let running_id = work_item_id("1c000000-0000-0000-0000-000000000003");
    let blocked_id = work_item_id("1c000000-0000-0000-0000-000000000004");
    let review_id = work_item_id("1c000000-0000-0000-0000-000000000005");
    let scheduled_due_id = work_item_id("1c000000-0000-0000-0000-000000000006");
    let scheduled_future_id = work_item_id("1c000000-0000-0000-0000-000000000007");
    let deadline_id = work_item_id("1c000000-0000-0000-0000-000000000008");
    let failed_id = work_item_id("1c000000-0000-0000-0000-000000000009");
    let recovery_id = work_item_id("1c000000-0000-0000-0000-00000000000a");
    let completed_id = work_item_id("1c000000-0000-0000-0000-00000000000b");
    let cancelled_id = work_item_id("1c000000-0000-0000-0000-00000000000c");
    let dependency_done_id = work_item_id("1c000000-0000-0000-0000-00000000000d");
    let dependent_ready_id = work_item_id("1c000000-0000-0000-0000-00000000000e");
    let dependent_blocked_id = work_item_id("1c000000-0000-0000-0000-00000000000f");
    let tagged_id = work_item_id("1c000000-0000-0000-0000-000000000010");
    let review_expired_id = work_item_id("1c000000-0000-0000-0000-000000000011");
    let review_active_id = work_item_id("1c000000-0000-0000-0000-000000000012");
    let tie_id = work_item_id("1c000000-0000-0000-0000-000000000013");
    let tie_later_id = work_item_id("1c000000-0000-0000-0000-000000000014");
    let stateless_assigned_id = work_item_id("1c000000-0000-0000-0000-000000000015");
    let stateless_dependency_id = work_item_id("1c000000-0000-0000-0000-000000000016");
    let dependent_on_stateless_id = work_item_id("1c000000-0000-0000-0000-000000000017");
    let dispatch_workspace = workspace_id("1d000000-0000-0000-0000-000000000001");
    let dispatch_document = document_id("1e000000-0000-0000-0000-000000000001");
    let dispatch_assigned_id = work_item_id("1f000000-0000-0000-0000-000000000001");
    let dispatch_unassigned_id = work_item_id("1f000000-0000-0000-0000-000000000002");
    let moved_completed_id = work_item_id("1f000000-0000-0000-0000-000000000003");
    let archived_workspace = workspace_id("20000000-0000-0000-0000-000000000001");
    let archived_document = document_id("21000000-0000-0000-0000-000000000001");
    let archived_assigned_id = work_item_id("22000000-0000-0000-0000-000000000001");
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allow_cross_workspace_agenda = true;
    let session = storage.session().await.unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace,
            slug: "operational-ready",
            display_name: "Operational ready",
            description: "first operational query tracer bullet",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 1_000,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: document,
            workspace_id: workspace,
            path: "operational-ready.org",
            source: "* READY Ready item",
            content_hash: "operational-ready-hash",
            now: 1_000,
        })
        .await
        .unwrap();
    let mut ready = projected_item(ready_id, workspace, document, None, 0, "Ready item");
    ready.state = Some("READY".into());
    ready.assignee = None;
    ready.priority = None;
    ready.created_at = 1_000;
    let mut assigned = projected_item(
        assigned_id,
        workspace,
        document,
        None,
        1,
        "Assigned backlog",
    );
    assigned.state = Some("BACKLOG".into());
    assigned.assignee = Some("agent-assigned".into());
    assigned.created_at = 1_001;
    let make_item = |id, order, title: &str, state: &str| {
        let mut item = projected_item(id, workspace, document, None, order, title);
        item.state = Some(state.into());
        item.assignee = None;
        item.created_at = 1_000 + order;
        item
    };
    let running = make_item(running_id, 2, "Running active", "RUNNING");
    let blocked = make_item(blocked_id, 3, "Blocked", "BLOCKED");
    let review = make_item(review_id, 4, "Review unleased", "REVIEW");
    let mut scheduled_due = make_item(scheduled_due_id, 5, "Scheduled due", "BACKLOG");
    scheduled_due.scheduled = Some(scheduled("<due>", "due", 1_000));
    let mut scheduled_future = make_item(scheduled_future_id, 6, "Scheduled future", "BACKLOG");
    scheduled_future.scheduled = Some(scheduled("<future>", "future", 1_100));
    let mut deadline = make_item(deadline_id, 7, "Deadline boundary", "BACKLOG");
    deadline.deadline = Some(scheduled("<deadline>", "deadline", 1_000));
    let failed = make_item(failed_id, 8, "Failed exhausted", "FAILED");
    let mut recovery = make_item(recovery_id, 9, "Running recovery", "RUNNING");
    recovery.priority = None;
    let completed = make_item(completed_id, 10, "Completed", "DONE");
    let mut cancelled = make_item(cancelled_id, 11, "Cancelled", "CANCELLED");
    cancelled.scheduled = Some(scheduled("<cancelled>", "cancelled", 1_000));
    cancelled.deadline = Some(scheduled("<cancelled-deadline>", "cancelled", 1_100));
    let dependency_done = make_item(dependency_done_id, 12, "Dependency done", "DONE");
    let mut dependent_ready = make_item(dependent_ready_id, 13, "Dependency ready", "READY");
    dependent_ready.priority = None;
    dependent_ready.dependencies = vec![dependency_done_id];
    let mut dependent_blocked =
        make_item(dependent_blocked_id, 14, "Dependency incomplete", "READY");
    dependent_blocked.dependencies = vec![blocked_id];
    let mut tagged = make_item(tagged_id, 15, "Tagged incident", "READY");
    tagged.item_type = WorkItemType::Incident;
    tagged.priority = Some('C');
    tagged.tags = vec!["Backend".into(), "ops".into()];
    let review_expired = make_item(review_expired_id, 16, "Review expired", "REVIEW");
    let review_active = make_item(review_active_id, 17, "Review active", "REVIEW");
    let mut tie = make_item(tie_id, 18, "Stable ordering tie", "READY");
    tie.priority = Some('B');
    tie.created_at = 1_050;
    tie.tags = vec!["ordering-tie".into()];
    tie.note_links = vec![NoteLink {
        purpose: "context".into(),
        note_id: "23000000-0000-0000-0000-000000000001".parse().unwrap(),
        description: "single-statement relation fixture".into(),
    }];
    let mut tie_later = make_item(tie_later_id, 19, "Stable item ordering tie", "READY");
    tie_later.priority = Some('B');
    tie_later.created_at = 1_050;
    tie_later.tags = vec!["ordering-tie".into()];
    let mut stateless_assigned =
        make_item(stateless_assigned_id, 20, "Stateless assigned", "READY");
    stateless_assigned.state = None;
    stateless_assigned.assignee = Some("stateless-agent".into());
    let mut stateless_dependency =
        make_item(stateless_dependency_id, 21, "Stateless dependency", "READY");
    stateless_dependency.state = None;
    let mut dependent_on_stateless = make_item(
        dependent_on_stateless_id,
        22,
        "Blocked by stateless dependency",
        "READY",
    );
    dependent_on_stateless.dependencies = vec![stateless_dependency_id];
    session
        .replace_org_document_projection(
            document,
            &[
                ready,
                assigned,
                running,
                blocked,
                review,
                scheduled_due,
                scheduled_future,
                deadline,
                failed,
                recovery,
                completed,
                cancelled,
                dependency_done,
                dependent_ready,
                dependent_blocked,
                tagged,
                review_expired,
                review_active,
                tie,
                tie_later,
                stateless_assigned,
                stateless_dependency,
                dependent_on_stateless,
            ],
        )
        .await
        .unwrap();

    let mut dispatch_policy = policy.clone();
    dispatch_policy.claim_policy = ClaimPolicy::ExplicitlyDispatched;
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: dispatch_workspace,
            slug: "operational-dispatch",
            display_name: "Operational dispatch",
            description: "explicit dispatch readiness",
            timezone: "America/New_York",
            policy_schema_version: 1,
            policy: &dispatch_policy,
            now: 1_000,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: dispatch_document,
            workspace_id: dispatch_workspace,
            path: "operational-dispatch.org",
            source: "* READY Dispatch items",
            content_hash: "operational-dispatch-hash",
            now: 1_000,
        })
        .await
        .unwrap();
    let mut dispatch_assigned = projected_item(
        dispatch_assigned_id,
        dispatch_workspace,
        dispatch_document,
        None,
        0,
        "Assigned dispatch",
    );
    dispatch_assigned.state = Some("READY".into());
    dispatch_assigned.assignee = Some("dispatch-agent".into());
    dispatch_assigned.priority = Some('B');
    dispatch_assigned.created_at = 1_050;
    dispatch_assigned.tags = vec!["ordering-tie".into()];
    let mut dispatch_unassigned = projected_item(
        dispatch_unassigned_id,
        dispatch_workspace,
        dispatch_document,
        None,
        1,
        "Unassigned dispatch",
    );
    dispatch_unassigned.state = Some("READY".into());
    dispatch_unassigned.assignee = None;
    let mut moved_completed = projected_item(
        moved_completed_id,
        dispatch_workspace,
        dispatch_document,
        None,
        2,
        "Moved completed",
    );
    moved_completed.state = Some("DONE".into());
    session
        .replace_org_document_projection(
            dispatch_document,
            &[dispatch_assigned, dispatch_unassigned, moved_completed],
        )
        .await
        .unwrap();

    let archived_policy = WorkspacePolicy::engineering_default();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: archived_workspace,
            slug: "operational-archived",
            display_name: "Operational archived",
            description: "archive filtering",
            timezone: "Asia/Shanghai",
            policy_schema_version: 1,
            policy: &archived_policy,
            now: 1_000,
        })
        .await
        .unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: archived_document,
            workspace_id: archived_workspace,
            path: "operational-archived.org",
            source: "* BACKLOG Archived assigned",
            content_hash: "operational-archived-hash",
            now: 1_000,
        })
        .await
        .unwrap();
    let mut archived_assigned = projected_item(
        archived_assigned_id,
        archived_workspace,
        archived_document,
        None,
        0,
        "Archived assigned",
    );
    archived_assigned.state = Some("BACKLOG".into());
    archived_assigned.assignee = Some("archived-agent".into());
    session
        .replace_org_document_projection(archived_document, &[archived_assigned])
        .await
        .unwrap();
    assert!(matches!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: archived_workspace,
                expected_revision: 1,
                slug: "operational-archived",
                display_name: "Operational archived",
                description: "archive filtering",
                timezone: "Asia/Shanghai",
                policy_schema_version: 1,
                policy: &archived_policy,
                archived_at: Some(1_000),
                updated_at: 1_001,
            })
            .await
            .unwrap(),
        CompareAndSwap::Applied(_)
    ));

    let seed = storage.begin(TransactionMode::Immediate).await.unwrap();
    for (id, item_id, actor, status, number) in [
        (
            "operational-running-attempt",
            running_id,
            "runner",
            OrgAttemptStatus::Running,
            1,
        ),
        (
            "operational-review-attempt",
            review_id,
            "author",
            OrgAttemptStatus::Submitted,
            1,
        ),
        (
            "operational-recovery-attempt",
            recovery_id,
            "recover",
            OrgAttemptStatus::Running,
            1,
        ),
        (
            "operational-failed-attempt-1",
            failed_id,
            "failed",
            OrgAttemptStatus::Failed,
            1,
        ),
        (
            "operational-failed-attempt-2",
            failed_id,
            "failed",
            OrgAttemptStatus::Failed,
            2,
        ),
        (
            "operational-failed-attempt-3",
            failed_id,
            "failed",
            OrgAttemptStatus::Failed,
            3,
        ),
        (
            "operational-review-expired-attempt",
            review_expired_id,
            "author",
            OrgAttemptStatus::Submitted,
            1,
        ),
        (
            "operational-review-active-attempt",
            review_active_id,
            "author",
            OrgAttemptStatus::Submitted,
            1,
        ),
    ] {
        seed.insert_org_attempt(NewOrgAttempt {
            id,
            workspace_id: workspace,
            work_item_id: item_id,
            attempt_number: number,
            actor_id: actor,
            status,
            started_at: 900 + number,
            note_refs: &[],
            artifacts: &[],
            metadata: &json!({}),
        })
        .await
        .unwrap();
    }
    for (id, item_id, attempt_id, kind, actor, hash, expires_at, insert_now) in [
        (
            "operational-running-lease",
            running_id,
            "operational-running-attempt",
            OrgLeaseKind::Execution,
            "runner",
            "1010101010101010101010101010101010101010101010101010101010101010",
            1_100,
            1_000,
        ),
        (
            "operational-recovery-lease",
            recovery_id,
            "operational-recovery-attempt",
            OrgLeaseKind::Execution,
            "recover",
            "2020202020202020202020202020202020202020202020202020202020202020",
            1_000,
            900,
        ),
        (
            "operational-review-expired-lease",
            review_expired_id,
            "operational-review-expired-attempt",
            OrgLeaseKind::Review,
            "reviewer-expired",
            "3030303030303030303030303030303030303030303030303030303030303030",
            1_000,
            900,
        ),
        (
            "operational-review-active-lease",
            review_active_id,
            "operational-review-active-attempt",
            OrgLeaseKind::Review,
            "reviewer-active",
            "4040404040404040404040404040404040404040404040404040404040404040",
            1_100,
            1_000,
        ),
    ] {
        assert!(matches!(
            seed.insert_org_lease_if_capacity(
                NewOrgLease {
                    id,
                    workspace_id: workspace,
                    work_item_id: item_id,
                    attempt_id,
                    kind,
                    actor_id: actor,
                    fencing_token_hash: hash,
                    acquired_at: insert_now,
                    last_heartbeat_at: insert_now,
                    expires_at,
                },
                4,
                insert_now,
            )
            .await
            .unwrap(),
            ConditionalUpdate::Applied(_)
        ));
    }
    seed.commit().await.unwrap();
    for (id, event_workspace, item_id, occurred_at) in [
        (
            "operational-completion-event",
            workspace,
            completed_id,
            1_000,
        ),
        (
            "operational-dependency-completion-event",
            workspace,
            dependency_done_id,
            900,
        ),
        (
            "operational-moved-completion-event",
            workspace,
            moved_completed_id,
            1_200,
        ),
    ] {
        let subject_id = item_id.to_string();
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id: event_workspace,
                subject_kind: "work_item",
                subject_id: &subject_id,
                actor_id: "operator",
                attempt_id: None,
                event_type: OrgEventType::Completion,
                occurred_at,
                summary: "Completed",
                metadata: &json!({}),
                previous_state: Some("RUNNING"),
                resulting_state: Some("DONE"),
            })
            .await
            .unwrap();
    }
    let moved_subject_id = moved_completed_id.to_string();
    session
        .append_org_event(NewOrgEvent {
            id: "operational-moved-target-event",
            workspace_id: dispatch_workspace,
            subject_kind: "work_item",
            subject_id: &moved_subject_id,
            actor_id: "operator",
            attempt_id: None,
            event_type: OrgEventType::DocumentMove,
            occurred_at: 1_201,
            summary: "Moved after completion",
            metadata: &json!({
                "lineage_previous_event_id": "operational-moved-completion-event"
            }),
            previous_state: Some("DONE"),
            resulting_state: Some("DONE"),
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .list_org_global_subject_events("work_item", &moved_subject_id)
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        vec![
            "operational-moved-completion-event",
            "operational-moved-target-event"
        ]
    );

    let rows = session
        .query_org_operational(operational_query(
            OrgOperationalView::Ready,
            &[workspace],
            1_000,
        ))
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.item.id).collect::<Vec<_>>(),
        vec![
            tie_id,
            tie_later_id,
            tagged_id,
            ready_id,
            recovery_id,
            dependent_ready_id,
        ]
    );
    assert!(rows.iter().any(|row| row.item.id == ready_id));
    assert!(rows.iter().any(|row| {
        row.item.id == recovery_id
            && row.ready_marker == Some(OrgReadyMarker::RecoveryCandidate)
            && row
                .lease
                .as_ref()
                .is_some_and(|lease| lease.expires_at == 1_000)
    }));
    assert!(!rows.iter().any(|row| row.item.id == dependent_blocked_id));
    assert!(!rows
        .iter()
        .any(|row| row.item.id == dependent_on_stateless_id));
    assert_eq!(
        rows.iter()
            .find(|row| row.item.id == dependent_ready_id)
            .unwrap()
            .item
            .dependencies,
        vec![dependency_done_id]
    );

    let workspace_ids = [workspace];
    let assigned_rows = session
        .query_org_operational({
            let mut query = operational_query(OrgOperationalView::Assigned, &workspace_ids, 1_000);
            query.assignee = Some("agent-assigned");
            query
        })
        .await
        .unwrap();
    assert_eq!(
        assigned_rows
            .iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![assigned_id]
    );
    assert_eq!(
        session
            .query_org_operational(operational_query(
                OrgOperationalView::Assigned,
                &workspace_ids,
                1_000,
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![assigned_id, stateless_assigned_id]
    );

    let tagged_filters = [" Backend ", "ops", "Backend"];
    let tagged_rows = session
        .query_org_operational({
            let mut query = operational_query(OrgOperationalView::Ready, &workspace_ids, 1_000);
            query.item_type = Some(WorkItemType::Incident);
            query.state = Some("READY");
            query.priority = Some('C');
            query.tags = &tagged_filters;
            query
        })
        .await
        .unwrap();
    assert_eq!(
        tagged_rows
            .iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![tagged_id]
    );
    let lowercase_tag = ["backend"];
    let mut lowercase_tag_query =
        operational_query(OrgOperationalView::Ready, &workspace_ids, 1_000);
    lowercase_tag_query.tags = &lowercase_tag;
    assert!(session
        .query_org_operational(lowercase_tag_query)
        .await
        .unwrap()
        .is_empty());
    let blank_tag = ["  "];
    let mut blank_tag_query = operational_query(OrgOperationalView::Ready, &workspace_ids, 1_000);
    blank_tag_query.tags = &blank_tag;
    assert_eq!(
        session
            .query_org_operational(blank_tag_query)
            .await
            .unwrap_err()
            .kind(),
        StorageErrorKind::Operation
    );

    let dispatch_workspace_ids = [dispatch_workspace];
    assert_eq!(
        session
            .query_org_operational(operational_query(
                OrgOperationalView::Ready,
                &dispatch_workspace_ids,
                1_000,
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![dispatch_assigned_id]
    );

    let tie_tags = ["ordering-tie"];
    let tied_workspaces = [workspace, dispatch_workspace];
    let mut tie_query = operational_query(OrgOperationalView::Ready, &tied_workspaces, 1_000);
    tie_query.tags = &tie_tags;
    let tied_rows = session.query_org_operational(tie_query).await.unwrap();
    assert!(tied_rows
        .iter()
        .all(|row| row.item.tags == vec!["ordering-tie"]));
    assert_eq!(
        tied_rows
            .iter()
            .find(|row| row.item.id == tie_id)
            .unwrap()
            .item
            .note_links,
        vec![NoteLink {
            purpose: "context".into(),
            note_id: "23000000-0000-0000-0000-000000000001".parse().unwrap(),
            description: "single-statement relation fixture".into(),
        }]
    );
    assert_eq!(
        tied_rows.iter().map(|row| row.item.id).collect::<Vec<_>>(),
        vec![tie_id, tie_later_id, dispatch_assigned_id]
    );
    let cursor = tied_rows[0].cursor(OrgOperationalView::Ready);
    let mut resumed_tie_query =
        operational_query(OrgOperationalView::Ready, &tied_workspaces, 1_000);
    resumed_tie_query.tags = &tie_tags;
    resumed_tie_query.after = Some(&cursor);
    assert_eq!(
        session
            .query_org_operational(resumed_tie_query)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![tie_later_id, dispatch_assigned_id]
    );

    let archived_workspace_ids = [archived_workspace];
    assert!(session
        .query_org_operational(operational_query(
            OrgOperationalView::Assigned,
            &archived_workspace_ids,
            1_000,
        ))
        .await
        .unwrap()
        .is_empty());
    let mut archived_query =
        operational_query(OrgOperationalView::Assigned, &archived_workspace_ids, 1_000);
    archived_query.include_archived = true;
    assert_eq!(
        session
            .query_org_operational(archived_query)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![archived_assigned_id]
    );

    for (view, expected) in [
        (OrgOperationalView::Running, vec![running_id, recovery_id]),
        (OrgOperationalView::Blocked, vec![blocked_id]),
        (
            OrgOperationalView::Review,
            vec![review_id, review_expired_id, review_active_id],
        ),
        (
            OrgOperationalView::Scheduled,
            vec![scheduled_due_id, scheduled_future_id],
        ),
        (OrgOperationalView::UpcomingDeadline, vec![deadline_id]),
        (OrgOperationalView::Failed, vec![failed_id]),
        (
            OrgOperationalView::ExpiredLease,
            vec![recovery_id, review_expired_id],
        ),
        (
            OrgOperationalView::Completed,
            vec![completed_id, dependency_done_id],
        ),
    ] {
        let actual = session
            .query_org_operational(operational_query(view, &[workspace], 1_000))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actual,
            expected.into_iter().collect(),
            "unexpected {view:?} rows"
        );
    }
    for (view, expected) in [
        (
            OrgOperationalView::Scheduled,
            vec![scheduled_due_id, scheduled_future_id],
        ),
        (
            OrgOperationalView::ExpiredLease,
            vec![review_expired_id, recovery_id],
        ),
        (
            OrgOperationalView::Completed,
            vec![completed_id, dependency_done_id],
        ),
    ] {
        assert_eq!(
            session
                .query_org_operational(operational_query(view, &workspace_ids, 1_000))
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.item.id)
                .collect::<Vec<_>>(),
            expected,
            "unexpected {view:?} ordering"
        );
    }
    let expired_rows = session
        .query_org_operational(operational_query(
            OrgOperationalView::ExpiredLease,
            &workspace_ids,
            1_000,
        ))
        .await
        .unwrap();
    let expired_cursor = expired_rows[0].cursor(OrgOperationalView::ExpiredLease);
    let mut expired_resume =
        operational_query(OrgOperationalView::ExpiredLease, &workspace_ids, 1_000);
    expired_resume.after = Some(&expired_cursor);
    assert_eq!(
        session
            .query_org_operational(expired_resume)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![recovery_id]
    );

    let mut scheduled_boundary =
        operational_query(OrgOperationalView::Scheduled, &workspace_ids, 1_000);
    scheduled_boundary.scheduled_from = Some(1_000);
    scheduled_boundary.scheduled_to = Some(1_000);
    assert_eq!(
        session
            .query_org_operational(scheduled_boundary)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![scheduled_due_id]
    );
    let mut deadline_boundary =
        operational_query(OrgOperationalView::UpcomingDeadline, &workspace_ids, 1_000);
    deadline_boundary.deadline_from = Some(1_000);
    deadline_boundary.deadline_to = Some(1_000);
    assert_eq!(
        session
            .query_org_operational(deadline_boundary)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![deadline_id]
    );
    let mut completed_boundary =
        operational_query(OrgOperationalView::Completed, &workspace_ids, 1_000);
    completed_boundary.completed_from = Some(1_000);
    completed_boundary.completed_to = Some(1_000);
    assert_eq!(
        session
            .query_org_operational(completed_boundary)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>(),
        vec![completed_id]
    );
    let mut moved_completion_window = operational_query(
        OrgOperationalView::Completed,
        &dispatch_workspace_ids,
        1_000,
    );
    moved_completion_window.completed_from = Some(1_200);
    moved_completion_window.completed_to = Some(1_200);
    let moved_completion_rows = session
        .query_org_operational(moved_completion_window)
        .await
        .unwrap();
    assert_eq!(moved_completion_rows.len(), 1);
    assert_eq!(moved_completion_rows[0].item.id, moved_completed_id);
    assert_eq!(moved_completion_rows[0].completion_at, Some(1_200));
    let review_rows = session
        .query_org_operational(operational_query(
            OrgOperationalView::Review,
            &[workspace],
            1_000,
        ))
        .await
        .unwrap();
    assert!(review_rows.iter().any(|row| {
        row.item.id == review_id && row.review_lease_marker == Some(OrgReviewLeaseMarker::Unleased)
    }));
    assert!(review_rows.iter().any(|row| {
        row.item.id == review_expired_id
            && row.review_lease_marker == Some(OrgReviewLeaseMarker::Expired)
    }));
    assert!(review_rows.iter().any(|row| {
        row.item.id == review_active_id
            && row.review_lease_marker == Some(OrgReviewLeaseMarker::Active)
    }));
    let failed_row = session
        .query_org_operational(operational_query(
            OrgOperationalView::Failed,
            &[workspace],
            1_000,
        ))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(failed_row.attempt_count, 3);
    assert!(failed_row.retry_exhausted);

    let summary = session
        .get_org_workspace_operational_summary(workspace, 1_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.workspace_id, workspace);
    assert_eq!(summary.timezone, "UTC");
    assert_eq!(summary.archived_at, None);
    assert_eq!(summary.workspace_revision, 1);
    assert_eq!(summary.evaluated_at, 1_000);
    assert_eq!(summary.counts.ready, 6);
    assert_eq!(summary.counts.assigned, 2);
    assert_eq!(summary.counts.running, 2);
    assert_eq!(summary.counts.blocked, 1);
    assert_eq!(summary.counts.review, 3);
    assert_eq!(summary.counts.scheduled, 2);
    assert_eq!(summary.counts.upcoming_deadline, 1);
    assert_eq!(summary.counts.failed, 1);
    assert_eq!(summary.counts.expired_lease, 2);
    assert_eq!(summary.counts.completed, 2);
    let archived_summary = session
        .get_org_workspace_operational_summary(archived_workspace, 1_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(archived_summary.timezone, "Asia/Shanghai");
    assert_eq!(archived_summary.archived_at, Some(1_000));
    assert_eq!(archived_summary.workspace_revision, 2);
    assert_eq!(archived_summary.counts.ready, 0);
    assert_eq!(archived_summary.counts.assigned, 1);
    assert!(session
        .get_org_workspace_operational_summary(
            workspace_id("1a000000-0000-0000-0000-000000000099"),
            1_000,
        )
        .await
        .unwrap()
        .is_none());
}

async fn run_lease_and_attempt_contracts(storage: Arc<dyn StorageBackend>) {
    let source_workspace = workspace_id("18000000-0000-0000-0000-000000000001");
    let target_workspace = workspace_id("18000000-0000-0000-0000-000000000002");
    let execution_item = work_item_id("19000000-0000-0000-0000-000000000001");
    let review_item = work_item_id("19000000-0000-0000-0000-000000000002");
    let expired_item = work_item_id("19000000-0000-0000-0000-000000000003");
    let policy = WorkspacePolicy::engineering_default();
    let session = storage.session().await.unwrap();
    for (id, slug) in [
        (source_workspace, "claims-source"),
        (target_workspace, "claims-target"),
    ] {
        session
            .insert_org_workspace(NewOrgWorkspace {
                id,
                slug,
                display_name: slug,
                description: "lease and attempt storage contracts",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &policy,
                now: 100,
            })
            .await
            .unwrap();
    }

    let metadata = json!({"contract": "lease-attempt"});
    let execution = storage.begin(TransactionMode::Immediate).await.unwrap();
    let first_attempt = execution
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "claim-attempt-1",
            workspace_id: source_workspace,
            work_item_id: execution_item,
            actor_id: "agent-execution",
            started_at: 100,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert_eq!(first_attempt.attempt_number, 1);
    let ConditionalUpdate::Applied(inserted) = execution
        .insert_org_lease_if_capacity(
            NewOrgLease {
                id: "claim-lease-1",
                workspace_id: source_workspace,
                work_item_id: execution_item,
                attempt_id: &first_attempt.id,
                kind: OrgLeaseKind::Execution,
                actor_id: "agent-execution",
                fencing_token_hash:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                acquired_at: 100,
                last_heartbeat_at: 100,
                expires_at: 200,
            },
            2,
            100,
        )
        .await
        .unwrap()
    else {
        panic!("first execution lease should fit workspace capacity");
    };
    assert_eq!(inserted.kind, OrgLeaseKind::Execution);
    execution.commit().await.unwrap();

    let internal = session
        .get_open_org_lease_internal(execution_item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        internal.fencing_token_hash,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    let active = session
        .get_active_org_lease(execution_item, 199)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.id, "claim-lease-1");
    assert!(session
        .get_active_org_lease(execution_item, 200)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session
            .count_active_org_leases(source_workspace, 199)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .count_active_org_leases(source_workspace, 200)
            .await
            .unwrap(),
        0
    );

    let proof = |hash: &'static str, now| OrgLeaseProof {
        lease_id: "claim-lease-1",
        workspace_id: source_workspace,
        work_item_id: execution_item,
        fencing_token_hash: hash,
        kind: OrgLeaseKind::Execution,
        actor_id: "agent-execution",
        now,
    };
    for invalid_proof in [
        proof(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            150,
        ),
        OrgLeaseProof {
            actor_id: "wrong-actor",
            ..proof(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                150,
            )
        },
        OrgLeaseProof {
            kind: OrgLeaseKind::Review,
            ..proof(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                150,
            )
        },
        OrgLeaseProof {
            workspace_id: target_workspace,
            ..proof(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                150,
            )
        },
        OrgLeaseProof {
            work_item_id: expired_item,
            ..proof(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                150,
            )
        },
        proof(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            200,
        ),
    ] {
        assert_eq!(
            session
                .heartbeat_org_lease(OrgLeaseHeartbeat {
                    proof: invalid_proof,
                    last_heartbeat_at: invalid_proof.now,
                    expires_at: invalid_proof.now + 100,
                })
                .await
                .unwrap(),
            ConditionalUpdate::Conflict,
            "every well-formed stale proof must have one generic result"
        );
    }
    let ConditionalUpdate::Applied(heartbeat) = session
        .heartbeat_org_lease(OrgLeaseHeartbeat {
            proof: proof(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                150,
            ),
            last_heartbeat_at: 150,
            expires_at: 250,
        })
        .await
        .unwrap()
    else {
        panic!("current lease proof should heartbeat");
    };
    assert_eq!(heartbeat.last_heartbeat_at, 150);
    assert_eq!(heartbeat.expires_at, 250);
    assert_eq!(
        session
            .heartbeat_org_lease(OrgLeaseHeartbeat {
                proof: proof(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    151,
                ),
                last_heartbeat_at: 151,
                expires_at: 249,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Conflict,
        "heartbeat must never shorten the stored lease expiry"
    );
    assert_eq!(
        session
            .get_active_org_lease(execution_item, 249)
            .await
            .unwrap()
            .unwrap()
            .expires_at,
        250
    );

    let close = || OrgLeaseClosure {
        proof: proof(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            160,
        ),
        ended_at: 160,
        end_reason: OrgLeaseEndReason::Release,
    };
    assert!(matches!(
        session.close_org_lease(close()).await.unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        session.close_org_lease(close()).await.unwrap(),
        ConditionalUpdate::Conflict
    );
    let history = session
        .list_org_lease_history(execution_item)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].end_reason, Some(OrgLeaseEndReason::Release));

    let review_tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    let review_attempt = review_tx
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "review-attempt-1",
            workspace_id: source_workspace,
            work_item_id: review_item,
            actor_id: "agent-author",
            started_at: 300,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    let ConditionalUpdate::Applied(submitted) = review_tx
        .update_org_attempt(OrgAttemptUpdate {
            id: &review_attempt.id,
            expected_status: OrgAttemptStatus::Running,
            status: OrgAttemptStatus::Submitted,
            ended_at: 310,
            error: None,
            result_summary: Some("ready for review"),
            review_outcome: None,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap()
    else {
        panic!("running attempt should become submitted");
    };
    assert_eq!(submitted.status, OrgAttemptStatus::Submitted);
    let ConditionalUpdate::Applied(review_lease) = review_tx
        .insert_org_lease_if_capacity(
            NewOrgLease {
                id: "review-lease-1",
                workspace_id: source_workspace,
                work_item_id: review_item,
                attempt_id: &review_attempt.id,
                kind: OrgLeaseKind::Review,
                actor_id: "agent-reviewer",
                fencing_token_hash:
                    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                acquired_at: 320,
                last_heartbeat_at: 320,
                expires_at: 420,
            },
            2,
            320,
        )
        .await
        .unwrap()
    else {
        panic!("submitted attempt should accept a review lease");
    };
    assert_eq!(review_lease.kind, OrgLeaseKind::Review);
    review_tx.commit().await.unwrap();

    session
        .rebuild_org_workspace_projection(source_workspace, &[])
        .await
        .unwrap();
    assert_eq!(
        session
            .get_active_org_lease(review_item, 350)
            .await
            .unwrap()
            .unwrap()
            .id,
        "review-lease-1"
    );

    let move_tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    let ConditionalUpdate::Applied(moved) = move_tx
        .move_org_lease_ownership(OrgLeaseOwnershipMove {
            proof: OrgLeaseProof {
                lease_id: "review-lease-1",
                workspace_id: source_workspace,
                work_item_id: review_item,
                fencing_token_hash:
                    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                kind: OrgLeaseKind::Review,
                actor_id: "agent-reviewer",
                now: 350,
            },
            target_workspace_id: target_workspace,
            target_capacity: 1,
        })
        .await
        .unwrap()
    else {
        panic!("active review lease should move into available capacity");
    };
    assert_eq!(moved.workspace_id, target_workspace);
    move_tx.commit().await.unwrap();
    assert_eq!(
        session
            .get_org_attempt(&review_attempt.id)
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        source_workspace,
        "attempt workspace is the workspace at execution start"
    );

    let moved_proof = OrgLeaseProof {
        lease_id: "review-lease-1",
        workspace_id: target_workspace,
        work_item_id: review_item,
        fencing_token_hash: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        kind: OrgLeaseKind::Review,
        actor_id: "agent-reviewer",
        now: 360,
    };
    assert!(matches!(
        session
            .close_org_lease(OrgLeaseClosure {
                proof: moved_proof,
                ended_at: 360,
                end_reason: OrgLeaseEndReason::Approval,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert!(matches!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: &review_attempt.id,
                expected_status: OrgAttemptStatus::Submitted,
                status: OrgAttemptStatus::Completed,
                ended_at: 360,
                error: None,
                result_summary: Some("approved"),
                review_outcome: Some("approved"),
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: &review_attempt.id,
                expected_status: OrgAttemptStatus::Submitted,
                status: OrgAttemptStatus::Failed,
                ended_at: 361,
                error: Some("must not close twice"),
                result_summary: None,
                review_outcome: Some("rejected"),
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Conflict
    );

    assert_submitted_terminal(
        session.as_ref(),
        source_workspace,
        work_item_id("19000000-0000-0000-0000-000000000004"),
        "failed-lifecycle-attempt",
        OrgAttemptStatus::Failed,
    )
    .await;
    assert_submitted_terminal(
        session.as_ref(),
        source_workspace,
        work_item_id("19000000-0000-0000-0000-000000000005"),
        "cancelled-lifecycle-attempt",
        OrgAttemptStatus::Cancelled,
    )
    .await;

    let expiry_tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    let expired_attempt = expiry_tx
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "expired-attempt-1",
            workspace_id: source_workspace,
            work_item_id: expired_item,
            actor_id: "agent-expired",
            started_at: 400,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert!(matches!(
        expiry_tx
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "expired-lease-1",
                    workspace_id: source_workspace,
                    work_item_id: expired_item,
                    attempt_id: &expired_attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-expired",
                    fencing_token_hash:
                        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                    acquired_at: 400,
                    last_heartbeat_at: 400,
                    expires_at: 450,
                },
                1,
                400,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    expiry_tx.commit().await.unwrap();
    let expiry_event = session
        .append_internal_org_event(NewOrgEvent {
            id: "event-expired-lease-contract",
            workspace_id: source_workspace,
            subject_kind: "work_item",
            subject_id: &expired_item.to_string(),
            actor_id: "system",
            attempt_id: Some(&expired_attempt.id),
            event_type: OrgEventType::LeaseExpiry,
            occurred_at: 450,
            summary: "Lease expired",
            metadata: &json!({}),
            previous_state: Some("ACTIVE"),
            resulting_state: Some("TODO"),
        })
        .await
        .unwrap();
    let blocked_retry = storage.begin(TransactionMode::Immediate).await.unwrap();
    let retry_attempt = blocked_retry
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "expired-attempt-blocked-retry",
            workspace_id: source_workspace,
            work_item_id: expired_item,
            actor_id: "agent-retry",
            started_at: 450,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert_eq!(retry_attempt.attempt_number, 2);
    assert_eq!(
        blocked_retry
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "expired-lease-blocked-retry",
                    workspace_id: source_workspace,
                    work_item_id: expired_item,
                    attempt_id: &retry_attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-retry",
                    fencing_token_hash:
                        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    acquired_at: 450,
                    last_heartbeat_at: 450,
                    expires_at: 550,
                },
                1,
                450,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Conflict,
        "expired-but-unclosed lease still owns the unique open slot"
    );
    blocked_retry.rollback().await.unwrap();
    let ConditionalUpdate::Applied(expired) = session
        .close_expired_org_lease(ExpiredOrgLeaseClosure {
            lease_id: "expired-lease-1",
            work_item_id: expired_item,
            now: 450,
            ended_at: 450,
            expiry_event_id: &expiry_event.id,
        })
        .await
        .unwrap()
    else {
        panic!("lease is expired at the exact boundary");
    };
    assert_eq!(expired.end_reason, Some(OrgLeaseEndReason::LeaseExpiry));
    assert_eq!(
        expired.expiry_event_id.as_deref(),
        Some("event-expired-lease-contract")
    );
    let reusable_slot = storage.begin(TransactionMode::Immediate).await.unwrap();
    let replacement_attempt = reusable_slot
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "expired-attempt-replacement",
            workspace_id: source_workspace,
            work_item_id: expired_item,
            actor_id: "agent-replacement",
            started_at: 451,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert_eq!(replacement_attempt.attempt_number, 2);
    assert!(matches!(
        reusable_slot
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "expired-lease-replacement",
                    workspace_id: source_workspace,
                    work_item_id: expired_item,
                    attempt_id: &replacement_attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-replacement",
                    fencing_token_hash:
                        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    acquired_at: 451,
                    last_heartbeat_at: 451,
                    expires_at: 551,
                },
                1,
                451,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    reusable_slot.rollback().await.unwrap();

    let allocation = storage.begin(TransactionMode::Immediate).await.unwrap();
    let retry = allocation
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "review-attempt-2",
            workspace_id: target_workspace,
            work_item_id: review_item,
            actor_id: "agent-retry",
            started_at: 500,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert_eq!(retry.attempt_number, 2);
    allocation.rollback().await.unwrap();
    assert_eq!(session.count_org_attempts(review_item).await.unwrap(), 1);
}

async fn assert_submitted_terminal(
    session: &dyn note_storage::StorageSession,
    workspace_id: WorkspaceId,
    work_item_id: WorkItemId,
    attempt_id: &str,
    terminal_status: OrgAttemptStatus,
) {
    let metadata = json!({"lifecycle": attempt_id});
    session
        .insert_org_attempt(NewOrgAttempt {
            id: attempt_id,
            workspace_id,
            work_item_id,
            attempt_number: 1,
            actor_id: "lifecycle-agent",
            status: OrgAttemptStatus::Running,
            started_at: 600,
            note_refs: &[],
            artifacts: &[],
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert!(matches!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: attempt_id,
                expected_status: OrgAttemptStatus::Running,
                status: OrgAttemptStatus::Submitted,
                ended_at: 610,
                error: None,
                result_summary: Some("submitted"),
                review_outcome: None,
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: attempt_id,
                expected_status: OrgAttemptStatus::Submitted,
                status: OrgAttemptStatus::Expired,
                ended_at: 619,
                error: Some("review attempts do not expire"),
                result_summary: None,
                review_outcome: None,
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap_err()
            .kind(),
        StorageErrorKind::Constraint
    );
    assert!(matches!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: attempt_id,
                expected_status: OrgAttemptStatus::Submitted,
                status: terminal_status,
                ended_at: 620,
                error: (terminal_status == OrgAttemptStatus::Failed).then_some("failed"),
                result_summary: None,
                review_outcome: Some("reviewed"),
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    assert_eq!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: attempt_id,
                expected_status: OrgAttemptStatus::Submitted,
                status: terminal_status,
                ended_at: 621,
                error: None,
                result_summary: None,
                review_outcome: Some("must not overwrite"),
                note_refs: &[],
                artifacts: &[],
                metadata: &metadata,
            })
            .await
            .unwrap(),
        ConditionalUpdate::Conflict
    );
}
