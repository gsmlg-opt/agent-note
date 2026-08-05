use std::sync::Arc;

use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_mcp::NoteMcpServer;
use note_pipelines::{
    org::{
        claim_item, CommandEnvelope, FixedOrgClock, OrgClaimKind, OrgContext, StartClaimRequest,
    },
    Context,
};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use rmcp::{
    model::{CallToolRequestParams, ErrorData},
    service::ServiceError,
    ServiceExt,
};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;
const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000001";

struct TestServer {
    server: NoteMcpServer,
    backend: Arc<dyn StorageBackend>,
    org: Arc<OrgContext>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-mcp.db"))
                .await
                .unwrap(),
        );
        let note = Arc::new(Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ));
        let org = Arc::new(OrgContext::new(
            backend.clone(),
            Arc::new(FixedOrgClock::new(NOW)),
        ));
        Self {
            server: NoteMcpServer::new(note, org.clone()),
            backend,
            org,
            _dir: dir,
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<Value, ErrorData> {
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let service = self.server.clone();
        let server_task = tokio::spawn(async move {
            service
                .serve(server_transport)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ().serve(client_transport).await.unwrap();
        let result = client
            .call_tool(
                CallToolRequestParams::new(name.to_owned())
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await;
        client.cancel().await.unwrap();
        server_task.await.unwrap();
        match result {
            Ok(result) => Ok(result.structured_content.expect("structured tool output")),
            Err(ServiceError::McpError(error)) => Err(error),
            Err(error) => panic!("unexpected MCP service error: {error:?}"),
        }
    }
}

async fn claim_execution(
    mcp: &TestServer,
    workspace_id: &str,
    document_id: &str,
    item_id: &str,
    expected_document_revision: i64,
    operation_id: &str,
) -> note_pipelines::org::OrgClaimResult {
    claim_item(
        &mcp.org,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id: workspace_id.parse().unwrap(),
            actor_id: "agent-one".into(),
            operation_id: operation_id.into(),
        },
        &StartClaimRequest {
            work_item_id: item_id.parse().unwrap(),
            document_id: document_id.parse().unwrap(),
            expected_document_revision,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap()
}

const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000001";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000001";
const SECOND_DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000002";
const SECOND_ITEM_ID: &str = "30000000-0000-4000-8000-000000000002";
const THIRD_ITEM_ID: &str = "30000000-0000-4000-8000-000000000003";

fn raw_source(title: &str) -> String {
    raw_source_for(ITEM_ID, title)
}

fn raw_source_for(item_id: &str, title: &str) -> String {
    format!(
        "#+TITLE: Exact bytes\r\n#+CUSTOM: opaque syntax\r\n* READY [#A] {title}\r\n:PROPERTIES:\r\n:ID: {item_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\nBody  \r\n"
    )
}

fn two_item_source(first_item_id: &str, second_item_id: &str) -> String {
    format!(
        "#+TITLE: Two active items\r\n* READY First active item\r\n:PROPERTIES:\r\n:ID: {first_item_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\nFirst body.\r\n* READY Second active item\r\n:PROPERTIES:\r\n:ID: {second_item_id}\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\nSecond body.\r\n"
    )
}

fn lease_proof(claim: &note_pipelines::org::OrgClaimResult) -> Value {
    json!({
        "lease_id": claim.lease_id,
        "kind": "execution",
        "fencing_token": claim.fencing_token
    })
}

fn engineering_policy() -> Value {
    json!({
        "allow_cross_workspace_agenda": false,
        "allowed_types": [
            "project", "epic", "issue", "task", "subtask", "review", "approval",
            "incident", "milestone"
        ],
        "states": [
            "BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED",
            "CANCELLED"
        ],
        "transitions": [
            ["BACKLOG", "READY"], ["BACKLOG", "CANCELLED"],
            ["READY", "RUNNING"], ["READY", "CANCELLED"],
            ["RUNNING", "BLOCKED"], ["RUNNING", "READY"], ["RUNNING", "REVIEW"],
            ["RUNNING", "DONE"], ["RUNNING", "FAILED"], ["RUNNING", "CANCELLED"],
            ["BLOCKED", "READY"], ["BLOCKED", "CANCELLED"],
            ["REVIEW", "DONE"], ["REVIEW", "READY"], ["REVIEW", "CANCELLED"],
            ["FAILED", "READY"], ["FAILED", "RUNNING"], ["FAILED", "CANCELLED"]
        ],
        "initial_state": "BACKLOG",
        "running_state": "RUNNING",
        "executable_states": ["READY"],
        "review_state": "REVIEW",
        "failed_state": "FAILED",
        "cancelled_state": "CANCELLED",
        "successful_terminal_states": ["DONE"],
        "terminal_states": ["DONE", "CANCELLED"],
        "release_state": "READY",
        "review_rejection_state": "READY",
        "lease_expiry_recovery_state": "READY",
        "review_required_types": [],
        "claim_policy": "assignment_restricted",
        "lease_duration_secs": 900,
        "retry_limit": 2,
        "concurrency_limit": 4,
        "tag_rules": {}
    })
}

fn create_workspace_input(workspace_id: &str, operation_id: &str) -> Value {
    json!({
        "schema_version": 1,
        "workspace_id": workspace_id,
        "actor_id": "agent-one",
        "operation_id": operation_id,
        "slug": format!("workspace-{workspace_id}"),
        "display_name": "Engineering",
        "description": "Org MCP workspace",
        "timezone": "Asia/Shanghai",
        "policy_schema_version": 1,
        "policy": engineering_policy()
    })
}

#[tokio::test]
async fn workspace_create_get_and_list_delegate_through_mcp() {
    let mcp = TestServer::new().await;

    let created = mcp
        .call(
            "org_create_workspace",
            create_workspace_input(WORKSPACE_ID, "create-workspace"),
        )
        .await
        .unwrap();
    assert_eq!(created["workspace_revision"], 1);
    assert_eq!(created["data"]["workspace_id"], WORKSPACE_ID);

    let workspace = mcp
        .call("org_get_workspace", json!({"workspace_id": WORKSPACE_ID}))
        .await
        .unwrap();
    assert_eq!(workspace["id"], WORKSPACE_ID);
    assert_eq!(workspace["timezone"], "Asia/Shanghai");
    assert_eq!(workspace["revision"], 1);

    let page = mcp
        .call(
            "org_list_workspaces",
            json!({"limit": 50, "include_archived": false}),
        )
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["workspace_id"], WORKSPACE_ID);
}

#[tokio::test]
async fn workspace_update_archive_and_archived_listing_follow_pipeline_rules() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();

    let events_before_stale = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let stale = mcp
        .call(
            "org_update_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "stale-workspace-update",
                "expected_revision": 9,
                "slug": "engineering",
                "display_name": "Stale update",
                "description": "Must not be applied",
                "timezone": "UTC",
                "policy_schema_version": 1,
                "policy": engineering_policy()
            }),
        )
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(stale["details"]["current_revision"], 1);
    assert_eq!(stale["retryable"], true);
    let unchanged = mcp
        .call("org_get_workspace", json!({"workspace_id": WORKSPACE_ID}))
        .await
        .unwrap();
    assert_eq!(unchanged["revision"], 1);
    assert_eq!(unchanged["display_name"], "Engineering");
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_stale
    );

    let updated = mcp
        .call(
            "org_update_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "update-workspace",
                "expected_revision": 1,
                "slug": "engineering",
                "display_name": "Platform Engineering",
                "description": "Updated through MCP",
                "timezone": "America/New_York",
                "policy_schema_version": 1,
                "policy": engineering_policy()
            }),
        )
        .await
        .unwrap();
    assert_eq!(updated["workspace_revision"], 2);
    assert_eq!(updated["data"]["revision"], 2);

    let archived = mcp
        .call(
            "org_archive_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "archive-workspace",
                "expected_revision": 2
            }),
        )
        .await
        .unwrap();
    assert_eq!(archived["workspace_revision"], 3);
    assert_eq!(archived["data"]["workspace_id"], WORKSPACE_ID);
    assert_eq!(archived["data"]["archived_at"], NOW);

    let active = mcp
        .call(
            "org_list_workspaces",
            json!({"limit": 50, "include_archived": false}),
        )
        .await
        .unwrap();
    assert!(active["items"].as_array().unwrap().is_empty());
    let all = mcp
        .call(
            "org_list_workspaces",
            json!({"limit": 50, "include_archived": true}),
        )
        .await
        .unwrap();
    assert_eq!(all["items"][0]["workspace_revision"], 3);
    assert_eq!(all["items"][0]["archived_at"], NOW);

    let read = mcp
        .call("org_get_workspace", json!({"workspace_id": WORKSPACE_ID}))
        .await
        .unwrap();
    assert_eq!(read["revision"], 3);

    let rejected = mcp
        .call(
            "org_update_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "update-archived-workspace",
                "expected_revision": 3,
                "slug": "engineering",
                "display_name": "Rejected",
                "description": "Archived is read-only",
                "timezone": "UTC",
                "policy_schema_version": 1,
                "policy": engineering_policy()
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.data.unwrap()["code"], "archived_workspace");
}

