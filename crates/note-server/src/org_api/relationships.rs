use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{
        safe_json, AddDependencyBody, DependencyPath, ItemPath, LinkNoteBody, NotePath,
        OrgPageQuery, RemoveDependencyBody, UnlinkNoteBody,
    },
    error::{OrgJson, OrgPath, OrgQuery},
};

#[utoipa::path(
    post,
    path = "/api/org/items/{item_id}/dependencies",
    operation_id = "org_add_dependency",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = AddDependencyBody,
    responses(
        (status = 200, description = "Dependency added", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn add_dependency(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<AddDependencyBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::add_dependency(&context, &command, &request).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    delete,
    path = "/api/org/items/{item_id}/dependencies/{dependency_item_id}",
    operation_id = "org_remove_dependency",
    tag = "org",
    params(
        ("item_id" = String, Path, description = "Work item UUID"),
        ("dependency_item_id" = String, Path, description = "Dependency work item UUID")
    ),
    request_body = RemoveDependencyBody,
    responses(
        (status = 200, description = "Dependency removed", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn remove_dependency(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DependencyPath>,
    OrgJson(body): OrgJson<RemoveDependencyBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let (item_id, dependency_id) = path.parse()?;
    let (command, request) = body.into_pipeline(item_id, dependency_id)?;
    let result = note_pipelines::org::remove_dependency(&context, &command, &request).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    post,
    path = "/api/org/items/{item_id}/note-links",
    operation_id = "org_link_note",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = LinkNoteBody,
    responses(
        (status = 200, description = "Markdown note linked", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn link_note(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<LinkNoteBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::link_note(&context, &command, &request).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    delete,
    path = "/api/org/items/{item_id}/note-links",
    operation_id = "org_unlink_note",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = UnlinkNoteBody,
    responses(
        (status = 200, description = "Markdown note unlinked", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn unlink_note(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<UnlinkNoteBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::unlink_note(&context, &command, &request).await?;
    Ok(Json(safe_json(result)?))
}

#[utoipa::path(
    get,
    path = "/api/org/notes/{note_id}/work-items",
    operation_id = "org_list_note_work_items",
    tag = "org",
    params(
        ("note_id" = String, Path, description = "Markdown note UUID"),
        OrgPageQuery
    ),
    responses(
        (status = 200, description = "Work items linked to the note", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn list_note_work_items(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<NotePath>,
    OrgQuery(query): OrgQuery<OrgPageQuery>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let result =
        note_pipelines::org::list_note_work_items(&context, &path.note_id, &query.into()).await?;
    Ok(Json(safe_json(result)?))
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(add_dependency))
        .routes(routes!(remove_dependency))
        .routes(routes!(link_note, unlink_note))
        .routes(routes!(list_note_work_items))
}
