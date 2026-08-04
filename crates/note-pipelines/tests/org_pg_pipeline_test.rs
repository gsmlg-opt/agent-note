mod support;

use note_org::{DocumentId, WorkItemId, WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    claim_item, create_workspace, move_document, put_document, update_workspace, CommandEnvelope,
    CreateWorkspaceRequest, LeaseProofInput, MoveDocumentRequest, OrgClaimKind, OrgContext,
    OrgErrorCode, PutDocumentRequest, StartClaimRequest, UpdateWorkspaceRequest,
};
use note_storage::StorageBackend;
use note_storage_pg::PgStorage;
use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::sync::Arc;
use support::DeterministicTokenSource;

const NOW: i64 = 1_900_000_000;

fn envelope(workspace_id: WorkspaceId, operation_id: &str) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        workspace_id,
        actor_id: "pg-race-agent".into(),
        operation_id: operation_id.into(),
    }
}

fn source(item_id: WorkItemId) -> String {
    format!(
        "* READY PostgreSQL race\r\n:PROPERTIES:\r\n:ID: {item_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n"
    )
}

async fn create_workspace_with_limit(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    suffix: &str,
    limit: usize,
) {
    create_workspace(
        context,
        &envelope(workspace_id, &format!("pg-create-{suffix}")),
        &CreateWorkspaceRequest {
            slug: format!("pg-race-{suffix}-{workspace_id}"),
            display_name: format!("PostgreSQL race {suffix}"),
            description: "Task 7 live PostgreSQL pipeline race".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
        },
    )
    .await
    .unwrap();
    if limit != WorkspacePolicy::engineering_default().concurrency_limit {
        let mut policy = WorkspacePolicy::engineering_default();
        policy.concurrency_limit = limit;
        update_workspace(
            context,
            &envelope(workspace_id, &format!("pg-limit-{suffix}")),
            &UpdateWorkspaceRequest {
                expected_revision: 1,
                slug: format!("pg-race-{suffix}-{workspace_id}"),
                display_name: format!("PostgreSQL race {suffix}"),
                description: "Task 7 live PostgreSQL pipeline race".into(),
                timezone: "UTC".into(),
                policy_schema_version: 1,
                policy,
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL live PostgreSQL"]
async fn live_postgres_move_and_claim_share_the_pipeline_capacity_boundary() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must be set for the live PostgreSQL pipeline race");
    let storage: Arc<dyn StorageBackend> = Arc::new(PgStorage::connect(&url, 10).await.unwrap());
    let context = OrgContext::new(
        storage.clone(),
        Arc::new(note_pipelines::org::FixedOrgClock::new(NOW)),
    )
    .with_token_source(Arc::new(DeterministicTokenSource::new([
        "pg-source-token",
        "pg-target-token",
    ])));
    let source_workspace = WorkspaceId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let target_workspace = WorkspaceId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let source_document = DocumentId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let target_document = DocumentId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let source_item = WorkItemId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let target_item = WorkItemId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    create_workspace_with_limit(&context, source_workspace, "source", 4).await;
    create_workspace_with_limit(&context, target_workspace, "target", 1).await;
    for (workspace, document, item, suffix) in [
        (source_workspace, source_document, source_item, "source"),
        (target_workspace, target_document, target_item, "target"),
    ] {
        put_document(
            &context,
            &envelope(workspace, &format!("pg-seed-{suffix}")),
            &PutDocumentRequest {
                document_id: document,
                path: format!("pg-{suffix}.org"),
                source: source(item),
                expected_revision: None,
                lease_proofs: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    }
    let source_claim = claim_item(
        &context,
        &envelope(source_workspace, "pg-claim-source"),
        &StartClaimRequest {
            work_item_id: source_item,
            document_id: source_document,
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let session = storage.session().await.unwrap();
    let source_revision = session
        .get_org_workspace(source_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let target_revision = session
        .get_org_workspace(target_workspace)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let source_events_before = session
        .list_org_events(source_workspace, None, 100)
        .await
        .unwrap()
        .len();
    let target_events_before = session
        .list_org_events(target_workspace, None, 100)
        .await
        .unwrap()
        .len();
    drop(session);

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let move_task = {
        let context = context.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            move_document(
                &context,
                &envelope(source_workspace, "pg-race-move"),
                &MoveDocumentRequest {
                    document_id: source_document,
                    target_workspace_id: target_workspace,
                    expected_document_revision: 2,
                    expected_source_workspace_revision: source_revision,
                    expected_target_workspace_revision: target_revision,
                    lease_proofs: BTreeMap::from([(
                        source_item,
                        LeaseProofInput {
                            lease_id: source_claim.lease_id,
                            kind: OrgClaimKind::Execution,
                            fencing_token: source_claim.fencing_token,
                        },
                    )]),
                },
            )
            .await
        })
    };
    let claim_task = {
        let context = context.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            claim_item(
                &context,
                &envelope(target_workspace, "pg-race-claim"),
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
    barrier.wait().await;
    let moved = move_task.await.unwrap();
    let claimed = claim_task.await.unwrap();
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

    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .count_active_org_leases(target_workspace, NOW)
            .await
            .unwrap(),
        1
    );
    let stored = session
        .get_org_document(source_document)
        .await
        .unwrap()
        .unwrap();
    let lease = session
        .get_open_org_lease_internal(source_item)
        .await
        .unwrap()
        .unwrap();
    let attempt = session
        .list_org_attempts(source_item)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(lease.attempt_id, attempt.id);
    assert_eq!(attempt.workspace_id, source_workspace);
    if moved.is_ok() {
        assert_eq!(stored.workspace_id, target_workspace);
        assert_eq!(lease.workspace_id, target_workspace);
        assert!(session
            .list_org_workspace_projection(source_workspace)
            .await
            .unwrap()
            .iter()
            .all(|item| item.id != source_item));
        assert!(session
            .list_org_workspace_projection(target_workspace)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == source_item));
        assert!(session
            .get_org_operation(target_workspace, "pg-race-claim")
            .await
            .unwrap()
            .is_none());
        assert!(
            session
                .list_org_events(source_workspace, None, 100)
                .await
                .unwrap()
                .len()
                > source_events_before
        );
        assert!(
            session
                .list_org_events(target_workspace, None, 100)
                .await
                .unwrap()
                .len()
                > target_events_before
        );
    } else {
        assert_eq!(stored.workspace_id, source_workspace);
        assert_eq!(lease.workspace_id, source_workspace);
        assert!(session
            .list_org_workspace_projection(source_workspace)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == source_item));
        assert!(session
            .list_org_workspace_projection(target_workspace)
            .await
            .unwrap()
            .iter()
            .all(|item| item.id != source_item));
        assert!(session
            .get_org_operation(source_workspace, "pg-race-move")
            .await
            .unwrap()
            .is_none());
        assert_eq!(stored.revision, 2);
    }
}
