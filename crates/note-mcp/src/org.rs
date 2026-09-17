use std::{collections::HashSet, future::Future, sync::Arc};

use rmcp::{
    handler::server::{
        router::tool::{ToolRoute, ToolRouter},
        tool::ToolCallContext,
    },
    model::{CallToolResult, ErrorData, JsonObject, Tool},
};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Serialize};

use crate::{
    org_dto::{
        AgendaViewInput, ApproveInput, AssignItemInput, ClaimInput, ClaimOutput, CommandOutput,
        ContextCommandData, CreateDocumentInput, CreateFollowUpInput, CreateItemInput,
        DependencyInput, DocumentCountData, DocumentLifecycleDataOutput, DocumentListInput,
        DocumentOutput, DocumentReadInput, DocumentRevisionInput, DocumentSourceOutput,
        EventListInput, EventOutput, HeartbeatClaimInput, HeartbeatCommandData,
        ImportWorkspaceInput, ItemCommandData, ItemContextOutput, ItemOutput, ItemReadInput,
        ListInput, MoveDocumentData, MoveDocumentInput, MoveItemData, MoveItemInput,
        NoteItemsInput, NoteLinkInput, NoteUnlinkInput, OperationalPageOutput,
        OperationalQueryInput, PageOutput, ProgressInput, PutDocumentInput, QueueViewInput,
        RejectInput, ReleaseClaimInput, RenameDocumentInput, RequestReviewInput, RetryInput,
        ScheduleItemInput, SubmitResultInput, TransitionInput, WorkspaceArchiveData,
        WorkspaceArchiveInput, WorkspaceCreateInput, WorkspaceExportOutput, WorkspaceOutput,
        WorkspaceReadInput, WorkspaceRevisionData, WorkspaceSummaryOutput, WorkspaceUpdateInput,
    },
    NoteMcpServer,
};

pub const ORG_TOOL_NAMES: [&str; 40] = [
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
];

pub const NOTE_TOOL_NAMES: [&str; 12] = [
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
];

