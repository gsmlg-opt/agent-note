use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_pipelines::{
    org::{
        claim_item, CommandEnvelope, FixedOrgClock, OrgClaimKind, OrgContext, StartClaimRequest,
    },
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

const NOW: i64 = 1_800_000_000;
const WORKSPACE_A: &str = "10000000-0000-4000-8000-000000000001";
const WORKSPACE_B: &str = "10000000-0000-4000-8000-000000000002";
const DOCUMENT_A: &str = "20000000-0000-4000-8000-000000000001";
const DOCUMENT_B: &str = "20000000-0000-4000-8000-000000000002";
const ITEM_ID: &str = "30000000-0000-4000-8000-000000000001";

struct TestApp {
    app: Router,
    backend: Arc<dyn StorageBackend>,
    org: Arc<OrgContext>,
    _dir: tempfile::TempDir,
}

impl TestApp {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-rest.db"))
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
        let (app, _) = rest_router();
        Self {
            app: app.with_state(AppState::new(note, org.clone())),
            backend,
            org,
            _dir: dir,
        }
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        let body = if let Some(body) = body {
            request = request.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "application/json");
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn events(&self, workspace_id: &str) -> usize {
        self.backend
            .session()
            .await
            .unwrap()
            .list_org_events(workspace_id.parse().unwrap(), None, 200)
            .await
            .unwrap()
            .len()
    }
}

fn policy() -> Value {
    serde_json::to_value(note_org::WorkspacePolicy::engineering_default()).unwrap()
}

fn envelope(operation_id: &str) -> Value {
    json!({
        "schema_version": 1,
        "actor_id": "agent-one",
        "operation_id": operation_id,
    })
}

fn create_workspace(workspace_id: &str, operation_id: &str) -> Value {
    let mut body = envelope(operation_id);
    let object = body.as_object_mut().unwrap();
    object.insert("workspace_id".into(), json!(workspace_id));
    object.insert("slug".into(), json!(format!("workspace-{workspace_id}")));
    object.insert("display_name".into(), json!("Engineering"));
    object.insert("description".into(), json!("Org REST workspace"));
    object.insert("timezone".into(), json!("Asia/Shanghai"));
    object.insert("policy_schema_version".into(), json!(1));
    object.insert("policy".into(), policy());
    body
}

fn with_fields(mut body: Value, fields: &[(&str, Value)]) -> Value {
    let object = body.as_object_mut().unwrap();
    for (key, value) in fields {
        object.insert((*key).to_owned(), value.clone());
    }
    body
}

fn raw_source(line_ending: &str) -> String {
    [
        "#+TITLE: Exact bytes",
        "#+CUSTOM: opaque syntax",
        "* READY [#A] Preserve me",
        ":PROPERTIES:",
        &format!(":ID: {ITEM_ID}"),
        ":AGENT_NOTE_TYPE: task",
        ":END:",
        "Body with trailing spaces  ",
        "",
    ]
    .join(line_ending)
}

#[tokio::test]
async fn workspace_routes_cover_active_archived_create_get_update_and_archive() {
    let api = TestApp::new().await;

    let (status, created) = api
        .call(
            Method::POST,
            "/api/org/workspaces",
            Some(create_workspace(WORKSPACE_A, "create-a")),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["workspace_revision"], 1);
    assert_eq!(created["data"]["workspace_id"], WORKSPACE_A);

    let (status, listed) = api
        .call(Method::GET, "/api/org/workspaces?limit=50", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["items"][0]["workspace_id"], WORKSPACE_A);

    let (status, workspace) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(workspace["revision"], 1);

    let update = with_fields(
        envelope("update-a"),
        &[
            ("expected_revision", json!(1)),
            ("slug", json!("engineering")),
            ("display_name", json!("Platform Engineering")),
            ("description", json!("Updated through REST")),
            ("timezone", json!("America/New_York")),
            ("policy_schema_version", json!(1)),
            ("policy", policy()),
        ],
    );
    let (status, updated) = api
        .call(
            Method::PATCH,
            &format!("/api/org/workspaces/{WORKSPACE_A}"),
            Some(update),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["workspace_revision"], 2);

    let archive = with_fields(envelope("archive-a"), &[("expected_revision", json!(2))]);
    let (status, archived) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_A}/archive"),
            Some(archive),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived["workspace_revision"], 3);
    assert_eq!(archived["data"]["archived_at"], NOW);

    let (_, active) = api
        .call(Method::GET, "/api/org/workspaces?limit=50", None)
        .await;
    assert_eq!(active["items"], json!([]));
    let (_, including_archived) = api
        .call(
            Method::GET,
            "/api/org/workspaces?limit=50&include_archived=true",
            None,
        )
        .await;
    assert_eq!(including_archived["items"][0]["workspace_id"], WORKSPACE_A);
}

