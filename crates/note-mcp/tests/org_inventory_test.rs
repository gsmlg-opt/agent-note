use std::{collections::HashSet, sync::Arc};

use note_attachments::FilesystemAttachmentStore;
use note_embedding::StubEmbedder;
use note_mcp::{
    org::{validate_unique_tool_names, NOTE_TOOL_NAMES, ORG_TOOL_NAMES},
    NoteMcpServer,
};
use note_pipelines::{
    org::{OrgContext, SystemOrgClock},
    Context,
};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use rmcp::model::Tool;
use serde_json::Value;

#[test]
fn org_inventory_is_the_ordered_36_tool_contract() {
    assert_eq!(
        ORG_TOOL_NAMES,
        [
            "org_list_workspaces",
            "org_create_workspace",
            "org_get_workspace",
            "org_update_workspace",
            "org_archive_workspace",
            "org_list_documents",
            "org_get_document",
            "org_put_document",
            "org_move_document",
            "org_move_item",
            "org_import_workspace",
            "org_export_workspace",
            "org_create_item",
            "org_get_item",
            "org_get_item_context",
            "org_create_follow_up",
            "org_assign_item",
            "org_schedule_item",
            "org_query_queue",
            "org_query_agenda",
            "org_claim_item",
            "org_heartbeat_claim",
            "org_release_claim",
            "org_report_progress",
            "org_submit_result",
            "org_transition_item",
            "org_retry_item",
            "org_request_review",
            "org_approve_item",
            "org_reject_item",
            "org_add_dependency",
            "org_remove_dependency",
            "org_link_note",
            "org_unlink_note",
            "org_list_note_work_items",
            "org_list_events",
        ]
    );
}

#[test]
fn note_inventory_is_the_ordered_12_tool_contract() {
    assert_eq!(
        NOTE_TOOL_NAMES,
        [
            "bulk_update_note_labels",
            "delete_note",
            "delete_note_attachment",
            "edit_note",
            "get_note",
            "get_note_attachment_content",
            "list_notes",
            "put_note_attachment",
            "read_note_lines",
            "save_note",
            "semantic_search",
            "update_note",
        ]
    );
}

async fn registered_tools() -> (Vec<Tool>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db"))
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
    let org = Arc::new(OrgContext::new(backend, Arc::new(SystemOrgClock)));
    (NoteMcpServer::new(note, org).registered_tools(), dir)
}

#[tokio::test]
async fn aggregate_inventory_is_48_and_preserves_the_12_note_tools() {
    let (tools, _dir) = registered_tools().await;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<HashSet<_>>();
    assert_eq!(tools.len(), 48);
    assert_eq!(names.len(), 48);
    assert_eq!(
        names.iter().filter(|name| name.starts_with("org_")).count(),
        36
    );
    for name in NOTE_TOOL_NAMES {
        assert!(names.contains(name), "missing note tool {name}");
    }
    for name in ORG_TOOL_NAMES {
        assert!(names.contains(name), "missing Org tool {name}");
    }
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool.name == "bulk_update_note_labels")
            .count(),
        1
    );
    assert_eq!(
        tools
            .iter()
            .find(|tool| tool.name == "bulk_update_note_labels")
            .unwrap()
            .description,
        Some(
            "Atomically set or replace labels on active notes matching a label selector while preserving unrelated labels."
                .into()
        )
    );
}

#[test]
fn duplicate_tool_names_are_rejected_before_router_merge() {
    let error = validate_unique_tool_names(["save_note", "org_list_events", "save_note"])
        .expect_err("duplicate name must be rejected");
    assert_eq!(error.duplicate_name, "save_note");
}