type WorkspaceRevisionOutput = CommandOutput<WorkspaceRevisionData>;
type WorkspaceArchiveOutput = CommandOutput<WorkspaceArchiveData>;
type DocumentCountOutput = CommandOutput<DocumentCountData>;
type DocumentLifecycleOutput = CommandOutput<DocumentLifecycleDataOutput>;
type MoveDocumentOutput = CommandOutput<MoveDocumentData>;
type ItemMutationOutput = CommandOutput<ItemCommandData>;
type MoveItemOutput = CommandOutput<MoveItemData>;
type ContextMutationOutput = CommandOutput<ContextCommandData>;
type HeartbeatMutationOutput = CommandOutput<HeartbeatCommandData>;

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
        "note tool inventory changed"
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

    add_handler::<ListInput, PageOutput<WorkspaceSummaryOutput>, _, _>(
        &mut router,
        ORG_TOOL_NAMES[0],
        "List Org workspaces",
        list_workspaces_handler,
    );
    add_handler::<WorkspaceCreateInput, WorkspaceRevisionOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[1],
        "Create an Org workspace",
        create_workspace_handler,
    );
    add_handler::<WorkspaceReadInput, WorkspaceOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[2],
        "Get an Org workspace",
        get_workspace_handler,
    );
    add_handler::<WorkspaceUpdateInput, WorkspaceRevisionOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[3],
        "Update an Org workspace",
        update_workspace_handler,
    );
    add_handler::<WorkspaceArchiveInput, WorkspaceArchiveOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[4],
        "Archive an Org workspace",
        archive_workspace_handler,
    );
    add_handler::<DocumentListInput, PageOutput<DocumentOutput>, _, _>(
        &mut router,
        ORG_TOOL_NAMES[5],
        "List Org documents",
        list_documents_handler,
    );
    add_handler::<DocumentReadInput, DocumentSourceOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[6],
        "Get an Org document",
        get_document_handler,
    );
    add_handler::<PutDocumentInput, DocumentCountOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[7],
        "Put an Org document",
        put_document_handler,
    );
    add_handler::<CreateDocumentInput, DocumentLifecycleOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[8],
        "Create an empty Org document",
        create_document_handler,
    );
    add_handler::<RenameDocumentInput, DocumentLifecycleOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[9],
        "Rename an Org document",
        rename_document_handler,
    );
    add_handler::<DocumentRevisionInput, DocumentLifecycleOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[10],
        "Archive an Org document",
        archive_document_handler,
    );
    add_handler::<DocumentRevisionInput, DocumentLifecycleOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[11],
        "Restore an Org document",
        restore_document_handler,
    );
    add_handler::<MoveDocumentInput, MoveDocumentOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[12],
        "Move an Org document",
        move_document_handler,
    );
    add_handler::<MoveItemInput, MoveItemOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[13],
        "Move an Org item",
        move_item_handler,
    );
    add_handler::<ImportWorkspaceInput, DocumentCountOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[14],
        "Import an Org workspace",
        import_workspace_handler,
    );
    add_handler::<WorkspaceReadInput, WorkspaceExportOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[15],
        "Export an Org workspace",
        export_workspace_handler,
    );
    add_handler::<CreateItemInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[16],
        "Create an Org item",
        create_item_handler,
    );
    add_handler::<ItemReadInput, ItemOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[17],
        "Get an Org item",
        get_item_handler,
    );
    add_handler::<ItemReadInput, ItemContextOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[18],
        "Get an Org item context",
        get_item_context_handler,
    );
    add_handler::<CreateFollowUpInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[19],
        "Create a follow-up item",
        create_follow_up_handler,
    );
    add_handler::<AssignItemInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[20],
        "Assign an Org item",
        assign_item_handler,
    );
    add_handler::<ScheduleItemInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[21],
        "Schedule an Org item",
        schedule_item_handler,
    );
    add_handler::<OperationalQueryInput<QueueViewInput>, OperationalPageOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[22],
        "Query the Org queue",
        query_queue_handler,
    );
    add_handler::<OperationalQueryInput<AgendaViewInput>, OperationalPageOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[23],
        "Query the Org agenda",
        query_agenda_handler,
    );
    add_handler::<ClaimInput, ClaimOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[24],
        "Claim an Org item",
        claim_item_handler,
    );
    add_handler::<HeartbeatClaimInput, HeartbeatMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[25],
        "Heartbeat an active Org claim",
        heartbeat_claim_handler,
    );
    add_handler::<ReleaseClaimInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[26],
        "Release an active Org claim",
        release_claim_handler,
    );
    add_handler::<ProgressInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[27],
        "Report progress on an active Org claim",
        progress_handler,
    );
    add_handler::<SubmitResultInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[28],
        "Submit an Org result",
        submit_result_handler,
    );
    add_handler::<TransitionInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[29],
        "Transition an Org item",
        transition_item_handler,
    );
    add_handler::<RetryInput, ClaimOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[30],
        "Retry and reclaim an Org item",
        retry_item_handler,
    );
    add_handler::<RequestReviewInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[31],
        "Request Org review",
        request_review_handler,
    );
    add_handler::<ApproveInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[32],
        "Approve an Org item",
        approve_item_handler,
    );
    add_handler::<RejectInput, ContextMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[33],
        "Reject an Org item",
        reject_item_handler,
    );
    add_handler::<DependencyInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[34],
        "Add an Org dependency",
        add_dependency_handler,
    );
    add_handler::<DependencyInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[35],
        "Remove an Org dependency",
        remove_dependency_handler,
    );
    add_handler::<NoteLinkInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[36],
        "Link a note to an Org item",
        link_note_handler,
    );
    add_handler::<NoteUnlinkInput, ItemMutationOutput, _, _>(
        &mut router,
        ORG_TOOL_NAMES[37],
        "Unlink a note from an Org item",
        unlink_note_handler,
    );
    add_handler::<NoteItemsInput, PageOutput<ItemOutput>, _, _>(
        &mut router,
        ORG_TOOL_NAMES[38],
        "List Org work items linked to a note",
        list_note_work_items_handler,
    );
    add_handler::<EventListInput, PageOutput<EventOutput>, _, _>(
        &mut router,
        ORG_TOOL_NAMES[39],
        "List Org event history",
        list_events_handler,
    );
    router
}

async fn list_workspaces_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ListInput,
) -> Result<PageOutput<WorkspaceSummaryOutput>, note_pipelines::org::OrgError> {
    let query = input.into();
    note_pipelines::org::list_workspaces(&context, &query)
        .await
        .map(Into::into)
}

async fn create_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: WorkspaceCreateInput,
) -> Result<WorkspaceRevisionOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::create_workspace(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn get_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: WorkspaceReadInput,
) -> Result<WorkspaceOutput, note_pipelines::org::OrgError> {
    let workspace_id = input.into_workspace_id()?;
    note_pipelines::org::get_workspace(&context, workspace_id)
        .await?
        .try_into()
}

