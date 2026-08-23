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

const ORG_ROUTES: [(&str, &str, &str); 40] = [
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
        "/api/org/workspaces/{workspace_id}/documents",
        "org_create_document",
    ),
    (
        "patch",
        "/api/org/documents/{document_id}/path",
        "org_rename_document",
    ),
    (
        "post",
        "/api/org/documents/{document_id}/archive",
        "org_archive_document",
    ),
    (
        "post",
        "/api/org/documents/{document_id}/restore",
        "org_restore_document",
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

const ORG_HANDLER_SOURCES: [(&str, &str); 8] = [
    ("workspaces", include_str!("../src/org_api/workspaces.rs")),
    ("documents", include_str!("../src/org_api/documents.rs")),
    ("items", include_str!("../src/org_api/items.rs")),
    ("operational", include_str!("../src/org_api/operational.rs")),
    ("execution", include_str!("../src/org_api/execution.rs")),
    ("review", include_str!("../src/org_api/review.rs")),
    (
        "relationships",
        include_str!("../src/org_api/relationships.rs"),
    ),
    ("audit", include_str!("../src/org_api/audit.rs")),
];

fn document() -> serde_json::Value {
    let (_, document) = rest_router();
    serde_json::to_value(document).expect("serialize generated OpenAPI")
}

fn expected_success_schema(operation_id: &str) -> &'static str {
    match operation_id {
        "org_list_workspaces" => "OrgWorkspacePageResult",
        "org_create_workspace" | "org_update_workspace" => "OrgWorkspaceCommandResult",
        "org_get_workspace" => "OrgWorkspaceResult",
        "org_archive_workspace" => "OrgArchiveCommandResult",
        "org_list_documents" => "OrgDocumentPageResult",
        "org_get_document" => "OrgDocumentSourceResult",
        "org_put_document" | "org_import_workspace" => "OrgDocumentCountCommandResult",
        "org_create_document"
        | "org_rename_document"
        | "org_archive_document"
        | "org_restore_document" => "OrgDocumentLifecycleCommandResult",
        "org_move_document" => "OrgMoveDocumentCommandResult",
        "org_move_item" => "OrgMoveItemCommandResult",
        "org_export_workspace" => "OrgWorkspaceExportResult",
        "org_create_item"
        | "org_create_follow_up"
        | "org_assign_item"
        | "org_schedule_item"
        | "org_add_dependency"
        | "org_remove_dependency"
        | "org_link_note"
        | "org_unlink_note" => "OrgItemCommandResult",
        "org_get_item" => "OrgItemResult",
        "org_get_item_context" => "OrgItemContextResult",
        "org_query_queue" | "org_query_agenda" => "OrgOperationalPageResult",
        "org_claim_item" | "org_retry_item" => "OrgClaimResult",
        "org_heartbeat_claim" => "OrgHeartbeatCommandResult",
        "org_release_claim"
        | "org_report_progress"
        | "org_submit_result"
        | "org_transition_item"
        | "org_request_review"
        | "org_approve_item"
        | "org_reject_item" => "OrgContextCommandResult",
        "org_list_note_work_items" => "OrgItemPageResult",
        "org_list_events" => "OrgEventPageResult",
        _ => panic!("unknown operation {operation_id}"),
    }
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
                if reference.contains(needle) {
                    return true;
                }
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

fn assert_fully_typed_schema_graph(
    value: &serde_json::Value,
    schemas: &serde_json::Map<String, serde_json::Value>,
    path: &str,
    visited: &mut BTreeSet<String>,
) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                let name = reference
                    .strip_prefix("#/components/schemas/")
                    .unwrap_or_else(|| panic!("{path} has non-component ref {reference}"));
                let schema = schemas
                    .get(name)
                    .unwrap_or_else(|| panic!("{path} references missing schema {name}"));
                if visited.insert(name.to_owned()) {
                    assert_fully_typed_schema_graph(
                        schema,
                        schemas,
                        &format!("{path} -> {name}"),
                        visited,
                    );
                }
                return;
            }
            if object.get("type") == Some(&serde_json::json!("object")) {
                let has_properties = object
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|properties| !properties.is_empty());
                let has_typed_additional_properties = object
                    .get("additionalProperties")
                    .is_some_and(|additional| additional.is_object());
                assert!(
                    has_properties || has_typed_additional_properties,
                    "{path} contains an unconstrained object placeholder: {value}"
                );
            }
            if object.get("type") == Some(&serde_json::json!("array")) {
                assert!(
                    object.contains_key("items"),
                    "{path} array has no item schema"
                );
            }
            for (key, child) in object {
                assert_fully_typed_schema_graph(child, schemas, &format!("{path}.{key}"), visited);
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                assert_fully_typed_schema_graph(
                    child,
                    schemas,
                    &format!("{path}[{index}]"),
                    visited,
                );
            }
        }
        _ => {}
    }
}

