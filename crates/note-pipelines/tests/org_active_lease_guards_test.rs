mod support;

use note_org::{DocumentId, WorkItemId, WorkItemType, WorkspaceId};
use note_pipelines::org::{
    add_dependency, approve_item, archive_workspace, assign_item, claim_item, create_item,
    get_item_context, heartbeat_claim, import_documents, import_offline_workspace_snapshot,
    import_workspace_snapshot, link_note, list_event_history, move_document, move_item,
    put_document, reject_item, release_claim, reparent_item, report_progress, retry_item,
    schedule_item, submit_result, transition_item, update_workspace, ApproveItemRequest,
    ArchiveWorkspaceRequest, AssignItemRequest, CommandEnvelope, CreateItemRequest,
    DependencyRequest, DocumentImport, FixedOrgClock, HeartbeatClaimRequest,
    ImportDocumentsRequest, ImportWorkspaceSnapshotRequest, LeaseProofInput, MoveDocumentRequest,
    MoveItemRequest, NoteLinkRequest, OrgClaimKind, OrgErrorCode, OrgEventQuery, OrgFieldPatch,
    OrgWorkflowPhase, PutDocumentRequest, RejectItemRequest, ReleaseClaimRequest,
    ReparentItemRequest, ReportProgressRequest, RetryItemRequest, ScheduleItemRequest,
    StartClaimRequest, SubmitResultRequest, TransitionItemRequest, UpdateWorkspaceRequest,
    WorkspaceImportMode, WorkspaceSnapshotMetadata,
};
use note_storage::{
    NewNote, OrgAttempt, OrgAttemptStatus, OrgDocument, OrgEvent, OrgLeaseEndReason,
    OrgProjectedWorkItem, SanitizedOrgLease, StorageBackend,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{DeterministicTokenSource, FailAtWorkflowPhaseOccurrence};

const NOW: i64 = 1_900_000_000;

fn document_id() -> DocumentId {
    DocumentId::from_str("76000000-0000-4000-8000-000000000001").unwrap()
}

fn item_id() -> WorkItemId {
    WorkItemId::from_str("77000000-0000-4000-8000-000000000001").unwrap()
}

fn second_document_id() -> DocumentId {
    DocumentId::from_str("76000000-0000-4000-8000-000000000002").unwrap()
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicPayloadSnapshot {
    document: OrgDocument,
    item: OrgProjectedWorkItem,
    attempts: Vec<OrgAttempt>,
    leases: Vec<SanitizedOrgLease>,
    events: Vec<OrgEvent>,
}

async fn public_payload_snapshot(
    backend: &Arc<dyn StorageBackend>,
    workspace_id: WorkspaceId,
) -> PublicPayloadSnapshot {
    let session = backend.session().await.unwrap();
    PublicPayloadSnapshot {
        document: session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap(),
        item: session.get_org_work_item(item_id()).await.unwrap().unwrap(),
        attempts: session.list_org_attempts(item_id()).await.unwrap(),
        leases: session.list_org_lease_history(item_id()).await.unwrap(),
        events: session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap(),
    }
}

async fn claimed_item() -> (
    note_pipelines::org::OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    note_org::WorkspaceId,
    note_pipelines::org::OrgClaimResult,
) {
    let (context, backend, dir, _path, workspace_id) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "active-guard-token",
        "fresh-after-reassignment-token",
    ])));
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-active-guard"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "active-guard.org".into(),
            source: format!(
                "* READY Guarded\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let claim = claim_item(
        &context,
        &envelope(workspace_id, "agent", "claim-active-guard"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    (context, backend, dir, workspace_id, claim)
}

async fn claimed_review_item() -> (
    note_pipelines::org::OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    note_org::WorkspaceId,
    note_pipelines::org::OrgClaimResult,
) {
    let (context, backend, dir, _path, workspace_id) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "raw-review-execution-token",
        "raw-review-token",
    ])));
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-raw-review"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "raw-review.org".into(),
            source: format!(
                "* READY Guarded\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:REQUIRES_REVIEW: true\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let execution = claim_item(
        &context,
        &envelope(workspace_id, "author", "claim-before-raw-review"),
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
        &envelope(workspace_id, "author", "submit-before-raw-review"),
        &SubmitResultRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            lease_id: execution.lease_id,
            fencing_token: execution.fencing_token,
            result_summary: "ready for review".into(),
            note_refs: Vec::new(),
            artifacts: Vec::new(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();
    let review = claim_item(
        &context,
        &envelope(workspace_id, "reviewer", "claim-raw-review"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    (context, backend, dir, workspace_id, review)
}

#[tokio::test]
async fn active_schedule_edits_require_the_current_proof_and_preserve_ownership() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let request = |revision, lease| ScheduleItemRequest {
        item_id: item_id(),
        document_id: document_id(),
        scheduled: OrgFieldPatch::Set("<2030-01-01 Tue 09:00>".into()),
        deadline: OrgFieldPatch::Unchanged,
        expected_revisions: BTreeMap::from([(document_id(), revision)]),
        lease,
    };

    let missing = schedule_item(
        &context,
        &envelope(workspace_id, "agent", "schedule-missing-proof"),
        &request(1, None),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::StaleLease, "{missing:?}");

    let before = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let attempt_before = backend
        .session()
        .await
        .unwrap()
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    schedule_item(
        &context,
        &envelope(workspace_id, "agent", "schedule-current-proof"),
        &request(
            2,
            Some(LeaseProofInput {
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token.clone(),
            }),
        ),
    )
    .await
    .unwrap();
    let after = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    let attempt_after = backend
        .session()
        .await
        .unwrap()
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt_after, attempt_before);
}

#[tokio::test]
async fn same_workspace_move_preserves_current_lease_and_attempt_identity() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-move-target"),
        &PutDocumentRequest {
            document_id: second_document_id(),
            path: "move-target.org".into(),
            source: "#+TITLE: Target\r\n".into(),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let lease_before = session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let attempt_before = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    drop(session);

    let result = move_item(
        &context,
        &envelope(workspace_id, "agent", "move-active-item"),
        &MoveItemRequest {
            item_id: item_id(),
            source_document_id: document_id(),
            target_document_id: second_document_id(),
            target_parent_id: None,
            expected_revisions: BTreeMap::from([(document_id(), 2), (second_document_id(), 1)]),
            lease_proofs: BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token,
                },
            )]),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let lease_after = session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let attempt_after = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(lease_after, lease_before);
    assert_eq!(attempt_after, attempt_before);
    assert_eq!(
        session
            .get_org_work_item(item_id())
            .await
            .unwrap()
            .unwrap()
            .document_id,
        second_document_id()
    );
    let move_event = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| result.event_ids.contains(&event.id))
        .unwrap();
    assert_eq!(
        move_event.attempt_id.as_deref(),
        Some(attempt_before.id.as_str())
    );
}