#[tokio::test]
async fn document_routes_preserve_raw_bytes_import_export_moves_and_archived_reads() {
    let api = TestApp::new().await;
    for (workspace, operation) in [(WORKSPACE_A, "create-a"), (WORKSPACE_B, "create-b")] {
        let (status, _) = api
            .call(
                Method::POST,
                "/api/org/workspaces",
                Some(create_workspace(workspace, operation)),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    let crlf = raw_source("\r\n");
    let put = with_fields(
        envelope("put-a"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("path", json!("tasks/exact.org")),
            ("source", json!(crlf)),
            ("expected_revision", Value::Null),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, put_result) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(put),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put_result["document_revisions"][DOCUMENT_A], 1);

    let (status, raw) = api
        .call(
            Method::GET,
            &format!("/api/org/documents/{DOCUMENT_A}?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw["source"], raw_source("\r\n"));

    let (_, listed) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_A}/documents?limit=50&include_archived=false"),
            None,
        )
        .await;
    assert_eq!(listed["items"][0]["id"], DOCUMENT_A);

    let import = with_fields(
        envelope("import-b"),
        &[
            (
                "documents",
                json!([{
                    "document_id": DOCUMENT_B,
                    "path": "tasks/lf.org",
                    "source": raw_source("\n").replace(ITEM_ID, "30000000-0000-4000-8000-000000000002")
                }]),
            ),
            ("expected_revisions", json!({})),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, imported) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_B}/import"),
            Some(import),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported["data"]["document_count"], 1);

    let move_document = with_fields(
        envelope("move-document"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("target_workspace_id", json!(WORKSPACE_B)),
            ("expected_document_revision", json!(1)),
            ("expected_source_workspace_revision", json!(1)),
            ("expected_target_workspace_revision", json!(1)),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, moved) = api
        .call(
            Method::POST,
            &format!("/api/org/documents/{DOCUMENT_A}/move"),
            Some(move_document),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["data"]["target_workspace_id"], WORKSPACE_B);

    let move_item = with_fields(
        envelope("move-item"),
        &[
            ("workspace_id", json!(WORKSPACE_B)),
            ("source_document_id", json!(DOCUMENT_A)),
            ("target_document_id", json!(DOCUMENT_B)),
            ("target_parent_id", Value::Null),
            ("expected_revisions", json!({DOCUMENT_A: 2, DOCUMENT_B: 1})),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, moved_item) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_ID}/move"),
            Some(move_item),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved_item["data"]["affected_document_count"], 2);

    let (_, exported) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_B}/export"),
            None,
        )
        .await;
    assert!(exported["documents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|document| document["source"]
            .as_str()
            .unwrap()
            .contains("#+CUSTOM: opaque syntax\r\n")));

    let revision = exported["workspace"]["revision"].as_i64().unwrap();
    let archive = with_fields(
        envelope("archive-b"),
        &[("expected_revision", json!(revision))],
    );
    let (status, _) = api
        .call(
            Method::POST,
            &format!("/api/org/workspaces/{WORKSPACE_B}/archive"),
            Some(archive),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, archived_raw) = api
        .call(
            Method::GET,
            &format!("/api/org/documents/{DOCUMENT_B}?workspace_id={WORKSPACE_B}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(archived_raw["source"].as_str().unwrap().contains(ITEM_ID));
    let (_, hidden_documents) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_B}/documents?limit=50"),
            None,
        )
        .await;
    assert_eq!(hidden_documents["items"], json!([]));
    let (_, visible_documents) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_B}/documents?limit=50&include_archived=true"),
            None,
        )
        .await;
    assert_eq!(visible_documents["items"].as_array().unwrap().len(), 2);
    let (status, archived_export) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_B}/export"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived_export["workspace"]["archived_at"], NOW);
}

#[tokio::test]
async fn revision_and_idempotency_conflicts_return_json_and_leave_all_state_unchanged() {
    let api = TestApp::new().await;
    let create = create_workspace(WORKSPACE_A, "create-a");
    let (_, original) = api
        .call(Method::POST, "/api/org/workspaces", Some(create.clone()))
        .await;
    let events_after_create = api.events(WORKSPACE_A).await;
    let (status, replay) = api
        .call(Method::POST, "/api/org/workspaces", Some(create.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, original);
    assert_eq!(api.events(WORKSPACE_A).await, events_after_create);

    let divergent = with_fields(create, &[("display_name", json!("Divergent"))]);
    let (status, conflict) = api
        .call(Method::POST, "/api/org/workspaces", Some(divergent))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");
    assert_eq!(api.events(WORKSPACE_A).await, events_after_create);

    let stale_update = with_fields(
        envelope("stale-update"),
        &[
            ("expected_revision", json!(9)),
            ("slug", json!("engineering")),
            ("display_name", json!("Must not apply")),
            ("description", json!("Must not apply")),
            ("timezone", json!("UTC")),
            ("policy_schema_version", json!(1)),
            ("policy", policy()),
        ],
    );
    let (status, stale) = api
        .call(
            Method::PATCH,
            &format!("/api/org/workspaces/{WORKSPACE_A}"),
            Some(stale_update),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(stale["details"]["current_revision"], 1);
    assert_eq!(api.events(WORKSPACE_A).await, events_after_create);
    let (_, unchanged) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_A}"),
            None,
        )
        .await;
    assert_eq!(unchanged["display_name"], "Engineering");
    assert_eq!(unchanged["revision"], 1);
}