#[tokio::test]
async fn document_put_reads_export_conflicts_and_replay_preserve_canonical_source() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    let source = raw_source("Preserve me");
    let put = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "put-document",
        "document_id": DOCUMENT_ID,
        "path": "tasks/main.org",
        "source": source,
        "expected_revision": null,
        "lease_proofs": {}
    });

    let created = mcp.call("org_put_document", put.clone()).await.unwrap();
    assert_eq!(created["document_revisions"][DOCUMENT_ID], 1);
    assert_eq!(created["data"]["document_count"], 1);
    assert_eq!(
        mcp.call("org_put_document", put.clone()).await.unwrap(),
        created
    );

    let listed = mcp
        .call(
            "org_list_documents",
            json!({
                "workspace_id": WORKSPACE_ID,
                "limit": 50,
                "include_archived": false
            }),
        )
        .await
        .unwrap();
    assert_eq!(listed["items"][0]["id"], DOCUMENT_ID);
    assert_eq!(listed["items"][0]["revision"], 1);

    let read = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert_eq!(
        read["source"].as_str().unwrap().as_bytes(),
        source.as_bytes()
    );
    assert!(!read["content_hash"].as_str().unwrap().is_empty());

    let exported = mcp
        .call(
            "org_export_workspace",
            json!({"workspace_id": WORKSPACE_ID}),
        )
        .await
        .unwrap();
    assert_eq!(
        exported["documents"][0]["source"]
            .as_str()
            .unwrap()
            .as_bytes(),
        source.as_bytes()
    );

    let mut stale = put.clone();
    stale["operation_id"] = json!("stale-document-update");
    stale["expected_revision"] = json!(9);
    stale["source"] = json!(raw_source("Stale"));
    let conflict = mcp
        .call("org_put_document", stale)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(conflict["code"], "stale_revision");
    assert_eq!(conflict["details"]["current_revision"], 1);

    let mut divergent = put.clone();
    divergent["source"] = json!(raw_source("Divergent replay"));
    let error = mcp
        .call("org_put_document", divergent)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(error["code"], "idempotency_conflict");

    let workspace_id = WORKSPACE_ID.parse().unwrap();
    let session = mcp.backend.session().await.unwrap();
    assert_eq!(
        session
            .list_org_events(workspace_id, None, 200)
            .await
            .unwrap()
            .len(),
        2,
        "one workspace event and one document event"
    );
    let read_after_replays = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert_eq!(read_after_replays["revision"], 1);
    assert_eq!(read_after_replays["source"], source);

    mcp.call(
        "org_archive_workspace",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "archive-workspace",
            "expected_revision": 1
        }),
    )
    .await
    .unwrap();
    let archived_read = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert_eq!(archived_read["source"], source);
    let archived_export = mcp
        .call(
            "org_export_workspace",
            json!({"workspace_id": WORKSPACE_ID}),
        )
        .await
        .unwrap();
    assert_eq!(archived_export["documents"][0]["revision"], 1);
    let hidden = mcp
        .call(
            "org_list_documents",
            json!({
                "workspace_id": WORKSPACE_ID,
                "limit": 50,
                "include_archived": false
            }),
        )
        .await
        .unwrap();
    assert!(hidden["items"].as_array().unwrap().is_empty());
    let visible = mcp
        .call(
            "org_list_documents",
            json!({
                "workspace_id": WORKSPACE_ID,
                "limit": 50,
                "include_archived": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(visible["items"][0]["id"], DOCUMENT_ID);

    assert_eq!(
        mcp.call("org_put_document", put.clone()).await.unwrap(),
        created,
        "exact replay is resolved before archived mutation rejection"
    );
    let mut new_mutation = put;
    new_mutation["operation_id"] = json!("put-after-archive");
    new_mutation["expected_revision"] = json!(1);
    let rejected = mcp
        .call("org_put_document", new_mutation)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(rejected["code"], "archived_workspace");
}

#[tokio::test]
async fn invalid_org_source_never_reflects_attacker_controlled_parse_reason() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    let attacker_value = "private-secret-token-value";
    let error = mcp
        .call(
            "org_put_document",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "invalid-source",
                "document_id": DOCUMENT_ID,
                "path": "invalid.org",
                "source": format!(
                    "* READY Invalid type\r\n:PROPERTIES:\r\n:ID: {ITEM_ID}\r\n:AGENT_NOTE_TYPE: {attacker_value}\r\n:END:\r\n"
                ),
                "expected_revision": null,
                "lease_proofs": {}
            }),
        )
        .await
        .unwrap_err();
    let rendered = format!("{error:?} {}", serde_json::to_string(&error).unwrap());
    assert!(!rendered.contains(attacker_value));
    let data = error.data.unwrap();
    assert_eq!(data["code"], "invalid_input");
    assert_eq!(data["details"]["reason"], "invalid Org input");
}

