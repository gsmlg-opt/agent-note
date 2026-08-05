use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{
        safe_json, ClaimItemBody, ClaimResultBody, HeartbeatClaimBody, ItemPath, ReleaseClaimBody,
        ReportProgressBody, RetryItemBody, SubmitResultBody, TransitionItemBody,
    },
    error::{OrgJson, OrgPath},
};

#[utoipa::path(
    post,
    path = "/api/org/items/{item_id}/claim",
    operation_id = "org_claim_item",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = ClaimItemBody,
    responses(
        (status = 200, description = "Work item claimed; response contains a sensitive raw fencing token", body = ClaimResultBody),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn claim_item(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<ClaimItemBody>,
) -> Result<Json<ClaimResultBody>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::claim_item(&context, &command, &request).await?;
    Ok(Json(result.try_into()?))
}

macro_rules! command_operation {
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

command_operation!(
    heartbeat_claim,
    HeartbeatClaimBody,
    "/api/org/items/{item_id}/claim/heartbeat",
    "org_heartbeat_claim",
    heartbeat_claim,
    "Claim heartbeat accepted"
);
command_operation!(
    release_claim,
    ReleaseClaimBody,
    "/api/org/items/{item_id}/claim/release",
    "org_release_claim",
    release_claim,
    "Claim released"
);
command_operation!(
    report_progress,
    ReportProgressBody,
    "/api/org/items/{item_id}/progress",
    "org_report_progress",
    report_progress,
    "Progress recorded"
);
command_operation!(
    submit_result,
    SubmitResultBody,
    "/api/org/items/{item_id}/result",
    "org_submit_result",
    submit_result,
    "Result submitted"
);
command_operation!(
    transition_item,
    TransitionItemBody,
    "/api/org/items/{item_id}/transition",
    "org_transition_item",
    transition_item,
    "Work item transitioned"
);

#[utoipa::path(
    post,
    path = "/api/org/items/{item_id}/retry",
    operation_id = "org_retry_item",
    tag = "org",
    params(("item_id" = String, Path, description = "Work item UUID")),
    request_body = RetryItemBody,
    responses(
        (status = 200, description = "Failed work item retried; response contains a sensitive raw fencing token", body = ClaimResultBody),
        (status = 400, description = "Invalid input", body = OrgApiError),
        (status = 404, description = "Resource not found", body = OrgApiError),
        (status = 409, description = "Org state conflict", body = OrgApiError),
        (status = 429, description = "Workspace concurrency limit reached", body = OrgApiError),
        (status = 500, description = "Storage failure", body = OrgApiError)
    )
)]
async fn retry_item(
    State(context): State<Arc<OrgContext>>,
    OrgPath(path): OrgPath<ItemPath>,
    OrgJson(body): OrgJson<RetryItemBody>,
) -> Result<Json<ClaimResultBody>, OrgApiError> {
    let (command, request) = body.into_pipeline(path.parse()?)?;
    let result = note_pipelines::org::retry_item(&context, &command, &request).await?;
    Ok(Json(result.try_into()?))
}

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(claim_item))
        .routes(routes!(heartbeat_claim))
        .routes(routes!(release_claim))
        .routes(routes!(report_progress))
        .routes(routes!(submit_result))
        .routes(routes!(transition_item))
        .routes(routes!(retry_item))
}
