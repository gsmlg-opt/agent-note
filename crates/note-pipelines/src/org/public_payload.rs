use super::{token_hash, OrgError};
use serde_json::Value;

pub(crate) fn validate_public_payload(
    raw_fencing_token: &str,
    payload: &Value,
) -> Result<(), OrgError> {
    let fencing_token_digest = token_hash(raw_fencing_token);
    if contains_fencing_material(payload, raw_fencing_token, &fencing_token_digest) {
        return Err(OrgError::invalid_input(
            "Org workflow public payload contains reserved fencing material",
        ));
    }
    Ok(())
}

fn contains_fencing_material(value: &Value, raw_token: &str, token_digest: &str) -> bool {
    match value {
        Value::String(value) => value.contains(raw_token) || value.contains(token_digest),
        Value::Array(values) => values
            .iter()
            .any(|value| contains_fencing_material(value, raw_token, token_digest)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            is_reserved_key(key) || contains_fencing_material(value, raw_token, token_digest)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn is_reserved_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("token") || normalized.contains("hash")
}
