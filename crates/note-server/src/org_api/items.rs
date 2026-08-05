use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{
        safe_json, AssignItemBody, CreateFollowUpBody, CreateItemBody, ItemPath, ItemReadQuery,
        ScheduleItemBody, WorkspacePath,
    },
    error::{OrgJson, OrgPath, OrgQuery},
};

#[utoipa::path(
    post,
    path = "/api/org/workspaces/{workspace_id}/items",
    operation_id = "org_create_item",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    request_body = CreateItemBody,
    responses(
        (status = 200, description = "Work item created", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn create_item(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgJson(body): OrgJson<CreateItemBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::create_item(&context, &command, &request).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    get,
    path = "/api/org/items/{item_id}",
    operation_id = "org_get_item",
    tag = "org",
    params(
        ("item_id" = String, Path, description = "Work item UUID"),
        ("workspace_id" = String, Query, description = "Owning workspace UUID")
    ),
    responses(
        (status = 200, description = "Work item", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn get_item(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgQuery(query): OrgQuery<ItemReadQuery>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let result = note_pipelines::org::get_item(&context, query.parse()?, path.parse()?).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    get,
    path = "/api/org/items/{item_id}/context",
    operation_id = "org_get_item_context",
    tag = "org",
    params(
        ("item_id" = String, Path, description = "Work item UUID"),
        ("workspace_id" = String, Query, description = "Owning workspace UUID")
    ),
    responses(
        (status = 200, description = "Recovery-oriented work item context", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn get_item_context(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgQuery(query): OrgQuery<ItemReadQuery>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let result =
        note_pipelines::org::get_item_context(&context, query.parse()?, path.parse()?).await?;
    Ok(Json(safe_json(result)?))
}

macro_rules! item_mutation {
    ($handler:ident, $body:ty, $path:literal, $operation_id:literal, $pipeline:ident, $description:literal) => {
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $operation_id,
            tag = "org",
            params(("item_id" = String, Path, description = "Work item UUID")),
            request_body = $body,
            responses(
                (status = 200, description = $description, body = serde_json::Value),
                (status = 400, description = "Invalid input", body = OrgApiError),
                (status = 404, description = "Resource not found", body = OrgApiError),
                (status = 409, description = "Org state conflict", body = OrgApiError),
                (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
                (status = 500, description = "Storage failure", body = OrgApiError)
            )
        )]
        async fn $handler(
            State(context): State<Arc<OrgContext>>,
            OrgPath(path): OrgPath<ItemPath>,
            OrgJson(body): OrgJson<$body>,
        ) -> Result<Json<serde_json::Value>, OrgApiError> {
            let (command, request) = body.into_pipeline(path.parse()?)?;
            let result = note_pipelines::org::$pipeline(&context, &command, &request).await?;
            Ok(Json(safe_json(result)?))
        }
    };
}

item_mutation!(
    create_follow_up,
    CreateFollowUpBody,
    "/api/org/items/{item_id}/follow-ups",
    "org_create_follow_up",
    create_follow_up,
    "Follow-up work item created"
);
item_mutation!(
    assign_item,
    AssignItemBody,
    "/api/org/items/{item_id}/assignment",
    "org_assign_item",
    assign_item,
    "Work item assignment updated"
);
item_mutation!(
    schedule_item,
    ScheduleItemBody,
    "/api/org/items/{item_id}/schedule",
    "org_schedule_item",
    schedule_item,
    "Work item schedule updated"
);

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_item))
        .routes(routes!(get_item))
        .routes(routes!(get_item_context))
        .routes(routes!(create_follow_up))
        .routes(routes!(assign_item))
        .routes(routes!(schedule_item))
}
