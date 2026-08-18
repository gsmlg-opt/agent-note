mod support;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    create_follow_up, create_item, get_item_context, list_event_history, move_document,
    put_document, CommandEnvelope, CreateFollowUpRequest, CreateItemRequest, FixedOrgClock,
    FollowUpOrigin, MoveDocumentRequest, OrgContext, OrgErrorCode, OrgEventQuery, OrgOriginView,
    PutDocumentRequest,
};
use note_storage::{
    ConditionalUpdate, NewNote, NewOrgAttempt, NewOrgEvent, NewOrgWorkspace, OrgArtifactReference,
    OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate, OrgEventType, OrgWorkspaceUpdate,
};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{event_log, org_test_context, EventStorageBackend};

const NOW: i64 = 1_820_000_000;

fn document_id(value: &str) -> DocumentId {
    DocumentId::from_str(value).unwrap()
}

fn item_id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

fn envelope(workspace_id: WorkspaceId, operation_id: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id,
        actor_id: "recovery-agent".into(),
        operation_id: operation_id.into(),
    }
}

fn create_request(
    document_id: DocumentId,
    item_id: WorkItemId,
    revision: i64,
) -> CreateItemRequest {
    CreateItemRequest {
        document_id,
        parent_id: None,
        item_id,
        item_type: WorkItemType::Task,
        title: "Recovery follow-up".into(),
        initial_state: None,
        priority: None,
        tags: BTreeSet::new(),
        assignee: None,
        requires_review: false,
        expected_revisions: BTreeMap::from([(document_id, revision)]),
    }
}

