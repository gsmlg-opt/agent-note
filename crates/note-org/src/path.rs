use thiserror::Error;

const DOCUMENT_PATH_ERROR_MESSAGE: &str =
    "Org document path must be a portable relative lowercase .org path";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{DOCUMENT_PATH_ERROR_MESSAGE}")]
pub struct DocumentPathError;

pub fn validate_document_path(value: &str) -> Result<(), DocumentPathError> {
    if value.is_empty()
        || value != value.trim()
        || value.starts_with('/')
        || value.contains('\\')
        || has_windows_drive_prefix(value)
        || !value.ends_with(".org")
    {
        return Err(DocumentPathError);
    }

    if value
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(DocumentPathError);
    }

    Ok(())
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}