#[tokio::test]
async fn document_replay_stale_revision_and_divergent_reuse_are_atomic() {
    let api = TestApp::new().await;
    api.call(
        Method::POST,
        "/api/org/workspaces",
        Some(create_workspace(WORKSPACE_A, "create-a")),
    )
    .await;

    let put = with_fields(
        envelope("put-a"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("path", json!("tasks/exact.org")),
            ("source", json!(raw_source("\r\n"))),
            ("expected_revision", Value::Null),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, original) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(put.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let events_after_put = api.events(WORKSPACE_A).await;
    let projection_after_put = api
        .backend
        .session()
        .await
        .unwrap()
        .list_org_workspace_projection(WORKSPACE_A.parse().unwrap())
        .await
        .unwrap();
    let (_, export_after_put) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_A}/export"),
            None,
        )
        .await;

    let (status, replay) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(put.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, original);
    assert_eq!(api.events(WORKSPACE_A).await, events_after_put);

    let divergent = with_fields(put, &[("source", json!(raw_source("\n")))]);
    let (status, conflict) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(divergent),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");
    assert_eq!(api.events(WORKSPACE_A).await, events_after_put);

    let stale = with_fields(
        envelope("stale-put"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("path", json!("tasks/stale.org")),
            ("source", json!(raw_source("\n"))),
            ("expected_revision", json!(99)),
            ("lease_proofs", json!({})),
        ],
    );
    let (status, stale) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(stale),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(stale["details"]["current_revision"], 1);
    assert_eq!(api.events(WORKSPACE_A).await, events_after_put);
    let (_, export_after_conflicts) = api
        .call(
            Method::GET,
            &format!("/api/org/workspaces/{WORKSPACE_A}/export"),
            None,
        )
        .await;
    assert_eq!(export_after_conflicts, export_after_put);
    assert_eq!(
        api.backend
            .session()
            .await
            .unwrap()
            .list_org_workspace_projection(WORKSPACE_A.parse().unwrap())
            .await
            .unwrap(),
        projection_after_put
    );
}

#[tokio::test]
async fn document_mutations_accept_valid_item_keyed_fencing_token_maps() {
    let api = TestApp::new().await;
    api.call(
        Method::POST,
        "/api/org/workspaces",
        Some(create_workspace(WORKSPACE_A, "create-a")),
    )
    .await;
    let put = with_fields(
        envelope("put-a"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("path", json!("tasks/exact.org")),
            ("source", json!(raw_source("\r\n"))),
            ("expected_revision", Value::Null),
            ("lease_proofs", json!({})),
        ],
    );
    api.call(
        Method::PUT,
        &format!("/api/org/documents/{DOCUMENT_A}"),
        Some(put),
    )
    .await;

    let claim = claim_item(
        &api.org,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id: WORKSPACE_A.parse().unwrap(),
            actor_id: "agent-one".into(),
            operation_id: "claim-item".into(),
        },
        &StartClaimRequest {
            work_item_id: ITEM_ID.parse().unwrap(),
            document_id: DOCUMENT_A.parse().unwrap(),
            expected_document_revision: 1,
            kind: OrgClaimKind::Execution,
        },
    )
    .await
    .unwrap();
    let (_, current) = api
        .call(
            Method::GET,
            &format!("/api/org/documents/{DOCUMENT_A}?workspace_id={WORKSPACE_A}"),
            None,
        )
        .await;
    let source = current["source"]
        .as_str()
        .unwrap()
        .replace("Preserve me", "Preserve me through REST");
    let update = with_fields(
        envelope("lease-proved-put"),
        &[
            ("workspace_id", json!(WORKSPACE_A)),
            ("path", json!("tasks/exact.org")),
            ("source", json!(source)),
            ("expected_revision", json!(claim.context.document.revision)),
            (
                "lease_proofs",
                json!({
                    ITEM_ID: {
                        "lease_id": claim.lease_id,
                        "kind": "execution",
                        "fencing_token": claim.fencing_token,
                    }
                }),
            ),
        ],
    );
    let raw_token = claim.fencing_token.clone();
    let (status, result) = api
        .call(
            Method::PUT,
            &format!("/api/org/documents/{DOCUMENT_A}"),
            Some(update),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["document_revisions"][DOCUMENT_A], 3);
    assert!(!result.to_string().contains(&raw_token));
}

#[tokio::test]
async fn malformed_path_query_and_body_are_structured_json_errors() {
    let api = TestApp::new().await;
    for (method, uri, body) in [
        (Method::GET, "/api/org/workspaces/not-a-uuid", None),
        (
            Method::GET,
            "/api/org/documents/not-a-uuid?workspace_id=also-not-a-uuid",
            None,
        ),
        (
            Method::POST,
            "/api/org/workspaces",
            Some(json!({"schema_version": "secret-token"})),
        ),
    ] {
        let (status, error) = api.call(method, uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "invalid_input");
        assert_eq!(error["details"], json!({}));
        assert!(!error.to_string().contains("secret-token"));
    }
}
