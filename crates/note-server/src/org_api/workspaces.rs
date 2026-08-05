use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::{OrgContext, OrgReadPage, OrgWorkspaceView, WorkspaceSummary};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{
        ArchiveWorkspaceBody, CreateWorkspaceBody, OrgPageQuery, UpdateWorkspaceBody, WorkspacePath,
    },
    error::{OrgJson, OrgPath, OrgQuery},
};

#[utoipa::path(
    get,
    path = "/api/org/workspaces",
    operation_id = "org_list_workspaces",
    tag = "org",
    params(OrgPageQuery),
    responses(
        (status = 200, description = "Workspace summaries", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn list_workspaces(
    State(context): State<Arc<OrgContext>>,
    OrgQuery(query): OrgQuery<OrgPageQuery>,
) -> Result<Json<OrgReadPage<WorkspaceSummary>>, OrgApiError> {
    note_pipelines::org::list_workspaces(&context, &query.into())
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/workspaces",
    operation_id = "org_create_workspace",
    tag = "org",
    request_body = CreateWorkspaceBody,
    responses(
        (status = 200, description = "Workspace created", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn create_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgJson(body): OrgJson<CreateWorkspaceBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline()?;
    note_pipelines::org::create_workspace(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/api/org/workspaces/{workspace_id}",
    operation_id = "org_get_workspace",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    responses(
        (status = 200, description = "Workspace", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn get_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
) -> Result<Json<OrgWorkspaceView>, OrgApiError> {
    note_pipelines::org::get_workspace(&context, path.parse()?)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    patch,
    path = "/api/org/workspaces/{workspace_id}",
    operation_id = "org_update_workspace",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    request_body = UpdateWorkspaceBody,
    responses(
        (status = 200, description = "Workspace updated", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn update_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgJson(body): OrgJson<UpdateWorkspaceBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::update_workspace(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/workspaces/{workspace_id}/archive",
    operation_id = "org_archive_workspace",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    request_body = ArchiveWorkspaceBody,
    responses(
        (status = 200, description = "Workspace archived", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn archive_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgJson(body): OrgJson<ArchiveWorkspaceBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::archive_workspace(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_workspaces, create_workspace))
        .routes(routes!(get_workspace, update_workspace))
        .routes(routes!(archive_workspace))
}
