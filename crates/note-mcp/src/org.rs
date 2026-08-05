use std::{collections::HashSet, sync::Arc};

use rmcp::{
    handler::server::{
        router::tool::{ToolRoute, ToolRouter},
        tool::ToolCallContext,
    },
    model::{ErrorData, JsonObject, Tool},
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::{
    org_dto::{
        ApproveInput, AssignItemInput, ClaimInput, ClaimOutput, CommandOutput, CreateFollowUpInput,
        CreateItemInput, DependencyInput, DocumentCommandData, DocumentOutput, DocumentReadInput,
        DocumentSourceOutput, EventListInput, EventOutput, HeartbeatClaimInput,
        ImportWorkspaceInput, ItemCommandData, ItemContextOutput, ItemOutput, ItemReadInput,
        LeaseCommandData, ListInput, MoveDocumentInput, MoveItemInput, NoteItemsInput,
        NoteLinkInput, NoteUnlinkInput, OperationalPageOutput, OperationalQueryInput, PageOutput,
        ProgressInput, PutDocumentInput, RejectInput, ReleaseClaimInput, RequestReviewInput,
        RetryInput, ScheduleItemInput, SubmitResultInput, TransitionInput, WorkspaceArchiveInput,
        WorkspaceCommandData, WorkspaceCreateInput, WorkspaceExportOutput, WorkspaceListInput,
        WorkspaceOutput, WorkspaceReadInput, WorkspaceSummaryOutput, WorkspaceUpdateInput,
    },
    NoteMcpServer,
};

pub const ORG_TOOL_NAMES: [&str; 36] = [
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
];

pub const NOTE_TOOL_NAMES: [&str; 11] = [
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
];

type WorkspaceMutationOutput = CommandOutput<WorkspaceCommandData>;
type DocumentMutationOutput = CommandOutput<DocumentCommandData>;
type ItemMutationOutput = CommandOutput<ItemCommandData>;
type LeaseMutationOutput = CommandOutput<LeaseCommandData>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRegistrationError {
    pub duplicate_name: String,
}

pub fn validate_unique_tool_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), ToolRegistrationError> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(ToolRegistrationError {
                duplicate_name: name.to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn checked_tool_router(
    note_router: ToolRouter<NoteMcpServer>,
) -> Result<ToolRouter<NoteMcpServer>, ToolRegistrationError> {
    validate_unique_tool_names(NOTE_TOOL_NAMES.into_iter().chain(ORG_TOOL_NAMES))?;
    let org_router = org_tool_router();
    let note_names = note_router
        .map
        .keys()
        .map(|name| name.as_ref())
        .collect::<HashSet<_>>();
    let expected_note_names = NOTE_TOOL_NAMES.into_iter().collect::<HashSet<_>>();
    assert_eq!(
        note_names, expected_note_names,
        "legacy note tool inventory changed"
    );

    let org_names = org_router
        .map
        .keys()
        .map(|name| name.as_ref())
        .collect::<HashSet<_>>();
    let expected_org_names = ORG_TOOL_NAMES.into_iter().collect::<HashSet<_>>();
    assert_eq!(org_names, expected_org_names, "Org tool inventory changed");

    checked_merge(note_router, org_router)
}

fn checked_merge(
    note_router: ToolRouter<NoteMcpServer>,
    org_router: ToolRouter<NoteMcpServer>,
) -> Result<ToolRouter<NoteMcpServer>, ToolRegistrationError> {
    let mut merged = ToolRouter::new();
    for route in note_router.into_iter().chain(org_router) {
        let name = route.name().to_string();
        if merged.map.contains_key(name.as_str()) {
            return Err(ToolRegistrationError {
                duplicate_name: name,
            });
        }
        merged.add_route(route);
    }
    Ok(merged)
}

pub(crate) fn org_tool_router() -> ToolRouter<NoteMcpServer> {
    let mut router = ToolRouter::new();

    add::<ListInput, PageOutput<WorkspaceSummaryOutput>>(
        &mut router,
        ORG_TOOL_NAMES[0],
        "List Org workspaces",
    );
    add::<WorkspaceCreateInput, WorkspaceMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[1],
        "Create an Org workspace",
    );
    add::<WorkspaceReadInput, WorkspaceOutput>(
        &mut router,
        ORG_TOOL_NAMES[2],
        "Get an Org workspace",
    );
    add::<WorkspaceUpdateInput, WorkspaceMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[3],
        "Update an Org workspace",
    );
    add::<WorkspaceArchiveInput, WorkspaceMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[4],
        "Archive an Org workspace",
    );
    add::<WorkspaceListInput, PageOutput<DocumentOutput>>(
        &mut router,
        ORG_TOOL_NAMES[5],
        "List Org documents",
    );
    add::<DocumentReadInput, DocumentSourceOutput>(
        &mut router,
        ORG_TOOL_NAMES[6],
        "Get an Org document",
    );
    add::<PutDocumentInput, DocumentMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[7],
        "Put an Org document",
    );
    add::<MoveDocumentInput, DocumentMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[8],
        "Move an Org document",
    );
    add::<MoveItemInput, ItemMutationOutput>(&mut router, ORG_TOOL_NAMES[9], "Move an Org item");
    add::<ImportWorkspaceInput, DocumentMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[10],
        "Import an Org workspace",
    );
    add::<WorkspaceReadInput, WorkspaceExportOutput>(
        &mut router,
        ORG_TOOL_NAMES[11],
        "Export an Org workspace",
    );
    add::<CreateItemInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[12],
        "Create an Org item",
    );
    add::<ItemReadInput, ItemOutput>(&mut router, ORG_TOOL_NAMES[13], "Get an Org item");
    add::<ItemReadInput, ItemContextOutput>(
        &mut router,
        ORG_TOOL_NAMES[14],
        "Get an Org item context",
    );
    add::<CreateFollowUpInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[15],
        "Create a follow-up item",
    );
    add::<AssignItemInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[16],
        "Assign an Org item",
    );
    add::<ScheduleItemInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[17],
        "Schedule an Org item",
    );
    add::<OperationalQueryInput, OperationalPageOutput>(
        &mut router,
        ORG_TOOL_NAMES[18],
        "Query the Org queue",
    );
    add::<OperationalQueryInput, OperationalPageOutput>(
        &mut router,
        ORG_TOOL_NAMES[19],
        "Query the Org agenda",
    );
    add::<ClaimInput, ClaimOutput>(&mut router, ORG_TOOL_NAMES[20], "Claim an Org item");
    add::<HeartbeatClaimInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[21],
        "Heartbeat an active Org claim",
    );
    add::<ReleaseClaimInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[22],
        "Release an active Org claim",
    );
    add::<ProgressInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[23],
        "Report progress on an active Org claim",
    );
    add::<SubmitResultInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[24],
        "Submit an Org result",
    );
    add::<TransitionInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[25],
        "Transition an Org item",
    );
    add::<RetryInput, ClaimOutput>(
        &mut router,
        ORG_TOOL_NAMES[26],
        "Retry and reclaim an Org item",
    );
    add::<RequestReviewInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[27],
        "Request Org review",
    );
    add::<ApproveInput, LeaseMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[28],
        "Approve an Org item",
    );
    add::<RejectInput, LeaseMutationOutput>(&mut router, ORG_TOOL_NAMES[29], "Reject an Org item");
    add::<DependencyInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[30],
        "Add an Org dependency",
    );
    add::<DependencyInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[31],
        "Remove an Org dependency",
    );
    add::<NoteLinkInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[32],
        "Link a note to an Org item",
    );
    add::<NoteUnlinkInput, ItemMutationOutput>(
        &mut router,
        ORG_TOOL_NAMES[33],
        "Unlink a note from an Org item",
    );
    add::<NoteItemsInput, PageOutput<ItemOutput>>(
        &mut router,
        ORG_TOOL_NAMES[34],
        "List Org work items linked to a note",
    );
    add::<EventListInput, PageOutput<EventOutput>>(
        &mut router,
        ORG_TOOL_NAMES[35],
        "List Org event history",
    );
    router
}

