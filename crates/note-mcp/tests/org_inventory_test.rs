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
fn org_inventory_is_the_ordered_40_tool_contract() {
    assert_eq!(
        ORG_TOOL_NAMES.as_slice(),
        &[
            "org_list_workspaces",
            "org_create_workspace",
            "org_get_workspace",
            "org_update_workspace",
            "org_archive_workspace",
            "org_list_documents",
            "org_get_document",
            "org_put_document",
            "org_create_document",
            "org_rename_document",
            "org_archive_document",
            "org_restore_document",
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
async fn aggregate_inventory_is_52_and_preserves_the_12_note_tools() {
    let (tools, _dir) = registered_tools().await;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<HashSet<_>>();
    assert_eq!(tools.len(), 52);
    assert_eq!(names.len(), 52);
    assert_eq!(
        names.iter().filter(|name| name.starts_with("org_")).count(),
        40
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
            "Atomically set, replace, or remove labels on active notes matching a label selector while preserving unrelated labels."
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
        ("org_rename_document", &["expected_revision"][..]),
        ("org_archive_document", &["expected_revision"][..]),
        ("org_restore_document", &["expected_revision"][..]),
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

    for (name, expected) in [
        (
            "org_create_document",
            &[
                "actor_id",
                "document_id",
                "operation_id",
                "path",
                "schema_version",
                "workspace_id",
            ][..],
        ),
        (
            "org_rename_document",
            &[
                "actor_id",
                "document_id",
                "expected_revision",
                "new_path",
                "operation_id",
                "schema_version",
                "workspace_id",
            ][..],
        ),
        (
            "org_archive_document",
            &[
                "actor_id",
                "document_id",
                "expected_revision",
                "operation_id",
                "schema_version",
                "workspace_id",
            ][..],
        ),
        (
            "org_restore_document",
            &[
                "actor_id",
                "document_id",
                "expected_revision",
                "operation_id",
                "schema_version",
                "workspace_id",
            ][..],
        ),
    ] {
        let input = tool(name).input_schema.as_ref();
        let mut properties = input["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        properties.sort_unstable();
        assert_eq!(properties, expected, "{name} exact input contract");
        assert_eq!(required_fields(input).len(), expected.len());
        for field in expected {
            assert!(required_fields(input).contains(field));
        }
    }

    let document_list = tool("org_list_documents").input_schema.as_ref();
    let mut document_list_properties = document_list["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    document_list_properties.sort_unstable();
    assert_eq!(
        document_list_properties,
        [
            "cursor",
            "include_archived",
            "limit",
            "status",
            "workspace_id"
        ]
    );
    let mut status_values = Vec::new();
    collect_enum_strings(
        document_list,
        &document_list["properties"]["status"],
        &mut status_values,
    );
    status_values.sort_unstable();
    assert_eq!(status_values, ["active", "all", "archived"]);
    assert_eq!(
        document_list["properties"]["cursor"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert_eq!(
        document_list["properties"]["status"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert_eq!(
        document_list["properties"]["status"]["enum"],
        serde_json::json!(["active", "archived", "all", null])
    );
    assert_eq!(
        document_list["properties"]["include_archived"]["type"],
        serde_json::json!(["boolean", "null"])
    );
    assert_eq!(
        document_list["required"],
        serde_json::json!(["workspace_id"])
    );
    assert_eq!(
        document_list["not"]["required"],
        serde_json::json!(["status", "include_archived"]),
        "mutual exclusion only applies when both properties are present"
    );
    assert_eq!(
        document_list["not"]["properties"],
        serde_json::json!({
            "status": {"not": {"type": "null"}},
            "include_archived": {"not": {"type": "null"}}
        }),
        "mutual exclusion must reject only two non-null filters"
    );

    let document_list_output = tool("org_list_documents").output_schema.as_ref().unwrap();
    let document_list_item = resolve_schema(
        document_list_output,
        &document_list_output["properties"]["items"]["items"],
    );
    assert_required_nullable_field(
        document_list_output,
        document_list_item,
        "archived_at",
        "org_list_documents item",
    );

    let document_source_output = tool("org_get_document").output_schema.as_ref().unwrap();
    assert_required_nullable_field(
        document_source_output,
        &Value::Object((**document_source_output).clone()),
        "archived_at",
        "org_get_document output",
    );

    let workspace_export_output = tool("org_export_workspace").output_schema.as_ref().unwrap();
    let exported_document = resolve_schema(
        workspace_export_output,
        &workspace_export_output["properties"]["documents"]["items"],
    );
    assert_required_nullable_field(
        workspace_export_output,
        exported_document,
        "archived_at",
        "org_export_workspace document",
    );

    for name in [
        "org_create_document",
        "org_rename_document",
        "org_archive_document",
        "org_restore_document",
    ] {
        let output = tool(name).output_schema.as_ref().unwrap();
        let data = resolve_schema(output, &output["properties"]["data"]);
        assert_required_nullable_field(output, data, "archived_at", name);
        let mut fields = data["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(fields, ["archived_at", "document_id", "path"]);
    }

    let import_input = tool("org_import_workspace").input_schema.as_ref();
    let documents = resolve_schema(
        import_input,
        &import_input["properties"]["documents"]["items"],
    );
    assert!(documents["properties"].get("archived_at").is_none());

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

fn assert_required_nullable_field(
    root: &serde_json::Map<String, Value>,
    schema: &Value,
    field: &str,
    context: &str,
) {
    let schema = resolve_schema(root, schema);
    assert!(
        schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == field)),
        "{context} must require {field}"
    );
    assert_eq!(
        schema["properties"][field]["type"],
        serde_json::json!(["integer", "null"]),
        "{context} {field} must be nullable"
    );
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

fn collect_enum_strings<'a>(
    root: &'a serde_json::Map<String, Value>,
    value: &'a Value,
    values: &mut Vec<&'a str>,
) {
    if value.get("$ref").is_some() {
        collect_enum_strings(root, resolve_schema(root, value), values);
        return;
    }
    match value {
        Value::Object(object) => {
            if let Some(enumeration) = object.get("enum").and_then(Value::as_array) {
                values.extend(enumeration.iter().filter_map(Value::as_str));
            }
            for nested in object.values() {
                collect_enum_strings(root, nested, values);
            }
        }
        Value::Array(nested) => {
            for value in nested {
                collect_enum_strings(root, value, values);
            }
        }
        _ => {}
    }
}
