mod documents;
pub mod dto;
pub mod error;
mod execution;
mod items;
mod operational;
mod review;
mod workspaces;

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
        .merge(workspaces::router())
        .merge(documents::router())
        .merge(items::router())
        .merge(operational::router())
        .merge(execution::router())
        .merge(review::router())
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
