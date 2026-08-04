mod support;

use note_org::{DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    claim_item, heartbeat_claim, put_document, release_claim, CommandEnvelope, FixedOrgClock,
    HeartbeatClaimRequest, OrgClaimKind, OrgClaimPhase, OrgContext, OrgErrorCode,
    PutDocumentRequest, ReleaseClaimRequest, StartClaimRequest,
};
use note_storage::{
    CompareAndSwap, ConditionalUpdate, NewOrgAttempt, NewOrgAttemptAllocation, NewOrgLease,
    OrgArtifactReference, OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate,
    OrgEventType, OrgLeaseEndReason, OrgLeaseKind, OrgWorkspaceUpdate, StorageBackend,
    TransactionMode,
};
use sha2::{Digest, Sha256};
use std::str::FromStr as _;
use std::sync::Arc;
use support::{DeterministicTokenSource, FailOnceAtPhase};

const NOW: i64 = 1_900_000_000;

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap()
}

fn document_id(value: u8) -> DocumentId {
    DocumentId::from_str(&format!("71000000-0000-4000-8000-{value:012}")).unwrap()
}

fn item_id(value: u8) -> WorkItemId {
    WorkItemId::from_str(&format!("72000000-0000-4000-8000-{value:012}")).unwrap()
}

fn envelope(actor: &str, operation: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id: workspace_id(),
        actor_id: actor.into(),
        operation_id: operation.into(),
    }
}

fn source(item: WorkItemId, state: &str, properties: &str, planning: &str) -> String {
    format!(
        "* {state} Work item\r\n{planning}:PROPERTIES:\r\n:ID: {item}\r\n:AGENT_NOTE_TYPE: task\r\n{properties}:END:\r\n"
    )
}

async fn seed_document(
    context: &OrgContext,
    document: DocumentId,
    item: WorkItemId,
    state: &str,
    properties: &str,
    planning: &str,
) {
    put_document(
        context,
        &envelope("seed-agent", &format!("seed-{document}")),
        &PutDocumentRequest {
            document_id: document,
            path: format!("{document}.org"),
            source: source(item, state, properties, planning),
            expected_revision: None,
            lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
}

async fn context_with_tokens(
    now: i64,
    tokens: &[&str],
) -> (OrgContext, Arc<dyn StorageBackend>, tempfile::TempDir) {
    let (context, backend, dir, _path, _workspace) = support::org_test_context(now).await;
    (
        context.with_token_source(Arc::new(DeterministicTokenSource::new(
            tokens.iter().copied(),
        ))),
        backend,
        dir,
    )
}

async fn update_workspace(
    backend: &Arc<dyn StorageBackend>,
    policy: WorkspacePolicy,
    archived_at: Option<i64>,
) {
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
                archived_at,
                updated_at: NOW,
            })
            .await
            .unwrap(),
        CompareAndSwap::Applied(_)
    ));
}

async fn seed_review_attempt(
    backend: &Arc<dyn StorageBackend>,
    item: WorkItemId,
    attempt_id: &str,
) {
    backend
        .session()
        .await
        .unwrap()
        .insert_org_attempt(note_storage::NewOrgAttempt {
            id: attempt_id,
            workspace_id: workspace_id(),
            work_item_id: item,
            attempt_number: 1,
            actor_id: "author",
            status: OrgAttemptStatus::Submitted,
            started_at: NOW - 10,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn execution_claim_is_atomic_context_complete_and_replays_exact_token() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["raw-token-one"]).await;
    let document = document_id(1);
    let item = item_id(1);
    seed_document(&context, document, item, "READY", "", "").await;
    let request = StartClaimRequest {
        work_item_id: item,
        document_id: document,
        expected_document_revision: 1,
        kind: OrgClaimKind::Execution,
    };

    let claimed = claim_item(&context, &envelope("agent-one", "claim-one"), &request)
        .await
        .unwrap();
    assert_eq!(claimed.fencing_token, "raw-token-one");
    assert_eq!(claimed.context.item.state.as_deref(), Some("RUNNING"));
    assert_eq!(claimed.context.document.revision, 2);
    assert_eq!(claimed.context.attempts.len(), 1);
    assert_eq!(claimed.context.attempts[0].status, "running");
    assert_eq!(claimed.context.lease.as_ref().unwrap().kind, "execution");
    assert_eq!(claimed.context.lease.as_ref().unwrap().status, "active");
    assert!(!serde_json::to_string(&claimed.context)
        .unwrap()
        .contains("raw-token-one"));
    assert!(!format!("{claimed:?}").contains("raw-token-one"));

    let replay = claim_item(&context, &envelope("agent-one", "claim-one"), &request)
        .await
        .unwrap();
    assert_eq!(replay, claimed);
    let session = backend.session().await.unwrap();
    let stored_operation = session
        .get_org_operation(workspace_id(), "claim-one")
        .await
        .unwrap()
        .unwrap();
    let stored_debug = format!("{stored_operation:?}");
    assert!(!stored_debug.contains("raw-token-one"));
    assert!(!stored_debug.contains(&format!("{:x}", Sha256::digest(b"raw-token-one"))));
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 1);
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.clone())
            .collect::<Vec<_>>(),
        vec![OrgEventType::Claim, OrgEventType::Start]
    );
    assert!(events.iter().all(|event| {
        !event.summary.contains("raw-token-one")
            && !event.actor_id.contains("raw-token-one")
            && !event.metadata.to_string().contains("raw-token-one")
    }));
}

