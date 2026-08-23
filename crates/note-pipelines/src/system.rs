use crate::Context;
use note_core::{
    parse_category_label_expression, validate_system_config, CategoryLabelConfigError, SystemConfig,
};
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

#[derive(Debug)]
enum CategorySelection {
    All,
    Exact(Vec<String>),
}

#[derive(Debug)]
struct CategoryRequest {
    key: String,
    selection: CategorySelection,
}

fn category_requests(category_labels: &[String]) -> Vec<CategoryRequest> {
    let mut requests: Vec<CategoryRequest> = Vec::new();
    let mut request_indices: HashMap<String, usize> = HashMap::new();

    for entry in category_labels {
        let expression = parse_category_label_expression(entry);
        match request_indices.get(expression.key).copied() {
            Some(index) => match (&mut requests[index].selection, expression.value) {
                (CategorySelection::Exact(values), Some(value)) => values.push(value.to_string()),
                _ => unreachable!("category label configuration was validated"),
            },
            None => {
                let selection = match expression.value {
                    Some(value) => CategorySelection::Exact(vec![value.to_string()]),
                    None => CategorySelection::All,
                };
                request_indices.insert(expression.key.to_string(), requests.len());
                requests.push(CategoryRequest {
                    key: expression.key.to_string(),
                    selection,
                });
            }
        }
    }

    requests
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
    let requests = category_requests(&config.category_labels);
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let catalog: HashSet<String> = transaction
            .list_label_keys()
            .await?
            .into_iter()
            .map(|label_key| label_key.key)
            .collect();
        for request in &requests {
            if !catalog.contains(&request.key) {
                return Err(anyhow::Error::new(CategoryLabelConfigError::UnknownKey {
                    key: request.key.clone(),
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
        let requests = category_requests(&config.category_labels);

        let mut descriptions: HashMap<String, String> = transaction
            .list_label_keys()
            .await?
            .into_iter()
            .map(|label_key| (label_key.key, label_key.description))
            .collect();
        for request in &requests {
            if !descriptions.contains_key(&request.key) {
                return Err(anyhow::Error::new(CategoryLabelConfigError::UnknownKey {
                    key: request.key.clone(),
                }));
            }
        }

        let mut values_by_key: HashMap<String, Vec<CategoryLabelValueSummary>> = HashMap::new();
        for count in transaction
            .label_value_counts(
                &requests
                    .iter()
                    .map(|request| request.key.clone())
                    .collect::<Vec<_>>(),
            )
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

        Ok(requests
            .into_iter()
            .map(|request| {
                let values = match request.selection {
                    CategorySelection::All => {
                        values_by_key.remove(&request.key).unwrap_or_default()
                    }
                    CategorySelection::Exact(configured_values) => {
                        let counts: HashMap<String, usize> = values_by_key
                            .remove(&request.key)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|summary| (summary.value, summary.count))
                            .collect();
                        configured_values
                            .into_iter()
                            .map(|value| CategoryLabelValueSummary {
                                count: counts.get(&value).copied().unwrap_or(0),
                                value,
                            })
                            .collect()
                    }
                };

                CategoryLabelSummary {
                    description: descriptions
                        .remove(&request.key)
                        .expect("category key validated"),
                    values,
                    key: request.key,
                }
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
