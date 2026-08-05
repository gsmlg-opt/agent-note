mod support;

use async_trait::async_trait;
use futures_util::FutureExt as _;
use note_org::{DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    archive_workspace, claim_item, create_workspace, heartbeat_claim, move_document, put_document,
    query_queue, release_claim, retry_item, transition_item, ArchiveWorkspaceRequest,
    CommandEnvelope, CreateWorkspaceRequest, FixedOrgClock, FixedOrgCursorSigner,
    HeartbeatClaimRequest, LeaseProofInput, MoveDocumentRequest, OperationalQuery, OperationalView,
    OrgClaimKind, OrgContext, OrgError, OrgErrorCode, OrgTokenSource, PutDocumentRequest,
    ReleaseClaimRequest, RetryItemRequest, StartClaimRequest, TransitionItemRequest,
};
use note_storage::{NewOrgAttempt, OrgAttemptStatus, OrgEventType, StorageBackend};
use note_storage_pg::PgStorage;
use note_storage_turso::TursoStorage;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::Arc;
use support::{BeginBarrierStorageBackend, DeterministicTokenSource, PgAcceptanceDatabase};

const NOW: i64 = 1_900_000_000;
const WORKSPACE: &str = "81000000-0000-4000-8000-000000000001";
const DOCUMENT: &str = "82000000-0000-4000-8000-000000000001";
const ITEM: &str = "83000000-0000-4000-8000-000000000001";
const READY_DOCUMENT: &str = "82000000-0000-4000-8000-000000000002";
const READY_ITEM: &str = "83000000-0000-4000-8000-000000000002";
const EXPIRED_DOCUMENT: &str = "82000000-0000-4000-8000-000000000003";
const EXPIRED_ITEM: &str = "83000000-0000-4000-8000-000000000003";
const REVIEW_DOCUMENT: &str = "82000000-0000-4000-8000-000000000004";
const REVIEW_ITEM: &str = "83000000-0000-4000-8000-000000000004";
const SAME_WORKSPACE: &str = "81000000-0000-4000-8000-000000000010";
const SAME_DOCUMENT: &str = "82000000-0000-4000-8000-000000000010";
const SAME_ITEM: &str = "83000000-0000-4000-8000-000000000010";
const CAPACITY_WORKSPACE: &str = "81000000-0000-4000-8000-000000000020";
const CAPACITY_DOCUMENT_A: &str = "82000000-0000-4000-8000-000000000020";
const CAPACITY_DOCUMENT_B: &str = "82000000-0000-4000-8000-000000000021";
const CAPACITY_ITEM_A: &str = "83000000-0000-4000-8000-000000000020";
const CAPACITY_ITEM_B: &str = "83000000-0000-4000-8000-000000000021";
const MOVE_SOURCE_WORKSPACE: &str = "81000000-0000-4000-8000-000000000030";
const MOVE_TARGET_WORKSPACE: &str = "81000000-0000-4000-8000-000000000031";
const MOVE_SOURCE_DOCUMENT: &str = "82000000-0000-4000-8000-000000000030";
const MOVE_TARGET_DOCUMENT: &str = "82000000-0000-4000-8000-000000000031";
const MOVE_SOURCE_ITEM: &str = "83000000-0000-4000-8000-000000000030";
const MOVE_TARGET_ITEM: &str = "83000000-0000-4000-8000-000000000031";

#[derive(Debug, PartialEq, Eq)]
struct NormalizedOutcome {
    state: String,
    document_revision: i64,
    attempts: i64,
    attempt_statuses: Vec<OrgAttemptStatus>,
    events: Vec<OrgEventType>,
    lease_count: usize,
    race_loser_codes: Vec<OrgErrorCode>,
    race_active_counts: Vec<i64>,
}

#[async_trait]
trait AcceptanceFixture: Send + Sync {
    async fn open(&self) -> Arc<dyn StorageBackend>;
}

struct TursoFixture {
    path: PathBuf,
}

#[async_trait]
impl AcceptanceFixture for TursoFixture {
    async fn open(&self) -> Arc<dyn StorageBackend> {
        Arc::new(TursoStorage::open(&self.path).await.unwrap())
    }
}