#[tokio::test]
async fn context_maps_canonical_relations_attempts_history_and_note_availability() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("61000000-0000-4000-8000-000000000001");
    let parent = item_id("62000000-0000-4000-8000-000000000001");
    let child = item_id("62000000-0000-4000-8000-000000000002");
    let note_id = "63000000-0000-4000-8000-000000000001";
    let session = backend.session().await.unwrap();
    session
        .insert_note(NewNote {
            id: note_id,
            title: "Recovery context",
            content: "secret body must not be exposed",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let source = format!(
        "* DONE Parent\r\n:PROPERTIES:\r\n:ID: {parent}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n** BACKLOG Child\r\n:PROPERTIES:\r\n:ID: {child}\r\n:AGENT_NOTE_TYPE: subtask\r\n:DEPENDS_ON: {parent}\r\n:END:\r\n[[agent-note:design:{note_id}][Design]]\r\n"
    );
    put_document(
        &context,
        &envelope(workspace_id, "seed-context"),
        &PutDocumentRequest {
            document_id: document,
            path: "recovery.org".into(),
            source,
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-1",
            workspace_id,
            work_item_id: child,
            attempt_number: 1,
            actor_id: "worker-1",
            status: OrgAttemptStatus::Submitted,
            started_at: NOW + 1,
            note_refs: &[OrgAttemptNoteReference {
                purpose: "runbook".into(),
                note_id: note_id.into(),
                description: "Runbook".into(),
            }],
            artifacts: &[OrgArtifactReference {
                uri: "s3://artifacts/log.txt".into(),
                media_type: "text/plain".into(),
                name: "log".into(),
                description: "Failure log".into(),
            }],
            metadata: &serde_json::json!({"runner": "ci"}),
        })
        .await
        .unwrap();
    assert!(matches!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: "attempt-1",
                expected_status: OrgAttemptStatus::Submitted,
                status: OrgAttemptStatus::Failed,
                ended_at: NOW + 1,
                error: Some("review requested changes"),
                result_summary: Some("candidate result"),
                review_outcome: Some("rejected"),
                note_refs: &[OrgAttemptNoteReference {
                    purpose: "runbook".into(),
                    note_id: note_id.into(),
                    description: "Runbook".into(),
                }],
                artifacts: &[OrgArtifactReference {
                    uri: "s3://artifacts/log.txt".into(),
                    media_type: "text/plain".into(),
                    name: "log".into(),
                    description: "Failure log".into(),
                }],
                metadata: &serde_json::json!({"runner": "ci"}),
            })
            .await
            .unwrap(),
        ConditionalUpdate::Applied(_)
    ));
    for (index, event_type) in [
        OrgEventType::Progress,
        OrgEventType::ResultSubmission,
        OrgEventType::Failure,
        OrgEventType::ReviewRequest,
        OrgEventType::Rejection,
    ]
    .into_iter()
    .enumerate()
    {
        session
            .append_org_event(NewOrgEvent {
                id: &format!("context-event-{index}"),
                workspace_id,
                subject_kind: "work_item",
                subject_id: &child.to_string(),
                actor_id: "worker-1",
                attempt_id: Some("attempt-1"),
                event_type,
                occurred_at: NOW + 2 + index as i64,
                summary: "context event",
                metadata: &serde_json::json!({"index": index}),
                previous_state: Some("BACKLOG"),
                resulting_state: Some("BACKLOG"),
            })
            .await
            .unwrap();
    }

    let hydrated = get_item_context(&context, workspace_id, child)
        .await
        .unwrap();
    assert_eq!(hydrated.workspace.id, workspace_id);
    assert_eq!(hydrated.workspace_revision, 1);
    assert_eq!(hydrated.document.path, "recovery.org");
    assert_eq!(hydrated.document.revision, 1);
    assert_eq!(hydrated.item.id, child);
    assert_eq!(hydrated.parent.as_ref().unwrap().id, parent);
    assert!(hydrated.children.is_empty());
    assert_eq!(hydrated.dependencies[0].item.id, parent);
    assert!(hydrated.dependencies[0].satisfied);
    assert!(hydrated.note_links[0].available);
    assert_eq!(hydrated.attempts[0].artifacts[0].name, "log");
    assert_eq!(
        hydrated.attempts[0].result_summary.as_deref(),
        Some("candidate result")
    );
    assert_eq!(
        hydrated.attempts[0].review_outcome.as_deref(),
        Some("rejected")
    );
    assert_eq!(
        hydrated.attempts[0].error.as_deref(),
        Some("review requested changes")
    );
    assert_eq!(hydrated.history_segments[0].workspace_id, workspace_id);
    let event_types = hydrated.history_segments[0]
        .events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"progress"));
    assert!(event_types.contains(&"result_submission"));
    assert!(event_types.contains(&"failure"));
    assert!(event_types.contains(&"review_request"));
    assert!(event_types.contains(&"rejection"));
    assert!(hydrated.history_segments[0]
        .events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert_eq!(hydrated.lease, None);
    assert_eq!(
        hydrated.operational.attempt_budget.execution_attempt_count,
        1
    );
    assert_eq!(hydrated.operational.attempt_budget.max_attempts, 3);
    assert_eq!(hydrated.operational.attempt_budget.remaining_attempts, 2);
    assert!(!hydrated.operational.recovery.candidate);
    assert!(!hydrated.operational.recovery.eligible);
    assert!(hydrated
        .operational
        .blockers
        .contains(&"non_executable_state".to_string()));
    let json = serde_json::to_value(&hydrated).unwrap();
    assert_eq!(json["lease"], serde_json::Value::Null);
    assert!(!json.to_string().contains("fencing_token"));
    assert!(!json.to_string().contains("fencing_token_hash"));
    assert!(!json.to_string().contains("token_hash"));
    assert!(!json.to_string().contains("secret body"));

    session
        .soft_delete_note(note_id, 1, NOW + 10)
        .await
        .unwrap();
    let deleted = get_item_context(&context, workspace_id, child)
        .await
        .unwrap();
    assert!(!deleted.note_links[0].available);
    assert_eq!(deleted.note_links[0].note_id, note_id);
    session.permanently_delete_note(note_id, 2).await.unwrap();
    let permanently_deleted = get_item_context(&context, workspace_id, child)
        .await
        .unwrap();
    assert!(!permanently_deleted.note_links[0].available);

    let workspace = session
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .unwrap();
    session
        .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
            id: workspace_id,
            expected_revision: workspace.revision,
            slug: &workspace.slug,
            display_name: &workspace.display_name,
            description: &workspace.description,
            timezone: &workspace.timezone,
            policy_schema_version: workspace.policy_schema_version,
            policy: &workspace.policy,
            archived_at: Some(NOW + 20),
            updated_at: NOW + 20,
        })
        .await
        .unwrap();
    let archived = get_item_context(&context, workspace_id, child)
        .await
        .unwrap();
    assert_eq!(archived.workspace.archived_at, Some(NOW + 20));
    assert!(!list_event_history(
        &context,
        &OrgEventQuery {
            workspace_id,
            subject_kind: Some("work_item".into()),
            subject_id: Some(child.to_string()),
            cursor: None,
            limit: None,
        },
    )
    .await
    .unwrap()
    .events
    .is_empty());
}

