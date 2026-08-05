use std::collections::{BTreeMap, BTreeSet};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use http_body_util::BodyExt;
use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_mcp::org::ORG_TOOL_NAMES;
use note_pipelines::{
    org::{OrgContext, OrgError, OrgErrorCode, SystemOrgClock},
    Context,
};
use note_server::{
    openapi::rest_router,
    org_api::error::{OrgApiError, OrgJson, OrgPath, OrgQuery},
    AppState,
};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde::Deserialize;
use std::sync::Arc;
use tower::ServiceExt;

const ORG_ROUTES: [(&str, &str, &str); 36] = [
    ("get", "/api/org/workspaces", "org_list_workspaces"),
    ("post", "/api/org/workspaces", "org_create_workspace"),
    (
        "get",
        "/api/org/workspaces/{workspace_id}",
        "org_get_workspace",
    ),
    (
        "patch",
        "/api/org/workspaces/{workspace_id}",
        "org_update_workspace",
    ),
    (
        "post",
        "/api/org/workspaces/{workspace_id}/archive",
        "org_archive_workspace",
    ),
    (
        "get",
        "/api/org/workspaces/{workspace_id}/documents",
        "org_list_documents",
    ),
    (
        "get",
        "/api/org/documents/{document_id}",
        "org_get_document",
    ),
    (
        "put",
        "/api/org/documents/{document_id}",
        "org_put_document",
    ),
    (
        "post",
        "/api/org/documents/{document_id}/move",
        "org_move_document",
    ),
    ("post", "/api/org/items/{item_id}/move", "org_move_item"),
    (
        "post",
        "/api/org/workspaces/{workspace_id}/import",
        "org_import_workspace",
    ),
    (
        "get",
        "/api/org/workspaces/{workspace_id}/export",
        "org_export_workspace",
    ),
    (
        "post",
        "/api/org/workspaces/{workspace_id}/items",
        "org_create_item",
    ),
    ("get", "/api/org/items/{item_id}", "org_get_item"),
    (
        "get",
        "/api/org/items/{item_id}/context",
        "org_get_item_context",
    ),
    (
        "post",
        "/api/org/items/{item_id}/follow-ups",
        "org_create_follow_up",
    ),
    (
        "post",
        "/api/org/items/{item_id}/assignment",
        "org_assign_item",
    ),
    (
        "post",
        "/api/org/items/{item_id}/schedule",
        "org_schedule_item",
    ),
    ("get", "/api/org/queue", "org_query_queue"),
    ("get", "/api/org/agenda", "org_query_agenda"),
    ("post", "/api/org/items/{item_id}/claim", "org_claim_item"),
    (
        "post",
        "/api/org/items/{item_id}/claim/heartbeat",
        "org_heartbeat_claim",
    ),
    (
        "post",
        "/api/org/items/{item_id}/claim/release",
        "org_release_claim",
    ),
    (
        "post",
        "/api/org/items/{item_id}/progress",
        "org_report_progress",
    ),
    (
        "post",
        "/api/org/items/{item_id}/result",
        "org_submit_result",
    ),
    (
        "post",
        "/api/org/items/{item_id}/transition",
        "org_transition_item",
    ),
    ("post", "/api/org/items/{item_id}/retry", "org_retry_item"),
    (
        "post",
        "/api/org/items/{item_id}/review/request",
        "org_request_review",
    ),
    (
        "post",
        "/api/org/items/{item_id}/review/approve",
        "org_approve_item",
    ),
    (
        "post",
        "/api/org/items/{item_id}/review/reject",
        "org_reject_item",
    ),
    (
        "post",
        "/api/org/items/{item_id}/dependencies",
        "org_add_dependency",
    ),
    (
        "delete",
        "/api/org/items/{item_id}/dependencies/{dependency_item_id}",
        "org_remove_dependency",
    ),
    (
        "post",
        "/api/org/items/{item_id}/note-links",
        "org_link_note",
    ),
    (
        "delete",
        "/api/org/items/{item_id}/note-links",
        "org_unlink_note",
    ),
    (
        "get",
        "/api/org/notes/{note_id}/work-items",
        "org_list_note_work_items",
    ),
    (
        "get",
        "/api/org/workspaces/{workspace_id}/events",
        "org_list_events",
    ),
];

fn document() -> serde_json::Value {
    let (_, document) = rest_router();
    serde_json::to_value(document).expect("serialize generated OpenAPI")
}

