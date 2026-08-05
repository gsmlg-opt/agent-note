pub mod dto;
pub mod error;

use axum::{extract::State, Json};
use note_pipelines::org::{OrgContext, OrgError, OrgErrorCode};
use std::sync::Arc;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::app_state::AppState;
use dto::{OrgMutationEnvelope, OrgPageQuery};
use error::OrgApiError;

macro_rules! org_read_route {
    ($handler:ident, $method:ident, $path:literal, $operation_id:literal) => {
        #[utoipa::path(
            $method,
            path = $path,
            operation_id = $operation_id,
            tag = "org",
            responses(
                (status = 200, description = "Org operation response", body = serde_json::Value),
                (status = 400, description = "Invalid input", body = OrgApiError),
                (status = 404, description = "Resource not found", body = OrgApiError),
                (status = 409, description = "Org state conflict", body = OrgApiError),
                (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
                (status = 500, description = "Storage failure", body = OrgApiError)
            )
        )]
        async fn $handler(
            State(_org_context): State<Arc<OrgContext>>,
        ) -> Result<Json<serde_json::Value>, OrgApiError> {
            Err(operation_not_available())
        }
    };
}

macro_rules! org_mutation_route {
    ($handler:ident, $method:ident, $path:literal, $operation_id:literal) => {
        #[utoipa::path(
            $method,
            path = $path,
            operation_id = $operation_id,
            tag = "org",
            request_body = OrgMutationEnvelope,
            responses(
                (status = 200, description = "Org mutation response", body = serde_json::Value),
                (status = 400, description = "Invalid input", body = OrgApiError),
                (status = 404, description = "Resource not found", body = OrgApiError),
                (status = 409, description = "Org state conflict", body = OrgApiError),
                (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
                (status = 500, description = "Storage failure", body = OrgApiError)
            )
        )]
        async fn $handler(
            State(_org_context): State<Arc<OrgContext>>,
        ) -> Result<Json<serde_json::Value>, OrgApiError> {
            Err(operation_not_available())
        }
    };
}

