use crate::context::Context;
use note_core::{LabelKey, LabelKeyValidationError, LabelValueType};
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
    if key.trim().is_empty() {
        return Err(anyhow::Error::new(LabelKeyValidationError::EmptyKey));
    }
    let conn = ctx.storage.connect()?;
    note_storage::insert_label_key_with_type(&conn, key, description, value_type).await
}

pub async fn list_label_keys(ctx: &Context) -> anyhow::Result<Vec<LabelKey>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_label_keys(&conn).await
}

pub async fn label_note_counts(ctx: &Context) -> anyhow::Result<Vec<(String, usize)>> {
    let conn = ctx.storage.connect()?;
    note_storage::label_note_counts(&conn).await
}

pub async fn update_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    let conn = ctx.storage.connect()?;
    note_storage::update_label_key(&conn, key, description).await
}

pub async fn update_label_key_with_type(
    ctx: &Context,
    key: &str,
    description: &str,
    value_type: LabelValueType,
) -> anyhow::Result<()> {
    let conn = ctx.storage.connect()?;
    note_storage::update_label_key_with_type(&conn, key, description, value_type).await
}

pub fn parse_label_value_type(input: &str) -> anyhow::Result<LabelValueType> {
    LabelValueType::from_str(input)
        .map_err(|_| anyhow::Error::new(LabelKeyValidationError::InvalidValueType(input.into())))
}

pub async fn delete_label_key(ctx: &Context, key: &str) -> anyhow::Result<()> {
    let conn = ctx.storage.connect()?;
    note_storage::delete_label_key(&conn, key).await
}
