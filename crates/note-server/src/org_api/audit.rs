use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use serde_json::json;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{safe_json, EventQueryBody, WorkspacePath},
    error::{OrgPath, OrgQuery},
};

#[utoipa::path(
    get,
    path = "/api/org/workspaces/{workspace_id}/events",
    operation_id = "org_list_events",
    tag = "org",
    params(
        ("workspace_id" = String, Path, description = "Workspace UUID"),
        EventQueryBody
    ),
    responses(
        (status = 200, description = "Workspace event history in sequence order", body = serde_json::Value),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn list_events(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<WorkspacePath>,
    OrgQuery(query): OrgQuery<EventQueryBody>,
) -> Result<Json<serde_json::Value>, OrgApiError> {
    let query = query.into_pipeline(path.parse()?);
    let page = note_pipelines::org::list_event_history(&context, &query).await?;
    Ok(Json(safe_json(json!({
        "items": page.events,
        "next_cursor": page.next_cursor,
    }))?))
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_events))
}