org_read_route!(
    list_workspaces,
    get,
    "/api/org/workspaces",
    "org_list_workspaces"
);
org_mutation_route!(
    create_workspace,
    post,
    "/api/org/workspaces",
    "org_create_workspace"
);
org_read_route!(
    get_workspace,
    get,
    "/api/org/workspaces/{workspace_id}",
    "org_get_workspace"
);
org_mutation_route!(
    update_workspace,
    patch,
    "/api/org/workspaces/{workspace_id}",
    "org_update_workspace"
);
org_mutation_route!(
    archive_workspace,
    post,
    "/api/org/workspaces/{workspace_id}/archive",
    "org_archive_workspace"
);
org_read_route!(
    list_documents,
    get,
    "/api/org/workspaces/{workspace_id}/documents",
    "org_list_documents"
);
org_read_route!(
    get_document,
    get,
    "/api/org/documents/{document_id}",
    "org_get_document"
);
org_mutation_route!(
    put_document,
    put,
    "/api/org/documents/{document_id}",
    "org_put_document"
);
org_mutation_route!(
    move_document,
    post,
    "/api/org/documents/{document_id}/move",
    "org_move_document"
);
org_mutation_route!(
    move_item,
    post,
    "/api/org/items/{item_id}/move",
    "org_move_item"
);
org_mutation_route!(
    import_workspace,
    post,
    "/api/org/workspaces/{workspace_id}/import",
    "org_import_workspace"
);
org_read_route!(
    export_workspace,
    get,
    "/api/org/workspaces/{workspace_id}/export",
    "org_export_workspace"
);
org_mutation_route!(
    create_item,
    post,
    "/api/org/workspaces/{workspace_id}/items",
    "org_create_item"
);
org_read_route!(get_item, get, "/api/org/items/{item_id}", "org_get_item");
org_read_route!(
    get_item_context,
    get,
    "/api/org/items/{item_id}/context",
    "org_get_item_context"
);
org_mutation_route!(
    create_follow_up,
    post,
    "/api/org/items/{item_id}/follow-ups",
    "org_create_follow_up"
);
org_mutation_route!(
    assign_item,
    post,
    "/api/org/items/{item_id}/assignment",
    "org_assign_item"
);
org_mutation_route!(
    schedule_item,
    post,
    "/api/org/items/{item_id}/schedule",
    "org_schedule_item"
);
org_read_route!(query_queue, get, "/api/org/queue", "org_query_queue");
org_read_route!(query_agenda, get, "/api/org/agenda", "org_query_agenda");
org_mutation_route!(
    claim_item,
    post,
    "/api/org/items/{item_id}/claim",
    "org_claim_item"
);
org_mutation_route!(
    heartbeat_claim,
    post,
    "/api/org/items/{item_id}/claim/heartbeat",
    "org_heartbeat_claim"
);
org_mutation_route!(
    release_claim,
    post,
    "/api/org/items/{item_id}/claim/release",
    "org_release_claim"
);
org_mutation_route!(
    report_progress,
    post,
    "/api/org/items/{item_id}/progress",
    "org_report_progress"
);
org_mutation_route!(
    submit_result,
    post,
    "/api/org/items/{item_id}/result",
    "org_submit_result"
);
org_mutation_route!(
    transition_item,
    post,
    "/api/org/items/{item_id}/transition",
    "org_transition_item"
);
org_mutation_route!(
    retry_item,
    post,
    "/api/org/items/{item_id}/retry",
    "org_retry_item"
);
org_mutation_route!(
    request_review,
    post,
    "/api/org/items/{item_id}/review/request",
    "org_request_review"
);
org_mutation_route!(
    approve_item,
    post,
    "/api/org/items/{item_id}/review/approve",
    "org_approve_item"
);
org_mutation_route!(
    reject_item,
    post,
    "/api/org/items/{item_id}/review/reject",
    "org_reject_item"
);
org_mutation_route!(
    add_dependency,
    post,
    "/api/org/items/{item_id}/dependencies",
    "org_add_dependency"
);
org_mutation_route!(
    remove_dependency,
    delete,
    "/api/org/items/{item_id}/dependencies/{dependency_item_id}",
    "org_remove_dependency"
);
org_mutation_route!(
    link_note,
    post,
    "/api/org/items/{item_id}/note-links",
    "org_link_note"
);
org_mutation_route!(
    unlink_note,
    delete,
    "/api/org/items/{item_id}/note-links",
    "org_unlink_note"
);
org_read_route!(
    list_note_work_items,
    get,
    "/api/org/notes/{note_id}/work-items",
    "org_list_note_work_items"
);
org_read_route!(
    list_events,
    get,
    "/api/org/workspaces/{workspace_id}/events",
    "org_list_events"
);

#[derive(OpenApi)]
#[openapi(components(schemas(OrgMutationEnvelope, OrgPageQuery, OrgApiError)))]
struct OrgApiSchemas;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(OrgApiSchemas::openapi())
        .routes(routes!(list_workspaces, create_workspace))
        .routes(routes!(get_workspace, update_workspace))
        .routes(routes!(archive_workspace))
        .routes(routes!(list_documents))
        .routes(routes!(get_document, put_document))
        .routes(routes!(move_document))
        .routes(routes!(move_item))
        .routes(routes!(import_workspace))
        .routes(routes!(export_workspace))
        .routes(routes!(create_item))
        .routes(routes!(get_item))
        .routes(routes!(get_item_context))
        .routes(routes!(create_follow_up))
        .routes(routes!(assign_item))
        .routes(routes!(schedule_item))
        .routes(routes!(query_queue))
        .routes(routes!(query_agenda))
        .routes(routes!(claim_item))
        .routes(routes!(heartbeat_claim))
        .routes(routes!(release_claim))
        .routes(routes!(report_progress))
        .routes(routes!(submit_result))
        .routes(routes!(transition_item))
        .routes(routes!(retry_item))
        .routes(routes!(request_review))
        .routes(routes!(approve_item))
        .routes(routes!(reject_item))
        .routes(routes!(add_dependency))
        .routes(routes!(remove_dependency))
        .routes(routes!(link_note, unlink_note))
        .routes(routes!(list_note_work_items))
        .routes(routes!(list_events))
}

fn operation_not_available() -> OrgApiError {
    OrgError::new(
        OrgErrorCode::StorageFailure,
        "Org REST operation is not available",
        serde_json::json!({}),
        false,
    )
    .into()
}
