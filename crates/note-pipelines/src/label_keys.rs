use crate::context::Context;
use note_core::{LabelKey, LabelKeyValidationError};

pub async fn define_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    // Preserve the typed error in the anyhow chain (rather than a bail! string) so the REST handler
    // can downcast to distinguish these caller-fault cases (400) from storage failures (500).
    if key.trim().is_empty() {
        return Err(anyhow::Error::new(LabelKeyValidationError::EmptyKey));
    }
    if description.trim().is_empty() {
        return Err(anyhow::Error::new(
            LabelKeyValidationError::EmptyDescription,
        ));
    }
    let conn = ctx.storage.connect()?;
    note_storage::insert_label_key(&conn, key, description).await
}

pub async fn list_label_keys(ctx: &Context) -> anyhow::Result<Vec<LabelKey>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_label_keys(&conn).await
}

pub async fn update_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    if description.trim().is_empty() {
        anyhow::bail!("description must not be empty");
    }
    let conn = ctx.storage.connect()?;
    note_storage::update_label_key(&conn, key, description).await
}

pub async fn delete_label_key(ctx: &Context, key: &str) -> anyhow::Result<()> {
    let conn = ctx.storage.connect()?;
    note_storage::delete_label_key(&conn, key).await
}