fn add<I: JsonSchema + DeserializeOwned, O: JsonSchema>(
    router: &mut ToolRouter<NoteMcpServer>,
    name: &'static str,
    description: &'static str,
) {
    let _error_adapter: fn(note_pipelines::org::OrgError) -> ErrorData =
        crate::org_error::to_error_data;
    let tool =
        Tool::new(name, description, schema::<I>()).with_raw_output_schema(Arc::new(schema::<O>()));
    router.add_route(ToolRoute::new_dyn(
        tool,
        |context: ToolCallContext<'_, NoteMcpServer>| {
            // The registration skeleton deliberately reaches the single injected Org
            // context; delivery Task 2 replaces this error with pipeline delegation.
            let _ = context.service.org_context().clock();
            let arguments = context.arguments.unwrap_or_default();
            if serde_json::from_value::<I>(serde_json::Value::Object(arguments)).is_err() {
                return Box::pin(async move { Err(invalid_org_tool_input()) });
            }
            Box::pin(async {
                Err(crate::org_error::to_error_data(
                    note_pipelines::org::OrgError::new(
                        note_pipelines::org::OrgErrorCode::UnsupportedSemanticEdit,
                        "Org tool handler is not implemented in this delivery task",
                        serde_json::json!({}),
                        false,
                    ),
                ))
            })
        },
    ));
}

