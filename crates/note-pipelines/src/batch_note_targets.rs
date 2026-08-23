use crate::NoteMutationError;
use note_storage::StorageTransaction;
use std::collections::HashMap;

pub const MAX_BATCH_NOTE_TARGETS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchNoteTarget {
    pub id: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchNoteTargetValidationError {
    Empty,
    BlankId,
    DuplicateId(String),
    TooMany { maximum: usize },
}

impl std::fmt::Display for BatchNoteTargetValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("at least one note target is required"),
            Self::BlankId => formatter.write_str("note target id must not be blank"),
            Self::DuplicateId(id) => write!(formatter, "duplicate note target id: {id}"),
            Self::TooMany { .. } => {
                formatter.write_str("note target count exceeds maximum of 1000")
            }
        }
    }
}

impl std::error::Error for BatchNoteTargetValidationError {}

pub(crate) fn validate_batch_note_targets(
    mut targets: Vec<BatchNoteTarget>,
) -> anyhow::Result<Vec<BatchNoteTarget>> {
    if targets.is_empty() {
        return Err(BatchNoteTargetValidationError::Empty.into());
    }
    if targets.len() > MAX_BATCH_NOTE_TARGETS {
        return Err(BatchNoteTargetValidationError::TooMany {
            maximum: MAX_BATCH_NOTE_TARGETS,
        }
        .into());
    }

    targets.sort_by(|left, right| left.id.cmp(&right.id));
    for target in &targets {
        if target.id.trim().is_empty() {
            return Err(BatchNoteTargetValidationError::BlankId.into());
        }
    }
    for pair in targets.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(BatchNoteTargetValidationError::DuplicateId(pair[0].id.clone()).into());
        }
    }
    Ok(targets)
}

pub(crate) async fn preflight_batch_note_targets(
    transaction: &dyn StorageTransaction,
    targets: &[BatchNoteTarget],
) -> anyhow::Result<()> {
    let ids: Vec<String> = targets.iter().map(|target| target.id.clone()).collect();
    let revisions: HashMap<String, i64> = transaction
        .active_note_revisions_for_update(&ids)
        .await?
        .into_iter()
        .collect();

    for target in targets {
        let Some(current_revision) = revisions.get(&target.id) else {
            return Err(NoteMutationError::NotFound(target.id.clone()).into());
        };
        if *current_revision != target.expected_revision {
            return Err(NoteMutationError::StaleRevision {
                note_id: target.id.clone(),
                expected_revision: target.expected_revision,
                current_revision: *current_revision,
            }
            .into());
        }
    }
    Ok(())
}
