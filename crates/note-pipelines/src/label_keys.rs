use crate::context::Context;
use note_core::LabelKey;

pub async fn define_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("label key must not be empty");
    }
    if description.trim().is_empty() {
        anyhow::bail!("label key description must not be empty");
    }
    let conn = ctx.storage.connect()?;
    note_storage::insert_label_key(&conn, key, description).await
}

pub async fn list_label_keys(ctx: &Context) -> anyhow::Result<Vec<LabelKey>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_label_keys(&conn).await
}
