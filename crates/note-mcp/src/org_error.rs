use note_pipelines::org::{OrgError as OrgPipelineError, OrgErrorCode};
use rmcp::model::ErrorData;
use serde::Serialize;

#[derive(Serialize)]
struct OrgErrorData<'a> {
    code: OrgErrorCode,
    message: &'a str,
    details: &'a serde_json::Value,
    retryable: bool,
}

pub(crate) fn to_error_data(error: OrgPipelineError) -> ErrorData {
    let data = serde_json::to_value(OrgErrorData {
        code: error.code,
        message: &error.message,
        details: &error.details,
        retryable: error.retryable,
    })
    .expect("Org error data is serializable");

    match error.code {
        OrgErrorCode::InvalidInput => ErrorData::invalid_params(error.message, Some(data)),
        OrgErrorCode::NotFound => ErrorData::resource_not_found(error.message, Some(data)),
        OrgErrorCode::StorageFailure => {
            ErrorData::internal_error("Org operation failed", Some(data))
        }
        _ => ErrorData::invalid_request(error.message, Some(data)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adapter_preserves_only_stable_structured_fields() {
        let mapped = to_error_data(OrgPipelineError::new(
            OrgErrorCode::StaleRevision,
            "document revision is stale",
            json!({"current_revision": 8}),
            false,
        ));
        assert_eq!(
            mapped.data,
            Some(json!({
                "code": "stale_revision",
                "message": "document revision is stale",
                "details": {"current_revision": 8},
                "retryable": false
            }))
        );
    }
}