#[tokio::test]
async fn stable_workspace_policy_reason_is_preserved_without_side_effects() {
    let mcp = TestServer::new().await;
    let mut input = create_workspace_input(WORKSPACE_ID, "invalid-policy");
    input["policy"]["concurrency_limit"] = json!(0);

    let error = mcp.call("org_create_workspace", input).await.unwrap_err();
    assert_eq!(error.message, "Org workspace policy is invalid");
    let data = error.data.unwrap();
    assert_eq!(data["code"], "invalid_input");
    assert_eq!(data["message"], "Org workspace policy is invalid");
    assert_eq!(
        data["details"]["reason"],
        "concurrency limit must be greater than zero"
    );
    assert_eq!(data["retryable"], false);

    let workspace_id = WORKSPACE_ID.parse().unwrap();
    let session = mcp.backend.session().await.unwrap();
    assert!(session
        .get_org_workspace(workspace_id)
        .await
        .unwrap()
        .is_none());
    assert!(session
        .list_org_events(workspace_id, None, 200)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn canonical_duplicate_revision_and_lease_keys_are_rejected_without_writes() {
    let mcp = TestServer::new().await;
    let document_id = "20000000-0000-4000-8000-0000000000aa";
    let document_alias = document_id.to_ascii_uppercase();
    let item_id = "30000000-0000-4000-8000-0000000000bb";
    let item_alias = item_id.to_ascii_uppercase();
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    let source = raw_source_for(item_id, "Canonical duplicate keys");
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-alias-document",
            "document_id": document_id,
            "path": "alias.org",
            "source": source,
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    let before_revision_alias = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": document_id}),
        )
        .await
        .unwrap();
    let events_before_revision_alias = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let mut duplicate_revisions = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "duplicate-revision-alias",
        "documents": [{
            "document_id": document_id,
            "path": "alias.org",
            "source": format!("{}#+CUSTOM: must not be stored\r\n", source)
        }],
        "expected_revisions": {},
        "lease_proofs": {}
    });
    duplicate_revisions["expected_revisions"][document_id] = json!(1);
    duplicate_revisions["expected_revisions"][&document_alias] = json!(1);
    let duplicate_revision_error = mcp
        .call("org_import_workspace", duplicate_revisions)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(duplicate_revision_error["code"], "invalid_input");
    assert_eq!(
        mcp.call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": document_id}),
        )
        .await
        .unwrap(),
        before_revision_alias
    );
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_revision_alias
    );

    let claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        document_id,
        item_id,
        1,
        "claim-alias-item",
    )
    .await;
    let before_lease_alias = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": document_id}),
        )
        .await
        .unwrap();
    let events_before_lease_alias = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let mut duplicate_proofs = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "duplicate-lease-alias",
        "documents": [{
            "document_id": document_id,
            "path": "alias.org",
            "source": format!(
                "{}#+CUSTOM: active item edit must not be stored\r\n",
                before_lease_alias["source"].as_str().unwrap()
            )
        }],
        "expected_revisions": {document_id: claim.context.document.revision},
        "lease_proofs": {}
    });
    duplicate_proofs["lease_proofs"][item_id] = lease_proof(&claim);
    duplicate_proofs["lease_proofs"][&item_alias] = lease_proof(&claim);
    let duplicate_proof_error = mcp
        .call("org_import_workspace", duplicate_proofs)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(duplicate_proof_error["code"], "invalid_input");
    assert_eq!(
        mcp.call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": document_id}),
        )
        .await
        .unwrap(),
        before_lease_alias
    );
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_lease_alias
    );
}

