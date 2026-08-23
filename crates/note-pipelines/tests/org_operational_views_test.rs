mod support;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    archive_document, export_workspace, get_document, get_item, get_item_context, get_workspace,
    get_workspace_summary, list_documents, list_note_work_items, list_workspaces, query_agenda,
    query_queue, restore_document, CommandEnvelope, DocumentRevisionRequest, DocumentStatus,
    FixedOrgClock, FixedOrgCursorSigner, OperationalQuery, OperationalView, OrgContext,
    OrgDocumentReadQuery, OrgErrorCode, OrgReadQuery, OrgReadyStatus, DEFAULT_OPERATIONAL_LIMIT,
    MAX_OPERATIONAL_LIMIT,
};
use note_storage::{
    ConditionalUpdate, NewOrgAttempt, NewOrgDocument, NewOrgEvent, NewOrgLease, NewOrgWorkspace,
    OrgAttemptStatus, OrgEventType, OrgLeaseKind, OrgProjectedWorkItem, OrgWorkspaceUpdate,
    StoredOrgTimestamp,
};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{event_log, org_test_context, EventStorageBackend};

const NOW: i64 = 1_830_000_000;

fn document_id(value: u32) -> DocumentId {
    DocumentId::from_str(&format!("61000000-0000-4000-8000-{value:012}")).unwrap()
}

fn item_id(value: u32) -> WorkItemId {
    WorkItemId::from_str(&format!("62000000-0000-4000-8000-{value:012}")).unwrap()
}

fn row(
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    id: WorkItemId,
    state: &str,
) -> OrgProjectedWorkItem {
    OrgProjectedWorkItem {
        id,
        workspace_id,
        document_id,
        parent_id: None,
        source_order: 1,
        item_type: WorkItemType::Task,
        title: format!("Item {id}"),
        state: Some(state.into()),
        priority: Some('B'),
        scheduled: None,
        deadline: None,
        assignee: None,
        requires_review: false,
        created_at: 100,
        tags: vec!["ops".into()],
        dependencies: Vec::new(),
        note_links: Vec::new(),
    }
}

async fn seed_operational_rows(
    backend: &Arc<dyn note_storage::StorageBackend>,
    workspace_id: WorkspaceId,
) -> Vec<WorkItemId> {
    let session = backend.session().await.unwrap();
    let document_id = document_id(1);
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "operational.org",
            source: "",
            content_hash: "seed",
            now: 1,
        })
        .await
        .unwrap();
    let ids = (1..=12).map(item_id).collect::<Vec<_>>();
    let mut rows = vec![
        row(workspace_id, document_id, ids[0], "READY"),
        row(workspace_id, document_id, ids[1], "BACKLOG"),
        row(workspace_id, document_id, ids[2], "RUNNING"),
        row(workspace_id, document_id, ids[3], "BLOCKED"),
        row(workspace_id, document_id, ids[4], "REVIEW"),
        row(workspace_id, document_id, ids[5], "BACKLOG"),
        row(workspace_id, document_id, ids[6], "BACKLOG"),
        row(workspace_id, document_id, ids[7], "FAILED"),
        row(workspace_id, document_id, ids[8], "DONE"),
        row(workspace_id, document_id, ids[9], "READY"),
        row(workspace_id, document_id, ids[10], "READY"),
        row(workspace_id, document_id, ids[11], "READY"),
    ];
    rows[0].priority = Some('A');
    rows[0].item_type = WorkItemType::Incident;
    rows[0].assignee = Some("agent-one".into());
    rows[0].scheduled = Some(timestamp("<2027-12-28 Tue 09:20>", NOW));
    rows[0].deadline = Some(timestamp("<2027-12-28 Tue 09:20>", NOW));
    rows[0].note_links = vec![note_org::NoteLink {
        purpose: "context".into(),
        note_id: "63000000-0000-4000-8000-000000000001".parse().unwrap(),
        description: "linked note".into(),
    }];
    rows[1].assignee = Some("agent-one".into());
    rows[5].scheduled = Some(timestamp("<2027-12-28 Tue 09:20>", NOW));
    rows[6].deadline = Some(timestamp("<2027-12-28 Tue 09:20>", NOW));
    rows[9].assignee = Some("other-agent".into());
    rows[9].priority = Some('A');
    rows[10].priority = None;
    rows[11].priority = None;
    for (index, item) in rows.iter_mut().enumerate() {
        item.source_order = index as i64 + 1;
    }
    session
        .replace_org_document_projection(document_id, &rows)
        .await
        .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "operational-attempt",
            workspace_id,
            work_item_id: ids[2],
            attempt_number: 1,
            actor_id: "agent-one",
            status: OrgAttemptStatus::Running,
            started_at: NOW - 100,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    for attempt_number in 1..=3 {
        session
            .insert_org_attempt(NewOrgAttempt {
                id: &format!("failed-attempt-{attempt_number}"),
                workspace_id,
                work_item_id: ids[7],
                attempt_number,
                actor_id: "agent-one",
                status: OrgAttemptStatus::Failed,
                started_at: NOW - 200 + attempt_number,
                note_refs: &[],
                artifacts: &[],
                metadata: &serde_json::json!({}),
            })
            .await
            .unwrap();
    }
    assert!(matches!(
        session
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "operational-expired-lease",
                    workspace_id,
                    work_item_id: ids[2],
                    attempt_id: "operational-attempt",
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent-one",
                    fencing_token_hash:
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    acquired_at: NOW - 100,
                    last_heartbeat_at: NOW - 100,
                    expires_at: NOW,
                },
                4,
                NOW - 100,
            )
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    session
        .append_org_event(NewOrgEvent {
            id: "operational-completion",
            workspace_id,
            subject_kind: "work_item",
            subject_id: &ids[8].to_string(),
            actor_id: "agent-one",
            attempt_id: None,
            event_type: OrgEventType::Completion,
            occurred_at: NOW,
            summary: "completed",
            metadata: &serde_json::json!({}),
            previous_state: Some("RUNNING"),
            resulting_state: Some("DONE"),
        })
        .await
        .unwrap();
    ids
}

