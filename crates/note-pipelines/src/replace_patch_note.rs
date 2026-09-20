use crate::{
    context::Context, update_note::commit_note_update, SaveNoteInput, UpdateNoteFieldsInput,
};
use note_core::{
    apply_content_patch, normalize_attachment_path, validate_label_key, validate_label_value,
    validate_note_input, ContentUpdate, LabelValueType, NoteAttachment, NoteInput, ValidationError,
};
use note_storage::TransactionMode;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct PatchNoteInput {
    pub title: Option<String>,
    pub content: Option<ContentUpdate>,
    pub attachments: Option<Vec<NoteAttachment>>,
    pub labels: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteMutation {
    pub id: String,
    pub revision: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacePatchError {
    InvalidRequest(String),
    PatchFailed(String),
}

impl std::fmt::Display for ReplacePatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "invalid request: {message}"),
            Self::PatchFailed(message) => write!(f, "content patch failed: {message}"),
        }
    }
}

impl std::error::Error for ReplacePatchError {}

pub async fn replace_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    input: SaveNoteInput,
) -> anyhow::Result<NoteMutation> {
    patch_note(
        ctx,
        id,
        expected_revision,
        PatchNoteInput {
            title: Some(input.title),
            content: Some(ContentUpdate::Replace(input.content)),
            attachments: Some(input.attachments),
            labels: Some(input.labels),
        },
    )
    .await
}

pub async fn patch_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    input: PatchNoteInput,
) -> anyhow::Result<NoteMutation> {
    validate_request(expected_revision, &input)?;

    let session = ctx.storage().session().await?;
    let mut existing = session
        .get_note(id)
        .await?
        .ok_or_else(|| crate::NoteMutationError::NotFound(id.to_string()))?;
    ensure_revision(id, expected_revision, existing.revision)?;

    let label_keys = session.list_label_keys().await?;
    drop(session);

    if input.attachments.is_some() {
        crate::hydrate_note_attachments(ctx, &mut existing).await?;
    }

    let desired_content = match input.content {
        None => existing.content.clone(),
        Some(ContentUpdate::Replace(content)) => content,
        Some(ContentUpdate::ApplyPatch(patch)) => {
            apply_content_patch(&existing.content, &patch.apply_patch).map_err(|error| {
                anyhow::Error::new(ReplacePatchError::PatchFailed(error.to_string()))
            })?
        }
    };
    let existing_labels = existing
        .labels
        .iter()
        .map(|label| (label.key.clone(), label.value.clone()))
        .collect::<Vec<_>>();
    let desired = SaveNoteInput {
        title: input.title.unwrap_or_else(|| existing.title.clone()),
        content: desired_content,
        attachments: input
            .attachments
            .clone()
            .unwrap_or_else(|| existing.attachments.clone()),
        labels: input.labels.unwrap_or_else(|| existing_labels.clone()),
    };

    validate_desired(&desired, &label_keys)?;

    let attachments_changed = input.attachments.as_ref().is_some_and(|attachments| {
        !attachments_logically_equal(attachments, &existing.attachments)
    });
    let changed = desired.title != existing.title
        || desired.content != existing.content
        || !labels_equal(&desired.labels, &existing_labels)
        || attachments_changed;

    if !changed && confirm_noop_state(ctx, id, expected_revision, &desired).await? {
        return Ok(NoteMutation {
            id: id.to_string(),
            revision: expected_revision,
            changed: false,
        });
    }

    let attachments = attachments_changed.then_some(desired.attachments);
    let committed = commit_note_update(
        ctx,
        id,
        UpdateNoteFieldsInput {
            expected_revision,
            title: desired.title,
            content: desired.content,
            labels: desired.labels,
        },
        attachments,
    )
    .await?;

    ctx.notify_note_mutated();
    Ok(NoteMutation {
        id: committed.id,
        revision: committed.revision,
        changed: true,
    })
}

fn validate_request(expected_revision: i64, input: &PatchNoteInput) -> anyhow::Result<()> {
    if expected_revision < 1 {
        return Err(anyhow::Error::new(ReplacePatchError::InvalidRequest(
            "expected_revision must be positive".into(),
        )));
    }
    if input.title.is_none()
        && input.content.is_none()
        && input.attachments.is_none()
        && input.labels.is_none()
    {
        return Err(anyhow::Error::new(ReplacePatchError::InvalidRequest(
            "patch must provide at least one field".into(),
        )));
    }
    Ok(())
}

fn validate_desired(
    input: &SaveNoteInput,
    label_keys: &[note_core::LabelKey],
) -> anyhow::Result<()> {
    let existing_keys = label_keys
        .iter()
        .map(|label_key| label_key.key.clone())
        .collect::<Vec<_>>();
    let label_value_types = label_keys
        .iter()
        .map(|label_key| (label_key.key.clone(), label_key.value_type))
        .collect::<HashMap<_, _>>();
    let mut known_for_validation = existing_keys.clone();
    for (key, _) in &input.labels {
        if !known_for_validation.iter().any(|known| known == key) {
            known_for_validation.push(key.clone());
        }
    }

    validate_note_input(
        &NoteInput {
            title: input.title.clone(),
            content: input.content.clone(),
            attachments: input.attachments.clone(),
            labels: input.labels.clone(),
        },
        &known_for_validation,
    )
    .map_err(anyhow::Error::new)?;

    for (key, value) in &input.labels {
        if !existing_keys.iter().any(|existing| existing == key) {
            validate_label_key(key).map_err(anyhow::Error::new)?;
        }
        let value_type = label_value_types
            .get(key)
            .copied()
            .unwrap_or(LabelValueType::Text);
        if !validate_label_value(value_type, value) {
            return Err(anyhow::Error::new(ValidationError::InvalidLabelValue {
                key: key.clone(),
                value: value.clone(),
                value_type,
            }));
        }
    }
    Ok(())
}

fn ensure_revision(id: &str, expected_revision: i64, current_revision: i64) -> anyhow::Result<()> {
    if expected_revision == current_revision {
        Ok(())
    } else {
        Err(crate::NoteMutationError::StaleRevision {
            note_id: id.to_string(),
            expected_revision,
            current_revision,
        }
        .into())
    }
}

async fn confirm_noop_state(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    desired: &SaveNoteInput,
) -> anyhow::Result<bool> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let current = transaction
            .get_note(id)
            .await?
            .ok_or_else(|| crate::NoteMutationError::NotFound(id.to_string()))?;
        ensure_revision(id, expected_revision, current.revision)?;
        let label_keys = transaction.list_label_keys().await?;
        validate_desired(desired, &label_keys)?;
        let current_labels = current
            .labels
            .iter()
            .map(|label| (label.key.clone(), label.value.clone()))
            .collect::<Vec<_>>();
        Ok(current.title == desired.title
            && current.content == desired.content
            && labels_equal(&current_labels, &desired.labels))
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

fn labels_equal(left: &[(String, String)], right: &[(String, String)]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

fn attachments_logically_equal(left: &[NoteAttachment], right: &[NoteAttachment]) -> bool {
    fn logical(attachments: &[NoteAttachment]) -> Vec<(String, String, String, String, Vec<u8>)> {
        let mut values = attachments
            .iter()
            .map(|attachment| {
                (
                    attachment.id.clone(),
                    normalize_attachment_path(&attachment.path),
                    attachment.mime.clone(),
                    attachment.description.clone(),
                    attachment.content.clone(),
                )
            })
            .collect::<Vec<_>>();
        values.sort();
        values
    }

    logical(left) == logical(right)
}