async fn update_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: WorkspaceUpdateInput,
) -> Result<WorkspaceRevisionOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::update_workspace(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn archive_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: WorkspaceArchiveInput,
) -> Result<WorkspaceArchiveOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::archive_workspace(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn list_documents_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DocumentListInput,
) -> Result<PageOutput<DocumentOutput>, note_pipelines::org::OrgError> {
    let (workspace_id, query) = input.into_pipeline()?;
    note_pipelines::org::list_documents(&context, workspace_id, &query)
        .await
        .map(Into::into)
}

async fn get_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DocumentReadInput,
) -> Result<DocumentSourceOutput, note_pipelines::org::OrgError> {
    let (workspace_id, document_id) = input.into_pipeline()?;
    note_pipelines::org::get_document(&context, workspace_id, document_id)
        .await
        .map(Into::into)
}

async fn put_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: PutDocumentInput,
) -> Result<DocumentCountOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::put_document(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn create_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: CreateDocumentInput,
) -> Result<DocumentLifecycleOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::create_document(&context, &envelope, &request).await?;
    crate::org_dto::document_lifecycle_command_output(result)
}

async fn rename_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: RenameDocumentInput,
) -> Result<DocumentLifecycleOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::rename_document(&context, &envelope, &request).await?;
    crate::org_dto::document_lifecycle_command_output(result)
}

async fn archive_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DocumentRevisionInput,
) -> Result<DocumentLifecycleOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::archive_document(&context, &envelope, &request).await?;
    crate::org_dto::document_lifecycle_command_output(result)
}

async fn restore_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DocumentRevisionInput,
) -> Result<DocumentLifecycleOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::restore_document(&context, &envelope, &request).await?;
    crate::org_dto::document_lifecycle_command_output(result)
}

async fn import_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ImportWorkspaceInput,
) -> Result<DocumentCountOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::import_documents(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn move_document_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: MoveDocumentInput,
) -> Result<MoveDocumentOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::move_document(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn move_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: MoveItemInput,
) -> Result<MoveItemOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::move_item(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn export_workspace_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: WorkspaceReadInput,
) -> Result<WorkspaceExportOutput, note_pipelines::org::OrgError> {
    let workspace_id = input.into_workspace_id()?;
    note_pipelines::org::export_workspace(&context, workspace_id)
        .await?
        .try_into()
}

async fn create_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: CreateItemInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::create_item(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn get_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ItemReadInput,
) -> Result<ItemOutput, note_pipelines::org::OrgError> {
    let (workspace_id, item_id) = input.into_pipeline()?;
    note_pipelines::org::get_item(&context, workspace_id, item_id)
        .await?
        .try_into()
}

async fn get_item_context_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ItemReadInput,
) -> Result<ItemContextOutput, note_pipelines::org::OrgError> {
    let (workspace_id, item_id) = input.into_pipeline()?;
    note_pipelines::org::get_item_context(&context, workspace_id, item_id)
        .await?
        .try_into()
}

async fn create_follow_up_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: CreateFollowUpInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::create_follow_up(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn assign_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: AssignItemInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::assign_item(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn schedule_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ScheduleItemInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::schedule_item(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn query_queue_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: OperationalQueryInput<QueueViewInput>,
) -> Result<OperationalPageOutput, note_pipelines::org::OrgError> {
    let query = input.into_pipeline()?;
    note_pipelines::org::query_queue(&context, &query)
        .await?
        .try_into()
}

async fn query_agenda_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: OperationalQueryInput<AgendaViewInput>,
) -> Result<OperationalPageOutput, note_pipelines::org::OrgError> {
    let query = input.into_pipeline()?;
    note_pipelines::org::query_agenda(&context, &query)
        .await?
        .try_into()
}

async fn claim_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ClaimInput,
) -> Result<ClaimOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    note_pipelines::org::claim_item(&context, &envelope, &request)
        .await?
        .try_into()
}

async fn heartbeat_claim_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: HeartbeatClaimInput,
) -> Result<HeartbeatMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::heartbeat_claim(&context, &envelope, &request).await?;
    crate::org_dto::heartbeat_command_output(result)
}