#[tokio::test]
async fn heartbeat_extends_only_the_lease_and_release_cancels_execution() {
    let (claim_context, backend, _dir) = context_with_tokens(NOW, &["raw-token-heartbeat"]).await;
    let document = document_id(2);
    let item = item_id(2);
    seed_document(&claim_context, document, item, "READY", "", "").await;
    let claim = claim_item(
        &claim_context,
        &envelope("agent-one", "claim-heartbeat"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let heartbeat_context =
        OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(NOW + 100)));
    let heartbeat = heartbeat_claim(
        &heartbeat_context,
        &envelope("agent-one", "heartbeat-one"),
        &HeartbeatClaimRequest {
            work_item_id: item,
            lease_id: claim.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(heartbeat.document_revisions[&document.to_string()], 2);
    assert_eq!(heartbeat.data["lease"]["expires_at"], NOW + 100 + 900);
    let replayed_heartbeat = heartbeat_claim(
        &heartbeat_context,
        &envelope("agent-one", "heartbeat-one"),
        &HeartbeatClaimRequest {
            work_item_id: item,
            lease_id: claim.lease_id.clone(),
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(replayed_heartbeat, heartbeat);

    let release_context = OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(NOW + 101)));
    let released = release_claim(
        &release_context,
        &envelope("agent-one", "release-one"),
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: 2,
            lease_id: claim.lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token,
            target_state: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(released.data["context"]["item"]["state"], "READY");
    let session = backend.session().await.unwrap();
    let attempt = session.list_org_attempts(item).await.unwrap().remove(0);
    assert_eq!(attempt.status, OrgAttemptStatus::Cancelled);
    assert_eq!(
        session.list_org_lease_history(item).await.unwrap()[0].end_reason,
        Some(OrgLeaseEndReason::Release)
    );
    let release_event = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == OrgEventType::Release)
        .unwrap();
    assert_eq!(release_event.previous_state.as_deref(), Some("RUNNING"));
    assert_eq!(release_event.resulting_state.as_deref(), Some("READY"));
}

#[tokio::test]
async fn review_claim_uses_submitted_attempt_and_default_release_keeps_review() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["raw-review-token"]).await;
    let document = document_id(3);
    let item = item_id(3);
    seed_document(&context, document, item, "REVIEW", "", "").await;
    let session = backend.session().await.unwrap();
    session
        .insert_org_attempt(note_storage::NewOrgAttempt {
            id: "submitted-attempt",
            workspace_id: workspace_id(),
            work_item_id: item,
            attempt_number: 1,
            actor_id: "author",
            status: OrgAttemptStatus::Submitted,
            started_at: NOW - 10,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    let claimed = claim_item(
        &context,
        &envelope("reviewer", "review-claim"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed.context.document.revision, 1);
    assert_eq!(claimed.context.attempts.len(), 1);
    assert_eq!(claimed.context.lease.as_ref().unwrap().kind, "review");

    let released = release_claim(
        &context,
        &envelope("reviewer", "review-release"),
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            lease_id: claimed.lease_id,
            kind: OrgClaimKind::Review,
            fencing_token: claimed.fencing_token,
            target_state: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(released.document_revisions[&document.to_string()], 1);
    assert_eq!(
        session
            .get_org_attempt("submitted-attempt")
            .await
            .unwrap()
            .unwrap()
            .status,
        OrgAttemptStatus::Submitted
    );
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    let release_event = events
        .iter()
        .find(|event| event.event_type == OrgEventType::Release)
        .unwrap();
    assert_eq!(release_event.previous_state, None);
    assert_eq!(release_event.resulting_state, None);
    assert_eq!(release_event.metadata["target_state"], "REVIEW");
}

#[tokio::test]
async fn heartbeat_boundary_and_wrong_token_are_stale_and_secret_safe() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["boundary-secret-token"]).await;
    let document = document_id(6);
    let item = item_id(6);
    seed_document(&context, document, item, "READY", "", "").await;
    let claim = claim_item(
        &context,
        &envelope("agent", "boundary-claim"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let wrong = HeartbeatClaimRequest {
        work_item_id: item,
        lease_id: claim.lease_id.clone(),
        kind: OrgClaimKind::Execution,
        fencing_token: "wrong-secret-token".into(),
    };
    let wrong_error = heartbeat_claim(
        &context
            .clone()
            .with_clock(Arc::new(FixedOrgClock::new(NOW + 1))),
        &envelope("agent", "wrong-heartbeat"),
        &wrong,
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_error.code, OrgErrorCode::StaleLease);
    assert!(!serde_json::to_string(&wrong_error)
        .unwrap()
        .contains("wrong-secret-token"));
    assert!(!format!("{wrong:?}").contains("wrong-secret-token"));
    assert_eq!(
        release_claim(
            &context,
            &envelope("agent", "wrong-release-stale-revision"),
            &ReleaseClaimRequest {
                schema_version: 1,
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: "wrong-secret-token".into(),
                target_state: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );

    for (operation, lease_id, fencing_token) in [
        ("blank-lease-heartbeat", " ", claim.fencing_token.as_str()),
        ("blank-token-heartbeat", claim.lease_id.as_str(), " "),
        (
            "malformed-token-heartbeat",
            claim.lease_id.as_str(),
            " surrounded ",
        ),
    ] {
        assert_eq!(
            heartbeat_claim(
                &context,
                &envelope("agent", operation),
                &HeartbeatClaimRequest {
                    work_item_id: item,
                    lease_id: lease_id.into(),
                    kind: OrgClaimKind::Execution,
                    fencing_token: fencing_token.into(),
                },
            )
            .await
            .unwrap_err()
            .code,
            OrgErrorCode::StaleLease
        );
    }
    assert_eq!(
        release_claim(
            &context,
            &envelope("agent", "blank-token-release"),
            &ReleaseClaimRequest {
                schema_version: 1,
                work_item_id: item,
                document_id: document,
                expected_document_revision: 2,
                lease_id: claim.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: " ".into(),
                target_state: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );

    let boundary_error = heartbeat_claim(
        &context.with_clock(Arc::new(FixedOrgClock::new(claim.expires_at))),
        &envelope("agent", "boundary-heartbeat"),
        &HeartbeatClaimRequest {
            work_item_id: item,
            lease_id: claim.lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(boundary_error.code, OrgErrorCode::StaleLease);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .iter()
            .filter(|event| event.event_type == OrgEventType::Heartbeat)
            .count(),
        0
    );
}

#[tokio::test]
async fn heartbeat_and_release_operation_reuse_with_another_token_conflicts() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["digest-token"]).await;
    let document = document_id(10);
    let item = item_id(10);
    seed_document(&context, document, item, "READY", "", "").await;
    let claim = claim_item(
        &context,
        &envelope("agent", "digest-claim"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let heartbeat = HeartbeatClaimRequest {
        work_item_id: item,
        lease_id: claim.lease_id.clone(),
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token.clone(),
    };
    heartbeat_claim(&context, &envelope("agent", "digest-heartbeat"), &heartbeat)
        .await
        .unwrap();
    let mut another_heartbeat_token = heartbeat;
    another_heartbeat_token.fencing_token = "another-heartbeat-token".into();
    assert_eq!(
        heartbeat_claim(
            &context,
            &envelope("agent", "digest-heartbeat"),
            &another_heartbeat_token,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::IdempotencyConflict
    );

    let release = ReleaseClaimRequest {
        schema_version: 1,
        work_item_id: item,
        document_id: document,
        expected_document_revision: 2,
        lease_id: claim.lease_id,
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token,
        target_state: None,
    };
    release_claim(&context, &envelope("agent", "digest-release"), &release)
        .await
        .unwrap();
    let mut another_release_token = release;
    another_release_token.fencing_token = "another-release-token".into();
    assert_eq!(
        release_claim(
            &context,
            &envelope("agent", "digest-release"),
            &another_release_token,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::IdempotencyConflict
    );
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(workspace_id(), "digest-heartbeat")
        .await
        .unwrap()
        .is_some());
    assert!(session
        .get_org_operation(workspace_id(), "digest-release")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn expired_execution_claim_reclaims_with_new_attempt_and_token() {
    let (first_context, backend, _dir) =
        context_with_tokens(NOW, &["first-token", "second-token"]).await;
    let document = document_id(4);
    let item = item_id(4);
    seed_document(&first_context, document, item, "READY", "", "").await;
    let request = StartClaimRequest {
        work_item_id: item,
        document_id: document,
        expected_document_revision: 1,
        kind: OrgClaimKind::Execution,
    };
    let first = claim_item(
        &first_context,
        &envelope("agent-one", "first-claim"),
        &request,
    )
    .await
    .unwrap();
    let reclaim_context = first_context.with_clock(Arc::new(FixedOrgClock::new(NOW + 900)));
    let reclaimed = claim_item(
        &reclaim_context,
        &envelope("agent-two", "reclaim"),
        &StartClaimRequest {
            expected_document_revision: 2,
            ..request
        },
    )
    .await
    .unwrap();
    assert_ne!(reclaimed.fencing_token, first.fencing_token);
    assert_eq!(reclaimed.context.attempts.len(), 2);
    assert_eq!(reclaimed.context.attempts[0].status, "expired");
    assert_eq!(reclaimed.context.attempts[1].status, "running");
    assert_eq!(reclaimed.context.item.state.as_deref(), Some("RUNNING"));
    let replay = claim_item(
        &reclaim_context,
        &envelope("agent-two", "reclaim"),
        &StartClaimRequest {
            expected_document_revision: 2,
            ..request
        },
    )
    .await
    .unwrap();
    assert_eq!(replay, reclaimed);
    let session = backend.session().await.unwrap();
    assert!(!session
        .get_org_document(document)
        .await
        .unwrap()
        .unwrap()
        .source
        .contains("* READY Work item"));
    assert_eq!(session.list_org_lease_history(item).await.unwrap().len(), 2);
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 2);
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_execution_reclaim_rolls_back_expiry_and_replays_exactly_once() {
    let (context, backend, _dir) =
        context_with_tokens(NOW, &["first", "failed-reclaim", "retry-reclaim"]).await;
    let document = document_id(8);
    let item = item_id(8);
    seed_document(&context, document, item, "READY", "", "").await;
    let first = claim_item(
        &context,
        &envelope("agent-one", "reclaim-rollback-first"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let baseline_projection = backend
        .session()
        .await
        .unwrap()
        .list_org_document_projection(document)
        .await
        .unwrap();
    let reclaim = context
        .with_clock(Arc::new(FixedOrgClock::new(first.expires_at)))
        .with_claim_test_hook(Arc::new(FailOnceAtPhase::new(OrgClaimPhase::Events)));
    let request = StartClaimRequest {
        work_item_id: item,
        document_id: document,
        expected_document_revision: 2,
        kind: OrgClaimKind::Execution,
    };
    assert_eq!(
        claim_item(
            &reclaim,
            &envelope("agent-two", "reclaim-rollback"),
            &request,
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StorageFailure
    );
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        session
            .list_org_document_projection(document)
            .await
            .unwrap(),
        baseline_projection
    );
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 1);
    assert_eq!(
        session.list_org_attempts(item).await.unwrap()[0].status,
        OrgAttemptStatus::Running
    );
    let old_lease = session
        .get_open_org_lease_internal(item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_lease.ended_at, None);
    assert_eq!(old_lease.expiry_event_id, None);
    assert!(session
        .get_org_operation(workspace_id(), "reclaim-rollback")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .count(),
        0
    );
    drop(session);

    let retried = claim_item(
        &reclaim,
        &envelope("agent-two", "reclaim-rollback"),
        &request,
    )
    .await
    .unwrap();
    assert_eq!(retried.fencing_token, "retry-reclaim");
    assert_eq!(
        claim_item(
            &reclaim,
            &envelope("agent-two", "reclaim-rollback"),
            &request,
        )
        .await
        .unwrap(),
        retried
    );
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 2);
    assert_eq!(session.list_org_lease_history(item).await.unwrap().len(), 2);
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    for (event_type, expected) in [
        (OrgEventType::LeaseExpiry, 1),
        (OrgEventType::Claim, 2),
        (OrgEventType::Start, 2),
    ] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == event_type)
                .count(),
            expected
        );
    }
}

#[tokio::test]
async fn capacity_blocked_execution_reclaim_leaves_expiry_unbookkept() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["capacity-first"]).await;
    let document = document_id(9);
    let item = item_id(9);
    seed_document(&context, document, item, "READY", "", "").await;
    let first = claim_item(
        &context,
        &envelope("agent-one", "capacity-reclaim-first"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = 0;
    update_workspace(&backend, policy, None).await;
    assert_eq!(
        claim_item(
            &context.with_clock(Arc::new(FixedOrgClock::new(first.expires_at))),
            &envelope("agent-two", "capacity-reclaim"),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 2,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::ConcurrencyLimit
    );
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 1);
    let lease = session
        .get_open_org_lease_internal(item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.ended_at, None);
    assert_eq!(lease.expiry_event_id, None);
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .count(),
        0
    );
    assert!(session
        .get_org_operation(workspace_id(), "capacity-reclaim")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn claim_validates_schedule_assignment_revision_and_stale_token_safely() {
    let (context, _backend, _dir) = context_with_tokens(NOW, &["unused"]).await;
    let document = document_id(5);
    let item = item_id(5);
    seed_document(
        &context,
        document,
        item,
        "READY",
        ":ASSIGNEE: agent-one\r\n",
        "SCHEDULED: <2035-01-01 Mon 00:00>\r\n",
    )
    .await;
    for (actor, revision, expected, expected_blocker) in [
        ("agent-one", 2, OrgErrorCode::StaleRevision, None),
        (
            "agent-two",
            1,
            OrgErrorCode::InvalidTransition,
            Some("AssignedToOtherActor"),
        ),
        (
            "agent-one",
            1,
            OrgErrorCode::InvalidTransition,
            Some("ScheduledForFuture"),
        ),
    ] {
        let error = claim_item(
            &context,
            &envelope(actor, &format!("blocked-{actor}-{revision}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: revision,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, expected);
        if let Some(blocker) = expected_blocker {
            assert!(error.details.to_string().contains(blocker));
        }
    }
}

#[tokio::test]
async fn expired_execution_reclaim_enforces_retry_budget_before_bookkeeping() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["only-attempt"]).await;
    let mut policy = WorkspacePolicy::engineering_default();
    policy.retry_limit = 0;
    update_workspace(&backend, policy, None).await;
    let document = document_id(7);
    let item = item_id(7);
    seed_document(&context, document, item, "READY", "", "").await;
    let claim = claim_item(
        &context,
        &envelope("agent", "retry-budget-first"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let error = claim_item(
        &context.with_clock(Arc::new(FixedOrgClock::new(claim.expires_at))),
        &envelope("agent", "retry-budget-reclaim"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 2,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::RetryLimit);
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 1);
    assert!(session
        .get_open_org_lease_internal(item)
        .await
        .unwrap()
        .unwrap()
        .ended_at
        .is_none());
    assert_eq!(
        session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .count(),
        0
    );
}

#[tokio::test]
async fn failure_after_each_claim_phase_rolls_back_and_same_operation_retries() {
    for (index, phase) in [
        OrgClaimPhase::SourceEdit,
        OrgClaimPhase::AttemptAllocation,
        OrgClaimPhase::LeaseInsert,
        OrgClaimPhase::Events,
        OrgClaimPhase::ContextHydration,
        OrgClaimPhase::OperationWrite,
    ]
    .into_iter()
    .enumerate()
    {
        let (context, backend, _dir) =
            context_with_tokens(NOW, &["failed-token", "retry-token"]).await;
        let document = document_id(20 + index as u8);
        let item = item_id(20 + index as u8);
        seed_document(&context, document, item, "READY", "", "").await;
        let baseline_projection = backend
            .session()
            .await
            .unwrap()
            .list_org_document_projection(document)
            .await
            .unwrap();
        let context = context.with_claim_test_hook(Arc::new(FailOnceAtPhase::new(phase)));
        let request = StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        };
        let operation = format!("failure-{index}");
        assert_eq!(
            claim_item(&context, &envelope("agent", &operation), &request)
                .await
                .unwrap_err()
                .code,
            OrgErrorCode::StorageFailure
        );
        let session = backend.session().await.unwrap();
        assert_eq!(
            session
                .get_org_document(document)
                .await
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        assert_eq!(session.count_org_attempts(item).await.unwrap(), 0);
        assert!(session
            .get_open_org_lease_internal(item)
            .await
            .unwrap()
            .is_none());
        assert!(session
            .get_org_operation(workspace_id(), &operation)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            session
                .list_org_document_projection(document)
                .await
                .unwrap(),
            baseline_projection
        );
        assert!(session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .iter()
            .all(|event| !matches!(event.event_type, OrgEventType::Claim | OrgEventType::Start)));
        drop(session);
        let retried = claim_item(&context, &envelope("agent", &operation), &request)
            .await
            .unwrap();
        assert_eq!(retried.fencing_token, "retry-token");
    }
}

#[tokio::test]
async fn expired_review_lease_is_bookkept_without_closing_submitted_attempt() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["review-one", "review-two"]).await;
    let document = document_id(40);
    let item = item_id(40);
    seed_document(&context, document, item, "REVIEW", "", "").await;
    seed_review_attempt(&backend, item, "review-attempt-expiry").await;
    let first = claim_item(
        &context,
        &envelope("reviewer-one", "review-expiry-first"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    let expired = context.with_clock(Arc::new(FixedOrgClock::new(first.expires_at)));
    let second = claim_item(
        &expired,
        &envelope("reviewer-two", "review-expiry-second"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Review,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.context.document.revision, 1);
    assert_eq!(second.context.attempts.len(), 1);
    assert_eq!(second.context.attempts[0].status, "submitted");
    let session = backend.session().await.unwrap();
    assert_eq!(session.list_org_lease_history(item).await.unwrap().len(), 2);
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .count(),
        1
    );
}

#[tokio::test]
async fn expired_release_commits_only_idempotent_expiry_bookkeeping() {
    for (index, kind, initial_state, actor, expected_attempt) in [
        (
            0_u8,
            OrgClaimKind::Execution,
            "READY",
            "executor",
            OrgAttemptStatus::Expired,
        ),
        (
            1_u8,
            OrgClaimKind::Review,
            "REVIEW",
            "reviewer",
            OrgAttemptStatus::Submitted,
        ),
    ] {
        let token = format!("expired-release-{index}");
        let (context, backend, _dir) = context_with_tokens(NOW, &[&token]).await;
        let document = document_id(43 + index);
        let item = item_id(43 + index);
        seed_document(&context, document, item, initial_state, "", "").await;
        if kind == OrgClaimKind::Review {
            seed_review_attempt(&backend, item, &format!("expired-release-attempt-{index}")).await;
        }
        let claim = claim_item(
            &context,
            &envelope(actor, &format!("expired-release-claim-{index}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind,
            },
        )
        .await
        .unwrap();
        let session = backend.session().await.unwrap();
        let baseline_document = session.get_org_document(document).await.unwrap().unwrap();
        let baseline_projection = session
            .list_org_document_projection(document)
            .await
            .unwrap();
        drop(session);
        let operation = format!("expired-release-command-{index}");
        let request = ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: baseline_document.revision,
            lease_id: claim.lease_id,
            kind,
            fencing_token: claim.fencing_token,
            target_state: Some("BLOCKED".into()),
        };
        let expired = context.with_clock(Arc::new(FixedOrgClock::new(claim.expires_at)));
        for _ in 0..2 {
            assert_eq!(
                release_claim(&expired, &envelope(actor, &operation), &request)
                    .await
                    .unwrap_err()
                    .code,
                OrgErrorCode::StaleLease
            );
        }

        let session = backend.session().await.unwrap();
        let document_after = session.get_org_document(document).await.unwrap().unwrap();
        assert_eq!(document_after.revision, baseline_document.revision);
        assert_eq!(document_after.source, baseline_document.source);
        assert_eq!(
            session
                .list_org_document_projection(document)
                .await
                .unwrap(),
            baseline_projection
        );
        assert_eq!(
            session.list_org_attempts(item).await.unwrap()[0].status,
            expected_attempt
        );
        let lease = &session.list_org_lease_history(item).await.unwrap()[0];
        assert_eq!(lease.end_reason, Some(OrgLeaseEndReason::LeaseExpiry));
        assert!(lease.expiry_event_id.is_some());
        let events = session
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap();
        let expiry_events = events
            .iter()
            .filter(|event| event.event_type == OrgEventType::LeaseExpiry)
            .collect::<Vec<_>>();
        assert_eq!(expiry_events.len(), 1);
        assert_eq!(expiry_events[0].actor_id, "system");
        let expected_state = if kind == OrgClaimKind::Execution {
            "RUNNING"
        } else {
            "REVIEW"
        };
        assert_eq!(
            expiry_events[0].previous_state.as_deref(),
            Some(expected_state)
        );
        assert_eq!(
            expiry_events[0].resulting_state.as_deref(),
            Some(expected_state)
        );
        assert_eq!(
            lease.expiry_event_id.as_deref(),
            Some(expiry_events[0].id.as_str())
        );
        assert!(!events
            .iter()
            .any(|event| event.event_type == OrgEventType::Release));
        assert!(session
            .get_org_operation(workspace_id(), &operation)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn claim_enforces_dependencies_archival_and_capacity() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["capacity-token"]).await;
    let document = document_id(41);
    let dependency = item_id(41);
    let dependent = item_id(42);
    put_document(
        &context,
        &envelope("seed-agent", "seed-dependency"),
        &PutDocumentRequest {
            document_id: document,
            path: "dependency.org".into(),
            source: format!(
                "* BACKLOG Dependency\r\n:PROPERTIES:\r\n:ID: {dependency}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n* READY Dependent\r\n:PROPERTIES:\r\n:ID: {dependent}\r\n:AGENT_NOTE_TYPE: task\r\n:DEPENDS_ON: {dependency}\r\n:END:\r\n"
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let blocked = claim_item(
        &context,
        &envelope("agent", "dependency-blocked"),
        &StartClaimRequest {
            work_item_id: dependent,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(blocked.code, OrgErrorCode::DependencyBlocked);

    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = 0;
    update_workspace(&backend, policy.clone(), None).await;
    let capacity_error = claim_item(
        &context,
        &envelope("agent", "capacity-blocked"),
        &StartClaimRequest {
            work_item_id: dependency,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(capacity_error.code, OrgErrorCode::ConcurrencyLimit);

    update_workspace(&backend, policy, Some(NOW)).await;
    let archived = claim_item(
        &context,
        &envelope("agent", "archived-blocked"),
        &StartClaimRequest {
            work_item_id: dependency,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(archived.code, OrgErrorCode::ArchivedWorkspace);
}

#[tokio::test]
async fn release_target_matrix_and_target_idempotency_are_enforced() {
    for (index, target) in ["BLOCKED", "FAILED", "CANCELLED"].into_iter().enumerate() {
        let (context, backend, _dir) =
            context_with_tokens(NOW, &[&format!("release-valid-{index}")]).await;
        let document = document_id(50 + index as u8);
        let item = item_id(50 + index as u8);
        seed_document(&context, document, item, "READY", "", "").await;
        let claim = claim_item(
            &context,
            &envelope("agent", &format!("release-valid-claim-{index}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap();
        let released = release_claim(
            &context,
            &envelope("agent", &format!("release-valid-{index}")),
            &ReleaseClaimRequest {
                schema_version: 1,
                work_item_id: item,
                document_id: document,
                expected_document_revision: 2,
                lease_id: claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: claim.fencing_token,
                target_state: Some(target.into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(released.data["context"]["item"]["state"], target);
        assert_eq!(
            backend
                .session()
                .await
                .unwrap()
                .list_org_attempts(item)
                .await
                .unwrap()[0]
                .status,
            OrgAttemptStatus::Cancelled
        );
        let release_event = backend
            .session()
            .await
            .unwrap()
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == OrgEventType::Release)
            .unwrap();
        assert_eq!(release_event.previous_state.as_deref(), Some("RUNNING"));
        assert_eq!(release_event.resulting_state.as_deref(), Some(target));
    }

    for (index, target) in ["RUNNING", "REVIEW", "DONE", "MISSING"]
        .into_iter()
        .enumerate()
    {
        let (context, _backend, _dir) =
            context_with_tokens(NOW, &[&format!("release-invalid-{index}")]).await;
        let document = document_id(60 + index as u8);
        let item = item_id(60 + index as u8);
        seed_document(&context, document, item, "READY", "", "").await;
        let claim = claim_item(
            &context,
            &envelope("agent", &format!("release-invalid-claim-{index}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind: OrgClaimKind::Execution,
            },
        )
        .await
        .unwrap();
        let request = ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: 2,
            lease_id: claim.lease_id,
            kind: OrgClaimKind::Execution,
            fencing_token: claim.fencing_token,
            target_state: Some(target.into()),
        };
        assert_eq!(
            release_claim(
                &context,
                &envelope("agent", &format!("release-invalid-{index}")),
                &request,
            )
            .await
            .unwrap_err()
            .code,
            OrgErrorCode::InvalidTransition
        );
    }

    let (context, _backend, _dir) = context_with_tokens(NOW, &["idempotency-target"]).await;
    let document = document_id(70);
    let item = item_id(70);
    seed_document(&context, document, item, "READY", "", "").await;
    let claim = claim_item(
        &context,
        &envelope("agent", "target-idempotency-claim"),
        &StartClaimRequest {
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let mut release = ReleaseClaimRequest {
        schema_version: 1,
        work_item_id: item,
        document_id: document,
        expected_document_revision: 2,
        lease_id: claim.lease_id,
        kind: OrgClaimKind::Execution,
        fencing_token: claim.fencing_token,
        target_state: Some("BLOCKED".into()),
    };
    let mut wrong_proof = release.clone();
    wrong_proof.expected_document_revision = 999;
    wrong_proof.fencing_token = "wrong-release-secret".into();
    let stale = release_claim(
        &context,
        &envelope("agent", "wrong-release-proof"),
        &wrong_proof,
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, OrgErrorCode::StaleLease);
    assert!(!serde_json::to_string(&stale)
        .unwrap()
        .contains("wrong-release-secret"));
    release_claim(&context, &envelope("agent", "same-release"), &release)
        .await
        .unwrap();
    release.target_state = Some("FAILED".into());
    assert_eq!(
        release_claim(&context, &envelope("agent", "same-release"), &release)
            .await
            .unwrap_err()
            .code,
        OrgErrorCode::IdempotencyConflict
    );
}

#[tokio::test]
async fn execution_release_revalidates_dependencies_before_entering_ready() {
    let (context, backend, _dir) = context_with_tokens(NOW, &[]).await;
    let document = document_id(71);
    let dependency = item_id(71);
    let item = item_id(72);
    put_document(
        &context,
        &envelope("seed-agent", "seed-release-dependency"),
        &PutDocumentRequest {
            document_id: document,
            path: "release-dependency.org".into(),
            source: format!(
                "* BACKLOG Dependency\r\n:PROPERTIES:\r\n:ID: {dependency}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n* RUNNING Work\r\n:PROPERTIES:\r\n:ID: {item}\r\n:AGENT_NOTE_TYPE: task\r\n:DEPENDS_ON: {dependency}\r\n:END:\r\n"
            ),
            expected_revision: None,
        lease_proofs: std::collections::BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let raw_token = "dependency-release-token";
    let token_hash = format!("{:x}", Sha256::digest(raw_token.as_bytes()));
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    let attempt = transaction
        .allocate_next_org_attempt(NewOrgAttemptAllocation {
            id: "dependency-release-attempt",
            workspace_id: workspace_id(),
            work_item_id: item,
            actor_id: "agent",
            started_at: NOW,
            note_refs: &[],
            artifacts: &[],
            metadata: &serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(matches!(
        transaction
            .insert_org_lease_if_capacity(
                NewOrgLease {
                    id: "dependency-release-lease",
                    workspace_id: workspace_id(),
                    work_item_id: item,
                    attempt_id: &attempt.id,
                    kind: OrgLeaseKind::Execution,
                    actor_id: "agent",
                    fencing_token_hash: &token_hash,
                    acquired_at: NOW,
                    last_heartbeat_at: NOW,
                    expires_at: NOW + 900,
                },
                4,
                NOW,
            )
            .await
            .unwrap(),
        note_storage::ConditionalUpdate::Applied(_)
    ));
    transaction.commit().await.unwrap();

    let error = release_claim(
        &context,
        &envelope("agent", "release-dependency-blocked"),
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: item,
            document_id: document,
            expected_document_revision: 1,
            lease_id: "dependency-release-lease".into(),
            kind: OrgClaimKind::Execution,
            fencing_token: raw_token.into(),
            target_state: Some("READY".into()),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, OrgErrorCode::DependencyBlocked);
    let session = backend.session().await.unwrap();
    assert!(session
        .get_active_org_lease(item, NOW)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        session
            .get_org_document(document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn changed_review_release_closes_attempt_with_matrix_status() {
    for (index, target, expected) in [
        (0_u8, "READY", OrgAttemptStatus::Failed),
        (1_u8, "CANCELLED", OrgAttemptStatus::Cancelled),
    ] {
        let (context, backend, _dir) =
            context_with_tokens(NOW, &[&format!("review-release-{index}")]).await;
        let document = document_id(80 + index);
        let item = item_id(80 + index);
        seed_document(&context, document, item, "REVIEW", "", "").await;
        let attempt_id = format!("review-release-attempt-{index}");
        let note_refs = vec![OrgAttemptNoteReference {
            purpose: "result".into(),
            note_id: format!("review-note-{index}"),
            description: "review result note".into(),
        }];
        let artifacts = vec![OrgArtifactReference {
            uri: format!("artifact://review-release-{index}"),
            media_type: "text/plain".into(),
            name: "result.txt".into(),
            description: "review result artifact".into(),
        }];
        let metadata = serde_json::json!({"preserved": index});
        backend
            .session()
            .await
            .unwrap()
            .insert_org_attempt(NewOrgAttempt {
                id: &attempt_id,
                workspace_id: workspace_id(),
                work_item_id: item,
                attempt_number: 1,
                actor_id: "author",
                status: OrgAttemptStatus::Running,
                started_at: NOW - 2,
                note_refs: &note_refs,
                artifacts: &artifacts,
                metadata: &metadata,
            })
            .await
            .unwrap();
        assert!(matches!(
            backend
                .session()
                .await
                .unwrap()
                .update_org_attempt(OrgAttemptUpdate {
                    id: &attempt_id,
                    expected_status: OrgAttemptStatus::Running,
                    status: OrgAttemptStatus::Submitted,
                    ended_at: NOW - 1,
                    error: Some("pre-release error"),
                    result_summary: Some("pre-release result"),
                    review_outcome: Some("pending"),
                    note_refs: &note_refs,
                    artifacts: &artifacts,
                    metadata: &metadata,
                })
                .await
                .unwrap(),
            ConditionalUpdate::Applied(_)
        ));
        let before_attempt = backend
            .session()
            .await
            .unwrap()
            .get_org_attempt(&attempt_id)
            .await
            .unwrap()
            .unwrap();
        let claim = claim_item(
            &context,
            &envelope("reviewer", &format!("review-release-claim-{index}")),
            &StartClaimRequest {
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                kind: OrgClaimKind::Review,
            },
        )
        .await
        .unwrap();
        release_claim(
            &context,
            &envelope("reviewer", &format!("review-release-command-{index}")),
            &ReleaseClaimRequest {
                schema_version: 1,
                work_item_id: item,
                document_id: document,
                expected_document_revision: 1,
                lease_id: claim.lease_id,
                kind: OrgClaimKind::Review,
                fencing_token: claim.fencing_token,
                target_state: Some(target.into()),
            },
        )
        .await
        .unwrap();
        let attempt = backend
            .session()
            .await
            .unwrap()
            .get_org_attempt(&attempt_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempt.status, expected);
        assert_eq!(attempt.review_outcome.as_deref(), Some("released"));
        assert_eq!(attempt.error.as_deref(), Some("pre-release error"));
        assert_eq!(
            attempt.result_summary.as_deref(),
            Some("pre-release result")
        );
        assert_eq!(attempt.note_refs, note_refs);
        assert_eq!(attempt.artifacts, artifacts);
        assert_eq!(attempt.metadata, metadata);
        assert_eq!(attempt.id, before_attempt.id);
        assert_eq!(attempt.workspace_id, before_attempt.workspace_id);
        assert_eq!(attempt.work_item_id, before_attempt.work_item_id);
        assert_eq!(attempt.attempt_number, before_attempt.attempt_number);
        assert_eq!(attempt.actor_id, before_attempt.actor_id);
        assert_eq!(attempt.started_at, before_attempt.started_at);
        assert_ne!(attempt.ended_at, before_attempt.ended_at);
        let release_event = backend
            .session()
            .await
            .unwrap()
            .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == OrgEventType::Release)
            .unwrap();
        assert_eq!(release_event.previous_state.as_deref(), Some("REVIEW"));
        assert_eq!(release_event.resulting_state.as_deref(), Some(target));
    }
}

#[tokio::test]
async fn concurrent_same_item_claim_has_one_winner_and_no_loser_side_effects() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["race-a", "race-b"]).await;
    let document = document_id(90);
    let item = item_id(90);
    seed_document(&context, document, item, "READY", "", "").await;
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for actor in ["racer-a", "racer-b"] {
        let context = context.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            let operation = format!("same-item-{actor}");
            barrier.wait().await;
            let result = claim_item(
                &context,
                &envelope(actor, &operation),
                &StartClaimRequest {
                    work_item_id: item,
                    document_id: document,
                    expected_document_revision: 1,
                    kind: OrgClaimKind::Execution,
                },
            )
            .await;
            (operation, result)
        }));
    }
    barrier.wait().await;
    let results = futures_join(handles).await;
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .find_map(|(_, result)| result.as_ref().err())
            .unwrap()
            .code,
        OrgErrorCode::ActiveLease
    );
    let loser_operation = results
        .iter()
        .find_map(|(operation, result)| result.is_err().then_some(operation))
        .unwrap();
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(workspace_id(), loser_operation)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_org_attempts(item).await.unwrap(), 1);
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", &item.to_string(), None, 20)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn concurrent_different_item_claim_at_limit_has_capacity_loser_without_side_effects() {
    let (context, backend, _dir) = context_with_tokens(NOW, &["limit-a", "limit-b"]).await;
    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = 1;
    update_workspace(&backend, policy, None).await;
    let first_document = document_id(91);
    let second_document = document_id(92);
    let first_item = item_id(91);
    let second_item = item_id(92);
    seed_document(&context, first_document, first_item, "READY", "", "").await;
    seed_document(&context, second_document, second_item, "READY", "", "").await;
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for (actor, operation, document, item) in [
        ("limit-a", "limit-operation-a", first_document, first_item),
        ("limit-b", "limit-operation-b", second_document, second_item),
    ] {
        let context = context.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let result = claim_item(
                &context,
                &envelope(actor, operation),
                &StartClaimRequest {
                    work_item_id: item,
                    document_id: document,
                    expected_document_revision: 1,
                    kind: OrgClaimKind::Execution,
                },
            )
            .await;
            (operation.to_string(), item, document, result)
        }));
    }
    barrier.wait().await;
    let results = futures_join(handles).await;
    assert_eq!(
        results
            .iter()
            .filter(|(_, _, _, result)| result.is_ok())
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .find_map(|(_, _, _, result)| result.as_ref().err())
            .unwrap()
            .code,
        OrgErrorCode::ConcurrencyLimit
    );
    let winning_item = results
        .iter()
        .find_map(|(_, item, _, result)| result.is_ok().then_some(*item))
        .unwrap();
    let loser_operation = results
        .iter()
        .find_map(|(operation, _, _, result)| result.is_err().then_some(operation))
        .unwrap();
    let losing_item = if winning_item == first_item {
        second_item
    } else {
        first_item
    };
    let losing_document = if losing_item == first_item {
        first_document
    } else {
        second_document
    };
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(workspace_id(), loser_operation)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_org_attempts(losing_item).await.unwrap(), 0);
    assert_eq!(
        session
            .get_org_document(losing_document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert!(session
        .get_open_org_lease_internal(losing_item)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .list_org_subject_events(
            workspace_id(),
            "work_item",
            &losing_item.to_string(),
            None,
            20
        )
        .await
        .unwrap()
        .is_empty());
}

async fn futures_join<T>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    results
}
