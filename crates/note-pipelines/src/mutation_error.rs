use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteMutationError {
    NotFound(String),
    StaleRevision {
        note_id: String,
        expected_revision: i64,
        current_revision: i64,
    },
    StaleContentTag {
        note_id: String,
        expected_tag: String,
        current_tag: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum NoteMutationErrorDetails<'a> {
    NotFound {
        note_id: &'a str,
    },
    StaleRevision {
        note_id: &'a str,
        expected_revision: i64,
        current_revision: i64,
    },
    StaleContentTag {
        note_id: &'a str,
        expected_tag: &'a str,
        current_tag: &'a str,
    },
}

impl NoteMutationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::StaleRevision { .. } => "stale_revision",
            Self::StaleContentTag { .. } => "stale_content_tag",
        }
    }

    pub fn retryable(&self) -> bool {
        false
    }

    pub fn details(&self) -> NoteMutationErrorDetails<'_> {
        match self {
            Self::NotFound(note_id) => NoteMutationErrorDetails::NotFound { note_id },
            Self::StaleRevision {
                note_id,
                expected_revision,
                current_revision,
            } => NoteMutationErrorDetails::StaleRevision {
                note_id,
                expected_revision: *expected_revision,
                current_revision: *current_revision,
            },
            Self::StaleContentTag {
                note_id,
                expected_tag,
                current_tag,
            } => NoteMutationErrorDetails::StaleContentTag {
                note_id,
                expected_tag,
                current_tag,
            },
        }
    }
}

impl std::fmt::Display for NoteMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(note_id) => write!(f, "note not found: {note_id}"),
            Self::StaleRevision {
                note_id,
                expected_revision,
                current_revision,
            } => write!(
                f,
                "stale revision for note {note_id}: expected {expected_revision}, current {current_revision}"
            ),
            Self::StaleContentTag {
                note_id,
                current_tag,
                ..
            } => write!(
                f,
                "stale content tag for note {note_id}: current tag {current_tag}"
            ),
        }
    }
}

impl std::error::Error for NoteMutationError {}

pub(crate) fn mutation_revision(
    note_id: &str,
    result: note_storage::NoteMutationResult<()>,
) -> anyhow::Result<i64> {
    match result {
        note_storage::NoteMutationResult::Applied { revision, .. } => Ok(revision),
        note_storage::NoteMutationResult::NotFound => {
            Err(NoteMutationError::NotFound(note_id.into()).into())
        }
        note_storage::NoteMutationResult::Conflict {
            expected_revision,
            current_revision,
        } => Err(NoteMutationError::StaleRevision {
            note_id: note_id.into(),
            expected_revision,
            current_revision,
        }
        .into()),
    }
}
