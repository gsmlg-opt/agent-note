#[path = "../../tests/support/mod.rs"]
mod support;

use super::{
    execute_idempotent, execute_idempotent_create, execute_idempotent_outcome, resolve_cas,
    CommandEnvelope, FixedOrgClock, OrgCommandKind, OrgCommandResult, OrgContext, OrgError,
    OrgErrorCode, OrgMutationOutcome,
};
use note_org::WorkspacePolicy;
use note_storage::{
    CompareAndSwap, NewOrgEvent, NewOrgWorkspace, OrgEventType, OrgWorkspaceUpdate, StorageBackend,
};
use note_storage_turso::TursoStorage;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr as _;
use std::sync::Arc;
use support::org_test_context;

const TEST_COMMAND: OrgCommandKind = OrgCommandKind::new("test_mutation", 1);
const FIRST_COMMAND: OrgCommandKind = OrgCommandKind::new("first_test_mutation", 1);
const SECOND_COMMAND: OrgCommandKind = OrgCommandKind::new("second_test_mutation", 1);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestRequest {
    value: String,
    nested: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
struct FirstCommandRequest {
    value: String,
}

#[derive(Debug, Clone, Serialize)]
struct SecondCommandRequest {
    value: String,
}

fn envelope(
    workspace_id: note_org::WorkspaceId,
    actor_id: &str,
    operation_id: &str,
) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id,
        actor_id: actor_id.into(),
        operation_id: operation_id.into(),
    }
}

fn request(value: &str) -> TestRequest {
    TestRequest {
        value: value.into(),
        nested: json!({"z": 1, "a": {"second": 2, "first": 1}}),
    }
}