fn timestamp(raw: &str, utc_timestamp: i64) -> StoredOrgTimestamp {
    StoredOrgTimestamp {
        raw: raw.into(),
        local: "2027-12-28T09:20:00".into(),
        timezone: "UTC".into(),
        utc_timestamp,
    }
}

async fn page_for(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    view: OperationalView,
) -> note_pipelines::org::OperationalPage {
    let query = OperationalQuery::for_workspace(workspace_id, view);
    if view.is_agenda() {
        query_agenda(context, &query).await.unwrap()
    } else {
        query_queue(context, &query).await.unwrap()
    }
}

fn command(workspace_id: WorkspaceId, operation_id: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id,
        actor_id: "agent-one".into(),
        operation_id: operation_id.into(),
    }
}

#[tokio::test]
async fn archived_document_work_disappears_operationally_but_still_blocks_dependencies() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let archived_item_ids = seed_operational_rows(&backend, workspace_id).await;
    let archived_document = document_id(1);
    let dependent_document = document_id(5);
    let dependent_id = item_id(5_000);
    let session = backend.session().await.unwrap();
    session
        .insert_org_document(NewOrgDocument {
            id: dependent_document,
            workspace_id,
            path: "dependent.org",
            source: "",
            content_hash: "dependent",
            now: 1,
        })
        .await
        .unwrap();
    let mut dependent = row(workspace_id, dependent_document, dependent_id, "READY");
    dependent.dependencies = vec![archived_item_ids[1]];
    session
        .replace_org_document_projection(dependent_document, &[dependent])
        .await
        .unwrap();
    session
        .append_org_event(NewOrgEvent {
            id: "archived-document-historical-event",
            workspace_id,
            subject_kind: "work_item",
            subject_id: &archived_item_ids[2].to_string(),
            actor_id: "agent-one",
            attempt_id: Some("operational-attempt"),
            event_type: OrgEventType::Progress,
            occurred_at: NOW - 50,
            summary: "historical progress",
            metadata: &serde_json::json!({}),
            previous_state: Some("RUNNING"),
            resulting_state: Some("RUNNING"),
        })
        .await
        .unwrap();
    drop(session);

    let active_context = get_item_context(&context, workspace_id, archived_item_ids[2])
        .await
        .unwrap();
    assert!(active_context
        .operational
        .classifications
        .contains(&OperationalView::Running));
    assert_eq!(active_context.attempts.len(), 1);
    assert_eq!(active_context.history_segments[0].events.len(), 1);

    let mut before_views = std::collections::BTreeMap::new();
    for view in OperationalView::ALL {
        before_views.insert(
            view as u8,
            page_for(&context, workspace_id, view)
                .await
                .items
                .into_iter()
                .map(|row| row.item.id)
                .collect::<Vec<_>>(),
        );
    }
    let before_summary = get_workspace_summary(&context, workspace_id).await.unwrap();
    assert!(get_item_context(&context, workspace_id, dependent_id)
        .await
        .unwrap()
        .operational
        .blockers
        .contains(&"dependencies_incomplete".to_string()));

    archive_document(
        &context,
        &command(workspace_id, "archive-operational-document"),
        &DocumentRevisionRequest {
            document_id: archived_document,
            expected_revision: 1,
        },
    )
    .await
    .unwrap();

    for view in OperationalView::ALL {
        let page = page_for(&context, workspace_id, view).await;
        assert!(page
            .items
            .iter()
            .all(|row| row.item.document_id != archived_document));
        assert!(page.items.iter().all(|row| row.item.id != dependent_id));
    }
    let archived_summary = get_workspace_summary(&context, workspace_id).await.unwrap();
    for view in OperationalView::ALL {
        assert_eq!(count(&archived_summary.counts, view), 0, "view {view:?}");
    }
    let archived_context = get_item_context(&context, workspace_id, archived_item_ids[2])
        .await
        .unwrap();
    assert_eq!(archived_context.item.id, archived_item_ids[2]);
    assert_eq!(archived_context.document.id, archived_document);
    assert_eq!(archived_context.document.archived_at, Some(NOW));
    assert_eq!(archived_context.attempts, active_context.attempts);
    assert_eq!(
        archived_context.history_segments,
        active_context.history_segments
    );
    assert!(archived_context.operational.classifications.is_empty());
    assert_eq!(archived_context.operational.readiness, None);
    assert_eq!(
        archived_context.operational.blockers,
        vec!["document_archived"]
    );
    assert!(!archived_context.operational.recovery.eligible);
    assert!(!archived_context.operational.recovery.candidate);
    assert_eq!(
        archived_context.operational.recovery.blockers,
        vec!["document_archived"]
    );
    assert!(!archived_context
        .operational
        .blockers
        .contains(&"workspace_archived".to_string()));
    let dependent_context = get_item_context(&context, workspace_id, dependent_id)
        .await
        .unwrap();
    assert_eq!(dependent_context.dependencies.len(), 1);
    assert_eq!(
        dependent_context.dependencies[0].item.id,
        archived_item_ids[1]
    );
    assert!(!dependent_context.dependencies[0].satisfied);
    assert!(dependent_context
        .operational
        .blockers
        .contains(&"dependencies_incomplete".to_string()));

    restore_document(
        &context,
        &command(workspace_id, "restore-operational-document"),
        &DocumentRevisionRequest {
            document_id: archived_document,
            expected_revision: 2,
        },
    )
    .await
    .unwrap();

    for view in OperationalView::ALL {
        let restored_ids = page_for(&context, workspace_id, view)
            .await
            .items
            .into_iter()
            .map(|row| row.item.id)
            .collect::<Vec<_>>();
        assert_eq!(restored_ids, before_views[&(view as u8)], "view {view:?}");
    }
    assert_eq!(
        get_workspace_summary(&context, workspace_id)
            .await
            .unwrap()
            .counts,
        before_summary.counts
    );
}

