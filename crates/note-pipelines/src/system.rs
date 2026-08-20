use crate::Context;
use note_core::{validate_system_config, CategoryLabelConfigError, SystemConfig};
use note_storage::TransactionMode;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryLabelValueSummary {
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryLabelSummary {
    pub key: String,
    pub description: String,
    pub values: Vec<CategoryLabelValueSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    pub attachments_location: Option<String>,
    pub embedding_engine: String,
    pub embedding_model: String,
    pub embedding_fingerprint: String,
}

pub async fn get_system_config(ctx: &Context) -> anyhow::Result<SystemConfig> {
    let session = ctx.storage().session().await?;
    Ok(session.get_system_config().await?)
}

pub async fn update_system_config(ctx: &Context, config: &SystemConfig) -> anyhow::Result<()> {
    validate_system_config(config).map_err(anyhow::Error::new)?;
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let catalog: HashSet<String> = transaction
            .list_label_keys()
            .await?
            .into_iter()
            .map(|label_key| label_key.key)
            .collect();
        for key in &config.category_labels {
            if !catalog.contains(key) {
                return Err(anyhow::Error::new(CategoryLabelConfigError::UnknownKey {
                    key: key.clone(),
                }));
            }
        }
        transaction.set_system_config(config).await?;
        anyhow::Ok(())
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

pub async fn category_label_summaries(ctx: &Context) -> anyhow::Result<Vec<CategoryLabelSummary>> {
    let transaction = ctx.storage().begin(TransactionMode::Snapshot).await?;
    let result = async {
        let config = transaction.get_system_config().await?;
        validate_system_config(&config).map_err(anyhow::Error::new)?;
        if config.category_labels.is_empty() {
            return Ok(Vec::new());
        }

        let mut descriptions: HashMap<String, String> = transaction
            .list_label_keys()
            .await?
            .into_iter()
            .map(|label_key| (label_key.key, label_key.description))
            .collect();
        for key in &config.category_labels {
            if !descriptions.contains_key(key) {
                return Err(anyhow::Error::new(CategoryLabelConfigError::UnknownKey {
                    key: key.clone(),
                }));
            }
        }

        let mut values_by_key: HashMap<String, Vec<CategoryLabelValueSummary>> = HashMap::new();
        for count in transaction
            .label_value_counts(&config.category_labels)
            .await?
        {
            values_by_key
                .entry(count.key)
                .or_default()
                .push(CategoryLabelValueSummary {
                    value: count.value,
                    count: count.count,
                });
        }

        Ok(config
            .category_labels
            .into_iter()
            .map(|key| CategoryLabelSummary {
                description: descriptions.remove(&key).expect("category key validated"),
                values: values_by_key.remove(&key).unwrap_or_default(),
                key,
            })
            .collect())
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

pub async fn get_system_info(ctx: &Context) -> anyhow::Result<SystemInfo> {
    let info = ctx.storage().info().await?;
    let attachment_info = ctx.attachments().info();
    let embedding_info = ctx.embedding_info();
    Ok(SystemInfo {
        database_engine: info.engine,
        database_path: info
            .location
            .map(|path| path.to_string_lossy().into_owned()),
        database_size_bytes: info.size_bytes,
        attachments_engine: attachment_info.engine,
        attachments_location: attachment_info.location,
        embedding_engine: embedding_info.engine.clone(),
        embedding_model: embedding_info.model.clone(),
        embedding_fingerprint: embedding_info.fingerprint.clone(),
    })
}