#[tokio::test]
async fn active_reassignment_closes_ownership_and_attempt_before_applying_assignment() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let result = assign_item(
        &context,
        &envelope(workspace_id, "agent", "reassign-active-item"),
        &AssignItemRequest {
            item_id: item_id(),
            document_id: document_id(),
            assignee: Some("agent-next".into()),
            expected_revisions: BTreeMap::from([(document_id(), 2)]),
            lease: Some(LeaseProofInput {
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token.clone(),
            }),
        },
    )
    .await
    .unwrap();
    let item = &result.data["resulting_items"][0];
    assert_eq!(item["state"], "READY");
    assert_eq!(item["assignee"], "agent-next");

    let session = backend.session().await.unwrap();
    let lease = session
        .list_org_lease_history(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::Reassignment));
    let attempt = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt.status, OrgAttemptStatus::Cancelled);
    drop(session);

    let stale = schedule_item(
        &context,
        &envelope(workspace_id, "agent", "old-token-after-reassignment"),
        &ScheduleItemRequest {
            item_id: item_id(),
            document_id: document_id(),
            scheduled: OrgFieldPatch::Set("<2030-01-02 Wed 09:00>".into()),
            deadline: OrgFieldPatch::Unchanged,
            expected_revisions: BTreeMap::from([(document_id(), 3)]),
            lease: Some(LeaseProofInput {
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token.clone(),
            }),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleLease);

    let fresh = claim_item(
        &context,
        &envelope(workspace_id, "agent-next", "claim-after-reassignment"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 3,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    assert_ne!(fresh.lease_id, claim.lease_id);
    assert_ne!(fresh.fencing_token, claim.fencing_token);

    assign_item(
        &context,
        &envelope(workspace_id, "agent-next", "unassign-active-item"),
        &AssignItemRequest {
            item_id: item_id(),
            document_id: document_id(),
            assignee: None,
            expected_revisions: BTreeMap::from([(document_id(), 4)]),
            lease: Some(LeaseProofInput {
                lease_id: fresh.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: fresh.fencing_token,
            }),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let lease_history = session.list_org_lease_history(item_id()).await.unwrap();
    assert_eq!(lease_history.len(), 2);
    assert!(lease_history
        .iter()
        .all(|lease| lease.end_reason == Some(OrgLeaseEndReason::Reassignment)));
    let attempts = session.list_org_attempts(item_id()).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts
        .iter()
        .all(|attempt| attempt.status == OrgAttemptStatus::Cancelled));
    let item = session.get_org_work_item(item_id()).await.unwrap().unwrap();
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.assignee, None);
}

#[tokio::test]
async fn active_assignment_failures_roll_back_each_boundary_and_same_operation_retries() {
    for phase in [
        OrgWorkflowPhase::Proof,
        OrgWorkflowPhase::LeaseUpdate,
        OrgWorkflowPhase::AttemptUpdate,
        OrgWorkflowPhase::SourceEdit,
        OrgWorkflowPhase::ProjectionUpdate,
        OrgWorkflowPhase::Events,
        OrgWorkflowPhase::OperationWrite,
    ] {
        let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
        let operation_id = format!("assignment-rollback-{phase:?}");
        let request = AssignItemRequest {
            item_id: item_id(),
            document_id: document_id(),
            assignee: Some("agent-next".into()),
            expected_revisions: BTreeMap::from([(document_id(), 2)]),
            lease: Some(LeaseProofInput {
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token.clone(),
            }),
        };
        let session = backend.session().await.unwrap();
        let document_before = session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let item_before = session.get_org_work_item(item_id()).await.unwrap().unwrap();
        let lease_before = session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .unwrap();
        let attempt_before = session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        let events_before = session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap();
        drop(session);
        let fault_context = context
            .clone()
            .with_workflow_test_hook(Arc::new(FailAtWorkflowPhaseOccurrence::new(phase, 1)));
        let error = assign_item(
            &fault_context,
            &envelope(workspace_id, "agent", &operation_id),
            &request,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure, "{phase:?}");
        let session = backend.session().await.unwrap();
        assert_eq!(
            session
                .get_org_document(document_id())
                .await
                .unwrap()
                .unwrap(),
            document_before,
            "{phase:?}"
        );
        assert_eq!(
            session.get_org_work_item(item_id()).await.unwrap().unwrap(),
            item_before,
            "{phase:?}"
        );
        assert_eq!(
            session
                .get_open_org_lease_internal(item_id())
                .await
                .unwrap()
                .unwrap(),
            lease_before,
            "{phase:?}"
        );
        assert_eq!(
            session
                .list_org_attempts(item_id())
                .await
                .unwrap()
                .pop()
                .unwrap(),
            attempt_before,
            "{phase:?}"
        );
        assert_eq!(
            session
                .list_org_events(workspace_id, None, 100)
                .await
                .unwrap(),
            events_before,
            "{phase:?}"
        );
        assert!(session
            .get_org_operation(workspace_id, &operation_id)
            .await
            .unwrap()
            .is_none());
        drop(session);
        assign_item(
            &context,
            &envelope(workspace_id, "agent", &operation_id),
            &request,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn raw_reassignment_applies_recovery_and_closes_ownership_once() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let source = current.source.replace("RUNNING", "READY").replace(
        ":AGENT_NOTE_TYPE: task\r\n",
        ":AGENT_NOTE_TYPE: task\r\n:ASSIGNEE: agent-next\r\n",
    );
    let result = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-reassign-active"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: current.path,
            source,
            expected_revision: Some(2),
            lease_proofs: BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token,
                },
            )]),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let lease_history = session.list_org_lease_history(item_id()).await.unwrap();
    assert_eq!(lease_history.len(), 1);
    assert_eq!(
        lease_history[0].end_reason,
        Some(OrgLeaseEndReason::Reassignment)
    );
    let attempt = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt.status, OrgAttemptStatus::Cancelled);
    let item = session.get_org_work_item(item_id()).await.unwrap().unwrap();
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.assignee.as_deref(), Some("agent-next"));
    let events = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| result.event_ids.contains(&event.id))
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            note_storage::OrgEventType::Assignment,
            note_storage::OrgEventType::DocumentImport,
        ]
    );
}

