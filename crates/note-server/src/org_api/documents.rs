use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::{
    OrgContext, OrgDocumentSourceView, OrgDocumentView, OrgReadPage, OrgWorkspaceExport,
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    app_state::AppState, openapi::OrgDocumentLifecycleCommandResult, org_api::error::OrgApiError,
};

use super::{
    dto::{
        CreateDocumentBody, DocumentListQuery, DocumentPath, DocumentReadQuery,
        DocumentRevisionBody, ImportWorkspaceBody, ItemPath, MoveDocumentBody, MoveItemBody,
        PutDocumentBody, RenameDocumentBody, WorkspacePath,
    },
    error::{OrgJson, OrgPath, OrgQuery},
};

#[utoipa::path(
    get,
    path = "/api/org/workspaces/{workspace_id}/documents",
    operation_id = "org_list_documents",
    tag = "org",
    params(
        ("workspace_id" = String, Path, description = "Workspace UUID"),
        DocumentListQuery
    ),
    responses(
        (status = 200, description = "Workspace documents", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn list_documents(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgQuery(query): OrgQuery<DocumentListQuery>,
) -> Result<Json<OrgReadPage<OrgDocumentView>>, OrgApiError> {
    note_pipelines::org::list_documents(&context, path.parse()?, &query.into_pipeline()?)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/workspaces/{workspace_id}/documents",
    operation_id = "org_create_document",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    request_body = CreateDocumentBody,
    responses(
        (status = 200, description = "Empty document created", body = OrgDocumentLifecycleCommandResult),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn create_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgJson(body): OrgJson<CreateDocumentBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::create_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/api/org/documents/{document_id}",
    operation_id = "org_get_document",
    tag = "org",
    params(
        ("document_id" = String, Path, description = "Document UUID"),
        ("workspace_id" = String, Query, description = "Owning workspace UUID")
    ),
    responses(
        (status = 200, description = "Raw Org document", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn get_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgQuery(query): OrgQuery<DocumentReadQuery>,
) -> Result<Json<OrgDocumentSourceView>, OrgApiError> {
    note_pipelines::org::get_document(&context, query.parse()?, path.parse()?)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    put,
    path = "/api/org/documents/{document_id}",
    operation_id = "org_put_document",
    tag = "org",
    params(("document_id" = String, Path, description = "Document UUID")),
    request_body = PutDocumentBody,
    responses(
        (status = 200, description = "Document created or replaced", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn put_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgJson(body): OrgJson<PutDocumentBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::put_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    patch,
    path = "/api/org/documents/{document_id}/path",
    operation_id = "org_rename_document",
    tag = "org",
    params(("document_id" = String, Path, description = "Document UUID")),
    request_body = RenameDocumentBody,
    responses(
        (status = 200, description = "Document renamed", body = OrgDocumentLifecycleCommandResult),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn rename_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgJson(body): OrgJson<RenameDocumentBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::rename_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/documents/{document_id}/archive",
    operation_id = "org_archive_document",
    tag = "org",
    params(("document_id" = String, Path, description = "Document UUID")),
    request_body = DocumentRevisionBody,
    responses(
        (status = 200, description = "Document archived", body = OrgDocumentLifecycleCommandResult),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn archive_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgJson(body): OrgJson<DocumentRevisionBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::archive_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/documents/{document_id}/restore",
    operation_id = "org_restore_document",
    tag = "org",
    params(("document_id" = String, Path, description = "Document UUID")),
    request_body = DocumentRevisionBody,
    responses(
        (status = 200, description = "Document restored", body = OrgDocumentLifecycleCommandResult),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn restore_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgJson(body): OrgJson<DocumentRevisionBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::restore_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/documents/{document_id}/move",
    operation_id = "org_move_document",
    tag = "org",
    params(("document_id" = String, Path, description = "Document UUID")),
    request_body = MoveDocumentBody,
    responses(
        (status = 200, description = "Document moved", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn move_document(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<DocumentPath>,
    OrgJson(body): OrgJson<MoveDocumentBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::move_document(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/items/{item_id}/move",
    operation_id = "org_move_item",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = MoveItemBody,
    responses(
        (status = 200, description = "Work item moved or reparented", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn move_item(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<MoveItemBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::move_item(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/api/org/workspaces/{workspace_id}/import",
    operation_id = "org_import_workspace",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    request_body = ImportWorkspaceBody,
    responses(
        (status = 200, description = "Workspace documents imported atomically", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn import_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgJson(body): OrgJson<ImportWorkspaceBody>,
) -> Result<Json<note_pipelines::org::OrgCommandResult>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    note_pipelines::org::import_documents(&context, &command, &request)
        .await
        .map(Json)
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/api/org/workspaces/{workspace_id}/export",
    operation_id = "org_export_workspace",
    tag = "org",
    params(("workspace_id" = String, Path, description = "Workspace UUID")),
    responses(
        (status = 200, description = "Transport-neutral workspace export", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn export_workspace(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
) -> Result<Json<OrgWorkspaceExport>, OrgApiError> {
    note_pipelines::org::export_workspace(&context, path.parse()?)
        .await
        .map(Json)
        .map_err(Into::into)
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_documents, create_document))
        .routes(routes!(get_document, put_document))
        .routes(routes!(rename_document))
        .routes(routes!(archive_document))
        .routes(routes!(restore_document))
        .routes(routes!(move_document))
        .routes(routes!(move_item))
        .routes(routes!(import_workspace))
        .routes(routes!(export_workspace))
}
