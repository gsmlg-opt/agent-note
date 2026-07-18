use crate::Context;
use note_core::{validate_system_config, SystemConfig};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    pub attachments_location: Option<String>,
}

pub async fn get_system_config(ctx: &Context) -> anyhow::Result<SystemConfig> {
    let session = ctx.storage().session().await?;
    Ok(session.get_system_config().await?)
}

pub async fn update_system_config(ctx: &Context, config: &SystemConfig) -> anyhow::Result<()> {
    validate_system_config(config).map_err(anyhow::Error::new)?;
    let session = ctx.storage().session().await?;
    Ok(session.set_system_config(config).await?)
}

pub async fn get_system_info(ctx: &Context) -> anyhow::Result<SystemInfo> {
    let info = ctx.storage().info().await?;
    let attachment_info = ctx.attachments().info();
    Ok(SystemInfo {
        database_engine: info.engine,
        database_path: info
            .location
            .map(|path| path.to_string_lossy().into_owned()),
        database_size_bytes: info.size_bytes,
        attachments_engine: attachment_info.engine,
        attachments_location: attachment_info.location,
    })
}