#[tokio::test]
async fn queue_defaults_to_fifty_and_rejects_family_and_limit_errors() {
    let (context, _backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let query = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);

    let page = query_queue(&context, &query).await.unwrap();
    assert_eq!(DEFAULT_OPERATIONAL_LIMIT, 50);
    assert_eq!(MAX_OPERATIONAL_LIMIT, 200);
    assert_eq!(page.evaluated_at, NOW);
    assert!(page.items.is_empty());

    for limit in [Some(0), Some(201)] {
        let error = query_queue(
            &context,
            &OperationalQuery {
                limit,
                ..query.clone()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
    }
    assert_eq!(
        query_queue(
            &context,
            &OperationalQuery::for_workspace(workspace_id, OperationalView::Scheduled),
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    assert_eq!(
        query_agenda(&context, &query).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );
    assert_eq!(
        query_queue(
            &context,
            &OperationalQuery {
                workspace_ids: Vec::new(),
                ..query
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    assert_eq!(
        query_queue(
            &context,
            &OperationalQuery {
                priority: Some('A'),
                priority_is_none: true,
                ..OperationalQuery::for_workspace(workspace_id, OperationalView::Ready)
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn default_page_returns_fifty_and_resumes_stable_ties() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
    let document_id = document_id(2);
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "fifty.org",
            source: "",
            content_hash: "fifty",
            now: 1,
        })
        .await
        .unwrap();
    let rows = (100..155)
        .enumerate()
        .map(|(index, value)| {
            let mut item = row(workspace_id, document_id, item_id(value), "READY");
            item.source_order = index as i64 + 1;
            item
        })
        .collect::<Vec<_>>();
    session
        .replace_org_document_projection(document_id, &rows)
        .await
        .unwrap();

    let mut query = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);
    let first = query_queue(&context, &query).await.unwrap();
    assert_eq!(first.items.len(), 50);
    assert!(first.next_cursor.is_some());
    query.cursor = first.next_cursor;
    let second = query_queue(&context, &query).await.unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(second.next_cursor.is_none());
    let ids = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|item| item.item.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 55);
}

#[tokio::test]
async fn exact_storage_batch_probes_end_without_returning_empty_continuation() {
    let (_context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
    let document_id = document_id(3);
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "exact-batch.org",
            source: "",
            content_hash: "exact-batch",
            now: 1,
        })
        .await
        .unwrap();
    let rows = (1_000..1_200)
        .enumerate()
        .map(|(index, value)| {
            let mut item = row(workspace_id, document_id, item_id(value), "READY");
            item.source_order = index as i64 + 1;
            item
        })
        .collect::<Vec<_>>();
    session
        .replace_org_document_projection(document_id, &rows)
        .await
        .unwrap();
    let traced = Arc::new(EventStorageBackend::new(backend, event_log()));
    let context = OrgContext::new(traced.clone(), Arc::new(FixedOrgClock::new(NOW)));
    let page = query_queue(
        &context,
        &OperationalQuery {
            limit: Some(200),
            ..OperationalQuery::for_workspace(workspace_id, OperationalView::Ready)
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 200);
    assert!(page.next_cursor.is_none());
    assert_eq!(traced.repository_call_count("query_org_operational"), 2);
}

#[tokio::test]
async fn readiness_overfetch_and_has_next_cross_multiple_full_rejected_batches() {
    let (_context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
    let document_id = document_id(4);
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "postfilter-batches.org",
            source: "",
            content_hash: "postfilter-batches",
            now: 1,
        })
        .await
        .unwrap();
    let rows = (2_000..=2_401)
        .enumerate()
        .map(|(index, value)| {
            let mut item = row(workspace_id, document_id, item_id(value), "READY");
            item.priority = Some('A');
            item.source_order = index as i64 + 1;
            if value != 2_000 && value != 2_401 {
                item.assignee = Some("another-agent".into());
            }
            item
        })
        .collect::<Vec<_>>();
    session
        .replace_org_document_projection(document_id, &rows)
        .await
        .unwrap();
    let traced = Arc::new(EventStorageBackend::new(backend, event_log()));
    let context = OrgContext::new(traced.clone(), Arc::new(FixedOrgClock::new(NOW)));
    let mut query = OperationalQuery {
        limit: Some(1),
        ..OperationalQuery::for_workspace(workspace_id, OperationalView::Ready)
    };

    let first = query_queue(&context, &query).await.unwrap();
    assert_eq!(first.evaluated_at, NOW);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].item.id, item_id(2_000));
    assert!(first.next_cursor.is_some());
    assert_eq!(traced.repository_call_count("query_org_operational"), 3);

    query.cursor = first.next_cursor;
    let later = context
        .clone()
        .with_clock(Arc::new(FixedOrgClock::new(NOW + 50_000)));
    let second = query_queue(&later, &query).await.unwrap();
    assert_eq!(second.evaluated_at, NOW);
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].item.id, item_id(2_401));
    assert!(second.next_cursor.is_none());
    assert_eq!(traced.repository_call_count("query_org_operational"), 6);
    assert_ne!(first.items[0].item.id, second.items[0].item.id);
}

#[tokio::test]
async fn ten_views_filters_timestamps_markers_and_summary_are_consistent() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let ids = seed_operational_rows(&backend, workspace_id).await;

    let expected_members = [
        (OperationalView::Ready, vec![ids[2], ids[10], ids[11]]),
        (OperationalView::Assigned, vec![ids[0], ids[1], ids[9]]),
        (OperationalView::Running, vec![ids[2]]),
        (OperationalView::Blocked, vec![ids[3]]),
        (OperationalView::Review, vec![ids[4]]),
        (OperationalView::Scheduled, vec![ids[0], ids[5]]),
        (OperationalView::UpcomingDeadline, vec![ids[0], ids[6]]),
        (OperationalView::Failed, vec![ids[7]]),
        (OperationalView::ExpiredLease, vec![ids[2]]),
        (OperationalView::Completed, vec![ids[8]]),
    ];
    let summary = get_workspace_summary(&context, workspace_id).await.unwrap();
    assert_eq!(summary.evaluated_at, NOW);
    assert_eq!(summary.timezone, "UTC");
    for (view, expected) in expected_members {
        let page = page_for(&context, workspace_id, view).await;
        let actual = page
            .items
            .iter()
            .map(|row| row.item.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected.into_iter().collect(), "view {view:?}");
        assert!(page
            .items
            .iter()
            .all(|row| row.item.workspace_id == workspace_id));
        assert_eq!(count(&summary.counts, view), page.items.len() as i64);
    }
    let recovery = page_for(&context, workspace_id, OperationalView::Ready)
        .await
        .items
        .into_iter()
        .find(|row| row.item.id == ids[2])
        .unwrap();
    assert_eq!(
        recovery.ready_status,
        Some(OrgReadyStatus::RecoveryCandidate)
    );
    assert_eq!(recovery.lease.as_ref().unwrap().status, "expired");
    assert_eq!(recovery.attempt_count, 1);
    assert_eq!(recovery.current_attempt_status.as_deref(), Some("running"));
    let lease_json = serde_json::to_string(&recovery).unwrap();
    assert!(!lease_json.contains("fencing_token"));
    assert!(!lease_json.contains("token_hash"));

    let assigned_context = get_item_context(&context, workspace_id, ids[0])
        .await
        .unwrap();
    assert!(!assigned_context
        .operational
        .classifications
        .contains(&OperationalView::Ready));
    assert!(assigned_context
        .operational
        .blockers
        .contains(&"assigned_to_other_actor".to_string()));

    let mut filter = OperationalQuery::for_workspace(workspace_id, OperationalView::Assigned);
    filter.item_type = Some(WorkItemType::Incident);
    filter.priority = Some('A');
    filter.tags = vec![" ops ".into(), "ops".into()];
    filter.assignee = Some("agent-one".into());
    assert_eq!(
        query_queue(&context, &filter).await.unwrap().items[0]
            .item
            .id,
        ids[0]
    );
    filter.state = Some("READY".into());
    filter.scheduled_from = Some(NOW);
    filter.scheduled_to = Some(NOW);
    filter.deadline_from = Some(NOW);
    filter.deadline_to = Some(NOW);
    assert_eq!(query_queue(&context, &filter).await.unwrap().items.len(), 1);

    let mut schedule = OperationalQuery::for_workspace(workspace_id, OperationalView::Scheduled);
    schedule.from = Some(NOW);
    schedule.to = Some(NOW);
    let row = query_agenda(&context, &schedule)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    let scheduled = row.item.scheduled.unwrap();
    assert_eq!(scheduled.raw, "<2027-12-28 Tue 09:20>");
    assert_eq!(scheduled.local, "2027-12-28T09:20:00");
    assert_eq!(scheduled.timezone, "UTC");
    assert_eq!(scheduled.utc_timestamp, NOW);
    let mut deadline =
        OperationalQuery::for_workspace(workspace_id, OperationalView::UpcomingDeadline);
    deadline.from = Some(NOW);
    deadline.to = Some(NOW);
    assert_eq!(
        query_agenda(&context, &deadline).await.unwrap().items.len(),
        2
    );
    let review = page_for(&context, workspace_id, OperationalView::Review).await;
    assert_eq!(
        review.items[0].review_lease_status,
        Some(note_pipelines::org::OrgReviewLeaseStatus::Unleased)
    );
    let failed = page_for(&context, workspace_id, OperationalView::Failed).await;
    assert_eq!(failed.items[0].attempt_count, 3);
    assert!(failed.items[0].retry_exhausted);
    let mut completed = OperationalQuery::for_workspace(workspace_id, OperationalView::Completed);
    completed.completed_from = Some(NOW);
    completed.completed_to = Some(NOW);
    assert_eq!(
        query_queue(&context, &completed).await.unwrap().items.len(),
        1
    );
}