#[tokio::test]
async fn workspace_import_creates_updates_and_rejects_omitted_items_atomically() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    let first = raw_source_for(ITEM_ID, "First import");
    let second = raw_source_for(SECOND_ITEM_ID, "Second import");
    let create_import = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "import-create",
        "documents": [
            {"document_id": DOCUMENT_ID, "path": "first.org", "source": first},
            {"document_id": SECOND_DOCUMENT_ID, "path": "second.org", "source": second}
        ],
        "expected_revisions": {},
        "lease_proofs": {}
    });
    let created = mcp
        .call("org_import_workspace", create_import)
        .await
        .unwrap();
    assert_eq!(created["data"]["document_count"], 2);
    assert_eq!(created["document_revisions"][DOCUMENT_ID], 1);
    assert_eq!(created["document_revisions"][SECOND_DOCUMENT_ID], 1);

    let first_updated = raw_source_for(ITEM_ID, "First updated");
    let second_updated = raw_source_for(SECOND_ITEM_ID, "Second updated");
    let update_import = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "import-update",
        "documents": [
            {"document_id": DOCUMENT_ID, "path": "first.org", "source": first_updated},
            {"document_id": SECOND_DOCUMENT_ID, "path": "second.org", "source": second_updated}
        ],
        "expected_revisions": {DOCUMENT_ID: 1, SECOND_DOCUMENT_ID: 1},
        "lease_proofs": {}
    });
    let updated = mcp
        .call("org_import_workspace", update_import)
        .await
        .unwrap();
    assert_eq!(updated["document_revisions"][DOCUMENT_ID], 2);
    assert_eq!(updated["document_revisions"][SECOND_DOCUMENT_ID], 2);
    let event_count_before_rejections = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();

    let incomplete = mcp
        .call(
            "org_import_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "import-incomplete-revisions",
                "documents": [
                    {"document_id": DOCUMENT_ID, "path": "first.org", "source": raw_source_for(ITEM_ID, "Incomplete first")},
                    {"document_id": SECOND_DOCUMENT_ID, "path": "second.org", "source": raw_source_for(SECOND_ITEM_ID, "Incomplete second")}
                ],
                "expected_revisions": {DOCUMENT_ID: 2},
                "lease_proofs": {}
            }),
        )
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(incomplete["code"], "invalid_input");

    let omitted = mcp
        .call(
            "org_import_workspace",
            json!({
                "schema_version": 1,
                "workspace_id": WORKSPACE_ID,
                "actor_id": "agent-one",
                "operation_id": "import-omits-item",
                "documents": [
                    {"document_id": DOCUMENT_ID, "path": "first.org", "source": "#+TITLE: item removed\r\n"}
                ],
                "expected_revisions": {DOCUMENT_ID: 2},
                "lease_proofs": {}
            }),
        )
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(omitted["code"], "unsupported_semantic_edit");
    assert_eq!(omitted["details"]["work_item_id"], ITEM_ID);

    let export = mcp
        .call(
            "org_export_workspace",
            json!({"workspace_id": WORKSPACE_ID}),
        )
        .await
        .unwrap();
    assert_eq!(export["documents"][0]["revision"], 2);
    assert_eq!(export["documents"][0]["source"], first_updated);
    assert_eq!(export["documents"][1]["revision"], 2);
    assert_eq!(export["documents"][1]["source"], second_updated);
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        event_count_before_rejections
    );
}

