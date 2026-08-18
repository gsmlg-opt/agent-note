// crates/note-core/src/validation.rs

use crate::{LabelValueType, NoteAttachment};
use std::collections::HashSet;

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
    EmptyAttachmentId,
    DuplicateAttachmentId(String),
    EmptyAttachmentPath,
    InvalidAttachmentPath(String),
    DuplicateAttachmentPath(String),
    EmptyAttachmentMime,
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
            ValidationError::EmptyAttachmentId => write!(f, "attachment id must not be empty"),
            ValidationError::DuplicateAttachmentId(id) => {
                write!(f, "duplicate attachment id: {id}")
            }
            ValidationError::EmptyAttachmentPath => {
                write!(f, "attachment path must not be empty")
            }
            ValidationError::InvalidAttachmentPath(path) => {
                write!(f, "attachment path must be relative: {path}")
            }
            ValidationError::DuplicateAttachmentPath(path) => {
                write!(f, "duplicate attachment path: {path}")
            }
            ValidationError::EmptyAttachmentMime => write!(f, "attachment mime must not be empty"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validation failures for label-key creation. Kept separate from `ValidationError` (which is
/// about note input) so each handler downcasts to the precise type it cares about. Like
/// `ValidationError`, this is carried through the anyhow chain so transports can map these
/// caller-fault cases precisely while genuine storage failures stay internal errors.
#[derive(Debug, PartialEq)]
pub enum LabelKeyValidationError {
    EmptyKey,
    ReservedCharacter(char),
    InvalidValueType(String),
}

impl std::fmt::Display for LabelKeyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LabelKeyValidationError::EmptyKey => write!(f, "label key must not be empty"),
            LabelKeyValidationError::ReservedCharacter(character) => write!(
                f,
                "label key must not contain selector-reserved character: {character}"
            ),
            LabelKeyValidationError::InvalidValueType(value_type) => {
                write!(f, "invalid label value type: {value_type}")
            }
        }
    }
}

impl std::error::Error for LabelKeyValidationError {}

pub fn validate_label_key(key: &str) -> Result<(), LabelKeyValidationError> {
    if key.trim().is_empty() {
        return Err(LabelKeyValidationError::EmptyKey);
    }
    if let Some(character) = key
        .chars()
        .find(|character| matches!(character, '&' | '=' | '!' | '<' | '>' | '^' | '$' | '~'))
    {
        return Err(LabelKeyValidationError::ReservedCharacter(character));
    }
    Ok(())
}

pub struct NoteInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>, // (key, value) pairs supplied by the caller
    pub attachments: Vec<NoteAttachment>,
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
    validate_attachments(&input.attachments)?;
    Ok(())
}

pub fn validate_attachments(attachments: &[NoteAttachment]) -> Result<(), ValidationError> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for attachment in attachments {
        let id = attachment.id.trim();
        if id.is_empty() {
            return Err(ValidationError::EmptyAttachmentId);
        }
        if !ids.insert(id.to_string()) {
            return Err(ValidationError::DuplicateAttachmentId(id.to_string()));
        }
        let path = attachment.path.trim();
        if path.is_empty() {
            return Err(ValidationError::EmptyAttachmentPath);
        }
        if !is_relative_attachment_path(path) {
            return Err(ValidationError::InvalidAttachmentPath(path.to_string()));
        }
        let normalized_path = normalize_attachment_path(path);
        if normalized_path.is_empty() {
            return Err(ValidationError::EmptyAttachmentPath);
        }
        if !paths.insert(normalized_path.clone()) {
            return Err(ValidationError::DuplicateAttachmentPath(normalized_path));
        }
        if attachment.mime.trim().is_empty() {
            return Err(ValidationError::EmptyAttachmentMime);
        }
    }
    Ok(())
}

/// Converts relative attachment path separators to `/` and removes empty and `.` components.
pub fn normalize_attachment_path(path: &str) -> String {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Returns whether an attachment path is relative and contains no parent traversal.
pub fn is_relative_attachment_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') || path.contains("://") {
        return false;
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return false;
    }
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .all(|part| part != "..")
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
            attachments: vec![],
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

    #[test]
    fn label_keys_reject_selector_reserved_characters() {
        for character in ['&', '=', '!', '<', '>', '^', '$', '~'] {
            let key = format!("project{character}name");
            assert_eq!(
                validate_label_key(&key),
                Err(LabelKeyValidationError::ReservedCharacter(character)),
                "{character:?} must be reserved for label selectors"
            );
        }
    }

    #[test]
    fn label_keys_preserve_existing_nonempty_grammar_otherwise() {
        assert_eq!(validate_label_key("project/name with spaces"), Ok(()));
        assert_eq!(
            validate_label_key(" \t "),
            Err(LabelKeyValidationError::EmptyKey)
        );
    }

    #[test]
    fn validates_attachment_identity_and_relative_path() {
        let mut input = input("title", "content", &[]);
        input.attachments = vec![NoteAttachment {
            id: "meta".to_string(),
            path: "./meta.json".to_string(),
            mime: "application/json".to_string(),
            description: "metadata".to_string(),
            content: b"{}".to_vec(),
            storage: None,
        }];
        assert_eq!(validate_note_input(&input, &[]), Ok(()));

        input.attachments.push(NoteAttachment {
            id: "meta".to_string(),
            path: "other.json".to_string(),
            mime: "application/json".to_string(),
            description: String::new(),
            content: b"{}".to_vec(),
            storage: None,
        });
        assert_eq!(
            validate_note_input(&input, &[]),
            Err(ValidationError::DuplicateAttachmentId("meta".to_string()))
        );

        input.attachments[1].id = "other".to_string();
        input.attachments[1].path = "../secret.json".to_string();
        assert_eq!(
            validate_note_input(&input, &[]),
            Err(ValidationError::InvalidAttachmentPath(
                "../secret.json".to_string()
            ))
        );
    }

    #[test]
    fn normalizes_attachment_path_separators_consistently() {
        assert_eq!(
            normalize_attachment_path(r"./folder\child/file.txt"),
            "folder/child/file.txt"
        );
    }

    #[test]
    fn rejects_duplicate_normalized_attachment_paths() {
        let mut input = input("title", "content", &[]);
        input.attachments = vec![
            NoteAttachment {
                id: "first".to_string(),
                path: "./assets//meta.json".to_string(),
                mime: "application/json".to_string(),
                description: String::new(),
                content: b"first".to_vec(),
                storage: None,
            },
            NoteAttachment {
                id: "second".to_string(),
                path: "assets/./meta.json".to_string(),
                mime: "application/json".to_string(),
                description: String::new(),
                content: b"second".to_vec(),
                storage: None,
            },
        ];

        assert_eq!(
            validate_note_input(&input, &[]),
            Err(ValidationError::DuplicateAttachmentPath(
                "assets/meta.json".to_string()
            ))
        );
    }

    #[test]
    fn rejects_attachment_paths_that_normalize_to_empty() {
        for path in [".", "./", ".//./"] {
            let attachment = NoteAttachment {
                id: "empty-path".to_string(),
                path: path.to_string(),
                mime: "text/plain".to_string(),
                description: String::new(),
                content: Vec::new(),
                storage: None,
            };

            assert_eq!(
                validate_attachments(&[attachment]),
                Err(ValidationError::EmptyAttachmentPath),
                "{path:?} must be rejected"
            );
        }
    }
}
