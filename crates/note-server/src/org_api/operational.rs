use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{safe_json, AgendaQueryBody, QueueQueryBody},
    error::OrgQuery,
};

#[utoipa::path(
    get,
    path = "/api/org/queue",
    operation_id = "org_query_queue",
    tag = "org",
    params(QueueQueryBody),
    responses(
        (status = 200, description = "Operational queue page", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn query_queue(
    State(context): State<Arc<OrgContext>>,
    OrgQuery(query): OrgQuery<QueueQueryBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let query = query.into_pipeline()?;
    let page = note_pipelines::org::query_queue(&context, &query).await?;
    Ok(Json(safe_json(page)?))
}

#[utoipa::path(
    get,
    path = "/api/org/agenda",
    operation_id = "org_query_agenda",
    tag = "org",
    params(AgendaQueryBody),
    responses(
        (status = 200, description = "Operational agenda page", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn query_agenda(
    State(context): State<Arc<OrgContext>>,
    OrgQuery(query): OrgQuery<AgendaQueryBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let query = query.into_pipeline()?;
    let page = note_pipelines::org::query_agenda(&context, &query).await?;
    Ok(Json(safe_json(page)?))
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(query_queue))
        .routes(routes!(query_agenda))
}