fn schema_accepts_null(value: &serde_json::Value) -> bool {
    value["type"]
        .as_array()
        .is_some_and(|types| types.contains(&serde_json::json!("null")))
        || ["oneOf", "anyOf"].into_iter().any(|keyword| {
            value[keyword]
                .as_array()
                .is_some_and(|variants| variants.iter().any(|variant| variant["type"] == "null"))
        })
}

fn handler_delegate_region<'a>(source: &'a str, operation_id: &str) -> &'a str {
    let operation_offset = source
        .find(&format!("\"{operation_id}\""))
        .unwrap_or_else(|| panic!("missing handler for {operation_id}"));
    let prefix = &source[..operation_offset];
    let macro_start = [
        "command_operation!(",
        "item_mutation!(",
        "review_operation!(",
    ]
    .into_iter()
    .filter_map(|marker| prefix.rfind(marker))
    .max();
    if let Some(start) = macro_start {
        if !source[start..operation_offset].contains(");") {
            let end = operation_offset
                + source[operation_offset..]
                    .find(");")
                    .unwrap_or_else(|| panic!("unterminated handler macro for {operation_id}"))
                + 2;
            return &source[start..end];
        }
    }

    let function_start = operation_offset
        + source[operation_offset..]
            .find("async fn ")
            .unwrap_or_else(|| panic!("missing function for {operation_id}"));
    let body_start = function_start
        + source[function_start..]
            .find('{')
            .unwrap_or_else(|| panic!("missing function body for {operation_id}"));
    let mut depth = 0_u32;
    for (offset, byte) in source[body_start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[function_start..body_start + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body for {operation_id}")
}

fn assert_handler_macro_delegates(source: &str, macro_name: &str) {
    let start = source
        .find(&format!("macro_rules! {macro_name}"))
        .unwrap_or_else(|| panic!("missing {macro_name} definition"));
    let invocation = source[start..]
        .find(&format!("{macro_name}!("))
        .map(|offset| start + offset)
        .unwrap_or(source.len());
    let definition = &source[start..invocation];
    assert!(
        definition.contains("note_pipelines::org::$pipeline(&context, &command, &request).await?"),
        "{macro_name} must directly call its selected pipeline"
    );
    assert_eq!(
        definition.matches("note_pipelines::org::").count(),
        1,
        "{macro_name} must have exactly one pipeline delegation"
    );
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
fn operational_priority_query_documents_none_or_uppercase_letter() {
    let document = document();
    for path in ["/api/org/queue", "/api/org/agenda"] {
        let priority = document["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "priority")
            .unwrap();
        assert_eq!(
            priority["schema"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert_eq!(priority["schema"]["pattern"], "^(none|[A-Z])$");
        assert_eq!(priority["required"], false);
    }
}

#[test]
fn every_org_operation_documents_success_structured_errors_and_input_constraints() {
    let document = document();
    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("OpenAPI schemas");

    for (method, path, operation_id) in ORG_ROUTES {
        let operation = &document["paths"][path][method];
        assert_eq!(operation["operationId"], operation_id);
        assert_eq!(operation["tags"], serde_json::json!(["org"]));

        let responses = operation["responses"]
            .as_object()
            .unwrap_or_else(|| panic!("{operation_id} responses"));
        let (_, success) = responses
            .iter()
            .find(|(status, _)| status.starts_with('2'))
            .unwrap_or_else(|| panic!("{operation_id} must document a success response"));
        let schema = &success["content"]["application/json"]["schema"];
        let expected_schema = expected_success_schema(operation_id);
        assert_eq!(
            schema["$ref"],
            format!("#/components/schemas/{expected_schema}"),
            "{operation_id} success schema"
        );
        let success_schema = &schemas[expected_schema];
        assert_eq!(success_schema["type"], "object");
        assert!(
            success_schema["properties"]
                .as_object()
                .is_some_and(|properties| !properties.is_empty()),
            "{operation_id} success schema must be typed, not empty/any"
        );
        assert!(
            success_schema["required"]
                .as_array()
                .is_some_and(|required| !required.is_empty()),
            "{operation_id} success schema must declare required result fields"
        );
        assert_fully_typed_schema_graph(schema, schemas, operation_id, &mut BTreeSet::new());
        for status in ["400", "404", "409", "429", "500"] {
            let response = responses
                .get(status)
                .unwrap_or_else(|| panic!("{operation_id} missing {status}"));
            assert!(
                schema_graph_contains(response, schemas, "OrgApiError", &mut BTreeSet::new()),
                "{operation_id} {status} must use the structured OrgApiError schema"
            );
        }

        if method != "get" {
            let request_body = operation
                .get("requestBody")
                .unwrap_or_else(|| panic!("{operation_id} mutation request body"));
            assert!(
                schema_graph_contains(
                    request_body,
                    schemas,
                    "schema_version",
                    &mut BTreeSet::new()
                ) && schema_graph_contains(request_body, schemas, "actor_id", &mut BTreeSet::new())
                    && schema_graph_contains(
                        request_body,
                        schemas,
                        "operation_id",
                        &mut BTreeSet::new()
                    ),
                "{operation_id} must document the mutation envelope"
            );
        }
    }

    for (schema_name, field) in [
        ("OrgItemResult", "scheduled"),
        ("OrgItemResult", "deadline"),
        ("OrgItemContextResult", "parent"),
        ("OrgItemContextResult", "origin"),
        ("OrgItemContextResult", "lease"),
        ("OrgOperationalItem", "lease"),
    ] {
        assert!(
            schema_accepts_null(&schemas[schema_name]["properties"][field]),
            "{schema_name}.{field} must document its nullable wire value"
        );
    }

    for operation_id in [
        "org_list_workspaces",
        "org_list_documents",
        "org_query_queue",
        "org_query_agenda",
        "org_list_note_work_items",
        "org_list_events",
    ] {
        let (method, path, _) = ORG_ROUTES
            .iter()
            .find(|(_, _, candidate)| *candidate == operation_id)
            .unwrap();
        let operation = &document["paths"][path][method];
        assert!(schema_graph_contains(
            operation,
            schemas,
            "cursor",
            &mut BTreeSet::new()
        ));
        assert!(schema_graph_contains(
            operation,
            schemas,
            "limit",
            &mut BTreeSet::new()
        ));
        let serialized = serde_json::to_string(operation)
            .unwrap()
            .to_ascii_lowercase();
        assert!(serialized.contains("opaque"), "{operation_id} cursor");
        assert!(
            serialized.contains("\"minimum\":1"),
            "{operation_id} limit min: {serialized}"
        );
        assert!(
            serialized.contains("\"maximum\":200"),
            "{operation_id} limit max"
        );
    }

    for (operation_id, revision_field) in [
        ("org_update_workspace", "expected_revision"),
        ("org_archive_workspace", "expected_revision"),
        ("org_put_document", "expected_revision"),
        ("org_rename_document", "expected_revision"),
        ("org_archive_document", "expected_revision"),
        ("org_restore_document", "expected_revision"),
        ("org_move_document", "expected_document_revision"),
        ("org_move_document", "expected_source_workspace_revision"),
        ("org_move_document", "expected_target_workspace_revision"),
        ("org_move_item", "expected_revisions"),
        ("org_import_workspace", "expected_revisions"),
        ("org_create_item", "expected_revisions"),
        ("org_create_follow_up", "expected_revisions"),
        ("org_assign_item", "expected_revisions"),
        ("org_schedule_item", "expected_revisions"),
        ("org_claim_item", "expected_document_revision"),
        ("org_release_claim", "expected_document_revision"),
        ("org_submit_result", "expected_document_revision"),
        ("org_transition_item", "expected_document_revision"),
        ("org_retry_item", "expected_document_revision"),
        ("org_request_review", "expected_document_revision"),
        ("org_approve_item", "expected_document_revision"),
        ("org_reject_item", "expected_document_revision"),
        ("org_add_dependency", "expected_revisions"),
        ("org_remove_dependency", "expected_revisions"),
        ("org_link_note", "expected_revisions"),
        ("org_unlink_note", "expected_revisions"),
    ] {
        let (method, path, _) = ORG_ROUTES
            .iter()
            .find(|(_, _, candidate)| *candidate == operation_id)
            .unwrap();
        let request = &document["paths"][path][method]["requestBody"];
        assert!(
            schema_graph_contains(request, schemas, revision_field, &mut BTreeSet::new()),
            "{operation_id} must document {revision_field}"
        );
    }

    for operation_id in [
        "org_put_document",
        "org_move_document",
        "org_move_item",
        "org_import_workspace",
        "org_assign_item",
        "org_schedule_item",
        "org_heartbeat_claim",
        "org_release_claim",
        "org_report_progress",
        "org_submit_result",
        "org_transition_item",
        "org_request_review",
        "org_approve_item",
        "org_reject_item",
        "org_add_dependency",
        "org_remove_dependency",
        "org_link_note",
        "org_unlink_note",
    ] {
        let (method, path, _) = ORG_ROUTES
            .iter()
            .find(|(_, _, candidate)| *candidate == operation_id)
            .unwrap();
        let request = &document["paths"][path][method]["requestBody"];
        assert!(
            schema_graph_contains(request, schemas, "fencing_token", &mut BTreeSet::new()),
            "{operation_id} must document its fencing-token lease proof"
        );
    }
    let serialized = serde_json::to_string(&document)
        .unwrap()
        .to_ascii_lowercase();
    assert!(serialized.contains("sensitive raw fencing token"));
}

#[test]
fn all_org_handlers_are_thin_named_pipeline_adapters() {
    let all_sources = ORG_HANDLER_SOURCES
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>()
        .join("\n");

    assert_handler_macro_delegates(ORG_HANDLER_SOURCES[2].1, "item_mutation");
    assert_handler_macro_delegates(ORG_HANDLER_SOURCES[4].1, "command_operation");
    assert_handler_macro_delegates(ORG_HANDLER_SOURCES[5].1, "review_operation");

    for (_, _, operation_id) in ORG_ROUTES {
        let pipeline = match operation_id {
            "org_import_workspace" => "import_documents",
            "org_list_events" => "list_event_history",
            operation_id => operation_id
                .strip_prefix("org_")
                .expect("Org operation prefix"),
        };
        assert_eq!(
            all_sources.matches(&format!("\"{operation_id}\"")).count(),
            1,
            "{operation_id} must identify exactly one handler"
        );
        let (_, handler_source) = ORG_HANDLER_SOURCES
            .iter()
            .find(|(_, source)| source.contains(&format!("\"{operation_id}\"")))
            .unwrap_or_else(|| panic!("missing handler source for {operation_id}"));
        let handler_region = handler_delegate_region(handler_source, operation_id);
        assert!(
            handler_region.contains(&format!("note_pipelines::org::{pipeline}"))
                || handler_region.contains(&format!("\n    {pipeline},")),
            "{operation_id} must locally delegate to named pipeline {pipeline}"
        );
    }

    for (module, source) in ORG_HANDLER_SOURCES {
        let source = source.to_ascii_lowercase();
        for forbidden in [
            "storagesession",
            "storagetransaction",
            ".session(",
            ".begin(",
            "select ",
            "insert ",
            "update ",
            "delete from ",
            ".execute(",
            ".query(",
            "statement",
            "sqlx",
            "libsql",
            "parse_policy",
            "token_source",
            "sha2",
            ".clock(",
            "systemtime",
            "utc::now",
        ] {
            assert!(
                !source.contains(forbidden),
                "Org REST {module} handler module crosses transport boundary via {forbidden}"
            );
        }
    }
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

#[test]
fn document_lifecycle_openapi_has_closed_bodies_filters_and_archived_fields() {
    let document = document();
    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("OpenAPI schemas");

    fn collect_contract(
        schema: &serde_json::Value,
        schemas: &serde_json::Map<String, serde_json::Value>,
        properties: &mut BTreeSet<String>,
        required: &mut BTreeSet<String>,
        closed: &mut bool,
        visited: &mut BTreeSet<String>,
    ) {
        if let Some(reference) = schema["$ref"].as_str() {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .expect("component schema reference");
            if visited.insert(name.to_owned()) {
                collect_contract(
                    &schemas[name],
                    schemas,
                    properties,
                    required,
                    closed,
                    visited,
                );
            }
        }
        if let Some(fields) = schema["properties"].as_object() {
            properties.extend(fields.keys().cloned());
        }
        if let Some(fields) = schema["required"].as_array() {
            required.extend(
                fields
                    .iter()
                    .map(|field| field.as_str().unwrap().to_owned()),
            );
        }
        *closed |= schema["additionalProperties"] == false;
        for keyword in ["allOf", "oneOf"] {
            if let Some(parts) = schema[keyword].as_array() {
                for part in parts {
                    collect_contract(part, schemas, properties, required, closed, visited);
                }
            }
        }
    }

    for (schema_name, expected) in [
        (
            "CreateDocumentBody",
            [
                "actor_id",
                "document_id",
                "operation_id",
                "path",
                "schema_version",
            ]
            .as_slice(),
        ),
        (
            "RenameDocumentBody",
            [
                "actor_id",
                "expected_revision",
                "new_path",
                "operation_id",
                "schema_version",
                "workspace_id",
            ]
            .as_slice(),
        ),
        (
            "DocumentRevisionBody",
            [
                "actor_id",
                "expected_revision",
                "operation_id",
                "schema_version",
                "workspace_id",
            ]
            .as_slice(),
        ),
    ] {
        let schema = &schemas[schema_name];
        let mut properties = BTreeSet::new();
        let mut required = BTreeSet::new();
        let mut closed = false;
        collect_contract(
            schema,
            schemas,
            &mut properties,
            &mut required,
            &mut closed,
            &mut BTreeSet::new(),
        );
        assert_eq!(
            properties,
            expected.iter().map(|field| (*field).to_owned()).collect()
        );
        assert!(closed, "{schema_name} must reject unknown fields");
        assert_eq!(
            required,
            expected.iter().map(|field| (*field).to_owned()).collect()
        );
        for forbidden in ["source", "lease", "lease_proofs", "fencing_token"] {
            assert!(!properties.contains(forbidden), "{schema_name}.{forbidden}");
        }
    }

    let list = &document["paths"]["/api/org/workspaces/{workspace_id}/documents"]["get"];
    let parameters = list["parameters"].as_array().unwrap();
    let status = parameters
        .iter()
        .find(|parameter| parameter["name"] == "status")
        .expect("document status parameter");
    assert_eq!(
        status["schema"]["enum"],
        serde_json::json!(["active", "archived", "all"])
    );
    assert!(status["description"]
        .as_str()
        .unwrap()
        .contains("include_archived"));
    assert_eq!(status["required"], false);
    assert_eq!(status["schema"]["type"], "string");
    let legacy = parameters
        .iter()
        .find(|parameter| parameter["name"] == "include_archived")
        .expect("legacy include_archived parameter");
    assert!(legacy["description"].as_str().unwrap().contains("status"));
    assert_eq!(legacy["required"], false);
    assert_eq!(legacy["schema"]["type"], "boolean");

    for schema_name in ["OrgDocumentResult", "OrgDocumentSourceResult"] {
        assert!(schemas[schema_name]["properties"]
            .get("archived_at")
            .is_some());
        assert!(schema_accepts_null(
            &schemas[schema_name]["properties"]["archived_at"]
        ));
        assert!(schemas[schema_name]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("archived_at")));
    }
    assert!(schema_accepts_null(
        &schemas["OrgDocumentLifecycleData"]["properties"]["archived_at"]
    ));
    assert_eq!(
        schemas["OrgDocumentLifecycleCommandResult"]["properties"]["data"]["$ref"],
        "#/components/schemas/OrgDocumentLifecycleData"
    );
    assert!(schemas["DocumentImportBody"]["properties"]
        .get("archived_at")
        .is_none());
    assert!(document["paths"].get("/mcp").is_none());
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
    assert!(matches!(
        response.status(),
        StatusCode::OK | StatusCode::INTERNAL_SERVER_ERROR
    ));
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(body.is_object());
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
        (OrgErrorCode::ArchivedDocument, StatusCode::CONFLICT, false),
        (
            OrgErrorCode::DocumentPathConflict,
            StatusCode::CONFLICT,
            false,
        ),
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