#[tokio::test]
async fn cursor_uses_last_scanned_fingerprint_and_frozen_time_without_duplicates() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let ids = seed_operational_rows(&backend, workspace_id).await;
    let mut query = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);
    query.limit = Some(2);
    let first = query_queue(&context, &query).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());
    assert!(!first.items.iter().any(|row| row.item.id == ids[9]));

    query.cursor = first.next_cursor.clone();
    let later = context
        .clone()
        .with_clock(Arc::new(FixedOrgClock::new(NOW + 10_000)));
    let second = query_queue(&later, &query).await.unwrap();
    assert_eq!(second.evaluated_at, NOW);
    let all = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|row| row.item.id)
        .collect::<Vec<_>>();
    assert_eq!(
        all.len(),
        all.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    assert_eq!(
        all.into_iter().collect::<std::collections::BTreeSet<_>>(),
        [ids[2], ids[10], ids[11]].into_iter().collect()
    );

    let mut mismatched = query.clone();
    mismatched.tags = vec!["changed".into()];
    assert_eq!(
        query_queue(&later, &mismatched).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );
    mismatched = query;
    mismatched.cursor = Some("not-a-cursor".into());
    assert_eq!(
        query_queue(&later, &mismatched).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn unprioritized_filter_is_distinct_from_an_omitted_priority_filter() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    seed_operational_rows(&backend, workspace_id).await;

    let mut unprioritized = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);
    unprioritized.priority_is_none = true;
    unprioritized.limit = Some(1);
    let first = query_queue(&context, &unprioritized).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.items[0].item.priority.is_none());

    let mut omitted = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);
    omitted.limit = Some(1);
    omitted.cursor = first.next_cursor;
    assert_eq!(
        query_queue(&context, &omitted).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn cursor_rejects_tampered_view_specific_storage_tuple_shape() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    seed_operational_rows(&backend, workspace_id).await;

    let mut ready = OperationalQuery::for_workspace(workspace_id, OperationalView::Ready);
    ready.limit = Some(1);
    let cursor = query_queue(&context, &ready)
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    let another_process = context
        .clone()
        .with_cursor_signer(Arc::new(FixedOrgCursorSigner::new([0xa5; 32])));
    ready.cursor = Some(cursor.clone());
    assert_eq!(
        query_queue(&another_process, &ready)
            .await
            .unwrap_err()
            .code,
        OrgErrorCode::InvalidInput
    );
    ready.cursor = Some(tamper_cursor(&cursor, |value| {
        value["last_scanned"]["primary_at"] = serde_json::json!(NOW);
    }));
    assert_eq!(
        query_queue(&context, &ready).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );

    for tampered in [
        tamper_cursor(&cursor, |value| {
            value["evaluated_at"] = serde_json::json!(NOW + 1);
        }),
        tamper_cursor(&cursor, |value| {
            value["last_scanned"]["created_at"] = serde_json::json!(101);
        }),
    ] {
        ready.cursor = Some(tampered);
        assert_eq!(
            query_queue(&context, &ready).await.unwrap_err().code,
            OrgErrorCode::InvalidInput
        );
    }

    let mut scheduled = OperationalQuery::for_workspace(workspace_id, OperationalView::Scheduled);
    scheduled.limit = Some(1);
    let cursor = query_agenda(&context, &scheduled)
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    for tampered in [
        tamper_cursor(&cursor, |value| {
            value["last_scanned"]["primary_at"] = serde_json::Value::Null;
        }),
        tamper_cursor(&cursor, |value| {
            value["last_scanned"]["primary_at"] = serde_json::json!(NOW + 1);
        }),
    ] {
        scheduled.cursor = Some(tampered);
        assert_eq!(
            query_agenda(&context, &scheduled).await.unwrap_err().code,
            OrgErrorCode::InvalidInput
        );
    }
}

