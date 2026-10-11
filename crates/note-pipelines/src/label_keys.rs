use crate::context::Context;
use note_core::{
    parse_category_label_expression, validate_label_key, CategoryLabelConfigError, LabelKey,
    LabelKeyValidationError, LabelValueType,
};
use note_storage::TransactionMode;
use std::str::FromStr;

pub async fn define_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    define_label_key_with_type(ctx, key, description, LabelValueType::Text).await
}

pub async fn define_label_key_with_type(
    ctx: &Context,
    key: &str,
    description: &str,
    value_type: LabelValueType,
) -> anyhow::Result<()> {
    // Preserve the typed error in the anyhow chain (rather than a bail! string) so the REST handler
    // can downcast to distinguish these caller-fault cases (400) from storage failures (500).
    validate_label_key(key).map_err(anyhow::Error::new)?;
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        transaction
            .insert_label_key_with_type(key, description, value_type)
            .await?;
        crate::sync::capture_catalog(ctx, transaction.as_ref(), key).await
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

pub async fn list_label_keys(ctx: &Context) -> anyhow::Result<Vec<LabelKey>> {
    let session = ctx.storage().session().await?;
    Ok(session.list_label_keys().await?)
}

pub async fn label_note_counts(ctx: &Context) -> anyhow::Result<Vec<(String, usize)>> {
    let session = ctx.storage().session().await?;
    Ok(session.label_note_counts().await?)
}

pub async fn update_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        transaction.update_label_key(key, description).await?;
        crate::sync::capture_catalog(ctx, transaction.as_ref(), key).await
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

pub async fn update_label_key_with_type(
    ctx: &Context,
    key: &str,
    description: &str,
    value_type: LabelValueType,
) -> anyhow::Result<()> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        transaction
            .update_label_key_with_type(key, description, value_type)
            .await?;
        crate::sync::capture_catalog(ctx, transaction.as_ref(), key).await
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}

pub fn parse_label_value_type(input: &str) -> anyhow::Result<LabelValueType> {
    LabelValueType::from_str(input)
        .map_err(|_| anyhow::Error::new(LabelKeyValidationError::InvalidValueType(input.into())))
}

pub async fn delete_label_key(ctx: &Context, key: &str) -> anyhow::Result<()> {
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let result = async {
        let config = transaction.get_system_config().await?;
        if config
            .category_labels
            .iter()
            .any(|entry| parse_category_label_expression(entry).key == key)
        {
            return Err(anyhow::Error::new(
                CategoryLabelConfigError::ConfiguredKeyCannotBeDeleted {
                    key: key.to_string(),
                },
            ));
        }
        let affected = crate::sync::catalog_note_ids(transaction.as_ref(), key).await?;
        transaction.delete_label_key(key).await?;
        for id in affected {
            crate::sync::capture_note(ctx, transaction.as_ref(), &id, false).await?;
        }
        anyhow::Ok(())
    }
    .await;
    crate::save_note::finish_transaction(transaction, result).await
}
