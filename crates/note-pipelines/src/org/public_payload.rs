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
    let mut normalized = String::new();
    let mut previous_was_lower_or_digit = false;
    for character in key.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_was_lower_or_digit {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
        } else if !normalized.ends_with('_') && !normalized.is_empty() {
            normalized.push('_');
            previous_was_lower_or_digit = false;
        }
    }
    let normalized = normalized.trim_matches('_');
    if normalized
        .split('_')
        .any(|word| matches!(word, "token" | "hash"))
    {
        return true;
    }
    matches!(
        normalized.replace('_', "").as_str(),
        "fencingtoken"
            | "fencingtokendigest"
            | "fencingtokenhash"
            | "leasetoken"
            | "leasetokenhash"
            | "leasehash"
            | "leasehashdigest"
            | "tokenhash"
            | "tokendigest"
            | "hashvalue"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reserved_key_matching_catches_fencing_forms_without_false_positives() {
        for key in [
            "token_hash",
            "leaseToken",
            "fencingtoken",
            "fencingTokenDigest",
            "hash_value",
            "token-digest",
        ] {
            assert!(is_reserved_key(key), "missed {key}");
        }
        for key in [
            "tokenized_count",
            "hashmap_size",
            "secretary",
            "passwordless",
        ] {
            assert!(!is_reserved_key(key), "rejected {key}");
        }
    }

    #[test]
    fn legal_structured_payloads_are_preserved() {
        validate_public_payload(
            "opaque-fencing-material",
            &json!({
                "tokenized_count": 2,
                "hashmap_size": 3,
                "nested": {"secretary": "available", "passwordless": true}
            }),
        )
        .unwrap();
    }
}