fn schema_graph_contains(
    value: &serde_json::Value,
    schemas: &serde_json::Map<String, serde_json::Value>,
    needle: &str,
    visited: &mut BTreeSet<String>,
) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
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
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| schema_graph_contains(value, schemas, needle, visited)),
        serde_json::Value::String(value) => value.contains(needle),
        _ => false,
    }
}

#[test]
fn generated_openapi_has_exact_mcp_org_inventory() {
    let document = document();
    let expected_names = ORG_ROUTES
        .iter()
        .map(|(_, _, operation_id)| *operation_id)
        .collect::<Vec<_>>();
    assert_eq!(expected_names, ORG_TOOL_NAMES);

    let mut actual = BTreeMap::new();
    for (path, item) in document["paths"].as_object().expect("OpenAPI paths") {
        if !path.starts_with("/api/org") {
            continue;
        }
        for method in ["get", "put", "post", "delete", "patch"] {
            if let Some(operation) = item.get(method) {
                let operation_id = operation["operationId"].as_str().expect("Org operationId");
                assert_eq!(operation["tags"], serde_json::json!(["org"]));
                assert!(
                    actual
                        .insert(
                            (method.to_owned(), path.to_owned()),
                            operation_id.to_owned()
                        )
                        .is_none(),
                    "duplicate {method} {path}"
                );
            }
        }
    }

    let expected = ORG_ROUTES
        .iter()
        .map(|(method, path, operation_id)| {
            (
                ((*method).to_owned(), (*path).to_owned()),
                (*operation_id).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert_eq!(
        actual.values().collect::<BTreeSet<_>>().len(),
        ORG_TOOL_NAMES.len()
    );

    let org_tags = document["tags"]
        .as_array()
        .expect("OpenAPI tags")
        .iter()
        .filter(|tag| tag["name"] == "org")
        .count();
    assert_eq!(org_tags, 1);
}

#[test]
fn common_org_schemas_document_mutations_pagination_errors_and_no_auth() {
    let document = document();
    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("OpenAPI schemas");

    let envelope = &schemas["OrgMutationEnvelope"];
    assert_eq!(
        envelope["required"],
        serde_json::json!(["schema_version", "actor_id", "operation_id"])
    );
    assert_eq!(
        envelope["properties"]
            .as_object()
            .expect("mutation properties")
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["actor_id", "operation_id", "schema_version"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );

    let page = &schemas["OrgPageQuery"];
    assert_eq!(page["properties"]["limit"]["default"], 50);
    assert_eq!(page["properties"]["limit"]["minimum"], 1);
    assert_eq!(page["properties"]["limit"]["maximum"], 200);
    assert!(
        serde_json::to_string(&page["properties"]["cursor"])
            .unwrap()
            .to_ascii_lowercase()
            .contains("opaque"),
        "cursor must be documented as opaque"
    );

    let error = &schemas["OrgApiError"];
    assert_eq!(
        error["required"]
            .as_array()
            .expect("error required fields")
            .iter()
            .map(|field| field.as_str().expect("field name"))
            .collect::<BTreeSet<_>>(),
        ["code", "details", "message", "retryable"]
            .into_iter()
            .collect()
    );
    assert_eq!(error["properties"]["code"]["example"], "active_lease");
    assert_eq!(error["properties"]["retryable"]["example"], false);
    assert_eq!(
        error["properties"]
            .as_object()
            .expect("error properties")
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["code", "details", "message", "retryable"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    for (path, item) in document["paths"].as_object().expect("OpenAPI paths") {
        if !path.starts_with("/api/org") {
            continue;
        }
        for method in ["get", "post", "put", "patch", "delete"] {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let operation_id = operation["operationId"].as_str().expect("operationId");
            for (status, response) in operation["responses"]
                .as_object()
                .expect("operation responses")
            {
                let contains_token =
                    schema_graph_contains(response, schemas, "fencing_token", &mut BTreeSet::new());
                let allowed_success = status.starts_with('2')
                    && matches!(operation_id, "org_claim_item" | "org_retry_item");
                assert!(
                    !contains_token || allowed_success,
                    "fencing_token is forbidden in {operation_id} response {status}"
                );
            }
        }
    }
    assert!(!serde_json::to_string(schemas)
        .unwrap()
        .to_ascii_lowercase()
        .contains("sql"));

    assert!(document.get("security").is_none());
    assert!(document["components"].get("securitySchemes").is_none());
}

#[tokio::test]
async fn org_routes_extract_the_shared_org_context_from_app_state() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("org-route-state.db"))
            .await
            .unwrap(),
    );
    let note = Arc::new(Context::new(
        storage.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    ));
    let org = Arc::new(OrgContext::new(storage, Arc::new(SystemOrgClock)));
    let state = AppState::new(note, org.clone());
    assert!(Arc::ptr_eq(&state.org, &org));
    let (router, _) = rest_router();

    let response = router
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/api/org/workspaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], "storage_failure");
    assert_eq!(body["message"], "Org storage operation failed");
}

#[tokio::test]
async fn structured_error_mapping_is_stable_and_storage_details_are_safe() {
    let cases = [
        (OrgErrorCode::InvalidInput, StatusCode::BAD_REQUEST, false),
        (
            OrgErrorCode::UnsupportedSemanticEdit,
            StatusCode::BAD_REQUEST,
            false,
        ),
        (OrgErrorCode::NotFound, StatusCode::NOT_FOUND, false),
        (OrgErrorCode::NoteUnavailable, StatusCode::NOT_FOUND, false),
        (OrgErrorCode::ArchivedWorkspace, StatusCode::CONFLICT, false),
        (OrgErrorCode::StaleRevision, StatusCode::CONFLICT, false),
        (
            OrgErrorCode::IdempotencyConflict,
            StatusCode::CONFLICT,
            false,
        ),
        (OrgErrorCode::InvalidTransition, StatusCode::CONFLICT, false),
        (OrgErrorCode::DependencyBlocked, StatusCode::CONFLICT, false),
        (OrgErrorCode::ReviewRequired, StatusCode::CONFLICT, false),
        (OrgErrorCode::ActiveLease, StatusCode::CONFLICT, false),
        (OrgErrorCode::StaleLease, StatusCode::CONFLICT, false),
        (OrgErrorCode::RetryLimit, StatusCode::CONFLICT, false),
        (
            OrgErrorCode::ConcurrencyLimit,
            StatusCode::TOO_MANY_REQUESTS,
            true,
        ),
        (
            OrgErrorCode::StorageFailure,
            StatusCode::INTERNAL_SERVER_ERROR,
            false,
        ),
    ];

    for (code, expected_status, expected_retryable) in cases {
        let expected_code = serde_json::to_value(code).unwrap();
        let response = OrgApiError::from(OrgError::new(
            code,
            if code == OrgErrorCode::StorageFailure {
                "unsafe database text token=secret"
            } else {
                "safe public error"
            },
            serde_json::json!({"fencing_token": "secret", "current_revision": 7}),
            false,
        ))
        .into_response();
        assert_eq!(response.status(), expected_status, "{code:?}");
        assert_eq!(
            response.headers()["content-type"],
            "application/json",
            "{code:?}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], expected_code, "{code:?}");
        assert_eq!(body["retryable"], expected_retryable, "{code:?}");
        assert!(!serde_json::to_string(&body).unwrap().contains("secret"));
        if code == OrgErrorCode::StorageFailure {
            assert_eq!(body["message"], "Org storage operation failed");
            assert_eq!(body["details"], serde_json::json!({}));
        } else {
            assert_eq!(body["details"]["current_revision"], 7);
            assert!(body["details"].get("fencing_token").is_none());
        }
    }
}

#[derive(Deserialize)]
struct NumberInput {
    value: u32,
}

async fn json_input(OrgJson(input): OrgJson<NumberInput>) -> String {
    input.value.to_string()
}

async fn query_input(OrgQuery(input): OrgQuery<NumberInput>) -> String {
    input.value.to_string()
}

async fn path_input(OrgPath(input): OrgPath<NumberInput>) -> String {
    input.value.to_string()
}

#[tokio::test]
async fn org_extractors_normalize_rejections_to_structured_invalid_input() {
    const SENTINEL: &str = "reserved-fencing-sentinel";
    let app = Router::new()
        .route("/json", post(json_input))
        .route("/query", get(query_input))
        .route("/path/{value}", get(path_input));

    for request in [
        Request::builder()
            .method("POST")
            .uri("/json")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"value":"{SENTINEL}"}}"#)))
            .unwrap(),
        Request::builder()
            .uri(format!("/query?value={SENTINEL}"))
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri(format!("/path/{SENTINEL}"))
            .body(Body::empty())
            .unwrap(),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["content-type"], "application/json");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "invalid_input");
        assert_eq!(body["retryable"], false);
        assert_eq!(body["details"], serde_json::json!({}));
        assert!(!serde_json::to_string(&body).unwrap().contains(SENTINEL));
    }
}
