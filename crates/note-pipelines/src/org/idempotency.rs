use super::OrgError;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn request_fingerprint<T: Serialize + ?Sized>(value: &T) -> Result<String, OrgError> {
    let value = serde_json::to_value(value)
        .map_err(|_| OrgError::invalid_input("Org command cannot be serialized"))?;
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|_| OrgError::invalid_input("Org command cannot be serialized"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect(),
            )
        }
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::request_fingerprint;
    use serde_json::json;

    #[test]
    fn nested_object_key_order_is_canonical() {
        assert_eq!(
            request_fingerprint(&json!({"z": 1, "a": {"second": 2, "first": 1}})).unwrap(),
            request_fingerprint(&json!({"a": {"first": 1, "second": 2}, "z": 1})).unwrap()
        );
    }
}