#[tokio::test]
async fn archive_rejects_active_ownership_but_bookkeeps_expiry_before_archiving() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let active = archive_workspace(
        &context,
        &envelope(workspace_id, "operator", "archive-active"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(active.code, OrgErrorCode::ActiveLease);
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id, "archive-active")
        .await
        .unwrap()
        .is_none());

    let expired_context = context.with_clock(Arc::new(FixedOrgClock::new(claim.expires_at)));
    let archived = archive_workspace(
        &expired_context,
        &envelope(workspace_id, "operator", "archive-expired"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    let replayed = archive_workspace(
        &expired_context,
        &envelope(workspace_id, "operator", "archive-expired"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(replayed, archived);

    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap()
            .archived_at,
        Some(claim.expires_at)
    );
    let attempts = session.list_org_attempts(item_id()).await.unwrap();
    assert_eq!(attempts[0].status, OrgAttemptStatus::Expired);
    let events = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == note_storage::OrgEventType::LeaseExpiry)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == note_storage::OrgEventType::WorkspaceArchive)
            .count(),
        1
    );

    let heartbeat = heartbeat_claim(
        &expired_context,
        &envelope(workspace_id, "agent", "heartbeat-after-archive"),
        &HeartbeatClaimRequest {
            work_item_id: item_id(),
            lease_id: claim.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token.clone(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(heartbeat.code, OrgErrorCode::ArchivedWorkspace);
    let progress = report_progress(
        &expired_context,
        &envelope(workspace_id, "agent", "progress-after-archive"),
        &ReportProgressRequest {
            schema_version: 1,
            work_item_id: item_id(),
            lease_id: claim.lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token,
            summary: "must be rejected".into(),
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(progress.code, OrgErrorCode::ArchivedWorkspace);
    let read = get_item_context(&expired_context, workspace_id, item_id())
        .await
        .unwrap();
    assert_eq!(read.item.id, item_id());
    let history = list_event_history(
        &expired_context,
        &OrgEventQuery {
            workspace_id,
            subject_kind: None,
            subject_id: None,
            cursor: None,
            limit: Some(100),
        },
    )
    .await
    .unwrap();
    assert!(history
        .events
        .iter()
        .any(|event| event.event_type == "workspace_archive"));
    let claim_after_archive = claim_item(
        &expired_context,
        &envelope(workspace_id, "agent", "claim-after-archive"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 2,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(claim_after_archive.code, OrgErrorCode::ArchivedWorkspace);

    macro_rules! assert_archived {
        ($operation:literal, $future:expr) => {{
            let error = $future.await.unwrap_err();
            assert_eq!(error.code, OrgErrorCode::ArchivedWorkspace, $operation);
        }};
    }
    let archived_workspace = backend
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .unwrap();
    let stored_document = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    assert!(stored_document.source.contains("Guarded"));
    assert_archived!(
        "update_workspace",
        update_workspace(
            &expired_context,
            &envelope(workspace_id, "operator", "update-after-archive"),
            &UpdateWorkspaceRequest {
                expected_revision: archived_workspace.revision,
                slug: archived_workspace.slug.clone(),
                display_name: archived_workspace.display_name.clone(),
                description: archived_workspace.description.clone(),
                timezone: archived_workspace.timezone.clone(),
                policy_schema_version: archived_workspace.policy_schema_version,
                policy: archived_workspace.policy.clone(),
            },
        )
    );
    assert_archived!(
        "put_document",
        put_document(
            &expired_context,
            &envelope(workspace_id, "operator", "put-after-archive"),
            &PutDocumentRequest {
                document_id: document_id(),
                path: stored_document.path.clone(),
                source: stored_document.source.clone(),
                expected_revision: Some(stored_document.revision),
                lease_proofs: BTreeMap::new(),
            },
        )
    );
    assert_archived!(
        "import_documents",
        import_documents(
            &expired_context,
            &envelope(workspace_id, "operator", "import-after-archive"),
            &ImportDocumentsRequest {
                documents: vec![DocumentImport {
                    document_id: document_id(),
                    path: stored_document.path.clone(),
                    source: stored_document.source.clone(),
                }],
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease_proofs: BTreeMap::new(),
            },
        )
    );
    assert_archived!(
        "move_document",
        move_document(
            &expired_context,
            &envelope(workspace_id, "operator", "document-move-after-archive"),
            &MoveDocumentRequest {
                document_id: document_id(),
                target_workspace_id: WorkspaceId::from_str("79000000-0000-4000-8000-000000000001")
                    .unwrap(),
                expected_document_revision: stored_document.revision,
                expected_source_workspace_revision: archived_workspace.revision,
                expected_target_workspace_revision: 1,
                lease_proofs: BTreeMap::new(),
            },
        )
    );
    assert_archived!(
        "create_item",
        create_item(
            &expired_context,
            &envelope(workspace_id, "operator", "create-item-after-archive"),
            &CreateItemRequest {
                document_id: document_id(),
                parent_id: None,
                item_id: WorkItemId::from_str("77000000-0000-4000-8000-000000000099").unwrap(),
                item_type: WorkItemType::Task,
                title: "Archived".into(),
                initial_state: Some("READY".into()),
                priority: None,
                tags: BTreeSet::new(),
                assignee: None,
                requires_review: false,
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
            },
        )
    );
    assert_archived!(
        "move_item",
        move_item(
            &expired_context,
            &envelope(workspace_id, "operator", "item-move-after-archive"),
            &MoveItemRequest {
                item_id: item_id(),
                source_document_id: document_id(),
                target_document_id: document_id(),
                target_parent_id: None,
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease_proofs: BTreeMap::new(),
            },
        )
    );
    assert_archived!(
        "reparent_item",
        reparent_item(
            &expired_context,
            &envelope(workspace_id, "operator", "reparent-after-archive"),
            &ReparentItemRequest {
                item_id: item_id(),
                document_id: document_id(),
                target_parent_id: None,
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease_proofs: BTreeMap::new(),
            },
        )
    );
    assert_archived!(
        "assign_item",
        assign_item(
            &expired_context,
            &envelope(workspace_id, "operator", "assign-after-archive"),
            &AssignItemRequest {
                item_id: item_id(),
                document_id: document_id(),
                assignee: Some("operator".into()),
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease: None,
            },
        )
    );
    assert_archived!(
        "schedule_item",
        schedule_item(
            &expired_context,
            &envelope(workspace_id, "operator", "schedule-after-archive"),
            &ScheduleItemRequest {
                item_id: item_id(),
                document_id: document_id(),
                scheduled: OrgFieldPatch::Set("<2030-01-01 Tue 09:00>".into()),
                deadline: OrgFieldPatch::Unchanged,
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease: None,
            },
        )
    );
    assert_archived!(
        "dependency",
        add_dependency(
            &expired_context,
            &envelope(workspace_id, "operator", "dependency-after-archive"),
            &DependencyRequest {
                item_id: item_id(),
                dependency_id: WorkItemId::from_str("77000000-0000-4000-8000-000000000098")
                    .unwrap(),
                document_id: document_id(),
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease: None,
            },
        )
    );
    assert_archived!(
        "note_link",
        link_note(
            &expired_context,
            &envelope(workspace_id, "operator", "link-after-archive"),
            &NoteLinkRequest {
                item_id: item_id(),
                document_id: document_id(),
                purpose: "design".into(),
                note_id: "78000000-0000-4000-8000-000000000099".parse().unwrap(),
                description: "Archived".into(),
                expected_revisions: BTreeMap::from([(document_id(), stored_document.revision,)]),
                lease: None,
            },
        )
    );
    let release = ReleaseClaimRequest {
        schema_version: 1,
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: stored_document.revision,
        lease_id: "archived-lease".into(),
        kind: OrgClaimKind::Execution,
        fencing_token: "archived-token".into(),
        target_state: Some("READY".into()),
    };
    assert_archived!(
        "release_claim",
        release_claim(
            &expired_context,
            &envelope(workspace_id, "operator", "release-after-archive"),
            &release,
        )
    );
    assert_archived!(
        "submit_result",
        submit_result(
            &expired_context,
            &envelope(workspace_id, "operator", "submit-after-archive"),
            &SubmitResultRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: stored_document.revision,
                lease_id: "archived-lease".into(),
                fencing_token: "archived-token".into(),
                result_summary: "Archived".into(),
                note_refs: Vec::new(),
                artifacts: Vec::new(),
                metadata: serde_json::json!({}),
            },
        )
    );
    assert_archived!(
        "transition_item",
        transition_item(
            &expired_context,
            &envelope(workspace_id, "operator", "transition-after-archive"),
            &TransitionItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: stored_document.revision,
                target_state: "READY".into(),
                lease: None,
                error: None,
                metadata: serde_json::json!({}),
            },
        )
    );
    assert_archived!(
        "approve_item",
        approve_item(
            &expired_context,
            &envelope(workspace_id, "operator", "approve-after-archive"),
            &ApproveItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: stored_document.revision,
                lease_id: "archived-lease".into(),
                fencing_token: "archived-token".into(),
                metadata: serde_json::json!({}),
            },
        )
    );
    assert_archived!(
        "reject_item",
        reject_item(
            &expired_context,
            &envelope(workspace_id, "operator", "reject-after-archive"),
            &RejectItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: stored_document.revision,
                lease_id: "archived-lease".into(),
                fencing_token: "archived-token".into(),
                reason: "Archived".into(),
                metadata: serde_json::json!({}),
            },
        )
    );
    assert_archived!(
        "retry_item",
        retry_item(
            &expired_context,
            &envelope(workspace_id, "operator", "retry-after-archive"),
            &RetryItemRequest {
                schema_version: 1,
                work_item_id: item_id(),
                document_id: document_id(),
                expected_document_revision: stored_document.revision,
            },
        )
    );
}

#[tokio::test]
async fn archive_rejects_an_active_review_lease_without_side_effects() {
    let (context, backend, _dir, workspace_id, _review) = claimed_review_item().await;
    let before_events = backend
        .session()
        .await
        .unwrap()
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap();
    let error = archive_workspace(
        &context,
        &envelope(workspace_id, "operator", "archive-active-review"),
        &ArchiveWorkspaceRequest {
            expected_revision: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::ActiveLease);
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .unwrap()
        .archived_at
        .is_none());
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap(),
        before_events
    );
    assert!(session
        .get_org_operation(workspace_id, "archive-active-review")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn archive_failures_roll_back_expiry_archive_event_and_operation_boundaries() {
    for (phase, occurrence) in [
        (OrgWorkflowPhase::LeaseUpdate, 1),
        (OrgWorkflowPhase::AttemptUpdate, 1),
        (OrgWorkflowPhase::Events, 1),
        (OrgWorkflowPhase::SourceEdit, 1),
        (OrgWorkflowPhase::Events, 2),
        (OrgWorkflowPhase::OperationWrite, 1),
    ] {
        let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
        let expired_context = context.with_clock(Arc::new(FixedOrgClock::new(claim.expires_at)));
        let operation_id = format!("archive-rollback-{phase:?}-{occurrence}");
        let before_workspace = backend
            .session()
            .await
            .unwrap()
            .get_org_workspace(workspace_id)
            .await
            .unwrap()
            .unwrap();
        let before_events = backend
            .session()
            .await
            .unwrap()
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap();
        let fault_context = expired_context.clone().with_workflow_test_hook(Arc::new(
            FailAtWorkflowPhaseOccurrence::new(phase, occurrence),
        ));
        let error = archive_workspace(
            &fault_context,
            &envelope(workspace_id, "operator", &operation_id),
            &ArchiveWorkspaceRequest {
                expected_revision: 1,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StorageFailure, "{phase:?}");
        let session = backend.session().await.unwrap();
        assert_eq!(
            session
                .get_org_workspace(workspace_id)
                .await
                .unwrap()
                .unwrap(),
            before_workspace,
            "{phase:?}"
        );
        assert_eq!(
            session
                .get_open_org_lease_internal(item_id())
                .await
                .unwrap()
                .unwrap()
                .id,
            claim.lease_id,
            "{phase:?}"
        );
        assert_eq!(
            session.list_org_attempts(item_id()).await.unwrap()[0].status,
            OrgAttemptStatus::Running,
            "{phase:?}"
        );
        assert_eq!(
            session
                .list_org_events(workspace_id, None, 100)
                .await
                .unwrap(),
            before_events,
            "{phase:?}"
        );
        assert!(session
            .get_org_operation(workspace_id, &operation_id)
            .await
            .unwrap()
            .is_none());
        drop(session);

        archive_workspace(
            &expired_context,
            &envelope(workspace_id, "operator", &operation_id),
            &ArchiveWorkspaceRequest {
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn raw_non_state_diff_requires_exact_proof_preserves_ownership_and_emits_typed_audit() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let dependency_id = WorkItemId::from_str("77000000-0000-4000-8000-000000000010").unwrap();
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-raw-nonstate-dependency"),
        &PutDocumentRequest {
            document_id: second_document_id(),
            path: "raw-nonstate-dependency.org".into(),
            source: format!(
                "* DONE Dependency\r\n:PROPERTIES:\r\n:ID: {dependency_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let note_id = "78000000-0000-4000-8000-000000000001";
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Raw context",
            content: "context",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let source = current
        .source
        .replace("* RUNNING Guarded", "* RUNNING [#A] Renamed")
        .replace(
            ":PROPERTIES:\r\n",
            &format!(
                "SCHEDULED: <2030-01-01 Tue 09:00>\r\nDEADLINE: <2030-01-02 Wed 17:00>\r\n:PROPERTIES:\r\n:DEPENDS_ON: {dependency_id}\r\n"
            ),
        )
        + &format!("[[agent-note:design:{note_id}][Design]]\r\n");
    let request = |revision, lease_proofs| PutDocumentRequest {
        document_id: document_id(),
        path: current.path.clone(),
        source: source.clone(),
        expected_revision: Some(revision),
        lease_proofs,
    };

    let missing = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-title-missing"),
        &request(1, BTreeMap::new()),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::StaleLease, "{missing:?}");

    let stale = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-nonstate-stale-proof"),
        &request(
            2,
            BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id.clone(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: "stale-raw-nonstate-token".into(),
                },
            )]),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleLease);

    let extra_item = WorkItemId::from_str("77000000-0000-4000-8000-000000000099").unwrap();
    let extra = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-title-extra-proof"),
        &request(
            2,
            BTreeMap::from([
                (
                    item_id(),
                    LeaseProofInput {
                        lease_id: claim.lease_id.clone(),
                        kind: OrgClaimKind::Execution,
                        fencing_token: claim.fencing_token.clone(),
                    },
                ),
                (
                    extra_item,
                    LeaseProofInput {
                        lease_id: "invented-lease".into(),
                        kind: OrgClaimKind::Execution,
                        fencing_token: "invented-token".into(),
                    },
                ),
            ]),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(extra.code, OrgErrorCode::StaleLease);

    let before = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    let attempt_before = backend
        .session()
        .await
        .unwrap()
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let result = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-title-current"),
        &request(
            2,
            BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token,
                },
            )]),
        ),
    )
    .await
    .unwrap();
    let after = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap(),
        attempt_before
    );
    let events = backend
        .session()
        .await
        .unwrap()
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| result.event_ids.contains(&event.id))
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            note_storage::OrgEventType::TitleChange,
            note_storage::OrgEventType::PriorityChange,
            note_storage::OrgEventType::ScheduleChange,
            note_storage::OrgEventType::DeadlineChange,
            note_storage::OrgEventType::DependencyChange,
            note_storage::OrgEventType::NoteLinkChange,
            note_storage::OrgEventType::DocumentImport,
        ]
    );
}

