use crate::Context;
use chrono::Utc;
use note_core::{
    parse_label_selectors, validate_label_key, validate_label_value, LabelValueType,
    ValidationError,
};
use note_storage::TransactionMode;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateNoteLabelsInput {
    pub selector: String,
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkUpdateNoteLabelsResult {
    pub matched: usize,
    pub updated: usize,
    pub unchanged: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BulkUpdateNoteLabelsValidationError {
    EmptySelector,
    MalformedSelector,
    EmptyMutations,
    DuplicateKey(String),
    DuplicateRemoveKey(String),
    ConflictingKey(String),
}

impl std::fmt::Display for BulkUpdateNoteLabelsValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySelector => formatter.write_str("selector must not be empty"),
            Self::MalformedSelector => formatter.write_str("label selector is malformed"),
            Self::EmptyMutations => {
                formatter.write_str("at least one label set or remove mutation is required")
            }
            Self::DuplicateKey(key) => {
                write!(formatter, "duplicate label assignment key: {key}")
            }
            Self::DuplicateRemoveKey(key) => {
                write!(formatter, "duplicate label removal key: {key}")
            }
            Self::ConflictingKey(key) => {
                write!(formatter, "label key cannot be both set and removed: {key}")
            }
        }
    }
}

impl std::error::Error for BulkUpdateNoteLabelsValidationError {}

pub async fn bulk_update_note_labels(
    ctx: &Context,
    input: BulkUpdateNoteLabelsInput,
) -> anyhow::Result<BulkUpdateNoteLabelsResult> {
    let selector = input.selector.trim();
    if selector.is_empty() {
        return Err(anyhow::Error::new(
            BulkUpdateNoteLabelsValidationError::EmptySelector,
        ));
    }
    if selector.split('&').any(|term| term.trim().is_empty()) {
        return Err(anyhow::Error::new(
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
        ));
    }
    let selectors = parse_label_selectors(selector);
    if selectors.is_empty() {
        return Err(anyhow::Error::new(
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
        ));
    }
    for selector in &selectors {
        validate_label_key(&selector.key).map_err(anyhow::Error::new)?;
    }
    if input.set.is_empty() && input.remove.is_empty() {
        return Err(anyhow::Error::new(
            BulkUpdateNoteLabelsValidationError::EmptyMutations,
        ));
    }
    for (key, _) in &input.set {
        validate_label_key(key).map_err(anyhow::Error::new)?;
    }
    for key in &input.remove {
        validate_label_key(key).map_err(anyhow::Error::new)?;
    }
    let mut set_keys = HashSet::new();
    for (key, _) in &input.set {
        if !set_keys.insert(key.as_str()) {
            return Err(anyhow::Error::new(
                BulkUpdateNoteLabelsValidationError::DuplicateKey(key.clone()),
            ));
        }
    }
    let mut remove_keys = HashSet::new();
    for key in &input.remove {
        if !remove_keys.insert(key.as_str()) {
            return Err(anyhow::Error::new(
                BulkUpdateNoteLabelsValidationError::DuplicateRemoveKey(key.clone()),
            ));
        }
    }
    for key in &input.remove {
        if set_keys.contains(key.as_str()) {
            return Err(anyhow::Error::new(
                BulkUpdateNoteLabelsValidationError::ConflictingKey(key.clone()),
            ));
        }
    }

    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let note_ids = match transaction.matching_note_ids_for_update(&selectors).await {
        Ok(note_ids) => note_ids,
        Err(error) => {
            return crate::save_note::finish_transaction(transaction, Err(error.into())).await;
        }
    };
    if note_ids.is_empty() {
        transaction.rollback().await?;
        return Ok(BulkUpdateNoteLabelsResult {
            matched: 0,
            updated: 0,
            unchanged: 0,
        });
    }

    let transaction_result = async {
        let matched = note_ids.len();
        if !input.set.is_empty() {
            let existing_keys: HashSet<String> = transaction
                .list_label_keys()
                .await?
                .into_iter()
                .map(|label_key| label_key.key)
                .collect();
            for (key, _) in &input.set {
                if !existing_keys.contains(key) {
                    transaction.insert_label_key_if_missing(key, "").await?;
                }
            }
            let final_types: HashMap<String, LabelValueType> = transaction
                .list_label_keys()
                .await?
                .into_iter()
                .map(|label_key| (label_key.key, label_key.value_type))
                .collect();
            for (key, value) in &input.set {
                let value_type = final_types
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
        }
        let now = Utc::now().timestamp();
        let mut updated = 0;

        for note_id in note_ids {
            let mut changed = false;
            for key in &input.remove {
                changed |= transaction.remove_note_label(&note_id, key).await?;
            }
            for (key, value) in &input.set {
                changed |= transaction.set_note_label(&note_id, key, value).await?;
            }
            if changed {
                transaction.advance_note_updated_at(&note_id, now).await?;
                updated += 1;
            }
        }

        anyhow::Ok(BulkUpdateNoteLabelsResult {
            matched,
            updated,
            unchanged: matched - updated,
        })
    }
    .await;

    let result = crate::save_note::finish_transaction(transaction, transaction_result).await?;
    if result.updated > 0 {
        ctx.notify_note_mutated();
    }
    Ok(result)
}
