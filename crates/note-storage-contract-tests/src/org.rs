use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, ConditionalUpdate, NewOrgAttempt, NewOrgDocument, NewOrgEvent, NewOrgWorkspace,
    OrgArtifactReference, OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEventType,
    OrgProjectedWorkItem, OrgWorkspaceUpdate, StorageBackend, StorageErrorKind, StoredOrgOperation,
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
}
