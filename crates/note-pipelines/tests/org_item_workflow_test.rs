mod support;

use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    add_dependency, assign_item, create_follow_up, create_item, link_note, move_item, put_document,
    remove_dependency, reparent_item, schedule_item, unlink_note, validate_item_transition,
    AssignItemRequest, CommandEnvelope, CreateFollowUpRequest, CreateItemRequest,
    DependencyRequest, FixedOrgClock, FollowUpOrigin, MoveItemRequest, NoteLinkRequest, OrgContext,
    OrgErrorCode, OrgFieldPatch, PutDocumentRequest, ReparentItemRequest, ScheduleItemRequest,
    UnlinkNoteRequest,
};
use note_storage::{
    NewNote, NewOrgWorkspace, OrgDocument, OrgEvent, OrgEventType, OrgProjectedWorkItem,
    OrgWorkspace, OrgWorkspaceUpdate, StorageBackend,
};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{event_log, org_test_context, EventStorageBackend};

const NOW: i64 = 1_810_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemWorkspaceSnapshot {
    workspace: OrgWorkspace,
    documents: Vec<OrgDocument>,
    projection: Vec<OrgProjectedWorkItem>,
    events: Vec<OrgEvent>,
}

async fn item_workspace_snapshot(
    backend: &dyn StorageBackend,
    workspace_id: WorkspaceId,
) -> ItemWorkspaceSnapshot {
    let session = backend.session().await.unwrap();
    ItemWorkspaceSnapshot {
        workspace: session
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap(),
        documents: session.list_org_documents(workspace_id).await.unwrap(),
        projection: session
            .list_org_workspace_projection(workspace_id)
            .await
            .unwrap(),
        events: session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
    }
}

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
        actor_id: "agent-task6".into(),
        operation_id: operation_id.into(),
    }
}

fn expected(document_id: DocumentId, revision: i64) -> BTreeMap<DocumentId, i64> {
    BTreeMap::from([(document_id, revision)])
}

fn source(id: WorkItemId, state: &str, title: &str, extra: &str) -> String {
    format!(
        "* {state} {title}\r\n:PROPERTIES:\r\n:ID: {id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n{extra}"
    )
}

async fn seed_document(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    operation_id: &str,
    path: &str,
    source: String,
) {
    put_document(
        context,
        &envelope(workspace_id, operation_id),
        &PutDocumentRequest {
            document_id,
            path: path.into(),
            source,
            expected_revision: None,
        },
    )
    .await
    .unwrap();
}

fn create_request(
    document_id: DocumentId,
    item_id: WorkItemId,
    expected_revision: i64,
) -> CreateItemRequest {
    CreateItemRequest {
        document_id,
        parent_id: None,
        item_id,
        item_type: WorkItemType::Task,
        title: "Implement workflow".into(),
        initial_state: None,
        priority: Some('A'),
        tags: BTreeSet::from(["rust".into()]),
        assignee: None,
        requires_review: false,
        expected_revisions: expected(document_id, expected_revision),
    }
}

