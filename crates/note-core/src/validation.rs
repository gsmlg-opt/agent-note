// crates/note-core/src/validation.rs

use crate::LabelValueType;

#[derive(Debug, PartialEq)]
pub enum ValidationError {
    EmptyTitle,
    EmptyContent,
    UnknownLabelKey(String),
    InvalidLabelValue {
        key: String,
        value: String,
        value_type: LabelValueType,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptyTitle => write!(f, "title must not be empty"),
            ValidationError::EmptyContent => write!(f, "content must not be empty"),
            ValidationError::UnknownLabelKey(k) => write!(f, "unknown label key: {k}"),
            ValidationError::InvalidLabelValue {
                key,
                value,
                value_type,
            } => write!(
                f,
                "invalid value for label {key}: {value} is not {value_type}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validation failures for the label-key catalog (define_label_key). Kept separate from
/// `ValidationError` (which is about note input) so each handler downcasts to the precise type it
/// cares about. Like `ValidationError`, this is carried through the anyhow chain so the REST layer
/// can map these caller-fault cases to 400 while genuine storage failures stay 500.
#[derive(Debug, PartialEq)]
pub enum LabelKeyValidationError {
    EmptyKey,
    InvalidValueType(String),
}

impl std::fmt::Display for LabelKeyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LabelKeyValidationError::EmptyKey => write!(f, "label key must not be empty"),
            LabelKeyValidationError::InvalidValueType(value_type) => {
                write!(f, "invalid label value type: {value_type}")
            }
        }
    }
}

impl std::error::Error for LabelKeyValidationError {}

pub struct NoteInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>, // (key, value) pairs supplied by the caller
}

pub fn validate_note_input(
    input: &NoteInput,
    known_keys: &[String],
) -> Result<(), ValidationError> {
    if input.title.trim().is_empty() {
        return Err(ValidationError::EmptyTitle);
    }
    if input.content.trim().is_empty() {
        return Err(ValidationError::EmptyContent);
    }
    for (key, _) in &input.labels {
        if !known_keys.iter().any(|k| k == key) {
            return Err(ValidationError::UnknownLabelKey(key.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(title: &str, content: &str, labels: &[(&str, &str)]) -> NoteInput {
        NoteInput {
            title: title.to_string(),
            content: content.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn rejects_empty_title() {
        let result = validate_note_input(&input("", "content", &[]), &[]);
        assert_eq!(result, Err(ValidationError::EmptyTitle));
    }

    #[test]
    fn rejects_empty_content() {
        let result = validate_note_input(&input("title", "", &[]), &[]);
        assert_eq!(result, Err(ValidationError::EmptyContent));
    }

    #[test]
    fn rejects_unknown_label_key() {
        let result = validate_note_input(
            &input("title", "content", &[("status", "done")]),
            &[], // no known keys registered
        );
        assert_eq!(
            result,
            Err(ValidationError::UnknownLabelKey("status".to_string()))
        );
    }

    #[test]
    fn accepts_valid_input_with_known_label() {
        let result = validate_note_input(
            &input("title", "content", &[("status", "done")]),
            &["status".to_string()],
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn accepts_valid_input_with_no_labels() {
        let result = validate_note_input(&input("title", "content", &[]), &[]);
        assert_eq!(result, Ok(()));
    }
}