#[tokio::test]
async fn workspace_import_requires_exact_proofs_for_touched_active_items_only() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-two-active-items",
            "document_id": DOCUMENT_ID,
            "path": "touched.org",
            "source": two_item_source(ITEM_ID, SECOND_ITEM_ID),
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-untouched-active-item",
            "document_id": SECOND_DOCUMENT_ID,
            "path": "untouched.org",
            "source": raw_source_for(THIRD_ITEM_ID, "Untouched active item"),
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();

    let first_claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        DOCUMENT_ID,
        ITEM_ID,
        1,
        "claim-first-import-item",
    )
    .await;
    let second_claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        DOCUMENT_ID,
        SECOND_ITEM_ID,
        first_claim.context.document.revision,
        "claim-second-import-item",
    )
    .await;
    let untouched_claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        SECOND_DOCUMENT_ID,
        THIRD_ITEM_ID,
        1,
        "claim-untouched-import-item",
    )
    .await;
    let touched_revision = second_claim.context.document.revision;
    let before = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    let events_before_failures = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let imported_source = format!(
        "{}#+CUSTOM: import preserves this exact line\r\n",
        before["source"]
            .as_str()
            .unwrap()
            .replace("First active item", "First active item changed")
            .replace("Second active item", "Second active item changed")
    );
    let base_import = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "import-active-items",
        "documents": [{
            "document_id": DOCUMENT_ID,
            "path": "touched.org",
            "source": imported_source
        }],
        "expected_revisions": {DOCUMENT_ID: touched_revision},
        "lease_proofs": {}
    });

    let assert_unchanged = async || {
        let current = mcp
            .call(
                "org_get_document",
                json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
            )
            .await
            .unwrap();
        assert_eq!(current, before);
        assert_eq!(
            mcp.backend
                .session()
                .await
                .unwrap()
                .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
                .await
                .unwrap()
                .len(),
            events_before_failures
        );
    };

    let missing = mcp
        .call("org_import_workspace", base_import.clone())
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(missing["code"], "stale_lease");
    assert_unchanged().await;

    let mut incomplete = base_import.clone();
    incomplete["lease_proofs"][ITEM_ID] = lease_proof(&first_claim);
    let incomplete_error = mcp
        .call("org_import_workspace", incomplete)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(incomplete_error["code"], "stale_lease");
    assert_unchanged().await;

    let mut wrong = base_import.clone();
    wrong["lease_proofs"][ITEM_ID] = lease_proof(&first_claim);
    wrong["lease_proofs"][SECOND_ITEM_ID] = json!({
        "lease_id": second_claim.lease_id,
        "kind": "execution",
        "fencing_token": "wrong-token"
    });
    let wrong_error = mcp
        .call("org_import_workspace", wrong)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(wrong_error["code"], "stale_lease");
    assert_unchanged().await;

    let mut complete = base_import;
    complete["lease_proofs"][ITEM_ID] = lease_proof(&first_claim);
    complete["lease_proofs"][SECOND_ITEM_ID] = lease_proof(&second_claim);
    let imported = mcp.call("org_import_workspace", complete).await.unwrap();
    assert_eq!(
        imported["document_revisions"][DOCUMENT_ID],
        touched_revision + 1
    );
    let stored = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert_eq!(stored["revision"], touched_revision + 1);
    assert_eq!(
        stored["source"].as_str().unwrap().as_bytes(),
        imported_source.as_bytes()
    );
    let untouched = mcp
        .call(
            "org_get_document",
            json!({
                "workspace_id": WORKSPACE_ID,
                "document_id": SECOND_DOCUMENT_ID
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        untouched["revision"],
        untouched_claim.context.document.revision
    );
}

#[tokio::test]
async fn document_move_requires_active_lease_proofs_and_returns_both_workspace_revisions() {
    let mcp = TestServer::new().await;
    let target_workspace = "10000000-0000-4000-8000-000000000002";
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-source-workspace"),
    )
    .await
    .unwrap();
    mcp.call(
        "org_create_workspace",
        create_workspace_input(target_workspace, "create-target-workspace"),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-moved-document",
            "document_id": DOCUMENT_ID,
            "path": "moving.org",
            "source": raw_source("Move across workspaces"),
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    let claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        DOCUMENT_ID,
        ITEM_ID,
        1,
        "claim-before-document-move",
    )
    .await;
    let claimed_revision = claim.context.document.revision;

    let base_move = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "move-document",
        "document_id": DOCUMENT_ID,
        "target_workspace_id": target_workspace,
        "expected_document_revision": claimed_revision,
        "expected_source_workspace_revision": 1,
        "expected_target_workspace_revision": 1,
        "lease_proofs": {}
    });
    let missing = mcp
        .call("org_move_document", base_move.clone())
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(missing["code"], "stale_lease");

    let mut proved = base_move;
    proved["lease_proofs"][ITEM_ID] = json!({
        "lease_id": claim.lease_id,
        "kind": "execution",
        "fencing_token": claim.fencing_token
    });
    let mut stale = proved.clone();
    stale["operation_id"] = json!("stale-document-move");
    stale["expected_document_revision"] = json!(claimed_revision + 10);
    let conflict = mcp
        .call("org_move_document", stale)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(conflict["code"], "stale_revision");
    assert_eq!(
        conflict["details"]["current_document_revision"],
        claimed_revision
    );
    assert_eq!(conflict["details"]["current_source_workspace_revision"], 1);
    assert_eq!(conflict["details"]["current_target_workspace_revision"], 1);
    let moved = mcp.call("org_move_document", proved).await.unwrap();
    assert_eq!(moved["data"]["document_id"], DOCUMENT_ID);
    assert_eq!(moved["data"]["source_workspace_id"], WORKSPACE_ID);
    assert_eq!(moved["data"]["target_workspace_id"], target_workspace);
    assert_eq!(moved["data"]["source_workspace_revision"], 2);
    assert_eq!(moved["data"]["target_workspace_revision"], 2);
    assert_eq!(
        moved["document_revisions"][DOCUMENT_ID],
        claimed_revision + 1
    );

    let missing_source = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(missing_source["code"], "not_found");
    let target_read = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": target_workspace, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert_eq!(target_read["revision"], claimed_revision + 1);
}