#[tokio::test]
async fn active_dependency_and_note_link_require_missing_stale_and_current_proofs() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let dependency_id = WorkItemId::from_str("77000000-0000-4000-8000-000000000020").unwrap();
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-active-api-dependency"),
        &PutDocumentRequest {
            document_id: second_document_id(),
            path: "active-api-dependency.org".into(),
            source: format!(
                "* DONE Dependency\r\n:PROPERTIES:\r\n:ID: {dependency_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let note_id = "78000000-0000-4000-8000-000000000020";
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "API context",
            content: "context",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let current = LeaseProofInput {
        lease_id: claim.lease_id.clone(),
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token.clone(),
    };
    let stale = LeaseProofInput {
        fencing_token: "stale-api-token".into(),
        ..current.clone()
    };
    for (operation, lease) in [
        ("active-dependency-missing", None),
        ("active-dependency-stale", Some(stale.clone())),
    ] {
        let error = add_dependency(
            &context,
            &envelope(workspace_id, "agent", operation),
            &DependencyRequest {
                item_id: item_id(),
                dependency_id,
                document_id: document_id(),
                expected_revisions: BTreeMap::from([(document_id(), 2)]),
                lease,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease, "{operation}");
    }
    add_dependency(
        &context,
        &envelope(workspace_id, "agent", "active-dependency-current"),
        &DependencyRequest {
            item_id: item_id(),
            dependency_id,
            document_id: document_id(),
            expected_revisions: BTreeMap::from([(document_id(), 2)]),
            lease: Some(current.clone()),
        },
    )
    .await
    .unwrap();
    for (operation, lease) in [
        ("active-note-link-missing", None),
        ("active-note-link-stale", Some(stale)),
    ] {
        let error = link_note(
            &context,
            &envelope(workspace_id, "agent", operation),
            &NoteLinkRequest {
                item_id: item_id(),
                document_id: document_id(),
                purpose: "design".into(),
                note_id: note_id.parse().unwrap(),
                description: "Design".into(),
                expected_revisions: BTreeMap::from([(document_id(), 3)]),
                lease,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::StaleLease, "{operation}");
    }
    link_note(
        &context,
        &envelope(workspace_id, "agent", "active-note-link-current"),
        &NoteLinkRequest {
            item_id: item_id(),
            document_id: document_id(),
            purpose: "design".into(),
            note_id: note_id.parse().unwrap(),
            description: "Design".into(),
            expected_revisions: BTreeMap::from([(document_id(), 3)]),
            lease: Some(current),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .unwrap()
            .id,
        claim.lease_id
    );
    assert_eq!(
        session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap()
            .status,
        OrgAttemptStatus::Running
    );
}

#[tokio::test]
async fn active_note_link_rejects_fencing_material_before_source_or_event_writes() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let note_id = "78000000-0000-4000-8000-000000000021";
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: note_id,
            title: "Public payload guard",
            content: "context",
            attachments: &[],
            created_at: NOW,
            updated_at: NOW,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let digest = format!("{:x}", Sha256::digest(claim.fencing_token.as_bytes()));
    let before = public_payload_snapshot(&backend, workspace_id).await;

    for (operation_id, purpose, description) in [
        (
            "reject-note-link-purpose",
            format!("design-{}", claim.fencing_token),
            "Design context".into(),
        ),
        (
            "reject-note-link-description",
            "design".into(),
            format!("Design digest {digest}"),
        ),
    ] {
        let error = link_note(
            &context,
            &envelope(workspace_id, "agent", operation_id),
            &NoteLinkRequest {
                item_id: item_id(),
                document_id: document_id(),
                purpose,
                note_id: note_id.parse().unwrap(),
                description,
                expected_revisions: BTreeMap::from([(document_id(), 2)]),
                lease: Some(LeaseProofInput {
                    lease_id: claim.lease_id.clone(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token.clone(),
                }),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, OrgErrorCode::InvalidInput);
        assert_eq!(error.details, serde_json::json!({}));
        assert_eq!(
            public_payload_snapshot(&backend, workspace_id).await,
            before
        );
        assert!(backend
            .session()
            .await
            .unwrap()
            .get_org_operation(workspace_id, operation_id)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn raw_opaque_only_diff_needs_no_proof_and_preserves_ownership() {
    let (context, backend, _dir, workspace_id, _claim) = claimed_item().await;
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let before = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-opaque-only"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: current.path,
            source: format!("{}\r\nOpaque operator note.\r\n", current.source),
            expected_revision: Some(2),
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let after = backend
        .session()
        .await
        .unwrap()
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn raw_put_and_import_reject_fencing_material_before_fingerprint_or_writes() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let digest = format!("{:x}", Sha256::digest(claim.fencing_token.as_bytes()));
    let proof = LeaseProofInput {
        lease_id: claim.lease_id.clone(),
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token.clone(),
    };
    let before = public_payload_snapshot(&backend, workspace_id).await;

    let put_operation = "reject-raw-put-payload";
    let put_error = put_document(
        &context,
        &envelope(workspace_id, "agent", put_operation),
        &PutDocumentRequest {
            document_id: document_id(),
            path: format!("guarded-{}.org", claim.fencing_token),
            source: current.source.replace("Guarded", "Put renamed"),
            expected_revision: Some(2),
            lease_proofs: BTreeMap::from([(item_id(), proof.clone())]),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(put_error.code, OrgErrorCode::InvalidInput);
    assert_eq!(
        put_error.message,
        "Org workflow public payload contains reserved fencing material"
    );
    assert_eq!(put_error.details, serde_json::json!({}));
    assert_eq!(
        public_payload_snapshot(&backend, workspace_id).await,
        before
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id, put_operation)
        .await
        .unwrap()
        .is_none());

    let import_operation = "reject-raw-import-payload";
    let import_error = import_documents(
        &context,
        &envelope(workspace_id, "agent", import_operation),
        &ImportDocumentsRequest {
            documents: vec![DocumentImport {
                document_id: document_id(),
                path: current.path,
                source: format!(
                    "{}\r\nOpaque digest {digest}.\r\n",
                    current.source.replace("Guarded", "Import renamed")
                ),
            }],
            expected_revisions: BTreeMap::from([(document_id(), 2)]),
            lease_proofs: BTreeMap::from([(item_id(), proof)]),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(import_error.code, OrgErrorCode::InvalidInput);
    assert_eq!(import_error.details, serde_json::json!({}));
    assert_eq!(
        public_payload_snapshot(&backend, workspace_id).await,
        before
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_org_operation(workspace_id, import_operation)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn workspace_snapshot_rejects_fencing_material_before_fingerprint_or_writes() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let before = public_payload_snapshot(&backend, workspace_id).await;
    let workspace_before = backend
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .unwrap();
    let proof = LeaseProofInput {
        lease_id: claim.lease_id,
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token.clone(),
    };
    let base = ImportWorkspaceSnapshotRequest {
        mode: WorkspaceImportMode::Update,
        workspace: WorkspaceSnapshotMetadata {
            slug: workspace_before.slug.clone(),
            display_name: workspace_before.display_name.clone(),
            description: workspace_before.description.clone(),
            timezone: workspace_before.timezone.clone(),
            policy_schema_version: workspace_before.policy_schema_version,
            policy: workspace_before.policy.clone(),
            revision: workspace_before.revision,
            archived_at: workspace_before.archived_at,
        },
        documents: vec![DocumentImport {
            document_id: before.document.id,
            path: before.document.path.clone(),
            source: before.document.source.clone(),
        }],
        document_revisions: BTreeMap::from([(before.document.id, before.document.revision)]),
        lease_proofs: BTreeMap::from([(item_id(), proof)]),
    };

    let digest = format!("{:x}", Sha256::digest(claim.fencing_token.as_bytes()));
    for (payload_case, request) in [
        {
            let mut request = base.clone();
            request.workspace.description = claim.fencing_token.clone();
            ("workspace-token", request)
        },
        {
            let mut request = base.clone();
            request.documents[0]
                .source
                .push_str(&format!("\r\nOpaque digest {digest}.\r\n"));
            ("document-digest", request)
        },
    ] {
        for mode in [WorkspaceImportMode::Create, WorkspaceImportMode::Update] {
            for offline in [false, true] {
                let mut request = request.clone();
                request.mode = mode;
                let mode_name = match mode {
                    WorkspaceImportMode::Create => "create",
                    WorkspaceImportMode::Update => "update",
                };
                let transport_name = if offline { "offline" } else { "online" };
                let operation_id =
                    format!("reject-snapshot-{payload_case}-{mode_name}-{transport_name}");
                let error = if offline {
                    import_offline_workspace_snapshot(
                        &context,
                        &envelope(workspace_id, "agent", &operation_id),
                        &request,
                    )
                    .await
                } else {
                    import_workspace_snapshot(
                        &context,
                        &envelope(workspace_id, "agent", &operation_id),
                        &request,
                    )
                    .await
                }
                .unwrap_err();
                assert_eq!(error.code, OrgErrorCode::InvalidInput);
                assert_eq!(
                    error.message,
                    "Org workflow public payload contains reserved fencing material"
                );
                assert_eq!(error.details, serde_json::json!({}));
                assert_eq!(
                    public_payload_snapshot(&backend, workspace_id).await,
                    before
                );
                assert_eq!(
                    backend
                        .session()
                        .await
                        .unwrap()
                        .get_org_workspace(workspace_id)
                        .await
                        .unwrap()
                        .unwrap(),
                    workspace_before
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
        }
    }
}

#[tokio::test]
async fn raw_multi_item_diff_rejects_one_missing_active_proof_without_writes() {
    let (context, backend, _dir, _path, workspace_id) = support::org_test_context(NOW).await;
    let context = context.with_token_source(Arc::new(DeterministicTokenSource::new([
        "multi-token-one",
        "multi-token-two",
    ])));
    let second_item = WorkItemId::from_str("77000000-0000-4000-8000-000000000002").unwrap();
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-multi-proof"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "multi-proof.org".into(),
            source: format!(
                "* READY First\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n* READY Second\r\n:PROPERTIES:\r\n:ID: {second_item}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let first = claim_item(
        &context,
        &envelope(workspace_id, "agent", "claim-multi-first"),
        &StartClaimRequest {
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    claim_item(
        &context,
        &envelope(workspace_id, "agent", "claim-multi-second"),
        &StartClaimRequest {
            work_item_id: second_item,
            document_id: document_id(),
            expected_document_revision: 2,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let changed = current
        .source
        .replace("First", "First renamed")
        .replace("Second", "Second renamed");
    let error = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-missing-one-proof"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: current.path,
            source: changed,
            expected_revision: Some(3),
            lease_proofs: BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: first.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: first.fencing_token,
                },
            )]),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::StaleLease);
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.revision, 3);
    assert_eq!(stored.source, current.source);
}

#[tokio::test]
async fn raw_multi_document_move_requires_proof_and_preserves_attempt_on_item_move_event() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    put_document(
        &context,
        &envelope(workspace_id, "seed", "seed-raw-move-target"),
        &PutDocumentRequest {
            document_id: second_document_id(),
            path: "raw-move-target.org".into(),
            source: "#+TITLE: Target\r\n".into(),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    let current = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let attempt = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    drop(session);
    let request = |lease_proofs| ImportDocumentsRequest {
        documents: vec![
            DocumentImport {
                document_id: document_id(),
                path: current.path.clone(),
                source: "#+TITLE: Source\r\n".into(),
            },
            DocumentImport {
                document_id: second_document_id(),
                path: "raw-move-target.org".into(),
                source: current.source.clone(),
            },
        ],
        expected_revisions: BTreeMap::from([(document_id(), 2), (second_document_id(), 1)]),
        lease_proofs,
    };
    let missing = note_pipelines::org::import_documents(
        &context,
        &envelope(workspace_id, "agent", "raw-move-missing-proof"),
        &request(BTreeMap::new()),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, OrgErrorCode::StaleLease);

    let result = note_pipelines::org::import_documents(
        &context,
        &envelope(workspace_id, "agent", "raw-move-current-proof"),
        &request(BTreeMap::from([(
            item_id(),
            LeaseProofInput {
                lease_id: claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token,
            },
        )])),
    )
    .await
    .unwrap();
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_work_item(item_id())
            .await
            .unwrap()
            .unwrap()
            .document_id,
        second_document_id()
    );
    let lease = session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.attempt_id, attempt.id);
    let move_event = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|event| {
            result.event_ids.contains(&event.id)
                && event.event_type == note_storage::OrgEventType::ItemMove
        })
        .unwrap();
    assert_eq!(move_event.attempt_id.as_deref(), Some(attempt.id.as_str()));
}

#[tokio::test]
async fn raw_import_rejects_unsupported_item_type_tags_and_review_requirement_changes() {
    for (suffix, replace_from, replace_to) in [
        ("type", ":AGENT_NOTE_TYPE: task", ":AGENT_NOTE_TYPE: issue"),
        ("tags", "* RUNNING Guarded", "* RUNNING Guarded :new:"),
        (
            "review",
            ":AGENT_NOTE_TYPE: task\r\n",
            ":AGENT_NOTE_TYPE: task\r\n:REQUIRES_REVIEW: true\r\n",
        ),
    ] {
        let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
        let current = backend
            .session()
            .await
            .unwrap()
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let error = put_document(
            &context,
            &envelope(workspace_id, "agent", &format!("raw-unsupported-{suffix}")),
            &PutDocumentRequest {
                document_id: document_id(),
                path: current.path,
                source: current.source.replace(replace_from, replace_to),
                expected_revision: Some(2),
                lease_proofs: BTreeMap::from([(
                    item_id(),
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: OrgClaimKind::Execution,
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            OrgErrorCode::UnsupportedSemanticEdit,
            "{suffix}"
        );
    }
}

#[tokio::test]
async fn raw_combined_state_and_assignment_uses_reassignment_lifecycle_for_execution_and_review() {
    for review in [false, true] {
        let (context, backend, _dir, workspace_id, claim) = if review {
            claimed_review_item().await
        } else {
            claimed_item().await
        };
        let current = backend
            .session()
            .await
            .unwrap()
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let from = if review { "REVIEW" } else { "RUNNING" };
        let source = current
            .source
            .replace(&format!("* {from}"), "* READY")
            .replace(
                ":AGENT_NOTE_TYPE: task\r\n",
                ":AGENT_NOTE_TYPE: task\r\n:ASSIGNEE: agent-next\r\n",
            );
        let result = put_document(
            &context,
            &envelope(
                workspace_id,
                if review { "reviewer" } else { "agent" },
                if review {
                    "raw-review-combined-reassignment"
                } else {
                    "raw-execution-combined-reassignment"
                },
            ),
            &PutDocumentRequest {
                document_id: document_id(),
                path: current.path,
                source,
                expected_revision: Some(if review { 3 } else { 2 }),
                lease_proofs: BTreeMap::from([(
                    item_id(),
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: if review {
                            OrgClaimKind::Review
                        } else {
                            OrgClaimKind::Execution
                        },
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap();
        let session = backend.session().await.unwrap();
        let lease = session
            .list_org_lease_history(item_id())
            .await
            .unwrap()
            .into_iter()
            .find(|lease| {
                lease.kind
                    == if review {
                        note_storage::OrgLeaseKind::Review
                    } else {
                        note_storage::OrgLeaseKind::Execution
                    }
            })
            .unwrap();
        assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::Reassignment));
        let attempt = session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(attempt.status, OrgAttemptStatus::Cancelled);
        assert_eq!(
            attempt.review_outcome.as_deref(),
            review.then_some("reassigned")
        );
        let events = session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| result.event_ids.contains(&event.id))
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                note_storage::OrgEventType::Assignment,
                note_storage::OrgEventType::DocumentImport,
            ]
        );
    }
}

#[tokio::test]
async fn raw_execution_state_diff_uses_task6_failure_lifecycle() {
    let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
    let current = backend
        .session()
        .await
        .unwrap()
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap();
    let source = current.source.replace("RUNNING", "FAILED");

    let result = put_document(
        &context,
        &envelope(workspace_id, "agent", "raw-running-failed"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: current.path,
            source,
            expected_revision: Some(2),
            lease_proofs: BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id,
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token,
                },
            )]),
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    assert!(session
        .get_open_org_lease_internal(item_id())
        .await
        .unwrap()
        .is_none());
    let lease = session
        .list_org_lease_history(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::Failure));
    let attempt = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt.status, OrgAttemptStatus::Failed);
    assert_eq!(
        session
            .get_org_work_item(item_id())
            .await
            .unwrap()
            .unwrap()
            .state
            .as_deref(),
        Some("FAILED")
    );
    let events = session
        .list_org_events(workspace_id, None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| result.event_ids.contains(&event.id))
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![
            note_storage::OrgEventType::Failure,
            note_storage::OrgEventType::DocumentImport,
        ]
    );
    assert_eq!(events[0].attempt_id.as_deref(), Some(attempt.id.as_str()));
}

#[tokio::test]
async fn raw_execution_state_diffs_match_the_task6_lifecycle_matrix() {
    let cases = [
        (
            "REVIEW",
            OrgLeaseEndReason::ReviewRequest,
            OrgAttemptStatus::Submitted,
            vec![note_storage::OrgEventType::ReviewRequest],
        ),
        (
            "BLOCKED",
            OrgLeaseEndReason::Block,
            OrgAttemptStatus::Cancelled,
            vec![note_storage::OrgEventType::Block],
        ),
        (
            "CANCELLED",
            OrgLeaseEndReason::Cancellation,
            OrgAttemptStatus::Cancelled,
            vec![note_storage::OrgEventType::Cancellation],
        ),
        (
            "DONE",
            OrgLeaseEndReason::Completion,
            OrgAttemptStatus::Completed,
            vec![note_storage::OrgEventType::Completion],
        ),
    ];
    for (target, reason, status, expected_events) in cases {
        let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
        let current = backend
            .session()
            .await
            .unwrap()
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let result = put_document(
            &context,
            &envelope(
                workspace_id,
                "agent",
                &format!("raw-running-{}", target.to_lowercase()),
            ),
            &PutDocumentRequest {
                document_id: document_id(),
                path: current.path,
                source: current.source.replace("RUNNING", target),
                expected_revision: Some(2),
                lease_proofs: BTreeMap::from([(
                    item_id(),
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: OrgClaimKind::Execution,
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap();
        let session = backend.session().await.unwrap();
        let lease = session
            .list_org_lease_history(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(lease.end_reason, Some(reason), "target {target}");
        let attempt = session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(attempt.status, status, "target {target}");
        let events = session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| result.event_ids.contains(&event.id))
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        let mut expected = expected_events;
        expected.push(note_storage::OrgEventType::DocumentImport);
        assert_eq!(events, expected, "target {target}");
    }
}

#[tokio::test]
async fn raw_review_state_diffs_match_the_task6_lifecycle_matrix() {
    let cases = [
        (
            "DONE",
            OrgLeaseEndReason::Approval,
            OrgAttemptStatus::Completed,
            Some("approved"),
            vec![
                note_storage::OrgEventType::Approval,
                note_storage::OrgEventType::Completion,
            ],
        ),
        (
            "READY",
            OrgLeaseEndReason::Rejection,
            OrgAttemptStatus::Failed,
            Some("rejected"),
            vec![note_storage::OrgEventType::Rejection],
        ),
        (
            "CANCELLED",
            OrgLeaseEndReason::Cancellation,
            OrgAttemptStatus::Cancelled,
            None,
            vec![note_storage::OrgEventType::Cancellation],
        ),
    ];
    for (target, reason, status, review_outcome, expected_events) in cases {
        let (context, backend, _dir, workspace_id, claim) = claimed_review_item().await;
        let current = backend
            .session()
            .await
            .unwrap()
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let result = put_document(
            &context,
            &envelope(
                workspace_id,
                "reviewer",
                &format!("raw-review-{}", target.to_lowercase()),
            ),
            &PutDocumentRequest {
                document_id: document_id(),
                path: current.path,
                source: current.source.replace("* REVIEW", &format!("* {target}")),
                expected_revision: Some(3),
                lease_proofs: BTreeMap::from([(
                    item_id(),
                    LeaseProofInput {
                        lease_id: claim.lease_id,
                        kind: OrgClaimKind::Review,
                        fencing_token: claim.fencing_token,
                    },
                )]),
            },
        )
        .await
        .unwrap();
        let session = backend.session().await.unwrap();
        let lease = session
            .list_org_lease_history(item_id())
            .await
            .unwrap()
            .into_iter()
            .find(|lease| lease.kind == note_storage::OrgLeaseKind::Review)
            .unwrap();
        assert_eq!(lease.end_reason, Some(reason), "target {target}");
        let attempt = session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(attempt.status, status, "target {target}");
        assert_eq!(
            attempt.review_outcome.as_deref(),
            review_outcome,
            "target {target}"
        );
        let events = session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| result.event_ids.contains(&event.id))
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        let mut expected = expected_events;
        expected.push(note_storage::OrgEventType::DocumentImport);
        assert_eq!(events, expected, "target {target}");
    }
}

#[tokio::test]
async fn raw_lifecycle_failures_roll_back_every_mutation_boundary_and_retry_cleanly() {
    for (phase, occurrence) in [
        (OrgWorkflowPhase::Proof, 1),
        (OrgWorkflowPhase::LeaseUpdate, 1),
        (OrgWorkflowPhase::AttemptUpdate, 1),
        (OrgWorkflowPhase::SourceEdit, 1),
        (OrgWorkflowPhase::ProjectionUpdate, 1),
        (OrgWorkflowPhase::Events, 1),
        (OrgWorkflowPhase::Events, 2),
        (OrgWorkflowPhase::OperationWrite, 1),
    ] {
        let (context, backend, _dir, workspace_id, claim) = claimed_item().await;
        let current = backend
            .session()
            .await
            .unwrap()
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        let operation_id = format!("raw-failure-{phase:?}-{occurrence}");
        let request = PutDocumentRequest {
            document_id: document_id(),
            path: current.path.clone(),
            source: current.source.replace("RUNNING", "FAILED"),
            expected_revision: Some(2),
            lease_proofs: BTreeMap::from([(
                item_id(),
                LeaseProofInput {
                    lease_id: claim.lease_id.clone(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: claim.fencing_token.clone(),
                },
            )]),
        };
        let session = backend.session().await.unwrap();
        let event_count = session
            .list_org_events(workspace_id, None, 100)
            .await
            .unwrap()
            .len();
        drop(session);
        let fault_context =
            context
                .clone()
                .with_workflow_test_hook(Arc::new(FailAtWorkflowPhaseOccurrence::new(
                    phase, occurrence,
                )));
        let error = put_document(
            &fault_context,
            &envelope(workspace_id, "agent", &operation_id),
            &request,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code,
            OrgErrorCode::StorageFailure,
            "phase {phase:?} occurrence {occurrence}"
        );

        let session = backend.session().await.unwrap();
        let document = session
            .get_org_document(document_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(document.revision, 2, "phase {phase:?}");
        assert!(document.source.contains("RUNNING"), "phase {phase:?}");
        let lease = session
            .get_open_org_lease_internal(item_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.id, claim.lease_id, "phase {phase:?}");
        let attempt = session
            .list_org_attempts(item_id())
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(attempt.status, OrgAttemptStatus::Running, "phase {phase:?}");
        assert_eq!(
            session
                .list_org_events(workspace_id, None, 100)
                .await
                .unwrap()
                .len(),
            event_count,
            "phase {phase:?}"
        );
        assert!(session
            .get_org_operation(workspace_id, &operation_id)
            .await
            .unwrap()
            .is_none());
        drop(session);

        put_document(
            &context,
            &envelope(workspace_id, "agent", &operation_id),
            &request,
        )
        .await
        .unwrap();
    }
}
