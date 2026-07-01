use axum::{extract::State, routing::post, Json, Router};
use note_core::LabelKey;
use note_pipelines::{define_label_key, list_label_keys, Context};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct DefineLabelKeyRequest {
    pub key: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct LabelKeyDto {
    pub key: String,
    pub description: String,
}

impl From<LabelKey> for LabelKeyDto {
    fn from(lk: LabelKey) -> Self {
        Self {
            key: lk.key,
            description: lk.description,
        }
    }
}

async fn define_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<DefineLabelKeyRequest>,
) -> Result<(), (axum::http::StatusCode, String)> {
    define_label_key(&ctx, &req.key, &req.description)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))
}

async fn list_label_keys_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<LabelKeyDto>>, (axum::http::StatusCode, String)> {
    let keys = list_label_keys(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub fn labels_router() -> Router<Arc<Context>> {
    // GET has no body (unlike /api/notes/search), so a browser GET is safe here.
    Router::new().route(
        "/api/labels",
        post(define_label_key_handler).get(list_label_keys_handler),
    )
}
