use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        FromRequest, FromRequestParts, Json, Path, Query, Request,
    },
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
};
use note_pipelines::org::{OrgError, OrgErrorCode};
use serde::{de::DeserializeOwned, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgApiError {
    #[schema(example = "active_lease")]
    pub code: String,
    #[schema(example = "work item already has an active lease")]
    pub message: String,
    #[schema(example = json!({}))]
    pub details: serde_json::Value,
    #[schema(example = false)]
    pub retryable: bool,
    #[serde(skip)]
    #[schema(ignore)]
    status: StatusCode,
}

impl OrgApiError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::from(OrgError::invalid_input(message))
    }
}

impl From<OrgError> for OrgApiError {
    fn from(error: OrgError) -> Self {
        let status = status_for(error.code);
        let code = serde_json::to_value(error.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "storage_failure".to_owned());
        let storage_failure = error.code == OrgErrorCode::StorageFailure;
        Self {
            code,
            message: if storage_failure {
                "Org storage operation failed".to_owned()
            } else {
                error.message
            },
            details: if storage_failure {
                serde_json::json!({})
            } else {
                sanitize_details(error.details)
            },
            retryable: error.retryable || error.code == OrgErrorCode::ConcurrencyLimit,
            status,
        }
    }
}

impl IntoResponse for OrgApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

impl From<JsonRejection> for OrgApiError {
    fn from(_rejection: JsonRejection) -> Self {
        Self::invalid_input("Invalid JSON request body")
    }
}

impl From<QueryRejection> for OrgApiError {
    fn from(_rejection: QueryRejection) -> Self {
        Self::invalid_input("Invalid query parameters")
    }
}

impl From<PathRejection> for OrgApiError {
    fn from(_rejection: PathRejection) -> Self {
        Self::invalid_input("Invalid path parameters")
    }
}

pub struct OrgJson<T>(pub T);

impl<S, T> FromRequest<S> for OrgJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = OrgApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(Into::into)
    }
}

pub struct OrgQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for OrgQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = OrgApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(Into::into)
    }
}

pub struct OrgPath<T>(pub T);

impl<S, T> FromRequestParts<S> for OrgPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = OrgApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(Into::into)
    }
}

fn status_for(code: OrgErrorCode) -> StatusCode {
    match code {
        OrgErrorCode::InvalidInput | OrgErrorCode::UnsupportedSemanticEdit => {
            StatusCode::BAD_REQUEST
        }
        OrgErrorCode::NotFound | OrgErrorCode::NoteUnavailable => StatusCode::NOT_FOUND,
        OrgErrorCode::ArchivedWorkspace
        | OrgErrorCode::ArchivedDocument
        | OrgErrorCode::DocumentPathConflict
        | OrgErrorCode::StaleRevision
        | OrgErrorCode::IdempotencyConflict
        | OrgErrorCode::InvalidTransition
        | OrgErrorCode::DependencyBlocked
        | OrgErrorCode::ReviewRequired
        | OrgErrorCode::ActiveLease
        | OrgErrorCode::StaleLease
        | OrgErrorCode::RetryLimit => StatusCode::CONFLICT,
        OrgErrorCode::ConcurrencyLimit => StatusCode::TOO_MANY_REQUESTS,
        OrgErrorCode::StorageFailure => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn sanitize_details(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .into_iter()
                .filter(|(key, _)| !is_sensitive_key(key))
                .map(|(key, value)| (key, sanitize_details(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sanitize_details).collect())
        }
        value => value,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token") || key.contains("hash") || key.contains("secret")
}
