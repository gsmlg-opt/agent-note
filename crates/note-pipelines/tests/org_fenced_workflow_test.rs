mod support;

use note_org::{DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    approve_item, claim_item, put_document, reject_item, release_claim, report_progress,
    request_review, retry_item, submit_result, transition_item, ApproveItemRequest,
    CommandEnvelope, FixedOrgClock, OrgClaimKind, OrgContext, OrgErrorCode, OrgWorkflowPhase,
    PutDocumentRequest, RejectItemRequest, ReleaseClaimRequest, ReportProgressRequest,
    RequestReviewRequest, RetryItemRequest, StartClaimRequest, SubmitResultRequest,
    TransitionItemRequest, TransitionLeaseProof,
};
use note_storage::{
    CompareAndSwap, NewOrgAttempt, NewOrgEvent, OrgArtifactReference, OrgAttempt,
    OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate, OrgDocument, OrgEvent,
    OrgEventType, OrgLeaseEndReason, OrgProjectedWorkItem, OrgWorkspaceUpdate, SanitizedOrgLease,
    StorageBackend,
};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{DeterministicTokenSource, FailOnceAtWorkflowPhase};

const NOW: i64 = 1_900_000_000;

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap()
}

async fn claimed_review() -> (
    OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    String,
    String,
) {
    let (context, backend, dir, _path, _workspace) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "review-progress-token",
    ])));
    put_document(
        &context,
        &envelope("seed", "seed-review-progress"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "review-progress.org".into(),
            source: format!(
                "* REVIEW Review\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    backend
        .session()
        .await
        .unwrap()
        .insert_org_attempt(NewOrgAttempt {
            id: "submitted-review-attempt",
            workspace_id: workspace_id(),
            work_item_id: item_id(),
            attempt_number: 1,
            actor_id: "author",
            status: OrgAttemptStatus::Submitted,
            started_at: NOW - 1,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    let claim = claim_item(
        &context,
        &envelope("reviewer", "claim-review-progress"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    (context, backend, dir, claim.lease_id, claim.fencing_token)
}

async fn submitted_review_claim() -> (
    OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    String,
    String,
) {
    let (context, backend, dir, _path, _workspace) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "execution-before-review",
        "active-review-token",
    ])));
    put_document(
        &context,
        &envelope("seed", "seed-submitted-review"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "submitted-review.org".into(),
            source: format!(
                "* READY Review\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:REQUIRES_REVIEW: true\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let execution = claim_item(
        &context,
        &envelope("agent", "claim-before-review"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    submit_result(
        &context,
        &envelope("agent", "submit-before-review"),
        &SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id: execution.lease_id,
            fencing_token: execution.fencing_token,
            result_summary: "review me".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({"preserved": true}),
        },
    )
    .await
    .unwrap();
    let review = claim_item(
        &context,
        &envelope("reviewer", "claim-submitted-review"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    (context, backend, dir, review.lease_id, review.fencing_token)
}

fn document_id() -> DocumentId {
    DocumentId::from_str("73000000-0000-4000-8000-000000000001").unwrap()
}

fn item_id() -> WorkItemId {
    WorkItemId::from_str("74000000-0000-4000-8000-000000000001").unwrap()
}

fn envelope(actor: &str, operation: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id: workspace_id(),
        actor_id: actor.into(),
        operation_id: operation.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowSnapshot {
    document: OrgDocument,
    projection: Vec<OrgProjectedWorkItem>,
    attempts: Vec<OrgAttempt>,
    leases: Vec<SanitizedOrgLease>,
    events: Vec<OrgEvent>,
}

async fn workflow_snapshot(backend: &Arc<dyn StorageBackend>) -> WorkflowSnapshot {
    let session = backend.session().await.unwrap();
    WorkflowSnapshot {
        document: session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        projection: session
            .list_org_document_projection(document_id())
            .await
            .unwrap(),
        attempts: session.list_org_attempts(item_id()).await.unwrap(),
        leases: session.list_org_lease_history(item_id()).await.unwrap(),
        events: session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                200,
            )
            .await
            .unwrap(),
    }
}

async fn lease_expiry(backend: &Arc<dyn StorageBackend>) -> i64 {
    backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap()
        .expires_at
}

async fn assert_expiry_bookkeeping(
    backend: &Arc<dyn StorageBackend>,
    before: &WorkflowSnapshot,
    operation_id: &str,
    expected_attempt_status: OrgAttemptStatus,
) {
    let after = workflow_snapshot(backend).await;
    assert_eq!(after.document, before.document);
    assert_eq!(after.projection, before.projection);
    assert_eq!(after.attempts.len(), before.attempts.len());
    assert_eq!(
        after.attempts.last().unwrap().status,
        expected_attempt_status
    );
    if expected_attempt_status == OrgAttemptStatus::Submitted {
        assert_eq!(after.attempts, before.attempts);
    }
    let lease = after
        .leases
        .iter()
        .find(|lease| lease.end_reason == Some(OrgLeaseEndReason::LeaseExpiry))
        .unwrap();
    assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::LeaseExpiry));
    assert_eq!(lease.ended_at, Some(lease.expires_at));
    assert!(lease.expiry_event_id.is_some());
    assert_eq!(after.events.len(), before.events.len() + 1);
    let expiry_events = after
        .events
        .iter()
        .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
        .collect::<Vec<_>>();
    assert_eq!(expiry_events.len(), 1);
    assert_eq!(expiry_events[0].actor_id, "system");
    assert_eq!(
        lease.expiry_event_id.as_deref(),
        Some(expiry_events[0].id.as_str())
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id(), operation_id)
        .await
        .unwrap()
        .is_none());
}

macro_rules! assert_fault_rolls_back_then_retries {
    ($context:expr, $backend:expr, $operation:literal, $phase:expr, |$ctx:ident| $call:expr) => {{
        let hook = Arc::new(FailOnceAtWorkflowPhase::new($phase));
        let fault_context = $context.clone().with_workflow_test_hook(hook);
        let before = workflow_snapshot(&$backend).await;
        let error = {
            let $ctx = &fault_context;
            $call
        }
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure);
        assert_eq!(workflow_snapshot(&$backend).await, before);
        assert!($backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id(), $operation)
            .await
            .unwrap()
            .is_none());
        {
            let $ctx = &fault_context;
            $call
        }
        .await
        .unwrap();
        assert!($backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id(), $operation)
            .await
            .unwrap()
            .is_some());
    }};
}

async fn update_workspace_policy(backend: &Arc<dyn StorageBackend>, policy: WorkspacePolicy) {
    let session = backend.session().await.unwrap();
    let workspace = session
        .get_org_workspace(workspace_id())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        session
            .compare_and_swap_org_workspace(OrgWorkspaceUpdate {
                id: workspace.id,
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
            .unwrap(),
        CompareAndSwap::Applied(_)
    ));
}

async fn seed_failed_attempt(backend: &Arc<dyn StorageBackend>, id: &str) {
    backend
        .session()
        .await
        .unwrap()
        .insert_org_attempt(NewOrgAttempt {
            id,
            workspace_id: workspace_id(),
            work_item_id: item_id(),
            attempt_number: 1,
            actor_id: "previous-agent",
            status: OrgAttemptStatus::Failed,
            started_at: NOW - 2,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
}

async fn claimed_execution() -> (
    OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    String,
    String,
) {
    claimed_execution_fixture(false, "progress-token").await
}

async fn claimed_execution_fixture(
    requires_review: bool,
    token: &str,
) -> (
    OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    String,
    String,
) {
    let (context, backend, dir, _path, _workspace) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([token])));
    put_document(
        &context,
        &envelope("seed", "seed-progress"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "progress.org".into(),
            source: format!(
                "* READY Progress\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n{}:END:\r\n",
                item_id(),
                if requires_review { ":REQUIRES_REVIEW: true\r\n" } else { "" }
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let claim = claim_item(
        &context,
        &envelope("agent", "claim-progress"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    (context, backend, dir, claim.lease_id, claim.fencing_token)
}

#[tokio::test]
async fn progress_is_proof_only_event_and_replays_without_touching_execution_state() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    let before = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let request = ReportProgressRequest {
        schema_version: 1,
        work_item_id: item_id(),
        lease_id,
        kind: OrgClaimKind::Execution,
        fencing_token,
        summary: "halfway".into(),
        metadata: serde_json::json!({"percent": 50}),
    };

    let result = report_progress(&context, &envelope("agent", "progress-1"), &request)
        .await
        .unwrap();
    assert_eq!(result.document_revisions[&document_id().to_string()], 2);
    assert_eq!(result.data["context"]["attempts"][0]["status"], "running");
    assert_eq!(
        report_progress(&context, &envelope("agent", "progress-1"), &request)
            .await
            .unwrap(),
        result
    );

    let session = backend.session().await.unwrap();
    let after = session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.last_heartbeat_at, before.last_heartbeat_at);
    assert_eq!(after.expires_at, before.expires_at);
    assert_eq!(session.count_org_attempts(item_id()).await.unwrap(), 1);
    let events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            20,
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == OrgEventType::Progress)
            .count(),
        1
    );
    let progress = events.last().unwrap();
    assert_eq!(progress.summary, "halfway");
    assert_eq!(progress.metadata["percent"], 50);

    let mut stale = request;
    stale.fencing_token = " ".into();
    assert_eq!(
        report_progress(&context, &envelope("agent", "progress-stale"), &stale)
            .await
            .unwrap_err()
            .code,
        OrgErrorCode::StaleLease
    );
}

#[tokio::test]
async fn operation_id_reuse_across_task6_command_kinds_conflicts_without_side_effects() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    report_progress(
        &context,
        &envelope("agent", "cross-task6-command"),
        &ReportProgressRequest {
            schema_version: 1,
            work_item_id: item_id(),
            lease_id: lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: fencing_token.clone(),
            summary: "first command".into(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();
    let before = workflow_snapshot(&backend).await;
    let error = submit_result(
        &context,
        &envelope("agent", "cross-task6-command"),
        &SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id,
            fencing_token,
            result_summary: "second command".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::IdempotencyConflict);
    assert_eq!(workflow_snapshot(&backend).await, before);
    assert_eq!(before.document.revision, 2);
    assert_eq!(before.attempts[0].status, OrgAttemptStatus::Running);
    assert_eq!(
        before
            .events
            .iter()
            .filter(|event| event.event_type == OrgEventType::Completion)
            .count(),
        0
    );
}

#[tokio::test]
async fn progress_rejects_a_valid_review_lease_with_the_safe_stale_shape() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_review().await;
    let error = report_progress(
        &context,
        &envelope("reviewer", "review-progress-is-invalid"),
        &ReportProgressRequest {
            schema_version: 1,
            work_item_id: item_id(),
            lease_id,
            kind: OrgClaimKind::Review,
            fencing_token,
            summary: "must not be accepted".into(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StaleLease);
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(workspace_id(), "review-progress-is-invalid")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                20
            )
            .await
            .unwrap()
            .iter()
            .filter(|event| event.event_type == OrgEventType::Progress)
            .count(),
        0
    );
}

#[tokio::test]
async fn superseded_and_released_owners_cannot_mutate_workflow_state() {
    let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "agent-a-token",
        "agent-b-token",
    ])));
    put_document(
        &context,
        &envelope("seed", "seed-reclaim-fencing"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "reclaim-fencing.org".into(),
            source: format!(
                "* READY Work\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let agent_a = claim_item(
        &context,
        &envelope("agent-a", "claim-agent-a"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let reclaimed_context = context.with_clock(Arc::new(FixedOrgClock::new(agent_a.expires_at)));
    let agent_b = claim_item(
        &reclaimed_context,
        &envelope("agent-b", "claim-agent-b"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let before_document = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let before_attempts = session.list_org_attempts(item_id()).await.unwrap();
    let before_leases = session.list_org_lease_history(item_id()).await.unwrap();
    let before_events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            100,
        )
        .await
        .unwrap();
    drop(session);

    let stale_progress = report_progress(
        &reclaimed_context,
        &envelope("agent-a", "delayed-agent-a-progress"),
        &ReportProgressRequest {
            schema_version: 1,
            work_item_id: item_id(),
            lease_id: agent_a.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: agent_a.fencing_token.clone(),
            summary: "late".into(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    let stale_submit = submit_result(
        &reclaimed_context,
        &envelope("agent-a", "delayed-agent-a-submit"),
        &SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id: agent_a.lease_id.clone(),
            fencing_token: agent_a.fencing_token.clone(),
            result_summary: "late".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    let stale_transition = transition_item(
        &reclaimed_context,
        &envelope("agent-a", "delayed-agent-a-transition"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            target_state: "FAILED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id: agent_a.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: agent_a.fencing_token,
            }),
            error: Some("late".into()),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    for error in [stale_progress, stale_submit, stale_transition] {
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(error.details, serde_json::json!({}));
    }

    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        before_document
    );
    assert_eq!(
        session.list_org_attempts(item_id()).await.unwrap(),
        before_attempts
    );
    assert_eq!(
        session.list_org_lease_history(item_id()).await.unwrap(),
        before_leases
    );
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                100
            )
            .await
            .unwrap(),
        before_events
    );
    for operation in [
        "delayed-agent-a-progress",
        "delayed-agent-a-submit",
        "delayed-agent-a-transition",
    ] {
        assert!(session
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
    }
    drop(session);

    release_claim(
        &reclaimed_context,
        &envelope("agent-b", "release-agent-b"),
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: agent_b.context.document.revision,
            lease_id: agent_b.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: agent_b.fencing_token.clone(),
            target_state: None,
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let released_document = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let released_events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            100,
        )
        .await
        .unwrap();
    drop(session);
    assert_eq!(
        report_progress(
            &reclaimed_context,
            &envelope("agent-b", "released-agent-b-progress"),
            &ReportProgressRequest {
                schema_version: 1,
                work_item_id: item_id(),
                lease_id: agent_b.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: agent_b.fencing_token,
                summary: "late after release".into(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        released_document
    );
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                100
            )
            .await
            .unwrap(),
        released_events
    );
}

#[tokio::test]
async fn every_active_workflow_command_uses_the_same_safe_stale_lease_shape() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    let session = backend.session().await.unwrap();
    let before_document = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let before_attempts = session.list_org_attempts(item_id()).await.unwrap();
    let before_lease = session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let before_events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            100,
        )
        .await
        .unwrap();
    drop(session);

    let errors = vec![
        report_progress(
            &context,
            &envelope("wrong-actor", "stale-progress-wrong-actor"),
            &ReportProgressRequest {
                schema_version: 1,
                work_item_id: item_id(),
                lease_id: lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: fencing_token.clone(),
                summary: "invalid".into(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
        submit_result(
            &context,
            &envelope("agent", "stale-submit-malformed-token"),
            &SubmitResultRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                lease_id: lease_id.clone(),
                fencing_token: " malformed ".into(),
                result_summary: "invalid".into(),
                note_refs: Vec::new(),
                artifacts: Vec::new(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
        request_review(
            &context,
            &envelope("agent", "stale-review-missing-lease"),
            &RequestReviewRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                lease_id: String::new(),
                fencing_token: fencing_token.clone(),
                result_summary: None,
                note_refs: Vec::new(),
                artifacts: Vec::new(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
        transition_item(
            &context,
            &envelope("agent", "stale-transition-wrong-kind"),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                target_state: "FAILED".into(),
                lease: Some(TransitionLeaseProof {
                    lease_id: lease_id.clone(),
                    kind: OrgClaimKind::Review,
                    fencing_token: fencing_token.clone(),
                }),
                error: Some("invalid".into()),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
    ];
    for (index, error) in errors.into_iter().enumerate() {
        assert_eq!(
            error.code,
            OrgErrorCode::StaleLease,
            "case {index}: {error:?}"
        );
        assert_eq!(error.message, "Org lease proof is stale or invalid");
        assert_eq!(error.details, serde_json::json!({}));
    }
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        before_document
    );
    assert_eq!(
        session.list_org_attempts(item_id()).await.unwrap(),
        before_attempts
    );
    assert_eq!(
        session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .unwrap(),
        before_lease
    );
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                100
            )
            .await
            .unwrap(),
        before_events
    );
    for operation in [
        "stale-progress-wrong-actor",
        "stale-submit-malformed-token",
        "stale-review-missing-lease",
        "stale-transition-wrong-kind",
    ] {
        assert!(session
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
    }
    drop(session);

    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let review_errors = vec![
        approve_item(
            &context,
            &envelope("wrong-reviewer", "stale-approve-wrong-actor"),
            &ApproveItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 2,
                lease_id: lease_id.clone(),
                fencing_token: fencing_token.clone(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
        reject_item(
            &context,
            &envelope("reviewer", "stale-reject-wrong-token"),
            &RejectItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 2,
                lease_id,
                fencing_token: "wrong-review-token".into(),
                reason: "invalid".into(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err(),
    ];
    for error in review_errors {
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(error.details, serde_json::json!({}));
    }
    let session = backend.session().await.unwrap();
    assert_eq!(
        session.list_org_attempts(item_id()).await.unwrap()[0].status,
        OrgAttemptStatus::Submitted
    );
    assert!(session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .is_some());
    for operation in ["stale-approve-wrong-actor", "stale-reject-wrong-token"] {
        assert!(session
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn submit_result_completes_direct_work_and_persists_the_attempt_payload() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    let request = SubmitResultRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 2,
        lease_id,
        fencing_token,
        result_summary: "finished".into(),
        note_refs: vec![OrgAttemptNoteReference {
            purpose: "evidence".into(),
            note_id: "note-1".into(),
            description: "result note".into(),
        }],
        artifacts: vec![OrgArtifactReference {
            uri: "artifact://one".into(),
            media_type: "text/plain".into(),
            name: "output".into(),
            description: "result artifact".into(),
        }],
        metadata: serde_json::json!({"quality": "verified"}),
    };

    let result = submit_result(&context, &envelope("agent", "submit-direct"), &request)
        .await
        .unwrap();
    assert_eq!(result.document_revisions[&document_id().to_string()], 3);
    assert_eq!(result.data["context"]["item"]["state"], "DONE");
    assert_eq!(
        submit_result(&context, &envelope("agent", "submit-direct"), &request)
            .await
            .unwrap(),
        result
    );

    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Completed);
    assert_eq!(attempt.result_summary.as_deref(), Some("finished"));
    assert_eq!(attempt.note_refs, request.note_refs);
    assert_eq!(attempt.artifacts, request.artifacts);
    assert_eq!(attempt.metadata, request.metadata);
    let lease = &session.list_org_lease_history(item_id()).await.unwrap()[0];
    assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::Completion));
    let events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            20,
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::ResultSubmission,
            OrgEventType::Completion,
        ]
    );
}

#[tokio::test]
async fn submit_and_approve_reject_missing_or_ambiguous_success_targets() {
    for ambiguous in [false, true] {
        let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
        let mut policy = WorkspacePolicy::engineering_default();
        if ambiguous {
            policy.states.insert("ACCEPTED".into());
            policy.terminal_states.insert("ACCEPTED".into());
            policy.successful_terminal_states.insert("ACCEPTED".into());
            policy
                .transitions
                .insert(("RUNNING".into(), "ACCEPTED".into()));
        } else {
            policy
                .transitions
                .remove(&("RUNNING".into(), "DONE".into()));
        }
        update_workspace_policy(&backend, policy).await;
        let operation = if ambiguous {
            "submit-ambiguous-success"
        } else {
            "submit-missing-success"
        };
        let error = submit_result(
            &context,
            &envelope("agent", operation),
            &SubmitResultRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 2,
                lease_id,
                fencing_token,
                result_summary: "must not choose".into(),
                note_refs: Vec::new(),
                artifacts: Vec::new(),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidTransition);
        if ambiguous {
            assert_eq!(error.message, "Successful Org terminal state is ambiguous");
            assert_eq!(
                error.details["targets"],
                serde_json::json!(["ACCEPTED", "DONE"])
            );
        } else {
            assert_eq!(
                error.message,
                "No successful Org terminal state is reachable"
            );
        }
        let session = backend.session().await.unwrap();
        assert_eq!(
            session
                .get_org_document(document_id())
                .await
                .unwrap()
                .unwrap()
                .revision,
            2
        );
        assert!(session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .is_some());
        assert!(session
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
    }

    for ambiguous in [false, true] {
        let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
        let mut policy = WorkspacePolicy::engineering_default();
        if ambiguous {
            policy.states.insert("ACCEPTED".into());
            policy.terminal_states.insert("ACCEPTED".into());
            policy.successful_terminal_states.insert("ACCEPTED".into());
            policy
                .transitions
                .insert(("REVIEW".into(), "ACCEPTED".into()));
        } else {
            policy.transitions.remove(&("REVIEW".into(), "DONE".into()));
        }
        update_workspace_policy(&backend, policy).await;
        let operation = if ambiguous {
            "approve-ambiguous-success"
        } else {
            "approve-missing-success"
        };
        let error = approve_item(
            &context,
            &envelope("reviewer", operation),
            &ApproveItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 3,
                lease_id,
                fencing_token,
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidTransition);
        if ambiguous {
            assert_eq!(error.message, "Successful Org terminal state is ambiguous");
            assert_eq!(
                error.details["targets"],
                serde_json::json!(["ACCEPTED", "DONE"])
            );
        } else {
            assert_eq!(
                error.message,
                "No successful Org terminal state is reachable"
            );
        }
        let session = backend.session().await.unwrap();
        assert_eq!(
            session.list_org_attempts(item_id()).await.unwrap()[0].status,
            OrgAttemptStatus::Submitted
        );
        assert!(session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .is_some());
        assert!(session
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn submit_result_routes_review_required_work_to_submitted_review() {
    let (context, backend, _dir, lease_id, fencing_token) =
        claimed_execution_fixture(true, "review-submit-token").await;
    let request = SubmitResultRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 2,
        lease_id,
        fencing_token,
        result_summary: "ready for review".into(),
        note_refs: Vec::new(),
        artifacts: Vec::new(),
        metadata: serde_json::json!({"review": true}),
    };

    let result = submit_result(&context, &envelope("agent", "submit-review"), &request)
        .await
        .unwrap();
    assert_eq!(result.document_revisions[&document_id().to_string()], 3);
    assert_eq!(result.data["context"]["item"]["state"], "REVIEW");
    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Submitted);
    assert_eq!(attempt.result_summary.as_deref(), Some("ready for review"));
    assert_eq!(
        session.list_org_lease_history(item_id()).await.unwrap()[0].end_reason,
        Some(OrgLeaseEndReason::ReviewRequest)
    );
    let events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            20,
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::ResultSubmission,
            OrgEventType::ReviewRequest,
        ]
    );
}

#[tokio::test]
async fn explicit_request_review_closes_execution_with_optional_partial_result() {
    let (context, backend, _dir, lease_id, fencing_token) =
        claimed_execution_fixture(false, "explicit-review-token").await;
    let request = RequestReviewRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 2,
        lease_id,
        fencing_token,
        result_summary: Some("partial result".into()),
        note_refs: Vec::new(),
        artifacts: Vec::new(),
        metadata: serde_json::json!({"partial": true}),
    };

    let result = request_review(&context, &envelope("agent", "request-review"), &request)
        .await
        .unwrap();
    assert_eq!(result.data["context"]["item"]["state"], "REVIEW");
    assert_eq!(result.document_revisions[&document_id().to_string()], 3);
    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Submitted);
    assert_eq!(attempt.result_summary.as_deref(), Some("partial result"));
    let events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            20,
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::ResultSubmission,
            OrgEventType::ReviewRequest,
        ]
    );
}

#[tokio::test]
async fn approval_completes_the_submitted_attempt_and_preserves_result_context() {
    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let request = ApproveItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 3,
        lease_id,
        fencing_token,
        metadata: serde_json::json!({"review_comment": "approved"}),
    };
    let result = approve_item(&context, &envelope("reviewer", "approve-item"), &request)
        .await
        .unwrap();
    assert_eq!(result.data["context"]["item"]["state"], "DONE");
    assert_eq!(result.document_revisions[&document_id().to_string()], 4);
    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Completed);
    assert_eq!(attempt.review_outcome.as_deref(), Some("approved"));
    assert_eq!(attempt.result_summary.as_deref(), Some("review me"));
    assert_eq!(attempt.metadata["preserved"], true);
    assert_eq!(
        approve_item(&context, &envelope("reviewer", "approve-item"), &request)
            .await
            .unwrap(),
        result
    );
    let mut closed_again = request.clone();
    closed_again.expected_document_revision = 4;
    assert_eq!(
        approve_item(
            &context,
            &envelope("reviewer", "approve-item-again"),
            &closed_again,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );
    let events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            20,
        )
        .await
        .unwrap();
    assert_eq!(events[events.len() - 2].event_type, OrgEventType::Approval);
    assert_eq!(events.last().unwrap().event_type, OrgEventType::Completion);
}

#[tokio::test]
async fn rejection_returns_to_recovery_and_fails_the_submitted_attempt() {
    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let request = RejectItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 3,
        lease_id,
        fencing_token,
        reason: "needs changes".into(),
        metadata: serde_json::json!({"line": 12}),
    };
    let result = reject_item(&context, &envelope("reviewer", "reject-item"), &request)
        .await
        .unwrap();
    assert_eq!(result.data["context"]["item"]["state"], "READY");
    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Failed);
    assert_eq!(attempt.review_outcome.as_deref(), Some("rejected"));
    assert_eq!(attempt.error.as_deref(), Some("needs changes"));
    assert_eq!(
        reject_item(&context, &envelope("reviewer", "reject-item"), &request)
            .await
            .unwrap(),
        result
    );
    let mut closed_again = request.clone();
    closed_again.expected_document_revision = 4;
    assert_eq!(
        reject_item(
            &context,
            &envelope("reviewer", "reject-item-again"),
            &closed_again,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );
    assert!(session
        .list_org_lease_history(item_id())
        .await
        .unwrap()
        .iter()
        .any(|lease| lease.end_reason == Some(OrgLeaseEndReason::Rejection)));
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                20
            )
            .await
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        OrgEventType::Rejection
    );
}

#[tokio::test]
async fn fenced_failure_closes_execution_and_records_structured_error_context() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    let result = transition_item(
        &context,
        &envelope("agent", "fail-item"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            target_state: "FAILED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token,
            }),
            error: Some("build failed".into()),
            metadata: serde_json::json!({"stage": "build"}),
        },
    )
    .await
    .unwrap();
    assert_eq!(result.data["context"]["item"]["state"], "FAILED");
    assert_eq!(result.document_revisions[&document_id().to_string()], 3);
    let session = backend.session().await.unwrap();
    let attempt = &session.list_org_attempts(item_id()).await.unwrap()[0];
    assert_eq!(attempt.status, OrgAttemptStatus::Failed);
    assert_eq!(attempt.error.as_deref(), Some("build failed"));
    assert_eq!(
        session.list_org_lease_history(item_id()).await.unwrap()[0].end_reason,
        Some(OrgLeaseEndReason::Failure)
    );
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                20
            )
            .await
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        OrgEventType::Failure
    );
}

