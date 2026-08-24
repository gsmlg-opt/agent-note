use crate::{
    batch_note_targets::{preflight_batch_note_targets, validate_batch_note_targets},
    Context,
};
use note_core::{validate_label_key, validate_label_value, LabelValueType, ValidationError};
use note_storage::{StorageTransaction, TransactionMode};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchLabelAction {
    Add {
        key: String,
        value: String,
    },
    Update {
        from_key: String,
        key: String,
        value: String,
    },
    Remove {
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchUpdateNoteLabelsInput {
    pub notes: Vec<crate::BatchNoteTarget>,
    pub action: BatchLabelAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchUpdateNoteLabelsResult {
    pub requested: usize,
    pub updated: usize,
    pub unchanged: usize,
}

pub async fn batch_update_note_labels(
    ctx: &Context,
    input: BatchUpdateNoteLabelsInput,
) -> anyhow::Result<BatchUpdateNoteLabelsResult> {
    let targets = validate_batch_note_targets(input.notes)?;
    let action = input.action;
    match &action {
        BatchLabelAction::Add { key, .. } | BatchLabelAction::Remove { key } => {
            validate_label_key(key).map_err(anyhow::Error::new)?;
        }
        BatchLabelAction::Update { from_key, key, .. } => {
            validate_label_key(from_key).map_err(anyhow::Error::new)?;
            validate_label_key(key).map_err(anyhow::Error::new)?;
        }
    }

    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let requested = targets.len();
    let transaction_result = async {
        preflight_batch_note_targets(transaction.as_ref(), &targets).await?;

        let mut eligible_ids = Vec::new();
        for target in &targets {
            let labels = transaction.labels_for_note(&target.id).await?;
            let eligible = match &action {
                BatchLabelAction::Add { key, .. } => !labels.iter().any(|label| label.key == *key),
                BatchLabelAction::Update { from_key, .. } => {
                    labels.iter().any(|label| label.key == *from_key)
                }
                BatchLabelAction::Remove { key } => labels.iter().any(|label| label.key == *key),
            };
            if eligible {
                eligible_ids.push(target.id.as_str());
            }
        }

        match &action {
            BatchLabelAction::Add { key, value } | BatchLabelAction::Update { key, value, .. } => {
                validate_destination_label(
                    transaction.as_ref(),
                    key,
                    value,
                    !eligible_ids.is_empty(),
                )
                .await?;
            }
            BatchLabelAction::Remove { .. } => {}
        }

        if eligible_ids.is_empty() {
            return Ok(BatchUpdateNoteLabelsResult {
                requested,
                updated: 0,
                unchanged: requested,
            });
        }

        let now = chrono::Utc::now().timestamp();
        let mut updated = 0;
        match &action {
            BatchLabelAction::Add { key, value } => {
                for note_id in eligible_ids {
                    if transaction.set_note_label(note_id, key, value).await? {
                        transaction.advance_note_updated_at(note_id, now).await?;
                        updated += 1;
                    }
                }
            }
            BatchLabelAction::Update {
                from_key,
                key,
                value,
            } => {
                for note_id in eligible_ids {
                    let changed = if from_key == key {
                        transaction.set_note_label(note_id, key, value).await?
                    } else {
                        transaction.remove_note_label(note_id, from_key).await?;
                        transaction.set_note_label(note_id, key, value).await?;
                        true
                    };
                    if changed {
                        transaction.advance_note_updated_at(note_id, now).await?;
                        updated += 1;
                    }
                }
            }
            BatchLabelAction::Remove { key } => {
                for note_id in eligible_ids {
                    if transaction.remove_note_label(note_id, key).await? {
                        transaction.advance_note_updated_at(note_id, now).await?;
                        updated += 1;
                    }
                }
            }
        }
        Ok(BatchUpdateNoteLabelsResult {
            requested,
            updated,
            unchanged: requested - updated,
        })
    }
    .await;

    let result = match transaction_result {
        Ok(result) if result.updated == 0 => {
            transaction.rollback().await?;
            result
        }
        result => crate::save_note::finish_transaction(transaction, result).await?,
    };
    if result.updated > 0 {
        ctx.notify_note_mutated();
    }
    Ok(result)
}

async fn validate_destination_label(
    transaction: &dyn StorageTransaction,
    key: &str,
    value: &str,
    create_if_missing: bool,
) -> anyhow::Result<()> {
    let existing_keys: HashMap<String, LabelValueType> = transaction
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label_key| (label_key.key, label_key.value_type))
        .collect();
    if !existing_keys.contains_key(key) && create_if_missing {
        transaction.insert_label_key_if_missing(key, "").await?;
    }
    let value_type = if create_if_missing {
        transaction
            .list_label_keys()
            .await?
            .into_iter()
            .find(|label_key| label_key.key == key)
            .map(|label_key| label_key.value_type)
            .unwrap_or(LabelValueType::Text)
    } else {
        existing_keys
            .get(key)
            .copied()
            .unwrap_or(LabelValueType::Text)
    };
    if !validate_label_value(value_type, value) {
        return Err(anyhow::Error::new(ValidationError::InvalidLabelValue {
            key: key.into(),
            value: value.into(),
            value_type,
        }));
    }
    Ok(())
}
