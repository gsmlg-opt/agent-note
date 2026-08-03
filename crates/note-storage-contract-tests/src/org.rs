use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, OrgDocumentUpdate,
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
            event_type: "state_changed",
            occurred_at: 200,
            summary: "Activated release",
            metadata: &alpha_first_metadata,
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
            event_type: "document_updated",
            occurred_at: 100,
            summary: "Updated canonical source",
            metadata: &alpha_second_metadata,
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
            event_type: "workspace_checked",
            occurred_at: 150,
            summary: "Checked beta workspace",
            metadata: &beta_metadata,
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
            event_type: "duplicate_attempted",
            occurred_at: 250,
            summary: "Duplicate event id",
            metadata: &duplicate_event_metadata,
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
            event_type: "append_recovered",
            occurred_at: 260,
            summary: "Appended after failed duplicate",
            metadata: &alpha_third_metadata,
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
        vec![("beta", beta_id)]
    );
    let all = session.list_org_workspaces(true).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|workspace| (workspace.slug.as_str(), workspace.id))
            .collect::<Vec<_>>(),
        vec![("alpha", alpha_id), ("beta", beta_id)]
    );
}