#[tokio::test]
async fn org_schemas_are_closed_and_expose_concurrency_contracts() {
    let (tools, _dir) = registered_tools().await;
    let tool = |name: &str| tools.iter().find(|tool| tool.name == name).unwrap();

    for current in tools.iter().filter(|tool| tool.name.starts_with("org_")) {
        assert_eq!(
            current.input_schema["additionalProperties"], false,
            "{} input",
            current.name
        );
        let output = current.output_schema.as_ref().expect("Org output schema");
        assert_eq!(
            output["additionalProperties"], false,
            "{} output",
            current.name
        );
    }

    let read_tools = HashSet::from([
        "org_list_workspaces",
        "org_get_workspace",
        "org_list_documents",
        "org_get_document",
        "org_export_workspace",
        "org_get_item",
        "org_get_item_context",
        "org_query_queue",
        "org_query_agenda",
        "org_list_note_work_items",
        "org_list_events",
    ]);
    for name in ORG_TOOL_NAMES
        .into_iter()
        .filter(|name| !read_tools.contains(name))
    {
        let required = required_fields(tool(name).input_schema.as_ref());
        for field in ["schema_version", "workspace_id", "actor_id", "operation_id"] {
            assert!(required.contains(field), "{name} must require {field}");
        }
    }

    for (name, fields) in [
        ("org_update_workspace", &["expected_revision"][..]),
        ("org_archive_workspace", &["expected_revision"][..]),
        (
            "org_move_document",
            &[
                "expected_document_revision",
                "expected_source_workspace_revision",
                "expected_target_workspace_revision",
                "lease_proofs",
            ][..],
        ),
        (
            "org_import_workspace",
            &["expected_revisions", "lease_proofs"][..],
        ),
        ("org_create_item", &["expected_revisions"][..]),
    ] {
        let required = required_fields(tool(name).input_schema.as_ref());
        for field in fields {
            assert!(required.contains(*field), "{name} must require {field}");
        }
    }

    for name in [
        "org_heartbeat_claim",
        "org_release_claim",
        "org_report_progress",
        "org_submit_result",
        "org_request_review",
        "org_approve_item",
        "org_reject_item",
    ] {
        assert!(required_fields(tool(name).input_schema.as_ref()).contains("fencing_token"));
    }

    for name in ["org_query_queue", "org_query_agenda"] {
        let priority = &tool(name).input_schema["properties"]["priority"];
        assert_eq!(priority["type"], serde_json::json!(["string", "null"]));
        assert_eq!(priority["pattern"], "^(none|[A-Z])$");
    }

    for name in [
        "org_put_document",
        "org_move_document",
        "org_import_workspace",
        "org_move_item",
    ] {
        let input = tool(name).input_schema.as_ref();
        let value_schema = &input["properties"]["lease_proofs"]["additionalProperties"];
        let value_schema = resolve_schema(input, value_schema);
        let required = value_schema["required"].as_array().unwrap();
        for field in ["lease_id", "kind", "fencing_token"] {
            assert!(
                required.iter().any(|value| value == field),
                "{name} token map must require {field}"
            );
        }
    }

    let retry = required_fields(tool("org_retry_item").input_schema.as_ref());
    assert!(!retry.contains("kind"));
    for (name, fields) in [
        ("org_get_document", &["workspace_id", "document_id"][..]),
        ("org_get_item", &["workspace_id", "item_id"][..]),
        ("org_get_item_context", &["workspace_id", "item_id"][..]),
        (
            "org_release_claim",
            &["document_id", "expected_document_revision", "target_state"][..],
        ),
        (
            "org_submit_result",
            &[
                "document_id",
                "expected_document_revision",
                "result_summary",
                "note_refs",
                "artifacts",
            ][..],
        ),
        (
            "org_transition_item",
            &["document_id", "expected_document_revision", "target_state"][..],
        ),
        (
            "org_add_dependency",
            &["item_id", "dependency_id", "document_id"][..],
        ),
        (
            "org_link_note",
            &[
                "item_id",
                "document_id",
                "purpose",
                "note_id",
                "description",
            ][..],
        ),
    ] {
        let schema = tool(name).input_schema.as_ref();
        let properties = schema["properties"].as_object().unwrap();
        for field in fields {
            assert!(
                properties.contains_key(*field),
                "{name} must advertise {field}"
            );
        }
    }

    for name in [
        "org_list_workspaces",
        "org_list_documents",
        "org_query_queue",
        "org_query_agenda",
        "org_list_note_work_items",
        "org_list_events",
    ] {
        let input = tool(name).input_schema.as_ref();
        assert!(!required_fields(input).contains("cursor"));
        let limit = &input["properties"]["limit"];
        assert_eq!(limit["default"], 50);
        assert_eq!(limit["maximum"], 200);
    }

    for current in tools.iter().filter(|tool| tool.name.starts_with("org_")) {
        let output = Value::Object((**current.output_schema.as_ref().unwrap()).clone());
        let mut keys = Vec::new();
        collect_property_keys(&output, &mut keys);
        let advertises_token = keys.contains(&"fencing_token");
        assert_eq!(
            advertises_token,
            matches!(current.name.as_ref(), "org_claim_item" | "org_retry_item"),
            "unexpected fencing-token output contract for {}",
            current.name
        );
    }
}

#[tokio::test]
async fn general_read_output_schemas_advertise_no_secret_or_authentication_fields() {
    let (tools, _dir) = registered_tools().await;
    for tool in tools.iter().filter(|tool| {
        matches!(
            tool.name.as_ref(),
            "org_list_workspaces"
                | "org_get_workspace"
                | "org_list_documents"
                | "org_get_document"
                | "org_export_workspace"
                | "org_get_item"
                | "org_get_item_context"
                | "org_query_queue"
                | "org_query_agenda"
                | "org_list_note_work_items"
                | "org_list_events"
        )
    }) {
        let output = Value::Object((**tool.output_schema.as_ref().unwrap()).clone());
        let mut keys = Vec::new();
        collect_property_keys(&output, &mut keys);
        for forbidden in [
            "fencing_token",
            "fencing_token_hash",
            "secret",
            "authentication",
        ] {
            assert!(
                !keys.iter().any(|key| *key == forbidden),
                "{} exposes {forbidden}",
                tool.name
            );
        }
    }
}

fn required_fields(schema: &serde_json::Map<String, Value>) -> HashSet<&str> {
    schema
        .get("required")
        .unwrap_or(&Value::Null)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn resolve_schema<'a>(root: &'a serde_json::Map<String, Value>, schema: &'a Value) -> &'a Value {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return schema;
    };
    reference
        .strip_prefix("#/$defs/")
        .and_then(|name| root.get("$defs")?.get(name))
        .expect("local schema reference")
}

fn collect_property_keys<'a>(value: &'a Value, keys: &mut Vec<&'a str>) {
    match value {
        Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                keys.extend(properties.keys().map(String::as_str));
            }
            for nested in object.values() {
                collect_property_keys(nested, keys);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_property_keys(nested, keys);
            }
        }
        _ => {}
    }
}