async fn release_claim_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ReleaseClaimInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::release_claim(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn progress_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ProgressInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::report_progress(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn submit_result_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: SubmitResultInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::submit_result(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn transition_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: TransitionInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::transition_item(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn retry_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: RetryInput,
) -> Result<ClaimOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    note_pipelines::org::retry_item(&context, &envelope, &request)
        .await?
        .try_into()
}

async fn request_review_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: RequestReviewInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::request_review(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn approve_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: ApproveInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::approve_item(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn reject_item_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: RejectInput,
) -> Result<ContextMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::reject_item(&context, &envelope, &request).await?;
    crate::org_dto::context_command_output(result)
}

async fn add_dependency_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DependencyInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::add_dependency(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn remove_dependency_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: DependencyInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::remove_dependency(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn link_note_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: NoteLinkInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::link_note(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn unlink_note_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: NoteUnlinkInput,
) -> Result<ItemMutationOutput, note_pipelines::org::OrgError> {
    let (envelope, request) = input.into_pipeline()?;
    let result = note_pipelines::org::unlink_note(&context, &envelope, &request).await?;
    crate::org_dto::command_output(result)
}

async fn list_note_work_items_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: NoteItemsInput,
) -> Result<PageOutput<ItemOutput>, note_pipelines::org::OrgError> {
    let (note_id, query) = input.into_pipeline();
    let page = note_pipelines::org::list_note_work_items(&context, &note_id, &query).await?;
    crate::org_dto::item_page_output(page)
}

async fn list_events_handler(
    context: Arc<note_pipelines::org::OrgContext>,
    input: EventListInput,
) -> Result<PageOutput<EventOutput>, note_pipelines::org::OrgError> {
    let query = input.into_pipeline()?;
    note_pipelines::org::list_event_history(&context, &query)
        .await
        .map(crate::org_dto::event_page_output)
}

fn add_handler<I, O, F, Fut>(
    router: &mut ToolRouter<NoteMcpServer>,
    name: &'static str,
    description: &'static str,
    handler: F,
) where
    I: JsonSchema + DeserializeOwned + 'static,
    O: JsonSchema + Serialize + 'static,
    F: Fn(Arc<note_pipelines::org::OrgContext>, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, note_pipelines::org::OrgError>> + Send + 'static,
{
    let tool =
        Tool::new(name, description, schema::<I>()).with_raw_output_schema(Arc::new(schema::<O>()));
    router.add_route(ToolRoute::new_dyn(
        tool,
        move |context: ToolCallContext<'_, NoteMcpServer>| {
            let org_context = context.service.org_context().clone();
            let arguments = context.arguments.unwrap_or_default();
            if has_incomplete_lease_proof(name, &arguments) {
                return Box::pin(async move { Err(stale_lease_tool_error()) });
            }
            let input = match serde_json::from_value::<I>(serde_json::Value::Object(arguments)) {
                Ok(input) => input,
                Err(_) => return Box::pin(async move { Err(invalid_org_tool_input()) }),
            };
            let future = handler(org_context, input);
            Box::pin(async move {
                let output = future.await.map_err(crate::org_error::to_error_data)?;
                let value = serde_json::to_value(output).map_err(|_| {
                    ErrorData::internal_error("failed to serialize Org tool output", None)
                })?;
                Ok(CallToolResult::structured(value).into())
            })
        },
    ));
}

fn has_incomplete_lease_proof(
    name: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let requires_top_level_lease = matches!(
        name,
        "org_heartbeat_claim"
            | "org_release_claim"
            | "org_report_progress"
            | "org_submit_result"
            | "org_request_review"
            | "org_approve_item"
            | "org_reject_item"
    );
    if requires_top_level_lease
        && (!arguments.contains_key("lease_id") || !arguments.contains_key("fencing_token"))
    {
        return true;
    }
    if matches!(
        name,
        "org_heartbeat_claim" | "org_release_claim" | "org_report_progress"
    ) && !arguments.contains_key("kind")
    {
        return true;
    }
    if matches!(
        name,
        "org_transition_item"
            | "org_add_dependency"
            | "org_remove_dependency"
            | "org_link_note"
            | "org_unlink_note"
    ) {
        return arguments
            .get("lease")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|lease| {
                !lease.contains_key("lease_id")
                    || !lease.contains_key("kind")
                    || !lease.contains_key("fencing_token")
            });
    }
    false
}

fn stale_lease_tool_error() -> ErrorData {
    crate::org_error::to_error_data(note_pipelines::org::OrgError::new(
        note_pipelines::org::OrgErrorCode::StaleLease,
        "Org lease proof is stale or invalid",
        serde_json::json!({}),
        true,
    ))
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
            .call_tool(CallToolRequestParams::new(ORG_TOOL_NAMES[16]).with_arguments(arguments))
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