fn invalid_org_tool_input() -> ErrorData {
    crate::org_error::to_error_data(note_pipelines::org::OrgError::invalid_input(
        "invalid Org tool input",
    ))
}

fn schema<T: JsonSchema>() -> JsonObject {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("JSON schema is serializable")
        .as_object()
        .expect("root JSON schema is an object")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_pipelines::{
        org::{OrgContext, SystemOrgClock},
        Context,
    };
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use rmcp::{
        model::{CallToolRequestParams, ErrorCode},
        service::ServiceError,
        ServiceExt,
    };

    #[test]
    fn checked_merge_rejects_a_real_duplicate_route() {
        let route = org_tool_router().map[ORG_TOOL_NAMES[0]].clone();
        let mut left = ToolRouter::new();
        left.add_route(route.clone());
        let mut right = ToolRouter::new();
        right.add_route(route);

        assert_eq!(
            checked_merge(left, right).unwrap_err().duplicate_name,
            ORG_TOOL_NAMES[0]
        );
    }

    #[tokio::test]
    async fn registered_route_never_echoes_malformed_sensitive_input() {
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
        let service = NoteMcpServer::new(note, org);
        let attacker_value = "raw-fencing-token";
        let arguments = serde_json::json!({
            "schema_version": 1,
            "workspace_id": "workspace",
            "actor_id": "actor",
            "operation_id": "operation",
            "document_id": "document",
            "parent_id": null,
            "item_id": "item",
            "item_type": "task",
            "title": "title",
            "initial_state": null,
            "priority": attacker_value,
            "tags": [],
            "assignee": null,
            "requires_review": false,
            "expected_revisions": {}
        })
        .as_object()
        .unwrap()
        .clone();
        let (server_transport, client_transport) = tokio::io::duplex(8 * 1024);
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
        let error = client
            .call_tool(CallToolRequestParams::new(ORG_TOOL_NAMES[12]).with_arguments(arguments))
            .await
            .unwrap_err();
        let ServiceError::McpError(error) = error else {
            panic!("expected MCP error, got {error:?}");
        };
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, "invalid Org tool input");
        let rendered = format!("{error:?} {}", serde_json::to_string(&error).unwrap());
        assert!(!rendered.contains(attacker_value));
        assert!(!rendered.contains("invalid type"));
        client.cancel().await.unwrap();
        server_task.await.unwrap();
    }
}