#[tokio::test]
async fn execution_transition_classifies_block_cancel_and_custom_nonterminal() {
    for (index, target, expected_event, expected_status, expected_end) in [
        (
            0_u8,
            "BLOCKED",
            OrgEventType::Block,
            OrgAttemptStatus::Cancelled,
            Some(OrgLeaseEndReason::Block),
        ),
        (
            1_u8,
            "CANCELLED",
            OrgEventType::Cancellation,
            OrgAttemptStatus::Cancelled,
            Some(OrgLeaseEndReason::Cancellation),
        ),
        (
            2_u8,
            "READY",
            OrgEventType::Transition,
            OrgAttemptStatus::Running,
            None,
        ),
    ] {
        let (context, backend, _dir, lease_id, fencing_token) =
            claimed_execution_fixture(false, &format!("transition-token-{index}")).await;
        transition_item(
            &context,
            &envelope("agent", &format!("transition-{index}")),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 2,
                target_state: target.into(),
                lease: Some(TransitionLeaseProof {
                    lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token,
                }),
                error: None,
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        let session = backend.session().await.unwrap();
        assert_eq!(
            session.list_org_attempts(item_id()).await.unwrap()[0].status,
            expected_status
        );
        let lease = &session.list_org_lease_history(item_id()).await.unwrap()[0];
        assert_eq!(lease.end_reason, expected_end);
        assert_eq!(
            session
                .list_org_subject_events(
                    workspace_id(),
                    "work_item",
                    &item_id().to_string(),
                    None,
                    20
                )
                .await
                .unwrap()
                .last()
                .unwrap()
                .event_type,
            expected_event
        );
    }
}

#[tokio::test]
async fn unleased_unblock_and_policy_transition_change_only_source_and_audit() {
    for (index, from, target, expected_event) in [
        (0_u8, "BLOCKED", "READY", OrgEventType::Unblock),
        (1_u8, "BACKLOG", "READY", OrgEventType::Transition),
    ] {
        let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
        put_document(
            &context,
            &envelope("seed", &format!("seed-unleased-{index}")),
            &PutDocumentRequest {
                document_id: document_id(),
                path: format!("unleased-{index}.org"),
                source: format!(
                    "* {from} Unleased\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                    item_id()
                ),
                expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let result = transition_item(
            &context,
            &envelope("agent", &format!("unleased-{index}")),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                target_state: target.into(),
                lease: None,
                error: None,
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.document_revisions[&document_id().to_string()], 2);
        let session = backend.session().await.unwrap();
        assert_eq!(session.count_org_attempts(item_id()).await.unwrap(), 0);
        assert!(session
            .list_org_lease_history(item_id())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            session
                .list_org_subject_events(
                    workspace_id(),
                    "work_item",
                    &item_id().to_string(),
                    None,
                    20
                )
                .await
                .unwrap()
                .last()
                .unwrap()
                .event_type,
            expected_event
        );
    }
}

#[tokio::test]
async fn every_default_unleased_cancellation_updates_only_source_projection_and_audit() {
    for (index, from) in ["BACKLOG", "READY", "BLOCKED", "FAILED"]
        .into_iter()
        .enumerate()
    {
        let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
        put_document(
            &context,
            &envelope("seed", &format!("seed-unleased-cancel-{index}")),
            &PutDocumentRequest {
                document_id: document_id(),
                path: format!("unleased-cancel-{index}.org"),
                source: format!(
                    "* {from} Work\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                    item_id()
                ),
                expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let result = transition_item(
            &context,
            &envelope("operator", &format!("unleased-cancel-{index}")),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                target_state: "CANCELLED".into(),
                lease: None,
                error: None,
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.data["context"]["item"]["state"], "CANCELLED");
        assert_eq!(result.document_revisions[&document_id().to_string()], 2);
        let session = backend.session().await.unwrap();
        assert!(session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .is_empty());
        assert!(session
            .list_org_lease_history(item_id())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            session
                .list_org_subject_events(
                    workspace_id(),
                    "work_item",
                    &item_id().to_string(),
                    None,
                    20,
                )
                .await
                .unwrap()
                .last()
                .unwrap()
                .event_type,
            OrgEventType::Cancellation
        );
    }
}

#[tokio::test]
async fn unleased_transitions_never_ignore_a_supplied_invalid_proof() {
    for (index, from) in ["BACKLOG", "BLOCKED"].into_iter().enumerate() {
        let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
        put_document(
            &context,
            &envelope("seed", &format!("seed-unleased-proof-{index}")),
            &PutDocumentRequest {
                document_id: document_id(),
                path: format!("unleased-proof-{index}.org"),
                source: format!(
                    "* {from} Work\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                    item_id()
                ),
                expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let before = workflow_snapshot(&backend).await;
        let operation = format!("unleased-invalid-proof-{index}");
        let error = transition_item(
            &context,
            &envelope("operator", &operation),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
                target_state: "READY".into(),
                lease: Some(TransitionLeaseProof {
                    lease_id: "missing-lease".into(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: "wrong-token".into(),
                }),
                error: None,
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(workflow_snapshot(&backend).await, before);
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id(), &operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn review_cancellation_closes_the_submitted_attempt_exactly_once() {
    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let request = TransitionItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 3,
        target_state: "CANCELLED".into(),
        lease: Some(TransitionLeaseProof {
            lease_id,
            kind: OrgClaimKind::Review,
            fencing_token,
        }),
        error: None,
        metadata: serde_json::json!({}),
    };
    let result = transition_item(&context, &envelope("reviewer", "cancel-review"), &request)
        .await
        .unwrap();
    assert_eq!(result.data["context"]["item"]["state"], "CANCELLED");
    assert_eq!(
        transition_item(&context, &envelope("reviewer", "cancel-review"), &request,)
            .await
            .unwrap(),
        result
    );
    let attempt = backend
        .session()
        .await
        .unwrap()
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(attempt.status, OrgAttemptStatus::Cancelled);
}

#[tokio::test]
async fn review_rejects_configured_non_lifecycle_transition_without_closing_ownership() {
    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let mut policy = WorkspacePolicy::engineering_default();
    policy
        .transitions
        .insert(("REVIEW".into(), "BLOCKED".into()));
    update_workspace_policy(&backend, policy).await;
    let before = workflow_snapshot(&backend).await;
    let error = transition_item(
        &context,
        &envelope("reviewer", "review-configured-non-lifecycle"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            target_state: "BLOCKED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id,
                kind: OrgClaimKind::Review,
                fencing_token,
            }),
            error: None,
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::InvalidTransition);
    assert_eq!(
        error.message,
        "Submitted review work only accepts approval, rejection, or cancellation"
    );
    assert_eq!(workflow_snapshot(&backend).await, before);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session.list_org_attempts(item_id()).await.unwrap()[0].status,
        OrgAttemptStatus::Submitted
    );
    assert!(session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .is_some());
    assert!(session
        .get_org_operation(workspace_id(), "review-configured-non-lifecycle")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn retry_failed_work_allocates_the_next_attempt_and_a_fresh_token() {
    let (context, backend, _dir, lease_id, fencing_token) =
        claimed_execution_fixture(false, "failed-attempt-token").await;
    transition_item(
        &context,
        &envelope("agent", "fail-before-retry"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            target_state: "FAILED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token,
            }),
            error: Some("first failure".into()),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();
    let retry_context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "retry-token-2",
        "retry-token-3",
    ])));
    let request = RetryItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 3,
    };
    let result = retry_item(&retry_context, &envelope("agent", "retry-item"), &request)
        .await
        .unwrap();
    assert_eq!(result.fencing_token, "retry-token-2");
    assert_eq!(result.context.item.state.as_deref(), Some("RUNNING"));
    assert_eq!(result.context.document.revision, 4);
    assert_eq!(result.context.attempts.len(), 2);
    assert_eq!(result.context.attempts[0].status, "failed");
    assert_eq!(
        result.context.attempts[0].error.as_deref(),
        Some("first failure")
    );
    assert_eq!(result.context.attempts[1].status, "running");
    assert_eq!(
        retry_item(&retry_context, &envelope("agent", "retry-item"), &request)
            .await
            .unwrap(),
        result
    );
    let events = backend
        .session()
        .await
        .unwrap()
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            30,
        )
        .await
        .unwrap();
    assert_eq!(events[events.len() - 3].event_type, OrgEventType::Retry);
    assert_eq!(events[events.len() - 2].event_type, OrgEventType::Claim);
    assert_eq!(events.last().unwrap().event_type, OrgEventType::Start);

    transition_item(
        &retry_context,
        &envelope("agent", "fail-second-attempt"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 4,
            target_state: "FAILED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id: result.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: result.fencing_token,
            }),
            error: Some("second failure".into()),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();
    let third = retry_item(
        &retry_context,
        &envelope("agent", "retry-third-attempt"),
        &RetryItemRequest {
            expected_document_revision: 5,
            ..request.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(third.fencing_token, "retry-token-3");
    assert_eq!(third.context.attempts.len(), 3);
    assert_eq!(
        third.context.attempts[1].error.as_deref(),
        Some("second failure")
    );
    assert_eq!(third.context.attempts[2].status, "running");

    transition_item(
        &retry_context,
        &envelope("agent", "fail-third-attempt"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 6,
            target_state: "FAILED".into(),
            lease: Some(TransitionLeaseProof {
                lease_id: third.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: third.fencing_token,
            }),
            error: Some("third failure".into()),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let before_document = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let before_attempts = session.list_org_attempts(item_id()).await.unwrap();
    let before_events = session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &item_id().to_string(),
            None,
            100,
        )
        .await
        .unwrap();
    drop(session);
    assert_eq!(
        retry_item(
            &retry_context,
            &envelope("agent", "retry-over-limit"),
            &RetryItemRequest {
                expected_document_revision: 7,
                ..request
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::RetryLimit
    );
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        before_document
    );
    assert_eq!(
        session.list_org_attempts(item_id()).await.unwrap(),
        before_attempts
    );
    assert_eq!(
        session
            .list_org_subject_events(
                workspace_id(),
                "work_item",
                &item_id().to_string(),
                None,
                100
            )
            .await
            .unwrap(),
        before_events
    );
    assert!(session
        .get_org_operation(workspace_id(), "retry-over-limit")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn retry_revalidates_assignment_schedule_dependencies_and_capacity() {
    let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
    put_document(
        &context,
        &envelope("seed", "seed-retry-assignment-schedule"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "retry-assignment-schedule.org".into(),
            source: format!(
                "* FAILED Retry\r\nSCHEDULED: <2035-01-01 Mon 00:00>\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:ASSIGNEE: other-agent\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    seed_failed_attempt(&backend, "retry-blocked-attempt").await;
    let blocked = retry_item(
        &context,
        &envelope("agent", "retry-assignment-schedule"),
        &RetryItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(blocked.code, OrgErrorCode::InvalidTransition);
    assert!(blocked.details.to_string().contains("AssignedToOtherActor"));
    assert!(blocked.details.to_string().contains("ScheduledForFuture"));
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id(), "retry-assignment-schedule")
        .await
        .unwrap()
        .is_none());

    let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
    let dependency = WorkItemId::from_str("74000000-0000-4000-8000-000000000002").unwrap();
    put_document(
        &context,
        &envelope("seed", "seed-retry-dependency"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "retry-dependency.org".into(),
            source: format!(
                "* BACKLOG Dependency\r\n:PROPERTIES:\r\n:ID: {dependency}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n* FAILED Retry\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:DEPENDS_ON: {dependency}\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    seed_failed_attempt(&backend, "retry-dependency-attempt").await;
    assert_eq!(
        retry_item(
            &context,
            &envelope("agent", "retry-dependency"),
            &RetryItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::DependencyBlocked
    );

    let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = 0;
    update_workspace_policy(&backend, policy).await;
    put_document(
        &context,
        &envelope("seed", "seed-retry-capacity"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "retry-capacity.org".into(),
            source: format!(
                "* FAILED Retry\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    seed_failed_attempt(&backend, "retry-capacity-attempt").await;
    assert_eq!(
        retry_item(
            &context,
            &envelope("agent", "retry-capacity"),
            &RetryItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 1,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::ConcurrencyLimit
    );
}

#[tokio::test]
async fn retry_preserves_prior_progress_result_error_notes_artifacts_and_metadata() {
    let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "history-retry-token",
    ])));
    put_document(
        &context,
        &envelope("seed", "seed-retry-history"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "retry-history.org".into(),
            source: format!(
                "* FAILED Work\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let note_refs = vec![OrgAttemptNoteReference {
        purpose: "evidence".into(),
        note_id: "note-history".into(),
        description: "previous evidence".into(),
    }];
    let artifacts = vec![OrgArtifactReference {
        uri: "artifact://history".into(),
        media_type: "text/plain".into(),
        name: "history.log".into(),
        description: "previous artifact".into(),
    }];
    let metadata = serde_json::json!({"preserve": true});
    let session = backend.session().await.unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "history-attempt-1",
            workspace_id: workspace_id(),
            work_item_id: item_id(),
            attempt_number: 1,
            actor_id: "previous-agent",
            status: OrgAttemptStatus::Running,
            started_at: NOW - 10,
            note_refs: &note_refs,
            artifacts: &artifacts,
            metadata: &metadata,
        })
        .await
        .unwrap();
    assert!(matches!(
        session
            .update_org_attempt(OrgAttemptUpdate {
                id: "history-attempt-1",
                expected_status: OrgAttemptStatus::Running,
                status: OrgAttemptStatus::Failed,
                ended_at: NOW - 1,
                error: Some("previous failure"),
                result_summary: Some("previous partial result"),
                review_outcome: None,
                note_refs: &note_refs,
                artifacts: &artifacts,
                metadata: &metadata,
            })
            .await
            .unwrap(),
        note_storage::ConditionalUpdate::Applied(_)
    ));
    for (id, event_type, summary) in [
        (
            "history-progress",
            OrgEventType::Progress,
            "previous progress",
        ),
        (
            "history-result",
            OrgEventType::ResultSubmission,
            "previous result",
        ),
    ] {
        session
            .append_org_event(NewOrgEvent {
                id,
                workspace_id: workspace_id(),
                subject_kind: "work_item",
                subject_id: &item_id().to_string(),
                actor_id: "previous-agent",
                attempt_id: Some("history-attempt-1"),
                event_type,
                occurred_at: NOW - 2,
                summary,
                metadata: &serde_json::json!({"preserve": true}),
                previous_state: Some("FAILED"),
                resulting_state: Some("FAILED"),
            })
            .await
            .unwrap();
    }
    drop(session);

    let result = retry_item(
        &context,
        &envelope("agent", "retry-preserved-history"),
        &RetryItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
        },
    )
    .await
    .unwrap();
    let previous = &result.context.attempts[0];
    assert_eq!(previous.status, "failed");
    assert_eq!(previous.error.as_deref(), Some("previous failure"));
    assert_eq!(
        previous.result_summary.as_deref(),
        Some("previous partial result")
    );
    assert_eq!(previous.note_refs[0].note_id, "note-history");
    assert_eq!(previous.artifacts[0].uri, "artifact://history");
    assert_eq!(previous.metadata["preserve"], true);
    assert_eq!(result.context.attempts[1].status, "running");
    let event_types = result.context.history_segments[0]
        .events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"progress"));
    assert!(event_types.contains(&"result_submission"));
    assert!(event_types.ends_with(&["retry", "claim", "start"]));
    let serialized = serde_json::to_string(&result.context).unwrap();
    assert!(!serialized.contains("history-retry-token"));
    assert!(!serialized.contains("fencing_token_hash"));
}

#[tokio::test]
async fn expired_execution_command_commits_only_expiry_bookkeeping_and_returns_stale() {
    let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
    let before = workflow_snapshot(&backend).await;
    let expired_context =
        context.with_clock(Arc::new(FixedOrgClock::new(lease_expiry(&backend).await)));
    let request = SubmitResultRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 2,
        lease_id,
        fencing_token,
        result_summary: "too late".into(),
        note_refs: Vec::new(),
        artifacts: Vec::new(),
        metadata: serde_json::json!({}),
    };

    for _ in 0..2 {
        let error = submit_result(
            &expired_context,
            &envelope("agent", "expired-submit"),
            &request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(error.details, serde_json::json!({}));
    }
    assert_expiry_bookkeeping(
        &backend,
        &before,
        "expired-submit",
        OrgAttemptStatus::Expired,
    )
    .await;
    let attempt = workflow_snapshot(&backend).await.attempts.pop().unwrap();
    assert_eq!(attempt.error.as_deref(), Some("Lease expired"));
}

#[tokio::test]
async fn expired_review_command_preserves_submitted_attempt_and_returns_stale() {
    let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
    let before = workflow_snapshot(&backend).await;
    let expired_context =
        context.with_clock(Arc::new(FixedOrgClock::new(lease_expiry(&backend).await)));
    let request = ApproveItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 3,
        lease_id,
        fencing_token,
        metadata: serde_json::json!({}),
    };

    for _ in 0..2 {
        let error = approve_item(
            &expired_context,
            &envelope("reviewer", "expired-approve"),
            &request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(error.details, serde_json::json!({}));
    }
    assert_expiry_bookkeeping(
        &backend,
        &before,
        "expired-approve",
        OrgAttemptStatus::Submitted,
    )
    .await;
}

#[tokio::test]
async fn retry_on_an_expired_running_lease_commits_expiry_bookkeeping_and_returns_stale() {
    let (context, backend, _dir, _lease_id, _fencing_token) = claimed_execution().await;
    let before = workflow_snapshot(&backend).await;
    let expired_context =
        context.with_clock(Arc::new(FixedOrgClock::new(lease_expiry(&backend).await)));
    let request = RetryItemRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 2,
    };

    for _ in 0..2 {
        let error = retry_item(
            &expired_context,
            &envelope("agent", "expired-retry"),
            &request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease);
        assert_eq!(error.details, serde_json::json!({}));
    }
    assert_expiry_bookkeeping(
        &backend,
        &before,
        "expired-retry",
        OrgAttemptStatus::Expired,
    )
    .await;
}

#[tokio::test]
async fn workflow_phase_failures_roll_back_every_command_class_and_allow_same_operation_retry() {
    {
        let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
        let request = ReportProgressRequest {
            schema_version: 1,
            work_item_id: item_id(),
            lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token,
            summary: "faulted progress".into(),
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-progress",
            OrgWorkflowPhase::Proof,
            |ctx| report_progress(ctx, &envelope("agent", "fault-progress"), &request)
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
        let request = SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id,
            fencing_token,
            result_summary: "faulted direct result".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-direct-submit",
            OrgWorkflowPhase::OperationWrite,
            |ctx| submit_result(ctx, &envelope("agent", "fault-direct-submit"), &request)
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) =
            claimed_execution_fixture(true, "fault-review-submit-token").await;
        let request = SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id,
            fencing_token,
            result_summary: "faulted review result".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-review-submit",
            OrgWorkflowPhase::ContextHydration,
            |ctx| submit_result(ctx, &envelope("agent", "fault-review-submit"), &request)
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
        let request = RequestReviewRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id,
            fencing_token,
            result_summary: Some("faulted partial result".into()),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-request-review",
            OrgWorkflowPhase::Events,
            |ctx| request_review(ctx, &envelope("agent", "fault-request-review"), &request)
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
        let request = ApproveItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            lease_id,
            fencing_token,
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-approve",
            OrgWorkflowPhase::AttemptUpdate,
            |ctx| approve_item(ctx, &envelope("reviewer", "fault-approve"), &request)
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) = submitted_review_claim().await;
        let request = RejectItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            lease_id,
            fencing_token,
            reason: "faulted rejection".into(),
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-reject",
            OrgWorkflowPhase::LeaseUpdate,
            |ctx| reject_item(ctx, &envelope("reviewer", "fault-reject"), &request)
        );
    }

    for (operation, target, phase) in [
        ("fault-failure", "FAILED", OrgWorkflowPhase::SourceEdit),
        ("fault-block", "BLOCKED", OrgWorkflowPhase::Events),
        (
            "fault-cancel",
            "CANCELLED",
            OrgWorkflowPhase::ContextHydration,
        ),
    ] {
        let (context, backend, _dir, lease_id, fencing_token) = claimed_execution().await;
        let request = TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            target_state: target.into(),
            lease: Some(TransitionLeaseProof {
                lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token,
            }),
            error: (target == "FAILED").then(|| "faulted failure".into()),
            metadata: serde_json::json!({}),
        };
        let hook = Arc::new(FailOnceAtWorkflowPhase::new(phase));
        let fault_context = context.clone().with_workflow_test_hook(hook);
        let before = workflow_snapshot(&backend).await;
        assert_eq!(
            transition_item(&fault_context, &envelope("agent", operation), &request)
                .await
                .unwrap_err()
                .code,
            OrgErrorCode::StorageFailure
        );
        assert_eq!(workflow_snapshot(&backend).await, before);
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id(), operation)
            .await
            .unwrap()
            .is_none());
        transition_item(&fault_context, &envelope("agent", operation), &request)
            .await
            .unwrap();
    }

    {
        let (context, backend, _dir, _path, _workspace) = support::org_test_context(NOW).await;
        put_document(
            &context,
            &envelope("seed", "seed-fault-unleased"),
            &PutDocumentRequest {
                document_id: document_id(),
                path: "fault-unleased.org".into(),
                source: format!(
                    "* BLOCKED Work\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                    item_id()
                ),
                expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        let request = TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            target_state: "READY".into(),
            lease: None,
            error: None,
            metadata: serde_json::json!({}),
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-unleased-transition",
            OrgWorkflowPhase::OperationWrite,
            |ctx| transition_item(
                ctx,
                &envelope("operator", "fault-unleased-transition"),
                &request,
            )
        );
    }

    {
        let (context, backend, _dir, lease_id, fencing_token) =
            claimed_execution_fixture(false, "fault-retry-initial-token").await;
        transition_item(
            &context,
            &envelope("agent", "fail-before-faulted-retry"),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: 2,
                target_state: "FAILED".into(),
                lease: Some(TransitionLeaseProof {
                    lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token,
                }),
                error: Some("retry me".into()),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
        let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
            "fault-retry-token",
            "fault-retry-token",
        ])));
        let request = RetryItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
        };
        assert_fault_rolls_back_then_retries!(
            context,
            backend,
            "fault-retry",
            OrgWorkflowPhase::LeaseUpdate,
            |ctx| retry_item(ctx, &envelope("agent", "fault-retry"), &request)
        );
    }
}