fn tamper_cursor(cursor: &str, mutate: impl FnOnce(&mut serde_json::Value)) -> String {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).unwrap();
    let mut signed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let payload = URL_SAFE_NO_PAD
        .decode(signed["payload"].as_str().unwrap())
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    mutate(&mut value);
    signed["payload"] =
        serde_json::Value::String(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap()));
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&signed).unwrap())
}

#[tokio::test]
async fn multi_workspace_queries_preflight_every_policy_and_never_return_partial_rows() {
    let (context, backend, _dir, _db_path, workspace_a) = org_test_context(NOW).await;
    let workspace_b = WorkspaceId::from_str("11111111-1111-4111-8111-111111111122").unwrap();
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allow_cross_workspace_agenda = true;
    let session = backend.session().await.unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_b,
            slug: "workspace-b",
            display_name: "Workspace B",
            description: "multi workspace fixture",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &policy,
            now: 1,
        })
        .await
        .unwrap();
    seed_operational_rows(&backend, workspace_a).await;
    let mut query = OperationalQuery::for_workspace(workspace_a, OperationalView::Assigned);
    query.workspace_ids.push(workspace_b);
    assert_eq!(
        query_queue(&context, &query).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );

    let current = session
        .get_org_workspace(workspace_a)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: workspace_a,
                expected_revision: current.revision,
                slug: &current.slug,
                display_name: &current.display_name,
                description: &current.description,
                timezone: &current.timezone,
                policy_schema_version: current.policy_schema_version,
                policy: &policy,
                archived_at: current.archived_at,
                updated_at: NOW,
            })
            .await
            .unwrap(),
        note_storage::CompareAndSwap::Applied(_)
    ));
    let page = query_queue(&context, &query).await.unwrap();
    assert!(page
        .items
        .iter()
        .all(|row| [workspace_a, workspace_b].contains(&row.item.workspace_id)));

    query
        .workspace_ids
        .push(WorkspaceId::from_str("11111111-1111-4111-8111-111111111199").unwrap());
    assert_eq!(
        query_queue(&context, &query).await.unwrap_err().code,
        OrgErrorCode::NotFound
    );
}