#[tokio::test]
async fn creates_default_and_alternate_initial_work_and_preserves_follow_up_origin() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("51000000-0000-4000-8000-000000000001");
    seed_document(
        &context,
        workspace_id,
        document,
        "seed-create-document",
        "create.org",
        "#+TITLE: Work\r\n".into(),
    )
    .await;
    let first = item_id("52000000-0000-4000-8000-000000000001");
    let mut first_request = create_request(document, first, 1);
    first_request.assignee = Some("agent-initial".into());
    let created = create_item(
        &context,
        &envelope(workspace_id, "create-default-item"),
        &first_request,
    )
    .await
    .unwrap();
    assert_eq!(created.document_revisions[&document.to_string()], 2);
    assert_eq!(created.event_ids.len(), 2);
    assert_eq!(created.data["resulting_items"][0]["id"], first.to_string());
    assert_eq!(
        created.data["resulting_items"][0]["assignee"],
        "agent-initial"
    );

    let second = item_id("52000000-0000-4000-8000-000000000002");
    let mut alternate = create_request(document, second, 2);
    alternate.title = "Ready immediately".into();
    alternate.initial_state = Some("READY".into());
    alternate.item_type = WorkItemType::Review;
    alternate.tags = BTreeSet::from(["review".into(), "urgent".into()]);
    create_item(
        &context,
        &envelope(workspace_id, "create-alternate-item"),
        &alternate,
    )
    .await
    .unwrap();

    let follow_up = item_id("52000000-0000-4000-8000-000000000003");
    let mut follow_up_item = create_request(document, follow_up, 3);
    follow_up_item.title = "Review discovered edge".into();
    let result = create_follow_up(
        &context,
        &envelope(workspace_id, "create-follow-up"),
        &CreateFollowUpRequest {
            item: follow_up_item,
            origin: FollowUpOrigin::WorkItem(first),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let source = session
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap()
        .source;
    assert!(source.contains(&format!("* BACKLOG [#A] Implement workflow :rust:\r\n")));
    assert!(source.contains("* READY [#A] Ready immediately :review:urgent:\r\n"));
    let projection = session
        .list_org_document_projection(document)
        .await
        .unwrap();
    assert_eq!(
        projection
            .iter()
            .find(|item| item.id == first)
            .unwrap()
            .state
            .as_deref(),
        Some("BACKLOG")
    );
    assert_eq!(
        projection
            .iter()
            .find(|item| item.id == second)
            .unwrap()
            .state
            .as_deref(),
        Some("READY")
    );
    let second_projection = projection.iter().find(|item| item.id == second).unwrap();
    assert_eq!(second_projection.item_type, WorkItemType::Review);
    assert_eq!(second_projection.tags, vec!["review", "urgent"]);
    let events = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|event| result.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(event.event_type, OrgEventType::Creation);
    assert_eq!(event.subject_id, follow_up.to_string());
    assert_eq!(event.metadata["origin"]["work_item_id"], first.to_string());
    assert_eq!(event.metadata["schema_version"], 1);
    assert_eq!(event.metadata["item_type"], "task");
    assert_eq!(event.metadata["title"], "Review discovered edge");
    assert_eq!(event.metadata["initial_state"], "BACKLOG");
    assert_eq!(event.metadata["priority"], "A");
    assert_eq!(event.metadata["tags"], serde_json::json!(["rust"]));
    assert_eq!(event.metadata["requires_review"], false);

    let first_events = session
        .list_org_subject_events(workspace_id, "work_item", &first.to_string(), None, 100)
        .await
        .unwrap();
    assert_eq!(first_events[0].event_type, OrgEventType::Creation);
    assert_eq!(first_events[1].event_type, OrgEventType::Assignment);
    assert_eq!(
        first_events[1].metadata["previous_assignee"],
        serde_json::Value::Null
    );
    assert_eq!(
        first_events[1].metadata["resulting_assignee"],
        "agent-initial"
    );

    let event_follow_up = item_id("52000000-0000-4000-8000-000000000005");
    let event_origin = events
        .iter()
        .find(|event| event.subject_id == first.to_string())
        .unwrap()
        .id
        .clone();
    let mut event_request = create_request(document, event_follow_up, 4);
    event_request.title = "Follow event".into();
    let event_result = create_follow_up(
        &context,
        &envelope(workspace_id, "create-event-follow-up"),
        &CreateFollowUpRequest {
            item: event_request,
            origin: FollowUpOrigin::Event(event_origin.clone()),
        },
    )
    .await
    .unwrap();
    let event_follow_up_audit = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event_result.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(event_follow_up_audit.metadata["origin"]["kind"], "event");
    assert_eq!(
        event_follow_up_audit.metadata["origin"]["event_id"],
        event_origin
    );

    let self_id = item_id("52000000-0000-4000-8000-000000000006");
    assert_eq!(
        create_follow_up(
            &context,
            &envelope(workspace_id, "self-origin"),
            &CreateFollowUpRequest {
                item: create_request(document, self_id, 5),
                origin: FollowUpOrigin::WorkItem(self_id),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    assert_eq!(
        create_follow_up(
            &context,
            &envelope(workspace_id, "missing-event-origin"),
            &CreateFollowUpRequest {
                item: create_request(document, item_id("52000000-0000-4000-8000-000000000007"), 5,),
                origin: FollowUpOrigin::Event("missing-event".into()),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::NotFound
    );

    for (index, state) in ["RUNNING", "UNKNOWN"].into_iter().enumerate() {
        let id = if index == 0 {
            "52000000-0000-4000-8000-000000000004"
        } else {
            "52000000-0000-4000-8000-000000000008"
        };
        let mut invalid = create_request(document, item_id(id), 5);
        invalid.initial_state = Some(state.into());
        assert_eq!(
            create_item(
                &context,
                &envelope(workspace_id, &format!("invalid-initial-{index}")),
                &invalid
            )
            .await
            .unwrap_err()
            .code,
            OrgErrorCode::InvalidInput
        );
    }
}

#[tokio::test]
async fn creation_rejects_running_or_review_even_when_custom_policy_selects_it_as_initial() {
    for (index, (state, explicit)) in [("RUNNING", false), ("REVIEW", true)]
        .into_iter()
        .enumerate()
    {
        let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
        let session = backend.session().await.unwrap();
        let workspace = session
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap();
        let mut policy = workspace.policy.clone();
        policy.initial_state = state.into();
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: workspace_id,
                expected_revision: workspace.revision,
                slug: &workspace.slug,
                display_name: &workspace.display_name,
                description: &workspace.description,
                timezone: &workspace.timezone,
                policy_schema_version: workspace.policy_schema_version,
                policy: &policy,
                archived_at: workspace.archived_at,
                updated_at: NOW,
            })
            .await
            .unwrap();
        let document = document_id(if index == 0 {
            "51000000-0000-4000-8000-000000000008"
        } else {
            "51000000-0000-4000-8000-000000000009"
        });
        seed_document(
            &context,
            workspace_id,
            document,
            &format!("seed-active-initial-{index}"),
            &format!("active-initial-{index}.org"),
            "Canonical prelude.\r\n".into(),
        )
        .await;
        let baseline = item_workspace_snapshot(backend.as_ref(), workspace_id).await;
        let operation_id = format!("reject-active-initial-{index}");
        let mut request = create_request(
            document,
            item_id(if index == 0 {
                "52000000-0000-4000-8000-000000000008"
            } else {
                "52000000-0000-4000-8000-000000000009"
            }),
            1,
        );
        request.initial_state = explicit.then(|| state.to_string());

        let error = create_item(&context, &envelope(workspace_id, &operation_id), &request)
            .await
            .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput, "{state}");
        assert_eq!(
            item_workspace_snapshot(backend.as_ref(), workspace_id).await,
            baseline,
            "{state}"
        );
        assert!(session
            .get_org_operation(workspace_id, &operation_id)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn moves_between_documents_and_reparents_with_complete_revision_maps() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let source_document = document_id("51000000-0000-4000-8000-000000000010");
    let target_document = document_id("51000000-0000-4000-8000-000000000011");
    let moved = item_id("52000000-0000-4000-8000-000000000010");
    let child = item_id("52000000-0000-4000-8000-000000000012");
    let parent = item_id("52000000-0000-4000-8000-000000000011");
    seed_document(
        &context,
        workspace_id,
        source_document,
        "seed-move-source",
        "source.org",
        format!(
            "Source opaque.\r\n{}** BACKLOG Child\r\n:PROPERTIES:\r\n:ID: {child}\r\n:AGENT_NOTE_TYPE: subtask\r\n:END:\r\n",
            source(moved, "BACKLOG", "Move me", "")
        ),
    )
    .await;
    seed_document(
        &context,
        workspace_id,
        target_document,
        "seed-move-target",
        "target.org",
        source(parent, "BACKLOG", "Parent", "Target opaque.\r\n"),
    )
    .await;

    let moved_result = move_item(
        &context,
        &envelope(workspace_id, "cross-document-move"),
        &MoveItemRequest {
            item_id: moved,
            source_document_id: source_document,
            target_document_id: target_document,
            target_parent_id: Some(parent),
            expected_revisions: BTreeMap::from([(source_document, 1), (target_document, 1)]),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        moved_result.document_revisions[&source_document.to_string()],
        2
    );
    assert_eq!(
        moved_result.document_revisions[&target_document.to_string()],
        2
    );
    let session = backend.session().await.unwrap();
    let moved_projection = session.get_org_work_item(moved).await.unwrap().unwrap();
    assert_eq!(moved_projection.document_id, target_document);
    assert_eq!(moved_projection.parent_id, Some(parent));
    let moved_child = session.get_org_work_item(child).await.unwrap().unwrap();
    assert_eq!(moved_child.document_id, target_document);
    assert_eq!(moved_child.parent_id, Some(moved));
    assert_eq!(moved_child.created_at, NOW);
    assert_eq!(
        moved_result.data["operation"]["source_document_id"],
        source_document.to_string()
    );
    assert_eq!(
        moved_result.data["resulting_items"][0]["document_id"],
        target_document.to_string()
    );
    assert!(session
        .get_org_document(source_document)
        .await
        .unwrap()
        .unwrap()
        .source
        .contains("Source opaque."));

    reparent_item(
        &context,
        &envelope(workspace_id, "same-document-reparent"),
        &ReparentItemRequest {
            item_id: moved,
            document_id: target_document,
            target_parent_id: None,
            expected_revisions: expected(target_document, 2),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        session
            .get_org_work_item(moved)
            .await
            .unwrap()
            .unwrap()
            .parent_id,
        None
    );

    for (operation, target_parent) in [
        ("self-descendant-reparent", Some(child)),
        ("reparent-noop", None),
    ] {
        let error = reparent_item(
            &context,
            &envelope(workspace_id, operation),
            &ReparentItemRequest {
                item_id: moved,
                document_id: target_document,
                target_parent_id: target_parent,
                expected_revisions: expected(target_document, 3),
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.code,
                OrgErrorCode::InvalidInput | OrgErrorCode::UnsupportedSemanticEdit
            ),
            "{operation}: {error:?}"
        );
    }

    assert_eq!(
        reparent_item(
            &context,
            &envelope(workspace_id, "stale-reparent"),
            &ReparentItemRequest {
                item_id: moved,
                document_id: target_document,
                target_parent_id: Some(parent),
                expected_revisions: expected(target_document, 2),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleRevision
    );

    let incomplete = move_item(
        &context,
        &envelope(workspace_id, "incomplete-move-revisions"),
        &MoveItemRequest {
            item_id: moved,
            source_document_id: target_document,
            target_document_id: source_document,
            target_parent_id: None,
            expected_revisions: expected(target_document, 3),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(incomplete.code, OrgErrorCode::InvalidInput);

    for (operation, expected_revisions) in [
        (
            "extra-reparent-revision",
            BTreeMap::from([(target_document, 3), (source_document, 2)]),
        ),
        (
            "nonpositive-reparent-revision",
            BTreeMap::from([(target_document, 0)]),
        ),
    ] {
        assert_eq!(
            reparent_item(
                &context,
                &envelope(workspace_id, operation),
                &ReparentItemRequest {
                    item_id: moved,
                    document_id: target_document,
                    target_parent_id: Some(parent),
                    expected_revisions,
                },
            )
            .await
            .unwrap_err()
            .code,
            OrgErrorCode::InvalidInput
        );
    }
}

#[tokio::test]
async fn assignment_and_schedule_mutations_are_minimal_and_audited() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("51000000-0000-4000-8000-000000000020");
    let item = item_id("52000000-0000-4000-8000-000000000020");
    seed_document(
        &context,
        workspace_id,
        document,
        "seed-assignment",
        "assignment.org",
        source(
            item,
            "READY",
            "Assign me",
            ":CUSTOM:\r\nkeep bytes\r\n:END:\r\n",
        ),
    )
    .await;

    for (index, assignee) in [Some("agent-a"), Some("agent-b"), None]
        .into_iter()
        .enumerate()
    {
        assign_item(
            &context,
            &envelope(workspace_id, &format!("assignment-{index}")),
            &AssignItemRequest {
                item_id: item,
                document_id: document,
                assignee: assignee.map(str::to_string),
                expected_revisions: expected(document, i64::try_from(index).unwrap() + 1),
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        assign_item(
            &context,
            &envelope(workspace_id, "assignment-noop"),
            &AssignItemRequest {
                item_id: item,
                document_id: document,
                assignee: None,
                expected_revisions: expected(document, 4),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    let mut invalid_assignee = AssignItemRequest {
        item_id: item,
        document_id: document,
        assignee: Some(" agent-invalid".into()),
        expected_revisions: expected(document, 4),
    };
    assert_eq!(
        assign_item(
            &context,
            &envelope(workspace_id, "assignment-invalid"),
            &invalid_assignee,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    invalid_assignee.assignee = Some("agent\ninvalid".into());
    assert_eq!(
        assign_item(
            &context,
            &envelope(workspace_id, "assignment-multiline"),
            &invalid_assignee,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    let scheduled_result = schedule_item(
        &context,
        &envelope(workspace_id, "schedule-and-deadline"),
        &ScheduleItemRequest {
            item_id: item,
            document_id: document,
            scheduled: OrgFieldPatch::Set("<2027-05-15 Sat 09:30>".into()),
            deadline: OrgFieldPatch::Set("<2027-05-16 Sun 18:00>".into()),
            expected_revisions: expected(document, 4),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let stored = session.get_org_document(document).await.unwrap().unwrap();
    assert!(stored
        .source
        .contains(":CUSTOM:\r\nkeep bytes\r\n:END:\r\n"));
    let item = session.get_org_work_item(item).await.unwrap().unwrap();
    assert_eq!(item.assignee, None);
    assert_eq!(
        item.scheduled.as_ref().unwrap().raw,
        "<2027-05-15 Sat 09:30>"
    );
    assert_eq!(
        item.deadline.as_ref().unwrap().raw,
        "<2027-05-16 Sun 18:00>"
    );
    let schedule_events = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| scheduled_result.event_ids.contains(&event.id))
        .collect::<Vec<_>>();
    assert_eq!(schedule_events.len(), 2);
    assert_eq!(schedule_events[0].event_type, OrgEventType::ScheduleChange);
    assert_eq!(schedule_events[1].event_type, OrgEventType::DeadlineChange);
    assert_eq!(
        schedule_events[0].metadata["resulting_scheduled"],
        "<2027-05-15 Sat 09:30>"
    );
    assert_eq!(
        schedule_events[1].metadata["resulting_deadline"],
        "<2027-05-16 Sun 18:00>"
    );
    schedule_item(
        &context,
        &envelope(workspace_id, "clear-deadline-only"),
        &ScheduleItemRequest {
            item_id: item.id,
            document_id: document,
            scheduled: OrgFieldPatch::Unchanged,
            deadline: OrgFieldPatch::Clear,
            expected_revisions: expected(document, 5),
        },
    )
    .await
    .unwrap();
    let cleared = session.get_org_work_item(item.id).await.unwrap().unwrap();
    assert_eq!(
        cleared.scheduled.as_ref().unwrap().raw,
        "<2027-05-15 Sat 09:30>"
    );
    assert!(cleared.deadline.is_none());
    assert_eq!(
        schedule_item(
            &context,
            &envelope(workspace_id, "schedule-noop"),
            &ScheduleItemRequest {
                item_id: item.id,
                document_id: document,
                scheduled: OrgFieldPatch::Unchanged,
                deadline: OrgFieldPatch::Unchanged,
                expected_revisions: expected(document, 6),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    let assignments = session
        .list_org_subject_events(workspace_id, "work_item", &item.id.to_string(), None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == OrgEventType::Assignment)
        .collect::<Vec<_>>();
    assert_eq!(assignments.len(), 3);
    assert_eq!(
        assignments[0].metadata["previous_assignee"],
        serde_json::Value::Null
    );
    assert_eq!(assignments[0].metadata["resulting_assignee"], "agent-a");
    assert_eq!(assignments[2].metadata["previous_assignee"], "agent-b");
    assert_eq!(
        assignments[2].metadata["resulting_assignee"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn active_descendants_block_moves_and_assignments_but_not_schedule_edits() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let source_document = document_id("51000000-0000-4000-8000-000000000025");
    let target_document = document_id("51000000-0000-4000-8000-000000000026");
    let parent = item_id("52000000-0000-4000-8000-000000000025");
    let child = item_id("52000000-0000-4000-8000-000000000026");
    seed_document(
        &context,
        workspace_id,
        source_document,
        "seed-active-subtree",
        "active.org",
        format!(
            "{}** RUNNING Active child\r\n:PROPERTIES:\r\n:ID: {child}\r\n:AGENT_NOTE_TYPE: subtask\r\n:END:\r\n",
            source(parent, "BACKLOG", "Parent", "")
        ),
    )
    .await;
    seed_document(
        &context,
        workspace_id,
        target_document,
        "seed-active-target",
        "active-target.org",
        "Target opaque.\r\n".into(),
    )
    .await;
    let baseline = item_workspace_snapshot(backend.as_ref(), workspace_id).await;

    assert_eq!(
        move_item(
            &context,
            &envelope(workspace_id, "move-active-subtree"),
            &MoveItemRequest {
                item_id: parent,
                source_document_id: source_document,
                target_document_id: target_document,
                target_parent_id: None,
                expected_revisions: BTreeMap::from([(source_document, 1), (target_document, 1),]),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidTransition
    );
    assert_eq!(
        assign_item(
            &context,
            &envelope(workspace_id, "assign-active-child"),
            &AssignItemRequest {
                item_id: child,
                document_id: source_document,
                assignee: Some("agent-active".into()),
                expected_revisions: expected(source_document, 1),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidTransition
    );
    assert_eq!(
        item_workspace_snapshot(backend.as_ref(), workspace_id).await,
        baseline
    );

    schedule_item(
        &context,
        &envelope(workspace_id, "schedule-active-child"),
        &ScheduleItemRequest {
            item_id: child,
            document_id: source_document,
            scheduled: OrgFieldPatch::Set("<2027-06-01 Tue 10:00>".into()),
            deadline: OrgFieldPatch::Unchanged,
            expected_revisions: expected(source_document, 1),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_org_work_item(child)
            .await
            .unwrap()
            .unwrap()
            .scheduled
            .unwrap()
            .raw,
        "<2027-06-01 Tue 10:00>"
    );
}

#[tokio::test]
async fn schedule_rejects_ambiguous_and_nonexistent_workspace_local_times_atomically() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let session = backend.session().await.unwrap();
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
            timezone: "America/New_York",
            policy_schema_version: workspace.policy_schema_version,
            policy: &workspace.policy,
            archived_at: workspace.archived_at,
            updated_at: NOW,
        })
        .await
        .unwrap();
    let document = document_id("51000000-0000-4000-8000-000000000027");
    let item = item_id("52000000-0000-4000-8000-000000000027");
    seed_document(
        &context,
        workspace_id,
        document,
        "seed-dst-schedule",
        "dst-schedule.org",
        source(item, "READY", "DST", ""),
    )
    .await;
    let baseline = item_workspace_snapshot(backend.as_ref(), workspace_id).await;
    for (operation, timestamp) in [
        ("schedule-ambiguous-dst", "<2026-11-01 Sun 01:30>"),
        ("schedule-nonexistent-dst", "<2026-03-08 Sun 02:30>"),
    ] {
        let error = schedule_item(
            &context,
            &envelope(workspace_id, operation),
            &ScheduleItemRequest {
                item_id: item,
                document_id: document,
                scheduled: OrgFieldPatch::Set(timestamp.into()),
                deadline: OrgFieldPatch::Unchanged,
                expected_revisions: expected(document, 1),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
        assert_eq!(
            item_workspace_snapshot(backend.as_ref(), workspace_id).await,
            baseline
        );
        assert!(session
            .get_org_operation(workspace_id, operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn dependencies_reject_invalid_graphs_and_transition_validation_uses_successful_prerequisites(
) {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("51000000-0000-4000-8000-000000000030");
    let prerequisite = item_id("52000000-0000-4000-8000-000000000030");
    let dependent = item_id("52000000-0000-4000-8000-000000000031");
    seed_document(
        &context,
        workspace_id,
        document,
        "seed-dependencies",
        "dependencies.org",
        format!(
            "{}{}",
            source(prerequisite, "DONE", "Prerequisite", ""),
            source(dependent, "BACKLOG", "Dependent", "")
        ),
    )
    .await;
    let added = add_dependency(
        &context,
        &envelope(workspace_id, "add-valid-dependency"),
        &DependencyRequest {
            item_id: dependent,
            dependency_id: prerequisite,
            document_id: document,
            expected_revisions: expected(document, 1),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let items = session
        .list_org_workspace_projection(workspace_id)
        .await
        .unwrap();
    let dependent_item = items.iter().find(|item| item.id == dependent).unwrap();
    validate_item_transition(
        &WorkspacePolicy::engineering_default(),
        dependent_item,
        &items,
        "READY",
        false,
    )
    .unwrap();

    let dependency_event = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| added.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(dependency_event.event_type, OrgEventType::DependencyChange);
    assert_eq!(
        dependency_event.metadata["previous_dependencies"],
        serde_json::json!([])
    );
    assert_eq!(
        dependency_event.metadata["resulting_dependencies"],
        serde_json::json!([prerequisite.to_string()])
    );

    let cycle = add_dependency(
        &context,
        &envelope(workspace_id, "dependency-cycle"),
        &DependencyRequest {
            item_id: prerequisite,
            dependency_id: dependent,
            document_id: document,
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(cycle.code, OrgErrorCode::InvalidInput);
    let missing = add_dependency(
        &context,
        &envelope(workspace_id, "missing-dependency-target"),
        &DependencyRequest {
            item_id: dependent,
            dependency_id: item_id("52000000-0000-4000-8000-000000000039"),
            document_id: document,
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::InvalidInput);

    for prerequisite_state in ["CANCELLED", "FAILED", "RUNNING", "REVIEW", "UNKNOWN"] {
        let mut unsatisfied_items = items.clone();
        unsatisfied_items
            .iter_mut()
            .find(|item| item.id == prerequisite)
            .unwrap()
            .state = Some(prerequisite_state.into());
        let dependent_unsatisfied = unsatisfied_items
            .iter()
            .find(|item| item.id == dependent)
            .unwrap();
        assert_eq!(
            validate_item_transition(
                &WorkspacePolicy::engineering_default(),
                dependent_unsatisfied,
                &unsatisfied_items,
                "READY",
                false,
            )
            .unwrap_err()
            .code,
            OrgErrorCode::DependencyBlocked,
            "{prerequisite_state} must not satisfy a dependency"
        );
    }

    let self_error = add_dependency(
        &context,
        &envelope(workspace_id, "self-dependency"),
        &DependencyRequest {
            item_id: dependent,
            dependency_id: dependent,
            document_id: document,
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(self_error.code, OrgErrorCode::InvalidInput);

    let mut review_item = dependent_item.clone();
    review_item.state = Some("RUNNING".into());
    review_item.requires_review = true;
    let review_error = validate_item_transition(
        &WorkspacePolicy::engineering_default(),
        &review_item,
        &items,
        "DONE",
        false,
    )
    .unwrap_err();
    assert_eq!(review_error.code, OrgErrorCode::ReviewRequired);

    let removed = remove_dependency(
        &context,
        &envelope(workspace_id, "remove-dependency"),
        &DependencyRequest {
            item_id: dependent,
            dependency_id: prerequisite,
            document_id: document,
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap();
    let removal_event = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| removed.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(
        removal_event.metadata["previous_dependencies"],
        serde_json::json!([prerequisite.to_string()])
    );
    assert_eq!(
        removal_event.metadata["resulting_dependencies"],
        serde_json::json!([])
    );
    assert_eq!(
        remove_dependency(
            &context,
            &envelope(workspace_id, "remove-missing-dependency"),
            &DependencyRequest {
                item_id: dependent,
                dependency_id: prerequisite,
                document_id: document,
                expected_revisions: expected(document, 3),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn cross_workspace_origins_dependencies_and_document_ownership_are_rejected() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let other_workspace = WorkspaceId::from_str("11111111-1111-4111-8111-111111111112").unwrap();
    backend
        .session()
        .await
        .unwrap()
        .insert_org_workspace(NewOrgWorkspace {
            id: other_workspace,
            slug: "other",
            display_name: "Other",
            description: "Other workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &WorkspacePolicy::engineering_default(),
            now: NOW,
        })
        .await
        .unwrap();
    let local_document = document_id("51000000-0000-4000-8000-000000000035");
    let foreign_document = document_id("51000000-0000-4000-8000-000000000036");
    let local_item = item_id("52000000-0000-4000-8000-000000000035");
    let foreign_item = item_id("52000000-0000-4000-8000-000000000036");
    seed_document(
        &context,
        workspace_id,
        local_document,
        "seed-local-work-item",
        "local.org",
        source(local_item, "BACKLOG", "Local", ""),
    )
    .await;
    seed_document(
        &context,
        other_workspace,
        foreign_document,
        "seed-foreign-work-item",
        "foreign.org",
        source(foreign_item, "DONE", "Foreign", ""),
    )
    .await;

    assert_eq!(
        add_dependency(
            &context,
            &envelope(workspace_id, "cross-workspace-dependency"),
            &DependencyRequest {
                item_id: local_item,
                dependency_id: foreign_item,
                document_id: local_document,
                expected_revisions: expected(local_document, 1),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput
    );
    let follow_up_id = item_id("52000000-0000-4000-8000-000000000037");
    assert_eq!(
        create_follow_up(
            &context,
            &envelope(workspace_id, "cross-workspace-origin"),
            &CreateFollowUpRequest {
                item: create_request(local_document, follow_up_id, 1),
                origin: FollowUpOrigin::WorkItem(foreign_item),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::NotFound
    );
    assert_eq!(
        assign_item(
            &context,
            &envelope(workspace_id, "foreign-document-ownership"),
            &AssignItemRequest {
                item_id: foreign_item,
                document_id: foreign_document,
                assignee: Some("wrong-workspace".into()),
                expected_revisions: expected(foreign_document, 1),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::NotFound
    );
}

#[tokio::test]
async fn note_links_require_active_targets_but_remain_weak_after_deletion() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("51000000-0000-4000-8000-000000000040");
    let item = item_id("52000000-0000-4000-8000-000000000040");
    seed_document(
        &context,
        workspace_id,
        document,
        "seed-links",
        "links.org",
        source(item, "BACKLOG", "Link notes", ""),
    )
    .await;
    let note_id = "53000000-0000-4000-8000-000000000040";
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Context",
            content: "context",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let linked_result = link_note(
        &context,
        &envelope(workspace_id, "link-active-note"),
        &NoteLinkRequest {
            item_id: item,
            document_id: document,
            purpose: "design".into(),
            note_id: note_id.parse().unwrap(),
            description: "Design context".into(),
            expected_revisions: expected(document, 1),
        },
    )
    .await
    .unwrap();
    let replay = link_note(
        &context,
        &envelope(workspace_id, "link-active-note"),
        &NoteLinkRequest {
            item_id: item,
            document_id: document,
            purpose: "design".into(),
            note_id: note_id.parse().unwrap(),
            description: "Design context".into(),
            expected_revisions: expected(document, 1),
        },
    )
    .await
    .unwrap();
    assert_eq!(replay, linked_result);
    assert!(matches!(
        link_note(
            &context,
            &envelope(workspace_id, "duplicate-link"),
            &NoteLinkRequest {
                item_id: item,
                document_id: document,
                purpose: "design".into(),
                note_id: note_id.parse().unwrap(),
                description: "Design context".into(),
                expected_revisions: expected(document, 2),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::InvalidInput | OrgErrorCode::UnsupportedSemanticEdit
    ));
    backend
        .session()
        .await
        .unwrap()
        .soft_delete_note(note_id, NOW + 1)
        .await
        .unwrap();
    let linked = backend
        .session()
        .await
        .unwrap()
        .get_org_work_item(item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(linked.note_links.len(), 1);

    let deleted_target = link_note(
        &context,
        &envelope(workspace_id, "link-deleted-note"),
        &NoteLinkRequest {
            item_id: item,
            document_id: document,
            purpose: "deleted".into(),
            note_id: note_id.parse().unwrap(),
            description: "Deleted target".into(),
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(deleted_target.code, OrgErrorCode::NoteUnavailable);

    let missing = link_note(
        &context,
        &envelope(workspace_id, "link-missing-note"),
        &NoteLinkRequest {
            item_id: item,
            document_id: document,
            purpose: "report".into(),
            note_id: "53000000-0000-4000-8000-000000000041".parse().unwrap(),
            description: "Missing".into(),
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::NoteUnavailable);
    let unlinked = unlink_note(
        &context,
        &envelope(workspace_id, "unlink-deleted-note"),
        &UnlinkNoteRequest {
            item_id: item,
            document_id: document,
            purpose: "design".into(),
            note_id: note_id.parse().unwrap(),
            expected_revisions: expected(document, 2),
        },
    )
    .await
    .unwrap();
    let unlink_event = backend
        .session()
        .await
        .unwrap()
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| unlinked.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(unlink_event.event_type, OrgEventType::NoteLinkChange);
    assert_eq!(unlink_event.metadata["action"], "unlink");
}

#[tokio::test]
async fn repository_and_commit_failures_rollback_item_source_projection_events_and_operation() {
    let (seed_context, backend, _dir, _db_path, workspace_id) = org_test_context(NOW).await;
    let document = document_id("51000000-0000-4000-8000-000000000050");
    seed_document(
        &seed_context,
        workspace_id,
        document,
        "seed-item-failpoints",
        "item-failpoints.org",
        "Opaque baseline.\r\n".into(),
    )
    .await;
    let wrapped = Arc::new(EventStorageBackend::new(backend.clone(), event_log()));
    let context = OrgContext::new(wrapped.clone(), Arc::new(FixedOrgClock::new(NOW)));
    let baseline = item_workspace_snapshot(backend.as_ref(), workspace_id).await;

    for (index, failpoint) in [
        "compare_and_swap_org_document",
        "rebuild_org_workspace_projection",
        "append_org_event",
        "insert_org_operation",
    ]
    .into_iter()
    .enumerate()
    {
        let operation_id = format!("item-repository-failure-{index}");
        wrapped.fail_next_repository_call(failpoint);
        let error = create_item(
            &context,
            &envelope(workspace_id, &operation_id),
            &create_request(document, item_id("52000000-0000-4000-8000-000000000050"), 1),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure, "{failpoint}");
        assert_eq!(
            item_workspace_snapshot(backend.as_ref(), workspace_id).await,
            baseline,
            "{failpoint}"
        );
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id, &operation_id)
            .await
            .unwrap()
            .is_none());
    }

    wrapped.fail_next_commit();
    let commit_operation = "item-commit-failure";
    let error = create_item(
        &context,
        &envelope(workspace_id, commit_operation),
        &create_request(document, item_id("52000000-0000-4000-8000-000000000051"), 1),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StorageFailure);
    assert_eq!(
        item_workspace_snapshot(backend.as_ref(), workspace_id).await,
        baseline
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id, commit_operation)
        .await
        .unwrap()
        .is_none());
}
