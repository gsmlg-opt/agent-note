use std::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_org::{WorkspaceId, WorkspacePolicy};
use note_pipelines::{
    org::{FixedOrgClock, OrgContext, OrgError, OrgTokenSource},
    Context,
};
use note_server::{openapi::rest_router, AppState};
use note_storage::{NewOrgEvent, OrgEventType, StorageBackend};
use note_storage_turso::TursoStorage;
use serde_json::{json, Value};
use tower::ServiceExt;

const NOW: i64 = 1_900_000_000;
const WORKSPACE: &str = "10000000-0000-4000-8000-000000000001";
const DOCUMENT: &str = "20000000-0000-4000-8000-000000000001";
const ITEM_A: &str = "30000000-0000-4000-8000-000000000001";
const ITEM_B: &str = "30000000-0000-4000-8000-000000000002";

struct TokenSequence(Mutex<VecDeque<String>>);

impl TokenSequence {
    fn new() -> Self {
        Self(Mutex::new(
            (1..=20).map(|index| format!("raw-token-{index}")).collect(),
        ))
    }
}

impl OrgTokenSource for TokenSequence {
    fn generate_token(&self) -> Result<String, OrgError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| OrgError::invalid_input("test token sequence exhausted"))
    }
}

struct TestApp {
    app: Router,
    note: Arc<Context>,
    backend: Arc<dyn StorageBackend>,
    tokens: Arc<TokenSequence>,
    _dir: tempfile::TempDir,
}