#[tokio::test]
async fn item_move_requires_exact_revisions_and_token_map_and_preserves_replay_boundary() {
    let mcp = TestServer::new().await;
    mcp.call(
        "org_create_workspace",
        create_workspace_input(WORKSPACE_ID, "create-workspace"),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-source-document",
            "document_id": DOCUMENT_ID,
            "path": "source.org",
            "source": raw_source("Move this item"),
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    mcp.call(
        "org_put_document",
        json!({
            "schema_version": 1,
            "workspace_id": WORKSPACE_ID,
            "actor_id": "agent-one",
            "operation_id": "seed-target-document",
            "document_id": SECOND_DOCUMENT_ID,
            "path": "target.org",
            "source": "#+TITLE: Target\r\n#+CUSTOM: retained\r\n",
            "expected_revision": null,
            "lease_proofs": {}
        }),
    )
    .await
    .unwrap();
    let claim = claim_execution(
        &mcp,
        WORKSPACE_ID,
        DOCUMENT_ID,
        ITEM_ID,
        1,
        "claim-before-item-move",
    )
    .await;
    let claimed_revision = claim.context.document.revision;
    let mut move_request = json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE_ID,
        "actor_id": "agent-one",
        "operation_id": "move-item",
        "item_id": ITEM_ID,
        "source_document_id": DOCUMENT_ID,
        "target_document_id": SECOND_DOCUMENT_ID,
        "target_parent_id": null,
        "expected_revisions": {
            DOCUMENT_ID: claimed_revision,
            SECOND_DOCUMENT_ID: 1
        },
        "lease_proofs": {}
    });
    let source_before_invalid = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    let target_before_invalid = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": SECOND_DOCUMENT_ID}),
        )
        .await
        .unwrap();
    let events_before_invalid = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    let mut incomplete_revisions = move_request.clone();
    incomplete_revisions["expected_revisions"]
        .as_object_mut()
        .unwrap()
        .remove(SECOND_DOCUMENT_ID);
    let invalid = mcp
        .call("org_move_item", incomplete_revisions)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(invalid["code"], "invalid_input");
    assert_eq!(
        mcp.call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap(),
        source_before_invalid
    );
    assert_eq!(
        mcp.call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": SECOND_DOCUMENT_ID}),
        )
        .await
        .unwrap(),
        target_before_invalid
    );
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        events_before_invalid
    );
    let missing = mcp
        .call("org_move_item", move_request.clone())
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(missing["code"], "stale_lease");

    move_request["lease_proofs"][ITEM_ID] = json!({
        "lease_id": claim.lease_id,
        "kind": "execution",
        "fencing_token": claim.fencing_token
    });
    let moved = mcp
        .call("org_move_item", move_request.clone())
        .await
        .unwrap();
    assert_eq!(moved["data"]["affected_document_count"], 2);
    assert_eq!(moved["data"]["resulting_items"][0]["id"], ITEM_ID);
    assert_eq!(
        moved["data"]["resulting_items"][0]["document_id"],
        SECOND_DOCUMENT_ID
    );
    assert_eq!(
        moved["data"]["resulting_items"][0]["parent_id"],
        Value::Null
    );
    assert_eq!(
        moved["data"]["operation"]["source_document_id"],
        DOCUMENT_ID
    );
    assert_eq!(
        moved["data"]["operation"]["target_document_id"],
        SECOND_DOCUMENT_ID
    );
    assert_eq!(
        moved["document_revisions"][DOCUMENT_ID],
        claimed_revision + 1
    );
    assert_eq!(moved["document_revisions"][SECOND_DOCUMENT_ID], 2);

    let event_count = mcp
        .backend
        .session()
        .await
        .unwrap()
        .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(
        mcp.call("org_move_item", move_request.clone())
            .await
            .unwrap(),
        moved
    );
    let mut divergent = move_request;
    divergent["target_parent_id"] = json!(SECOND_ITEM_ID);
    let conflict = mcp
        .call("org_move_item", divergent)
        .await
        .unwrap_err()
        .data
        .unwrap();
    assert_eq!(conflict["code"], "idempotency_conflict");
    assert_eq!(
        mcp.backend
            .session()
            .await
            .unwrap()
            .list_org_events(WORKSPACE_ID.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len(),
        event_count
    );

    let source = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": DOCUMENT_ID}),
        )
        .await
        .unwrap();
    let target = mcp
        .call(
            "org_get_document",
            json!({"workspace_id": WORKSPACE_ID, "document_id": SECOND_DOCUMENT_ID}),
        )
        .await
        .unwrap();
    assert!(!source["source"].as_str().unwrap().contains(ITEM_ID));
    assert!(target["source"].as_str().unwrap().contains(ITEM_ID));
    assert!(target["source"]
        .as_str()
        .unwrap()
        .contains("#+CUSTOM: retained\r\n"));
}