#[tokio::test]
async fn context_hydrates_work_item_and_event_follow_up_origins() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("61000000-0000-4000-8000-000000000010");
    put_document(
        &context,
        &envelope(workspace_id, "seed-origin-document"),
        &PutDocumentRequest {
            document_id: document,
            path: "origins.org".into(),
            source: "Origin prelude.\r\n".into(),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let origin_item = item_id("62000000-0000-4000-8000-000000000010");
    let created = create_item(
        &context,
        &envelope(workspace_id, "create-origin-item"),
        &create_request(document, origin_item, 1),
    )
    .await
    .unwrap();
    let work_item_follow_up = item_id("62000000-0000-4000-8000-000000000011");
    create_follow_up(
        &context,
        &envelope(workspace_id, "create-work-item-origin-follow-up"),
        &CreateFollowUpRequest {
            item: create_request(document, work_item_follow_up, 2),
            origin: FollowUpOrigin::WorkItem(origin_item),
        },
    )
    .await
    .unwrap();
    let event_follow_up = item_id("62000000-0000-4000-8000-000000000012");
    create_follow_up(
        &context,
        &envelope(workspace_id, "create-event-origin-follow-up"),
        &CreateFollowUpRequest {
            item: create_request(document, event_follow_up, 3),
            origin: FollowUpOrigin::Event(created.event_ids[0].clone()),
        },
    )
    .await
    .unwrap();

    match get_item_context(&context, workspace_id, work_item_follow_up)
        .await
        .unwrap()
        .origin
        .unwrap()
    {
        OrgOriginView::WorkItem { work_item_id, item } => {
            assert_eq!(work_item_id, origin_item);
            assert_eq!(item.unwrap().id, origin_item);
        }
        other => panic!("unexpected origin: {other:?}"),
    }
    match get_item_context(&context, workspace_id, event_follow_up)
        .await
        .unwrap()
        .origin
        .unwrap()
    {
        OrgOriginView::Event { event_id, event } => {
            assert_eq!(event_id, created.event_ids[0]);
            assert_eq!(event.unwrap().id, created.event_ids[0]);
        }
        other => panic!("unexpected origin: {other:?}"),
    }
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        4
    );
}