#[tokio::test]
async fn archived_rows_require_explicit_inclusion() {
    let (context, backend, _dir, _db_path, _workspace_id) = org_test_context(NOW).await;
    let workspace_id = WorkspaceId::from_str("11111111-1111-4111-8111-111111111133").unwrap();
    let session = backend.session().await.unwrap();
    let policy = WorkspacePolicy::engineering_default();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id,
            slug: "archived",
            display_name: "Archived",
            description: "archived fixture",
            timezone: "Asia/Shanghai",
            policy_schema_version: 1,
            policy: &policy,
            now: 1,
        })
        .await
        .unwrap();
    let document_id = document_id(99);
    session
        .insert_org_document(NewOrgDocument {
            id: document_id,
            workspace_id,
            path: "archived.org",
            source: "",
            content_hash: "archived",
            now: 1,
        })
        .await
        .unwrap();
    let mut item = row(workspace_id, document_id, item_id(99), "BACKLOG");
    item.assignee = Some("agent-one".into());
    session
        .replace_org_document_projection(document_id, &[item])
        .await
        .unwrap();
    let current = session
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: workspace_id,
                expected_revision: current.revision,
                slug: &current.slug,
                display_name: &current.display_name,
                description: &current.description,
                timezone: &current.timezone,
                policy_schema_version: current.policy_schema_version,
                policy: &current.policy,
                archived_at: Some(NOW),
                updated_at: NOW,
            })
            .await
            .unwrap(),
        note_storage::CompareAndSwap::Applied(_)
    ));

    let mut query = OperationalQuery::for_workspace(workspace_id, OperationalView::Assigned);
    assert!(query_queue(&context, &query)
        .await
        .unwrap()
        .items
        .is_empty());
    query.include_archived = true;
    assert_eq!(query_queue(&context, &query).await.unwrap().items.len(), 1);
    assert_eq!(
        get_workspace_summary(&context, workspace_id)
            .await
            .unwrap()
            .archived_at,
        Some(NOW)
    );
}