impl TestApp {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("org-execution-rest.db"))
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
        let tokens = Arc::new(TokenSequence::new());
        let org = Arc::new(
            OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(NOW)))
                .with_token_source(tokens.clone()),
        );
        let (app, _) = rest_router();
        Self {
            app: app.with_state(AppState::new(note.clone(), org)),
            note,
            backend,
            tokens,
            _dir: dir,
        }
    }

    async fn call(&self, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
        self.call_on(&self.app, method, uri, body).await
    }

    async fn call_on(
        &self,
        app: &Router,
        method: Method,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = self
            .app_from(app)
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn app_from<'a>(&self, app: &'a Router) -> &'a Router {
        app
    }

    fn app_at(&self, now: i64) -> Router {
        let org = Arc::new(
            OrgContext::new(self.backend.clone(), Arc::new(FixedOrgClock::new(now)))
                .with_token_source(self.tokens.clone()),
        );
        let (app, _) = rest_router();
        app.with_state(AppState::new(self.note.clone(), org))
    }

    async fn seed(&self, source: &str, concurrency_limit: usize) {
        let mut policy = serde_json::to_value(WorkspacePolicy::engineering_default()).unwrap();
        policy["concurrency_limit"] = json!(concurrency_limit);
        let (status, body) = self
            .call(
                Method::POST,
                "/api/org/workspaces",
                json!({
                    "schema_version": 1,
                    "actor_id": "seed",
                    "operation_id": "create-workspace",
                    "workspace_id": WORKSPACE,
                    "slug": "execution-rest",
                    "display_name": "Execution REST",
                    "description": "Task 4 integration fixture",
                    "timezone": "UTC",
                    "policy_schema_version": 1,
                    "policy": policy
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = self
            .call(
                Method::PUT,
                &format!("/api/org/documents/{DOCUMENT}"),
                json!({
                    "schema_version": 1,
                    "actor_id": "seed",
                    "operation_id": "put-document",
                    "workspace_id": WORKSPACE,
                    "path": "execution.org",
                    "source": source,
                    "expected_revision": null,
                    "lease_proofs": {}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

fn command(actor_id: &str, operation_id: &str) -> Value {
    json!({
        "schema_version": 1,
        "workspace_id": WORKSPACE,
        "actor_id": actor_id,
        "operation_id": operation_id
    })
}

fn merge(mut command: Value, fields: Value) -> Value {
    command
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    command
}

fn claim_body(actor_id: &str, operation_id: &str, revision: i64, kind: &str) -> Value {
    merge(
        command(actor_id, operation_id),
        json!({
            "document_id": DOCUMENT,
            "expected_document_revision": revision,
            "kind": kind
        }),
    )
}

fn source(items: &[(&str, &str, bool)]) -> String {
    items
        .iter()
        .map(|(id, state, review)| {
            format!(
                "* {state} Work item\n:PROPERTIES:\n:ID: {id}\n:AGENT_NOTE_TYPE: task\n{}:END:\n",
                if *review {
                    ":REQUIRES_REVIEW: true\n"
                } else {
                    ""
                }
            )
        })
        .collect()
}

#[tokio::test]
async fn claim_rejects_an_invalid_item_path_with_the_structured_error_contract() {
    let api = TestApp::new().await;
    let (status, body) = api
        .call(
            Method::POST,
            "/api/org/items/not-a-uuid/claim",
            json!({
                "schema_version": 1,
                "workspace_id": "10000000-0000-4000-8000-000000000001",
                "actor_id": "agent-one",
                "operation_id": "invalid-claim",
                "document_id": "20000000-0000-4000-8000-000000000001",
                "expected_document_revision": 1,
                "kind": "execution"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_input");
    assert_eq!(body["retryable"], false);
    assert!(body.get("details").is_some());
}

#[tokio::test]
async fn claim_replays_the_exact_token_and_lease_bound_operations_never_echo_it() {
    let api = TestApp::new().await;
    api.seed(&source(&[(ITEM_A, "READY", false)]), 4).await;
    let sensitive_metadata = json!({
        "api_token": "legacy-api-token",
        "nested": {"token_hash": "legacy-token-digest"},
        "secret": "legacy-secret",
        "ordinary": "preserved"
    });
    api.backend
        .session()
        .await
        .unwrap()
        .append_org_event(NewOrgEvent {
            id: "legacy-sensitive-event",
            workspace_id: WORKSPACE.parse::<WorkspaceId>().unwrap(),
            subject_kind: "work_item",
            subject_id: ITEM_A,
            actor_id: "legacy-import",
            attempt_id: None,
            event_type: OrgEventType::Progress,
            occurred_at: NOW - 1,
            summary: "Imported legacy event",
            metadata: &sensitive_metadata,
            previous_state: Some("READY"),
            resulting_state: Some("READY"),
        })
        .await
        .unwrap();

    let claim_request = claim_body("agent-one", "claim-one", 1, "execution");
    let (status, claim) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_request.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    assert_eq!(claim["fencing_token"], "raw-token-1");
    assert_eq!(claim["context"]["item"]["state"], "RUNNING");
    assert_eq!(claim["context"]["document"]["revision"], 2);
    let legacy_metadata = &claim["context"]["history_segments"][0]["events"][0]["metadata"];
    assert_eq!(legacy_metadata["api_token"], "[REDACTED]");
    assert_eq!(legacy_metadata["nested"]["token_hash"], "[REDACTED]");
    assert_eq!(legacy_metadata["secret"], "[REDACTED]");
    assert_eq!(legacy_metadata["ordinary"], "preserved");
    assert!(!claim.to_string().contains("legacy-api-token"));
    assert!(!claim.to_string().contains("legacy-token-digest"));
    assert!(!claim.to_string().contains("legacy-secret"));
    let lease_id = claim["lease_id"].as_str().unwrap().to_owned();
    let token = claim["fencing_token"].as_str().unwrap().to_owned();

    let (status, replay) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_request,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, claim, "claim replay must preserve the raw token");

    let (status, active) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-two", "claim-active", 2, "execution"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(active["code"], "active_lease");
    assert_eq!(active["retryable"], true);
    assert!(!active.to_string().contains(&token));
    assert!(active.get("fencing_token").is_none());

    let heartbeat = merge(
        command("agent-one", "heartbeat-one"),
        json!({
            "lease_id": lease_id,
            "kind": "execution",
            "fencing_token": token
        }),
    );
    let (status, heartbeat_result) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim/heartbeat"),
            heartbeat,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{heartbeat_result}");
    assert_eq!(heartbeat_result["document_revisions"][DOCUMENT], 2);
    assert!(!heartbeat_result.to_string().contains("raw-token-1"));
    assert!(!heartbeat_result.to_string().contains("fencing_token"));

    let (status, stale_owner) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim/heartbeat"),
            merge(
                command("agent-two", "heartbeat-stale-owner"),
                json!({
                    "lease_id": claim["lease_id"],
                    "kind": "execution",
                    "fencing_token": claim["fencing_token"]
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale_owner["code"], "stale_lease");
    assert_eq!(stale_owner["retryable"], true);
    assert!(!stale_owner.to_string().contains("raw-token-1"));

    let (status, progress) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/progress"),
            merge(
                command("agent-one", "progress-one"),
                json!({
                    "lease_id": claim["lease_id"],
                    "kind": "execution",
                    "fencing_token": claim["fencing_token"],
                    "summary": "halfway",
                    "metadata": {"percent": 50}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{progress}");
    assert_eq!(progress["document_revisions"][DOCUMENT], 2);
    assert!(!progress.to_string().contains("raw-token-1"));

    let (status, released) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim/release"),
            merge(
                command("agent-one", "release-one"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 2,
                    "lease_id": claim["lease_id"],
                    "kind": "execution",
                    "fencing_token": claim["fencing_token"],
                    "target_state": null
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert_eq!(released["data"]["context"]["item"]["state"], "READY");
    assert!(!released.to_string().contains("raw-token-1"));
}

#[tokio::test]
async fn claims_map_capacity_to_429_and_expired_lease_reclaim_returns_a_new_token() {
    let api = TestApp::new().await;
    api.seed(
        &source(&[(ITEM_A, "READY", false), (ITEM_B, "READY", false)]),
        1,
    )
    .await;

    let (status, first) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-one", "claim-capacity-one", 1, "execution"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, capacity) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_B}/claim"),
            claim_body("agent-two", "claim-capacity-two", 2, "execution"),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(capacity["code"], "concurrency_limit");
    assert_eq!(capacity["retryable"], true);
    assert!(capacity.get("fencing_token").is_none());

    let expired_app = api.app_at(NOW + 901);
    let (status, reclaimed) = api
        .call_on(
            &expired_app,
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-two", "reclaim-expired", 2, "execution"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reclaimed}");
    assert_eq!(reclaimed["fencing_token"], "raw-token-2");
    assert_ne!(reclaimed["lease_id"], first["lease_id"]);
    assert_eq!(reclaimed["context"]["item"]["state"], "RUNNING");
}

#[tokio::test]
async fn result_and_explicit_review_flows_support_reviewer_claims_approval_and_rejection() {
    let api = TestApp::new().await;
    api.seed(&source(&[(ITEM_A, "READY", true)]), 4).await;
    let (_, execution) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-one", "claim-for-result", 1, "execution"),
        )
        .await;
    let (status, submitted) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/result"),
            merge(
                command("agent-one", "submit-review-required-result"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 2,
                    "lease_id": execution["lease_id"],
                    "fencing_token": execution["fencing_token"],
                    "result_summary": "ready for review",
                    "note_refs": [],
                    "artifacts": [],
                    "metadata": {"review": true}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    assert_eq!(submitted["data"]["context"]["item"]["state"], "REVIEW");
    assert_eq!(submitted["document_revisions"][DOCUMENT], 3);
    assert!(!submitted.to_string().contains("raw-token-1"));

    let (status, review_claim) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("reviewer", "claim-review", 3, "review"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{review_claim}");
    assert_eq!(review_claim["fencing_token"], "raw-token-2");
    assert_eq!(review_claim["context"]["lease"]["kind"], "review");

    let (status, approved) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/review/approve"),
            merge(
                command("reviewer", "approve-review"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 3,
                    "lease_id": review_claim["lease_id"],
                    "fencing_token": review_claim["fencing_token"],
                    "metadata": {"checked": true}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["data"]["context"]["item"]["state"], "DONE");
    assert!(!approved.to_string().contains("raw-token-2"));

    let api = TestApp::new().await;
    api.seed(&source(&[(ITEM_A, "READY", false)]), 4).await;
    let (_, execution) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-one", "claim-for-request-review", 1, "execution"),
        )
        .await;
    let (status, requested) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/review/request"),
            merge(
                command("agent-one", "request-review"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 2,
                    "lease_id": execution["lease_id"],
                    "fencing_token": execution["fencing_token"],
                    "result_summary": "partial result",
                    "note_refs": [],
                    "artifacts": [],
                    "metadata": {"partial": true}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{requested}");
    assert_eq!(requested["data"]["context"]["item"]["state"], "REVIEW");

    let (_, review_claim) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("reviewer", "claim-for-rejection", 3, "review"),
        )
        .await;
    let (status, rejected) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/review/reject"),
            merge(
                command("reviewer", "reject-review"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 3,
                    "lease_id": review_claim["lease_id"],
                    "fencing_token": review_claim["fencing_token"],
                    "reason": "needs revision",
                    "metadata": {"section": "tests"}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rejected}");
    assert_eq!(rejected["data"]["context"]["item"]["state"], "READY");
    assert!(!rejected.to_string().contains("raw-token-2"));
}

#[tokio::test]
async fn failure_retry_block_unblock_and_cancellation_follow_pipeline_policy() {
    let api = TestApp::new().await;
    api.seed(&source(&[(ITEM_A, "READY", false)]), 4).await;
    let (_, first_claim) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-one", "claim-before-failure", 1, "execution"),
        )
        .await;
    let (status, failed) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/transition"),
            merge(
                command("agent-one", "transition-failed"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 2,
                    "target_state": "FAILED",
                    "lease": {
                        "lease_id": first_claim["lease_id"],
                        "kind": "execution",
                        "fencing_token": first_claim["fencing_token"]
                    },
                    "error": "execution failed",
                    "metadata": {"phase": "build"}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{failed}");
    assert_eq!(failed["data"]["context"]["item"]["state"], "FAILED");

    let retry_request = merge(
        command("agent-one", "retry-failure"),
        json!({
            "document_id": DOCUMENT,
            "expected_document_revision": 3
        }),
    );
    let (status, retried) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/retry"),
            retry_request.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(retried["fencing_token"], "raw-token-2");
    assert_eq!(retried["context"]["item"]["state"], "RUNNING");
    let (_, retry_replay) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/retry"),
            retry_request,
        )
        .await;
    assert_eq!(
        retry_replay, retried,
        "retry replay must preserve the raw token"
    );

    let (status, blocked) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/transition"),
            merge(
                command("agent-one", "transition-blocked"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 4,
                    "target_state": "BLOCKED",
                    "lease": {
                        "lease_id": retried["lease_id"],
                        "kind": "execution",
                        "fencing_token": retried["fencing_token"]
                    },
                    "error": null,
                    "metadata": {}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{blocked}");
    assert_eq!(blocked["data"]["context"]["item"]["state"], "BLOCKED");

    let (status, unblocked) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/transition"),
            merge(
                command("agent-one", "transition-ready"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 5,
                    "target_state": "READY",
                    "lease": null,
                    "error": null,
                    "metadata": {}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{unblocked}");
    assert_eq!(unblocked["data"]["context"]["item"]["state"], "READY");

    let (_, final_claim) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/claim"),
            claim_body("agent-one", "claim-before-cancel", 6, "execution"),
        )
        .await;
    let (status, cancelled) = api
        .call(
            Method::POST,
            &format!("/api/org/items/{ITEM_A}/transition"),
            merge(
                command("agent-one", "transition-cancelled"),
                json!({
                    "document_id": DOCUMENT,
                    "expected_document_revision": 7,
                    "target_state": "CANCELLED",
                    "lease": {
                        "lease_id": final_claim["lease_id"],
                        "kind": "execution",
                        "fencing_token": final_claim["fencing_token"]
                    },
                    "error": null,
                    "metadata": {}
                }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["data"]["context"]["item"]["state"], "CANCELLED");
}

#[test]
fn openapi_limits_raw_tokens_to_claim_retry_success_and_marks_request_tokens_sensitive() {
    let (_, openapi) = rest_router();
    let document = serde_json::to_value(openapi).unwrap();
    let serialized = document.to_string();

    for schema in [
        "HeartbeatClaimBody",
        "ReleaseClaimBody",
        "ReportProgressBody",
        "SubmitResultBody",
        "LeaseProofBody",
        "RequestReviewBody",
        "ApproveItemBody",
        "RejectItemBody",
    ] {
        let definition = &document["components"]["schemas"][schema];
        assert!(definition.to_string().contains("fencing_token"), "{schema}");
        assert!(
            definition
                .to_string()
                .to_ascii_lowercase()
                .contains("sensitive"),
            "{schema} must mark fencing material sensitive"
        );
    }
    assert!(document["components"]["schemas"]["ClaimResultBody"]
        .to_string()
        .to_ascii_lowercase()
        .contains("sensitive"));
    assert!(
        document["components"]["schemas"]["OrgClaimResult"]["properties"]
            .get("fencing_token")
            .is_some()
    );
    assert!(!document["components"]["schemas"]["OrgApiError"]
        .to_string()
        .contains("fencing_token"));

    for (path, operation_id) in [
        ("/api/org/items/{item_id}/claim", "org_claim_item"),
        ("/api/org/items/{item_id}/retry", "org_retry_item"),
    ] {
        let response = &document["paths"][path]["post"]["responses"]["200"];
        assert_eq!(
            response["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/OrgClaimResult",
            "{operation_id} must publish the transport-neutral claim schema"
        );
        assert!(
            response["description"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("sensitive"),
            "{operation_id} must identify its raw fencing token as sensitive"
        );
    }
    for path in [
        "/api/org/items/{item_id}/claim/heartbeat",
        "/api/org/items/{item_id}/claim/release",
        "/api/org/items/{item_id}/progress",
        "/api/org/items/{item_id}/result",
        "/api/org/items/{item_id}/transition",
        "/api/org/items/{item_id}/review/request",
        "/api/org/items/{item_id}/review/approve",
        "/api/org/items/{item_id}/review/reject",
    ] {
        assert!(document["paths"][path]["post"]["requestBody"]
            .to_string()
            .contains("Body"));
        assert_ne!(
            document["paths"][path]["post"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/OrgClaimResult"
        );
    }

    let schemas = document["components"]["schemas"].as_object().unwrap();
    for path in [
        "/api/org/items/{item_id}/claim",
        "/api/org/items/{item_id}/claim/heartbeat",
        "/api/org/items/{item_id}/claim/release",
        "/api/org/items/{item_id}/progress",
        "/api/org/items/{item_id}/result",
        "/api/org/items/{item_id}/transition",
        "/api/org/items/{item_id}/retry",
        "/api/org/items/{item_id}/review/request",
        "/api/org/items/{item_id}/review/approve",
        "/api/org/items/{item_id}/review/reject",
    ] {
        let operation = &document["paths"][path]["post"];
        let operation_id = operation["operationId"].as_str().unwrap();
        for (status, response) in operation["responses"].as_object().unwrap() {
            let contains_token =
                schema_graph_contains(response, schemas, "fencing_token", &mut BTreeSet::new());
            let allowed =
                status == "200" && matches!(operation_id, "org_claim_item" | "org_retry_item");
            assert_eq!(
                contains_token, allowed,
                "unexpected fencing-token schema boundary for {operation_id} response {status}"
            );
        }
    }
    assert!(!serialized.to_ascii_lowercase().contains("securityschemes"));
}

fn schema_graph_contains(
    value: &Value,
    schemas: &serde_json::Map<String, Value>,
    needle: &str,
    visited: &mut BTreeSet<String>,
) -> bool {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if let Some(name) = reference.strip_prefix("#/components/schemas/") {
                    return visited.insert(name.to_owned())
                        && schemas.get(name).is_some_and(|schema| {
                            schema_graph_contains(schema, schemas, needle, visited)
                        });
                }
            }
            object.iter().any(|(key, value)| {
                key.contains(needle) || schema_graph_contains(value, schemas, needle, visited)
            })
        }
        Value::Array(values) => values
            .iter()
            .any(|value| schema_graph_contains(value, schemas, needle, visited)),
        Value::String(value) => value.contains(needle),
        _ => false,
    }
}
