mod audit;
mod documents;
pub mod dto;
pub mod error;
mod execution;
mod items;
mod operational;
mod relationships;
mod review;
mod workspaces;

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

use crate::app_state::AppState;
use dto::{OrgMutationEnvelope, OrgPageQuery};
use error::OrgApiError;
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
        .merge(relationships::router())
        .merge(audit::router())
}