#[tokio::test]
async fn page_and_summary_use_bounded_set_reads_not_per_item_queries() {
    let (_context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    seed_operational_rows(&backend, workspace_id).await;
    let traced = Arc::new(EventStorageBackend::new(backend, event_log()));
    let context = OrgContext::new(traced.clone(), Arc::new(FixedOrgClock::new(NOW)));

    let page = query_queue(
        &context,
        &OperationalQuery::for_workspace(workspace_id, OperationalView::Ready),
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3);
    assert_eq!(traced.repository_call_count("get_org_workspace"), 1);
    assert_eq!(
        traced.repository_call_count("list_org_workspace_projection"),
        1
    );
    assert_eq!(traced.repository_call_count("count_active_org_leases"), 1);
    assert_eq!(traced.repository_call_count("query_org_operational"), 1);
    assert_eq!(traced.repository_call_count("get_org_work_item"), 0);
    assert_eq!(traced.repository_call_count("list_org_attempts"), 0);

    let summary = get_workspace_summary(&context, workspace_id).await.unwrap();
    assert_eq!(summary.counts.ready, 3);
    assert_eq!(
        traced.repository_call_count("get_org_workspace_operational_summary"),
        1
    );
    assert_eq!(traced.repository_call_count("query_org_operational"), 2);
    assert_eq!(traced.repository_call_count("get_org_work_item"), 0);
    assert_eq!(traced.repository_call_count("list_org_attempts"), 0);
}

#[tokio::test]
async fn workspace_directory_summarizes_only_the_requested_identity_page() {
    let (_context, backend, _dir, _db_path, _workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
    for suffix in 40..44 {
        let workspace_id =
            WorkspaceId::from_str(&format!("11111111-1111-4111-8111-{suffix:012}")).unwrap();
        session
            .insert_org_workspace(NewOrgWorkspace {
                id: workspace_id,
                slug: &format!("workspace-{suffix}"),
                display_name: "Paged workspace",
                description: "workspace page fixture",
                timezone: "UTC",
                policy_schema_version: 1,
                policy: &WorkspacePolicy::engineering_default(),
                now: 1,
            })
            .await
            .unwrap();
    }
    let traced = Arc::new(EventStorageBackend::new(backend, event_log()));
    let context = OrgContext::new(traced.clone(), Arc::new(FixedOrgClock::new(NOW)));
    let mut query = OrgReadQuery {
        limit: Some(1),
        ..OrgReadQuery::default()
    };
    let page = list_workspaces(&context, &query).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_some());
    assert_eq!(traced.repository_call_count("list_org_workspaces"), 1);
    assert_eq!(
        traced.repository_call_count("get_org_workspace_operational_summary"),
        1
    );
    assert_eq!(
        traced.repository_call_count("list_org_workspace_projection"),
        1
    );
    assert_eq!(traced.repository_call_count("query_org_operational"), 1);
    query.cursor = Some(tamper_cursor(page.next_cursor.as_ref().unwrap(), |value| {
        value["last_id"] = serde_json::json!("11111111-1111-4111-8111-000000000099");
    }));
    assert_eq!(
        list_workspaces(&context, &query).await.unwrap_err().code,
        OrgErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn transport_read_facades_return_canonical_data_with_opaque_paging() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let ids = seed_operational_rows(&backend, workspace_id).await;
    let query = OrgReadQuery::default();
    let document_query = OrgDocumentReadQuery::default();

    assert_eq!(
        get_workspace(&context, workspace_id).await.unwrap().id,
        workspace_id
    );
    assert_eq!(
        list_workspaces(&context, &query).await.unwrap().items.len(),
        1
    );
    let documents = list_documents(&context, workspace_id, &document_query)
        .await
        .unwrap();
    assert_eq!(documents.items.len(), 1);
    let document = get_document(&context, workspace_id, document_id(1))
        .await
        .unwrap();
    assert_eq!(document.path, "operational.org");
    assert_eq!(
        export_workspace(&context, workspace_id)
            .await
            .unwrap()
            .documents
            .len(),
        1
    );
    assert_eq!(
        get_item(&context, workspace_id, ids[0]).await.unwrap().id,
        ids[0]
    );
    let linked = list_note_work_items(&context, "63000000-0000-4000-8000-000000000001", &query)
        .await
        .unwrap();
    assert_eq!(linked.items.len(), 1);
    assert_eq!(linked.items[0].id, ids[0]);

    let mismatched = OrgDocumentReadQuery {
        cursor: Some("not-a-cursor".into()),
        status: DocumentStatus::Active,
        ..OrgDocumentReadQuery::default()
    };
    assert_eq!(
        list_documents(&context, workspace_id, &mismatched)
            .await
            .unwrap_err()
            .code,
        OrgErrorCode::InvalidInput
    );
}

fn count(counts: &note_pipelines::org::OperationalCounts, view: OperationalView) -> i64 {
    match view {
        OperationalView::Ready => counts.ready,
        OperationalView::Assigned => counts.assigned,
        OperationalView::Running => counts.running,
        OperationalView::Blocked => counts.blocked,
        OperationalView::Review => counts.review,
        OperationalView::Scheduled => counts.scheduled,
        OperationalView::UpcomingDeadline => counts.upcoming_deadline,
        OperationalView::Failed => counts.failed,
        OperationalView::ExpiredLease => counts.expired_lease,
        OperationalView::Completed => counts.completed,
    }
}