#[tokio::test]
async fn validates_actor_and_operation_identifiers_before_opening_a_mutation() {
    let (context, _backend, _dir, _db_path, workspace_id) = org_test_context(42).await;
    for (actor_id, operation_id, expected) in [
        ("", "op-1", OrgErrorCode::InvalidInput),
        ("  ", "op-1", OrgErrorCode::InvalidInput),
        ("system", "op-1", OrgErrorCode::InvalidInput),
        (" system ", "op-1", OrgErrorCode::InvalidInput),
        ("agent-one", "", OrgErrorCode::InvalidInput),
        ("agent-one", "  ", OrgErrorCode::InvalidInput),
        ("agent-one", " op-1 ", OrgErrorCode::InvalidInput),
    ] {
        let error = execute_idempotent(
            &context,
            TEST_COMMAND,
            &envelope(workspace_id, actor_id, operation_id),
            &request("never-runs"),
            |_transaction, _now| Box::pin(async { panic!("invalid command executed") }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, expected);
        assert!(!error.retryable);
    }

    let mut unsupported = envelope(workspace_id, "agent-one", "op-version");
    unsupported.schema_version = 2;
    let error = execute_idempotent(
        &context,
        TEST_COMMAND,
        &unsupported,
        &request("never-runs"),
        |_transaction, _now| Box::pin(async { panic!("invalid command executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);

    for invalid_kind in [
        OrgCommandKind::new("", 1),
        OrgCommandKind::new(" command ", 1),
        OrgCommandKind::new("command", 0),
    ] {
        let error = execute_idempotent(
            &context,
            invalid_kind,
            &envelope(workspace_id, "agent-one", "op-invalid-kind"),
            &request("never-runs"),
            |_transaction, _now| Box::pin(async { panic!("invalid command kind executed") }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
    }
}

#[test]
fn maps_compare_and_swap_results_to_stable_org_errors() {
    assert_eq!(
        resolve_cas(CompareAndSwap::Applied(7), "document").unwrap(),
        7
    );
    let missing = resolve_cas::<i32>(CompareAndSwap::NotFound, "document").unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::NotFound);
    let stale = resolve_cas::<i32>(
        CompareAndSwap::Conflict {
            current_revision: 9,
        },
        "document",
    )
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleRevision);
    assert_eq!(stale.details["current_revision"], 9);
    assert!(stale.retryable);
}

#[tokio::test]
async fn rejects_new_mutations_after_workspace_archive() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(42).await;
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
            timezone: &workspace.timezone,
            policy_schema_version: workspace.policy_schema_version,
            policy: &workspace.policy,
            archived_at: Some(42),
            updated_at: 42,
        })
        .await
        .unwrap();

    let error = execute_idempotent(
        &context,
        TEST_COMMAND,
        &envelope(workspace_id, "agent-one", "archived-operation"),
        &request("archived"),
        |_transaction, _now| Box::pin(async { panic!("archived command executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::ArchivedWorkspace);
    assert!(session
        .get_org_operation(workspace_id, "archived-operation")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn create_boundary_commits_and_replays_a_caller_chosen_workspace_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("org-create-test.db");
    let backend: Arc<dyn StorageBackend> = Arc::new(TursoStorage::open(&db_path).await.unwrap());
    let context = OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(55)));
    let workspace_id =
        note_org::WorkspaceId::from_str("22222222-2222-4222-8222-222222222222").unwrap();
    let command = envelope(workspace_id, "agent-one", "create-operation");
    let input = request("create");

    let missing = execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &input,
        |_transaction, _now| Box::pin(async { panic!("missing workspace mutation executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::NotFound);

    let created = execute_idempotent_create(
        &context,
        TEST_COMMAND,
        &command,
        &input,
        |transaction, now| {
            Box::pin(async move {
                let policy = WorkspacePolicy::engineering_default();
                transaction
                    .insert_org_workspace(NewOrgWorkspace {
                        id: workspace_id,
                        slug: "created-engineering",
                        display_name: "Created Engineering",
                        description: "Created through the idempotency boundary",
                        timezone: "UTC",
                        policy_schema_version: 1,
                        policy: &policy,
                        now,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                let metadata = json!({"source": "create-boundary-test"});
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: "workspace-created-event",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "22222222-2222-4222-8222-222222222222",
                        actor_id: "agent-one",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "workspace created",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: 1,
                    workspace_id,
                    operation_id: "create-operation".into(),
                    event_ids: vec![event.id],
                    workspace_revision: Some(1),
                    document_revisions: Default::default(),
                    data: json!({"created_at": now}),
                })
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(created.data, json!({"created_at": 55}));

    let replayed = execute_idempotent_create(
        &context,
        TEST_COMMAND,
        &command,
        &input,
        |_transaction, _now| Box::pin(async { panic!("create replay reran mutation") }),
    )
    .await
    .unwrap();
    assert_eq!(replayed, created);

    let divergent = execute_idempotent_create(
        &context,
        TEST_COMMAND,
        &command,
        &request("different"),
        |_transaction, _now| Box::pin(async { panic!("divergent create replay executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(divergent.code, OrgErrorCode::IdempotencyConflict);

    let new_operation = envelope(workspace_id, "agent-one", "second-create-operation");
    let already_exists = execute_idempotent_create(
        &context,
        TEST_COMMAND,
        &new_operation,
        &input,
        |_transaction, _now| Box::pin(async { panic!("duplicate create mutation executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(already_exists.code, OrgErrorCode::InvalidTransition);

    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .is_some());
    assert!(session
        .get_org_operation(workspace_id, "create-operation")
        .await
        .unwrap()
        .is_some());
    assert!(session
        .get_org_operation(workspace_id, "second-create-operation")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn commits_one_event_and_replays_the_stored_result_after_restart() {
    let (context, backend, _dir, db_path, workspace_id) = org_test_context(1_234).await;
    let command = envelope(workspace_id, "agent-one", "operation-1");
    let input = request("same");
    let event_metadata = json!({"source": "test"});

    let first = execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &input,
        |transaction, now| {
            let event_metadata = event_metadata.clone();
            Box::pin(async move {
                assert_eq!(now, 1_234);
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: "event-1",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "11111111-1111-4111-8111-111111111111",
                        actor_id: "agent-one",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "test mutation",
                        metadata: &event_metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: 1,
                    workspace_id,
                    operation_id: "operation-1".into(),
                    event_ids: vec![event.id],
                    workspace_revision: Some(1),
                    document_revisions: Default::default(),
                    data: json!({"at": now, "value": "same"}),
                })
            })
        },
    )
    .await
    .unwrap();

    drop(context);
    drop(backend);
    let restarted_backend: Arc<dyn StorageBackend> =
        Arc::new(TursoStorage::open(&db_path).await.unwrap());
    let restarted = OrgContext::new(
        restarted_backend.clone(),
        Arc::new(FixedOrgClock::new(9_999)),
    );
    let replayed = execute_idempotent(
        &restarted,
        TEST_COMMAND,
        &command,
        &input,
        |_transaction, _now| Box::pin(async { panic!("replay executed the mutation") }),
    )
    .await
    .unwrap();
    assert_eq!(replayed, first);
    assert_eq!(replayed.data, json!({"at": 1_234, "value": "same"}));

    let session = restarted_backend.session().await.unwrap();
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(session
        .get_org_operation(workspace_id, "operation-1")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn divergent_replay_conflicts_without_side_effects() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(7).await;
    let command = envelope(workspace_id, "agent-one", "operation-conflict");
    let first_result = OrgCommandResult {
        schema_version: 1,
        workspace_id,
        operation_id: "operation-conflict".into(),
        event_ids: Vec::new(),
        workspace_revision: Some(1),
        document_revisions: Default::default(),
        data: json!({"value": "first"}),
    };
    execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &request("first"),
        |_transaction, _now| Box::pin(async move { Ok(first_result) }),
    )
    .await
    .unwrap();

    let error = execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &request("different"),
        |_transaction, _now| Box::pin(async { panic!("conflicting replay executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::IdempotencyConflict);
    assert!(!error.retryable);
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_org_events(workspace_id, None, 50)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn different_command_kinds_cannot_replay_an_identically_shaped_request() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(7).await;
    let command = envelope(workspace_id, "agent-one", "operation-kind-conflict");
    let metadata = json!({});

    execute_idempotent(
        &context,
        FIRST_COMMAND,
        &command,
        &FirstCommandRequest {
            value: "same-shape".into(),
        },
        |transaction, now| {
            let metadata = metadata.clone();
            Box::pin(async move {
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: "command-kind-event",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "11111111-1111-4111-8111-111111111111",
                        actor_id: "agent-one",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "first command mutation",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: 1,
                    workspace_id,
                    operation_id: "operation-kind-conflict".into(),
                    event_ids: vec![event.id],
                    workspace_revision: Some(1),
                    document_revisions: Default::default(),
                    data: json!({"command": "first"}),
                })
            })
        },
    )
    .await
    .unwrap();

    let error = execute_idempotent(
        &context,
        SECOND_COMMAND,
        &command,
        &SecondCommandRequest {
            value: "same-shape".into(),
        },
        |_transaction, _now| Box::pin(async { panic!("second command mutation executed") }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::IdempotencyConflict);
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_org_events(workspace_id, None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn failed_mutation_rolls_back_event_and_operation_result() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(88).await;
    let command = envelope(workspace_id, "agent-one", "operation-rollback");
    let metadata = json!({});

    let error = execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &request("rollback"),
        |transaction, now| {
            let metadata = metadata.clone();
            Box::pin(async move {
                transaction
                    .append_org_event(NewOrgEvent {
                        id: "rolled-back-event",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "11111111-1111-4111-8111-111111111111",
                        actor_id: "agent-one",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "must roll back",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Err(OrgError::invalid_input("injected failure"))
            })
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidInput);

    let session = backend.session().await.unwrap();
    assert!(session
        .list_org_events(workspace_id, None, 50)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .get_org_operation(workspace_id, "operation-rollback")
        .await
        .unwrap()
        .is_none());
    drop(session);

    let retry_metadata = json!({"retry": true});
    let retried = execute_idempotent(
        &context,
        TEST_COMMAND,
        &command,
        &request("rollback"),
        |transaction, now| {
            let retry_metadata = retry_metadata.clone();
            Box::pin(async move {
                let event = transaction
                    .append_org_event(NewOrgEvent {
                        id: "rolled-back-event",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "11111111-1111-4111-8111-111111111111",
                        actor_id: "agent-one",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "retry succeeds once",
                        metadata: &retry_metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgCommandResult {
                    schema_version: 1,
                    workspace_id,
                    operation_id: "operation-rollback".into(),
                    event_ids: vec![event.id],
                    workspace_revision: Some(1),
                    document_revisions: Default::default(),
                    data: json!({"retried": true}),
                })
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(retried.event_ids, vec!["rolled-back-event"]);
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_org_events(workspace_id, None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn commit_error_persists_system_bookkeeping_without_an_operation_result() {
    let (context, backend, _dir, _db_path, workspace_id) = org_test_context(88).await;
    let command = envelope(workspace_id, "agent-one", "operation-commit-error");
    let metadata = json!({"schema_version": 1});

    let error = execute_idempotent_outcome(
        &context,
        TEST_COMMAND,
        &command,
        &request("commit-error"),
        |transaction, now| {
            let metadata = metadata.clone();
            Box::pin(async move {
                transaction
                    .append_internal_org_event(NewOrgEvent {
                        id: "committed-bookkeeping-event",
                        workspace_id,
                        subject_kind: "workspace",
                        subject_id: "11111111-1111-4111-8111-111111111111",
                        actor_id: "system",
                        attempt_id: None,
                        event_type: OrgEventType::WorkspaceChange,
                        occurred_at: now,
                        summary: "must commit",
                        metadata: &metadata,
                        previous_state: None,
                        resulting_state: None,
                    })
                    .await
                    .map_err(OrgError::storage)?;
                Ok(OrgMutationOutcome::CommitError(OrgError::new(
                    OrgErrorCode::StaleLease,
                    "Org lease proof is stale or invalid",
                    json!({}),
                    true,
                )))
            })
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StaleLease, "{error:?}");
    assert_eq!(error.details, json!({}));

    let session = backend.session().await.unwrap();
    let events = session
        .list_org_events(workspace_id, None, 50)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "committed-bookkeeping-event");
    assert!(session
        .get_org_operation(workspace_id, "operation-commit-error")
        .await
        .unwrap()
        .is_none());
}