struct PgFixture {
    url: String,
}

#[async_trait]
impl AcceptanceFixture for PgFixture {
    async fn open(&self) -> Arc<dyn StorageBackend> {
        Arc::new(PgStorage::connect(&self.url, 10).await.unwrap())
    }
}

struct RejectingTokenSource;

impl OrgTokenSource for RejectingTokenSource {
    fn generate_token(&self) -> Result<String, OrgError> {
        panic!("operation replay generated a new fencing token")
    }
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_str(WORKSPACE).unwrap()
}

fn document_id() -> DocumentId {
    DocumentId::from_str(DOCUMENT).unwrap()
}

fn item_id() -> WorkItemId {
    WorkItemId::from_str(ITEM).unwrap()
}

fn envelope(actor: &str, operation: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id: workspace_id(),
        actor_id: actor.into(),
        operation_id: operation.into(),
    }
}

fn new_context(
    backend: Arc<dyn StorageBackend>,
    token_source: Arc<dyn OrgTokenSource>,
) -> OrgContext {
    OrgContext::new(backend, Arc::new(FixedOrgClock::new(NOW)))
        .with_token_source(token_source)
        .with_cursor_signer(Arc::new(FixedOrgCursorSigner::new([0x91; 32])))
}

async fn run_high_risk_scenario(fixture: &dyn AcceptanceFixture) -> NormalizedOutcome {
    let backend = fixture.open().await;
    let context = new_context(
        backend.clone(),
        Arc::new(DeterministicTokenSource::new([
            "acceptance-token",
            "reclaim-token",
            "retry-token",
            "expired-view-token",
        ])),
    );
    create_workspace(
        &context,
        &envelope("seed", "create-workspace"),
        &CreateWorkspaceRequest {
            slug: "acceptance".into(),
            display_name: "Acceptance".into(),
            description: "Task 9 shared acceptance".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    put_document(
        &context,
        &envelope("seed", "put-document"),
        &PutDocumentRequest {
            document_id: document_id(),
            path: "acceptance.org".into(),
            source: format!(
                "* READY Replay\r\n:PROPERTIES:\r\n:ID: {}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n",
                item_id()
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
    let request = StartClaimRequest {
        work_item_id: item_id(),
        document_id: document_id(),
        expected_document_revision: 1,
        kind: OrgClaimKind::Execution,
    };
    let claimed = claim_item(&context, &envelope("agent-one", "claim-replay"), &request)
        .await
        .unwrap();
    let boundary = context
        .clone()
        .with_clock(Arc::new(FixedOrgClock::new(claimed.expires_at)));
    assert_eq!(
        query_queue(
            &boundary,
            &OperationalQuery::for_workspace(workspace_id(), OperationalView::ExpiredLease),
        )
        .await
        .unwrap()
        .items
        .iter()
        .map(|item| item.item.id)
        .collect::<Vec<_>>(),
        vec![item_id()]
    );
    assert_eq!(
        heartbeat_claim(
            &boundary,
            &envelope("agent-one", "heartbeat-at-expiry"),
            &HeartbeatClaimRequest {
                work_item_id: item_id(),
                lease_id: claimed.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claimed.fencing_token.clone(),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );
    let reclaimed = claim_item(
        &boundary,
        &envelope("agent-two", "reclaim-expired"),
        &StartClaimRequest {
            expected_document_revision: 2,
            ..request.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        heartbeat_claim(
            &boundary,
            &envelope("agent-one", "stale-old-token"),
            &HeartbeatClaimRequest {
                work_item_id: item_id(),
                lease_id: claimed.lease_id.clone(),
                kind: OrgClaimKind::Execution,
                fencing_token: claimed.fencing_token.clone(),
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::StaleLease
    );
    let workspace_revision = backend
        .session()
        .await
        .unwrap()
        .get_org_workspace(workspace_id())
        .await
        .unwrap()
        .unwrap()
        .revision;
    assert_eq!(
        archive_workspace(
            &boundary,
            &envelope("operator", "archive-active"),
            &ArchiveWorkspaceRequest {
                expected_revision: workspace_revision,
            },
        )
        .await
        .unwrap_err()
        .code,
        OrgErrorCode::ActiveLease
    );
    let failed = transition_item(
        &boundary,
        &envelope("agent-two", "fail-reclaimed"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: reclaimed.context.document.revision,
            target_state: "FAILED".into(),
            lease: Some(LeaseProofInput {
                lease_id: reclaimed.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: reclaimed.fencing_token,
            }),
            error: Some("acceptance failure".into()),
            metadata: serde_json::json!({"scenario": "task-9"}),
        },
    )
    .await
    .unwrap();
    let failed_revision = failed.document_revisions[DOCUMENT];
    let retried = retry_item(
        &boundary,
        &envelope("agent-two", "retry-failed"),
        &RetryItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: failed_revision,
        },
    )
    .await
    .unwrap();
    transition_item(
        &boundary,
        &envelope("agent-two", "complete-retry"),
        &TransitionItemRequest {
            schema_version: 1,
            work_item_id: item_id(),
            document_id: document_id(),
            expected_document_revision: retried.context.document.revision,
            target_state: "DONE".into(),
            lease: Some(LeaseProofInput {
                lease_id: retried.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: retried.fencing_token,
            }),
            error: None,
            metadata: serde_json::json!({}),
        },
    )
    .await
    .unwrap();

    seed_document(&context, READY_DOCUMENT, READY_ITEM, "READY", "ready-view").await;
    seed_document(
        &context,
        EXPIRED_DOCUMENT,
        EXPIRED_ITEM,
        "READY",
        "expired-view",
    )
    .await;
    let expired_document = DocumentId::from_str(EXPIRED_DOCUMENT).unwrap();
    let expired_item = WorkItemId::from_str(EXPIRED_ITEM).unwrap();
    let expired_claim = claim_item(
        &context,
        &CommandEnvelope {
            workspace_id: workspace_id(),
            actor_id: "expiry-agent".into(),
            operation_id: "claim-expired-view".into(),
            schema_version: 1,
        },
        &StartClaimRequest {
            work_item_id: expired_item,
            document_id: expired_document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let view_context = context
        .clone()
        .with_clock(Arc::new(FixedOrgClock::new(expired_claim.expires_at)));
    assert_view(
        &view_context,
        OperationalView::Ready,
        &[READY_ITEM, EXPIRED_ITEM],
    )
    .await;
    assert_view(
        &view_context,
        OperationalView::ExpiredLease,
        &[EXPIRED_ITEM],
    )
    .await;
    assert_view(&view_context, OperationalView::Completed, &[ITEM]).await;
    prove_reviewer_exclusivity(backend.clone(), &context).await;
    let (mut race_loser_codes, mut race_active_counts) =
        prove_execution_claim_races(backend.clone()).await;
    let (move_loser_code, move_active_count) = prove_move_claim_race(backend.clone()).await;
    race_loser_codes.push(move_loser_code);
    race_active_counts.push(move_active_count);
    let before = snapshot(backend.as_ref()).await;

    drop(view_context);
    drop(boundary);
    drop(context);
    drop(backend);

    let reopened = fixture.open().await;
    let replay_context = new_context(reopened.clone(), Arc::new(RejectingTokenSource));
    let replay = claim_item(
        &replay_context,
        &envelope("agent-one", "claim-replay"),
        &request,
    )
    .await
    .unwrap();
    assert_eq!(replay, claimed);
    let mut after = snapshot(reopened.as_ref()).await;
    assert_eq!(after, before);
    after.race_loser_codes = race_loser_codes;
    after.race_active_counts = race_active_counts;
    after
}

async fn prove_move_claim_race(backend: Arc<dyn StorageBackend>) -> (OrgErrorCode, i64) {
    let seed_context = new_context(
        backend.clone(),
        Arc::new(DeterministicTokenSource::new(["move-source-token"])),
    );
    let source_workspace = WorkspaceId::from_str(MOVE_SOURCE_WORKSPACE).unwrap();
    let target_workspace = WorkspaceId::from_str(MOVE_TARGET_WORKSPACE).unwrap();
    create_scenario_workspace(&seed_context, source_workspace, "move-source", 4).await;
    create_scenario_workspace(&seed_context, target_workspace, "move-target", 1).await;
    seed_document_in(
        &seed_context,
        source_workspace,
        MOVE_SOURCE_DOCUMENT,
        MOVE_SOURCE_ITEM,
        "READY",
        "move-source-document",
    )
    .await;
    seed_document_in(
        &seed_context,
        target_workspace,
        MOVE_TARGET_DOCUMENT,
        MOVE_TARGET_ITEM,
        "READY",
        "move-target-document",
    )
    .await;
    let source_document = DocumentId::from_str(MOVE_SOURCE_DOCUMENT).unwrap();
    let target_document = DocumentId::from_str(MOVE_TARGET_DOCUMENT).unwrap();
    let source_item = WorkItemId::from_str(MOVE_SOURCE_ITEM).unwrap();
    let target_item = WorkItemId::from_str(MOVE_TARGET_ITEM).unwrap();
    let source_claim = claim_item(
        &seed_context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id: source_workspace,
            actor_id: "move-agent".into(),
            operation_id: "claim-move-source".into(),
        },
        &StartClaimRequest {
            work_item_id: source_item,
            document_id: source_document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let source_workspace_before = session
        .get_org_workspace(source_workspace)
        .await
        .unwrap()
        .unwrap();
    let target_workspace_before = session
        .get_org_workspace(target_workspace)
        .await
        .unwrap()
        .unwrap();
    let source_document_before = session
        .get_org_document(source_document)
        .await
        .unwrap()
        .unwrap();
    let source_projection_before = session
        .list_org_workspace_projection(source_workspace)
        .await
        .unwrap();
    let source_lease_before = session
        .get_open_org_lease_internal(source_item)
        .await
        .unwrap()
        .unwrap();
    let source_attempts_before = session.list_org_attempts(source_item).await.unwrap();
    let source_events_before = session
        .list_org_events(source_workspace, None, 100)
        .await
        .unwrap()
        .len();
    let target_move_events_before = session
        .list_org_subject_events(
            target_workspace,
            "document",
            MOVE_SOURCE_DOCUMENT,
            None,
            100,
        )
        .await
        .unwrap();
    drop(session);

    let move_request = MoveDocumentRequest {
        document_id: source_document,
        target_workspace_id: target_workspace,
        expected_document_revision: source_claim.context.document.revision,
        expected_source_workspace_revision: source_workspace_before.revision,
        expected_target_workspace_revision: target_workspace_before.revision,
        lease_proofs: BTreeMap::from([(
            source_item,
            LeaseProofInput {
                lease_id: source_claim.lease_id,
                kind: OrgClaimKind::Execution,
                fencing_token: source_claim.fencing_token,
            },
        )]),
    };
    let gated = Arc::new(BeginBarrierStorageBackend::new(backend.clone()));
    gated.arm_immediate_pair();
    let race_context = new_context(
        gated,
        Arc::new(DeterministicTokenSource::new(["move-target-token"])),
    );
    let move_task = {
        let context = race_context.clone();
        tokio::spawn(async move {
            move_document(
                &context,
                &CommandEnvelope {
                    schema_version: 1,
                    workspace_id: source_workspace,
                    actor_id: "move-agent".into(),
                    operation_id: "move-capacity-race".into(),
                },
                &move_request,
            )
            .await
        })
    };
    let claim_task = {
        let context = race_context.clone();
        tokio::spawn(async move {
            claim_item(
                &context,
                &CommandEnvelope {
                    schema_version: 1,
                    workspace_id: target_workspace,
                    actor_id: "target-agent".into(),
                    operation_id: "target-capacity-race".into(),
                },
                &StartClaimRequest {
                    work_item_id: target_item,
                    document_id: target_document,
                    expected_document_revision: 1,
                    kind: OrgClaimKind::Execution,
                },
            )
            .await
        })
    };
    let (moved, claimed) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        (move_task.await.unwrap(), claim_task.await.unwrap())
    })
    .await
    .expect("move versus claim boundary barrier deadlocked");
    assert_eq!(usize::from(moved.is_ok()) + usize::from(claimed.is_ok()), 1);
    assert_eq!(
        moved
            .as_ref()
            .err()
            .or_else(|| claimed.as_ref().err())
            .unwrap()
            .code,
        OrgErrorCode::ConcurrencyLimit
    );

    let session = backend.session().await.unwrap();
    let target_active = session
        .count_active_org_leases(target_workspace, NOW)
        .await
        .unwrap();
    assert_eq!(target_active, 1);
    let stored_document = session
        .get_org_document(source_document)
        .await
        .unwrap()
        .unwrap();
    let stored_lease = session
        .get_open_org_lease_internal(source_item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_document.workspace_id, stored_lease.workspace_id);
    if moved.is_ok() {
        assert_eq!(stored_document.workspace_id, target_workspace);
        assert!(session
            .get_org_operation(target_workspace, "target-capacity-race")
            .await
            .unwrap()
            .is_none());
        assert_eq!(session.count_org_attempts(target_item).await.unwrap(), 0);
        assert!(session
            .get_open_org_lease_internal(target_item)
            .await
            .unwrap()
            .is_none());
        assert!(session
            .list_org_subject_events(target_workspace, "work_item", MOVE_TARGET_ITEM, None, 100)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            session
                .get_org_document(target_document)
                .await
                .unwrap()
                .unwrap()
                .revision,
            1
        );
    } else {
        assert_eq!(stored_document, source_document_before);
        assert_eq!(stored_lease, source_lease_before);
        assert_eq!(
            session.list_org_attempts(source_item).await.unwrap(),
            source_attempts_before
        );
        assert!(session
            .get_org_operation(source_workspace, "move-capacity-race")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            session
                .get_org_workspace(source_workspace)
                .await
                .unwrap()
                .unwrap(),
            source_workspace_before
        );
        assert_eq!(
            session
                .get_org_workspace(target_workspace)
                .await
                .unwrap()
                .unwrap(),
            target_workspace_before
        );
        assert_eq!(
            session
                .list_org_workspace_projection(source_workspace)
                .await
                .unwrap(),
            source_projection_before
        );
        assert_eq!(
            session
                .list_org_events(source_workspace, None, 100)
                .await
                .unwrap()
                .len(),
            source_events_before
        );
        assert_eq!(
            session
                .list_org_subject_events(
                    target_workspace,
                    "document",
                    MOVE_SOURCE_DOCUMENT,
                    None,
                    100,
                )
                .await
                .unwrap(),
            target_move_events_before
        );
    }
    (OrgErrorCode::ConcurrencyLimit, target_active)
}

async fn prove_execution_claim_races(
    backend: Arc<dyn StorageBackend>,
) -> (Vec<OrgErrorCode>, Vec<i64>) {
    let seed_context = new_context(
        backend.clone(),
        Arc::new(DeterministicTokenSource::new(["seed-unused"])),
    );
    let same_workspace = WorkspaceId::from_str(SAME_WORKSPACE).unwrap();
    create_scenario_workspace(&seed_context, same_workspace, "same-item-race", 4).await;
    seed_document_in(
        &seed_context,
        same_workspace,
        SAME_DOCUMENT,
        SAME_ITEM,
        "READY",
        "same-race-document",
    )
    .await;
    let same_document = DocumentId::from_str(SAME_DOCUMENT).unwrap();
    let same_item = WorkItemId::from_str(SAME_ITEM).unwrap();
    let same_results = race_claims(
        backend.clone(),
        same_workspace,
        [
            ("same-agent-a", "same-operation-a", same_document, same_item),
            ("same-agent-b", "same-operation-b", same_document, same_item),
        ],
        ["same-token-a", "same-token-b"],
    )
    .await;
    assert_race_shape(&same_results, OrgErrorCode::ActiveLease);
    let same_loser_operation = same_results
        .iter()
        .find_map(|(_, operation, _, _, result)| result.is_err().then_some(operation))
        .unwrap();
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(same_workspace, same_loser_operation)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_org_attempts(same_item).await.unwrap(), 1);
    assert_eq!(
        session
            .list_org_subject_events(same_workspace, "work_item", SAME_ITEM, None, 100)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        session
            .count_active_org_leases(same_workspace, NOW)
            .await
            .unwrap(),
        1
    );
    drop(session);

    let capacity_workspace = WorkspaceId::from_str(CAPACITY_WORKSPACE).unwrap();
    create_scenario_workspace(&seed_context, capacity_workspace, "capacity-race", 1).await;
    for (document, item, operation) in [
        (CAPACITY_DOCUMENT_A, CAPACITY_ITEM_A, "capacity-seed-a"),
        (CAPACITY_DOCUMENT_B, CAPACITY_ITEM_B, "capacity-seed-b"),
    ] {
        seed_document_in(
            &seed_context,
            capacity_workspace,
            document,
            item,
            "READY",
            operation,
        )
        .await;
    }
    let session = backend.session().await.unwrap();
    let capacity_projection_a = session
        .list_org_document_projection(DocumentId::from_str(CAPACITY_DOCUMENT_A).unwrap())
        .await
        .unwrap();
    let capacity_projection_b = session
        .list_org_document_projection(DocumentId::from_str(CAPACITY_DOCUMENT_B).unwrap())
        .await
        .unwrap();
    drop(session);
    let capacity_results = race_claims(
        backend.clone(),
        capacity_workspace,
        [
            (
                "capacity-agent-a",
                "capacity-operation-a",
                DocumentId::from_str(CAPACITY_DOCUMENT_A).unwrap(),
                WorkItemId::from_str(CAPACITY_ITEM_A).unwrap(),
            ),
            (
                "capacity-agent-b",
                "capacity-operation-b",
                DocumentId::from_str(CAPACITY_DOCUMENT_B).unwrap(),
                WorkItemId::from_str(CAPACITY_ITEM_B).unwrap(),
            ),
        ],
        ["capacity-token-a", "capacity-token-b"],
    )
    .await;
    assert_race_shape(&capacity_results, OrgErrorCode::ConcurrencyLimit);
    let (_, loser_operation, loser_document, loser_item, _) = capacity_results
        .iter()
        .find(|(_, _, _, _, result)| result.is_err())
        .unwrap();
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .count_active_org_leases(capacity_workspace, NOW)
            .await
            .unwrap(),
        1
    );
    assert!(session
        .get_org_operation(capacity_workspace, loser_operation)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_org_attempts(*loser_item).await.unwrap(), 0);
    assert!(session
        .get_open_org_lease_internal(*loser_item)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .list_org_subject_events(
            capacity_workspace,
            "work_item",
            &loser_item.to_string(),
            None,
            100,
        )
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session
            .get_org_document(*loser_document)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    let expected_loser_projection = if loser_document.to_string() == CAPACITY_DOCUMENT_A {
        capacity_projection_a
    } else {
        capacity_projection_b
    };
    assert_eq!(
        session
            .list_org_document_projection(*loser_document)
            .await
            .unwrap(),
        expected_loser_projection
    );
    (
        vec![OrgErrorCode::ActiveLease, OrgErrorCode::ConcurrencyLimit],
        vec![1, 1],
    )
}

type ClaimRaceResult = (
    &'static str,
    &'static str,
    DocumentId,
    WorkItemId,
    Result<note_pipelines::org::OrgClaimResult, OrgError>,
);

async fn race_claims(
    backend: Arc<dyn StorageBackend>,
    workspace_id: WorkspaceId,
    specs: [(&'static str, &'static str, DocumentId, WorkItemId); 2],
    tokens: [&'static str; 2],
) -> [ClaimRaceResult; 2] {
    let gated = Arc::new(BeginBarrierStorageBackend::new(backend));
    gated.arm_immediate_pair();
    let context = new_context(gated, Arc::new(DeterministicTokenSource::new(tokens)));
    let mut tasks = specs
        .into_iter()
        .map(|(actor, operation, document, item)| {
            let context = context.clone();
            tokio::spawn(async move {
                let result = claim_item(
                    &context,
                    &CommandEnvelope {
                        schema_version: 1,
                        workspace_id,
                        actor_id: actor.into(),
                        operation_id: operation.into(),
                    },
                    &StartClaimRequest {
                        work_item_id: item,
                        document_id: document,
                        expected_document_revision: 1,
                        kind: OrgClaimKind::Execution,
                    },
                )
                .await;
                (actor, operation, document, item, result)
            })
        })
        .collect::<Vec<_>>();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let first = tasks.remove(0).await.unwrap();
        let second = tasks.remove(0).await.unwrap();
        [first, second]
    })
    .await
    .expect("execution claim boundary barrier deadlocked")
}

fn assert_race_shape(results: &[ClaimRaceResult; 2], loser_code: OrgErrorCode) {
    assert_eq!(
        results
            .iter()
            .filter(|(_, _, _, _, result)| result.is_ok())
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .find_map(|(_, _, _, _, result)| result.as_ref().err())
            .unwrap()
            .code,
        loser_code
    );
}

async fn prove_reviewer_exclusivity(backend: Arc<dyn StorageBackend>, context: &OrgContext) {
    seed_document(
        context,
        REVIEW_DOCUMENT,
        REVIEW_ITEM,
        "REVIEW",
        "review-race",
    )
    .await;
    let review_item = WorkItemId::from_str(REVIEW_ITEM).unwrap();
    let review_document = DocumentId::from_str(REVIEW_DOCUMENT).unwrap();
    let session = backend.session().await.unwrap();
    session
        .insert_org_attempt(NewOrgAttempt {
            id: "acceptance-submitted-attempt",
            workspace_id: workspace_id(),
            work_item_id: review_item,
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
    let baseline_events = session
        .list_org_subject_events(workspace_id(), "work_item", REVIEW_ITEM, None, 100)
        .await
        .unwrap()
        .len();
    drop(session);

    let gated = Arc::new(BeginBarrierStorageBackend::new(backend.clone()));
    gated.arm_immediate_pair();
    let race_context = new_context(
        gated,
        Arc::new(DeterministicTokenSource::new([
            "review-token-a",
            "review-token-b",
        ])),
    );
    let mut tasks = Vec::new();
    for actor in ["reviewer-a", "reviewer-b"] {
        let context = race_context.clone();
        tasks.push(tokio::spawn(async move {
            let operation = format!("review-race-{actor}");
            let result = claim_item(
                &context,
                &CommandEnvelope {
                    schema_version: 1,
                    workspace_id: workspace_id(),
                    actor_id: actor.into(),
                    operation_id: operation.clone(),
                },
                &StartClaimRequest {
                    work_item_id: review_item,
                    document_id: review_document,
                    expected_document_revision: 1,
                    kind: OrgClaimKind::Review,
                },
            )
            .await;
            (actor, operation, result)
        }));
    }
    let results = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let first = tasks.remove(0).await.unwrap();
        let second = tasks.remove(0).await.unwrap();
        [first, second]
    })
    .await
    .expect("review claim boundary barrier deadlocked");
    assert_eq!(
        results
            .iter()
            .filter(|(_, _, result)| result.is_ok())
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .find_map(|(_, _, result)| result.as_ref().err())
            .unwrap()
            .code,
        OrgErrorCode::ActiveLease
    );
    let loser_operation = results
        .iter()
        .find_map(|(_, operation, result)| result.is_err().then_some(operation))
        .unwrap();
    let session = backend.session().await.unwrap();
    assert!(session
        .get_org_operation(workspace_id(), loser_operation)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_org_attempts(review_item).await.unwrap(), 1);
    assert_eq!(
        session
            .list_org_subject_events(workspace_id(), "work_item", REVIEW_ITEM, None, 100)
            .await
            .unwrap()
            .len(),
        baseline_events + 1
    );
    drop(session);

    let (winner_actor, winner) = results
        .into_iter()
        .find_map(|(actor, _, result)| result.ok().map(|claim| (actor, claim)))
        .unwrap();
    release_claim(
        context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id: workspace_id(),
            actor_id: winner_actor.into(),
            operation_id: "release-review-race".into(),
        },
        &ReleaseClaimRequest {
            schema_version: 1,
            work_item_id: review_item,
            document_id: review_document,
            expected_document_revision: 1,
            lease_id: winner.lease_id,
            kind: OrgClaimKind::Review,
            fencing_token: winner.fencing_token,
            target_state: None,
        },
    )
    .await
    .unwrap();
}

async fn snapshot(storage: &dyn StorageBackend) -> NormalizedOutcome {
    let session = storage.session().await.unwrap();
    let item = session.get_org_work_item(item_id()).await.unwrap().unwrap();
    let document_revision = session
        .get_org_document(document_id())
        .await
        .unwrap()
        .unwrap()
        .revision;
    let attempts = session.count_org_attempts(item_id()).await.unwrap();
    let attempt_statuses = session
        .list_org_attempts(item_id())
        .await
        .unwrap()
        .into_iter()
        .map(|attempt| attempt.status)
        .collect();
    let events = session
        .list_org_subject_events(workspace_id(), "work_item", ITEM, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    let lease_count = session
        .list_org_lease_history(item_id())
        .await
        .unwrap()
        .len();
    NormalizedOutcome {
        state: item.state.unwrap(),
        document_revision,
        attempts,
        attempt_statuses,
        events,
        lease_count,
        race_loser_codes: Vec::new(),
        race_active_counts: Vec::new(),
    }
}

async fn create_scenario_workspace(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    slug: &str,
    concurrency_limit: usize,
) {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = concurrency_limit;
    create_workspace(
        context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id,
            actor_id: "seed".into(),
            operation_id: format!("create-{slug}"),
        },
        &CreateWorkspaceRequest {
            slug: slug.into(),
            display_name: slug.into(),
            description: "Task 9 barrier race".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy,
        },
    )
    .await
    .unwrap();
}

async fn seed_document(
    context: &OrgContext,
    document: &str,
    item: &str,
    state: &str,
    operation: &str,
) {
    seed_document_in(context, workspace_id(), document, item, state, operation).await;
}

async fn seed_document_in(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    document: &str,
    item: &str,
    state: &str,
    operation: &str,
) {
    put_document(
        context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id,
            actor_id: "seed".into(),
            operation_id: operation.into(),
        },
        &PutDocumentRequest {
            document_id: DocumentId::from_str(document).unwrap(),
            path: format!("{operation}.org"),
            source: format!(
                "* {state} Acceptance item\r\n:PROPERTIES:\r\n:ID: {item}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
            ),
            expected_revision: None,
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .unwrap();
}

async fn assert_view(context: &OrgContext, view: OperationalView, expected: &[&str]) {
    let actual = query_queue(
        context,
        &OperationalQuery::for_workspace(workspace_id(), view),
    )
    .await
    .unwrap()
    .items
    .into_iter()
    .map(|item| item.item.id.to_string())
    .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn expected_outcome() -> NormalizedOutcome {
    NormalizedOutcome {
        state: "DONE".into(),
        document_revision: 6,
        attempts: 3,
        attempt_statuses: vec![
            OrgAttemptStatus::Expired,
            OrgAttemptStatus::Failed,
            OrgAttemptStatus::Completed,
        ],
        events: vec![
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::LeaseExpiry,
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::Failure,
            OrgEventType::Retry,
            OrgEventType::Claim,
            OrgEventType::Start,
            OrgEventType::Completion,
        ],
        lease_count: 3,
        race_loser_codes: vec![
            OrgErrorCode::ActiveLease,
            OrgErrorCode::ConcurrencyLimit,
            OrgErrorCode::ConcurrencyLimit,
        ],
        race_active_counts: vec![1, 1, 1],
    }
}

#[tokio::test]
async fn turso_high_risk_claims_match_the_shared_acceptance_scenario() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = TursoFixture {
        path: dir.path().join("claims-acceptance.db"),
    };
    assert_eq!(run_high_risk_scenario(&fixture).await, expected_outcome());
}

#[tokio::test]
async fn postgresql_high_risk_claims_match_the_shared_acceptance_scenario() {
    let Some(database) = PgAcceptanceDatabase::provision(
        "postgresql_high_risk_claims_match_the_shared_acceptance_scenario",
    )
    .await
    else {
        return;
    };
    let fixture = PgFixture {
        url: database.url().to_owned(),
    };
    let scenario = std::panic::AssertUnwindSafe(run_high_risk_scenario(&fixture))
        .catch_unwind()
        .await;
    drop(fixture);
    let cleanup = database.cleanup().await;
    match scenario {
        Ok(outcome) => {
            cleanup.unwrap();
            assert_eq!(outcome, expected_outcome());
        }
        Err(payload) => {
            cleanup.unwrap();
            std::panic::resume_unwind(payload);
        }
    }
}
