use std::sync::Arc;

use axum::{extract::State, Json};
use note_pipelines::org::OrgContext;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app_state::AppState, org_api::error::OrgApiError};

use super::{
    dto::{safe_json, ApproveItemBody, ItemPath, RejectItemBody, RequestReviewBody},
    error::{OrgJson, OrgPath},
};

macro_rules! review_operation {
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

review_operation!(
    request_review,
    RequestReviewBody,
    "/api/org/items/{item_id}/review/request",
    "org_request_review",
    request_review,
    "Review requested"
);
review_operation!(
    approve_item,
    ApproveItemBody,
    "/api/org/items/{item_id}/review/approve",
    "org_approve_item",
    approve_item,
    "Review approved"
);
review_operation!(
    reject_item,
    RejectItemBody,
    "/api/org/items/{item_id}/review/reject",
    "org_reject_item",
    reject_item,
    "Review rejected"
);

pub(super) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(request_review))
        .routes(routes!(approve_item))
        .routes(routes!(reject_item))
}