#[tokio::test]
async fn context_orders_a_to_b_to_a_history_by_lineage_and_keeps_attempt_ownership() {
    let (context, backend, _dir, _db_path, workspace_a) = org_test_context(NOW).await;
    let workspace_b = WorkspaceId::from_str("11111111-1111-4111-8111-111111111122").unwrap();
    let session = backend.session().await.unwrap();
    session
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_b,
            slug: "lineage-b",
            display_name: "Lineage B",
            description: "Second workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &WorkspacePolicy::engineering_default(),
            now: NOW,
        })
        .await
        .unwrap();
    let document = document_id("61000000-0000-4000-8000-000000000020");
    let item = item_id("62000000-0000-4000-8000-000000000020");
    put_document(
        &context,
        &envelope(workspace_a, "seed-lineage-item"),
        &PutDocumentRequest {
            document_id: document,
            path: "lineage.org".into(),
            source: format!(
                "* READY Lineage\r\n:PROPERTIES:\r\n:ID: {item}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-a",
            workspace_id: workspace_a,
            work_item_id: item,
            attempt_number: 1,
            actor_id: "worker-a",
            status: OrgAttemptStatus::Failed,
            started_at: NOW,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    let late_context = OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(NOW + 100)));
    move_document(
        &late_context,
        &envelope(workspace_a, "move-lineage-a-b"),
        &MoveDocumentRequest {
            document_id: document,
            target_workspace_id: workspace_b,
            expected_document_revision: 1,
            expected_source_workspace_revision: 1,
            expected_target_workspace_revision: 1,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "attempt-b",
            workspace_id: workspace_b,
            work_item_id: item,
            attempt_number: 2,
            actor_id: "worker-b",
            status: OrgAttemptStatus::Completed,
            started_at: NOW + 101,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    let document_after_first = session.get_org_document(document).await.unwrap().unwrap();
    let workspace_b_current = session
        .get_org_workspace(workspace_b)
        .await
        .unwrap()
        .unwrap();
    let workspace_a_current = session
        .get_org_workspace(workspace_a)
        .await
        .unwrap()
        .unwrap();
    let early_context = OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(NOW - 100)));
    move_document(
        &early_context,
        &envelope(workspace_b, "move-lineage-b-a"),
        &MoveDocumentRequest {
            document_id: document,
            target_workspace_id: workspace_a,
            expected_document_revision: document_after_first.revision,
            expected_source_workspace_revision: workspace_b_current.revision,
            expected_target_workspace_revision: workspace_a_current.revision,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();

    let recovered = get_item_context(&context, workspace_a, item).await.unwrap();
    assert_eq!(
        recovered
            .history_segments
            .iter()
            .map(|segment| segment.workspace_id)
            .collect::<Vec<_>>(),
        vec![workspace_a, workspace_b, workspace_a]
    );
    assert_eq!(
        recovered
            .attempts
            .iter()
            .map(|attempt| attempt.workspace_id)
            .collect::<Vec<_>>(),
        vec![workspace_a, workspace_b]
    );
    let flattened = recovered
        .history_segments
        .iter()
        .flat_map(|segment| segment.events.iter())
        .collect::<Vec<_>>();
    assert!(flattened
        .windows(2)
        .any(|pair| pair[0].occurred_at > pair[1].occurred_at));
}

#[tokio::test]
async fn empty_relations_and_read_failures_are_mapped_without_partial_context() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("61000000-0000-4000-8000-000000000030");
    let item = item_id("62000000-0000-4000-8000-000000000030");
    put_document(
        &context,
        &envelope(workspace_id, "seed-empty-context"),
        &PutDocumentRequest {
            document_id: document,
            path: "empty-context.org".into(),
            source: format!(
                "* BACKLOG Empty\r\n:PROPERTIES:\r\n:ID: {item}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let empty = get_item_context(&context, workspace_id, item)
        .await
        .unwrap();
    assert!(empty.parent.is_none());
    assert!(empty.children.is_empty());
    assert!(empty.dependencies.is_empty());
    assert!(empty.note_links.is_empty());
    assert!(empty.attempts.is_empty());
    assert!(empty.origin.is_none());
    assert!(empty.lease.is_none());

    let transaction_events = event_log();
    let wrapped = Arc::new(EventStorageBackend::new(
        backend.clone(),
        transaction_events.clone(),
    ));
    wrapped.fail_next_repository_call("get_org_document");
    let failing_context = OrgContext::new(wrapped, Arc::new(FixedOrgClock::new(NOW)));
    assert_eq!(
        get_item_context(&failing_context, workspace_id, item)
            .await
            .unwrap_err()
            .code,
        OrgErrorCode::StorageFailure
    );
    assert_eq!(
        transaction_events.lock().unwrap().as_slice(),
        ["begin", "rollback"]
    );

    backend
        .session()
        .await
        .unwrap()
        .append_org_event(NewOrgEvent {
            id: "malformed-lineage",
            workspace_id,
            subject_kind: "work_item",
            subject_id: &item.to_string(),
            actor_id: "audit",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: NOW + 1,
            summary: "Malformed lineage",
            metadata: &serde_json::json!({"lineage_previous_event_id": 7}),
            previous_state: None,
            resulting_state: None,
        })
        .await
        .unwrap();
    let malformed = get_item_context(&context, workspace_id, item)
        .await
        .unwrap_err();
    assert_eq!(malformed.code, OrgErrorCode::StorageFailure);
    assert_eq!(malformed.details["storage_kind"], "corrupt");
}

#[tokio::test]
async fn event_pages_bind_opaque_cursor_and_probe_after_full_storage_page() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
    for index in 0..201 {
        session
            .append_org_event(NewOrgEvent {
                id: &format!("page-event-{index:03}"),
                workspace_id,
                subject_kind: "work_item",
                subject_id: "subject-1",
                actor_id: "audit",
                attempt_id: None,
                event_type: OrgEventType::Progress,
                occurred_at: NOW - index,
                summary: "page event",
                metadata: &serde_json::json!({}),
                previous_state: None,
                resulting_state: None,
            })
            .await
            .unwrap();
    }
    let request = OrgEventQuery {
        workspace_id,
        subject_kind: Some("work_item".into()),
        subject_id: Some("subject-1".into()),
        cursor: None,
        limit: Some(200),
    };
    let default_page = list_event_history(
        &context,
        &OrgEventQuery {
            limit: None,
            ..request.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(default_page.events.len(), 50);
    assert!(default_page.next_cursor.is_some());
    let unfiltered = list_event_history(
        &context,
        &OrgEventQuery {
            workspace_id,
            subject_kind: None,
            subject_id: None,
            cursor: None,
            limit: Some(1),
        },
    )
    .await
    .unwrap();
    assert_eq!(unfiltered.events.len(), 1);
    let first = list_event_history(&context, &request).await.unwrap();
    assert_eq!(first.events.len(), 200);
    assert!(first.next_cursor.is_some());
    assert!(first
        .events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(first
        .events
        .windows(2)
        .all(|pair| pair[0].occurred_at > pair[1].occurred_at));
    let second = list_event_history(
        &context,
        &OrgEventQuery {
            cursor: first.next_cursor.clone(),
            ..request.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(second.events.len(), 1);
    assert!(second.next_cursor.is_none());
    assert!(second.events[0].sequence > first.events[199].sequence);

    let cursor_bytes = URL_SAFE_NO_PAD
        .decode(first.next_cursor.as_ref().unwrap())
        .unwrap();
    let mut wrong_version: serde_json::Value = serde_json::from_slice(&cursor_bytes).unwrap();
    wrong_version["version"] = serde_json::json!(2);
    let wrong_version = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wrong_version).unwrap());

    for invalid in [
        OrgEventQuery {
            cursor: Some("malformed".into()),
            ..request.clone()
        },
        OrgEventQuery {
            workspace_id: WorkspaceId::from_str("11111111-1111-4111-8111-111111111112").unwrap(),
            cursor: first.next_cursor.clone(),
            ..request.clone()
        },
        OrgEventQuery {
            limit: Some(201),
            ..request.clone()
        },
        OrgEventQuery {
            limit: Some(0),
            ..request.clone()
        },
        OrgEventQuery {
            subject_id: Some("other-subject".into()),
            cursor: first.next_cursor.clone(),
            ..request.clone()
        },
        OrgEventQuery {
            subject_id: None,
            cursor: None,
            ..request.clone()
        },
        OrgEventQuery {
            cursor: Some(wrong_version.clone()),
            ..request.clone()
        },
    ] {
        assert_eq!(
            list_event_history(&context, &invalid)
                .await
                .unwrap_err()
                .code,
            OrgErrorCode::InvalidInput
        );
    }
}
