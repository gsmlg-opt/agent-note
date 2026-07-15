use crate::Context;
use note_core::{validate_system_config, SystemConfig};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemInfo {
    pub database_path: String,
    pub database_size_bytes: u64,
    pub attachments_path: String,
}

pub async fn get_system_config(ctx: &Context) -> anyhow::Result<SystemConfig> {
    let conn = ctx.storage.connect()?;
    note_storage::get_system_config(&conn).await
}

pub async fn update_system_config(ctx: &Context, config: &SystemConfig) -> anyhow::Result<()> {
    validate_system_config(config).map_err(anyhow::Error::new)?;
    let conn = ctx.storage.connect()?;
    note_storage::set_system_config(&conn, config).await
}

pub async fn get_system_info(ctx: &Context) -> anyhow::Result<SystemInfo> {
    Ok(SystemInfo {
        database_path: ctx.storage.path().to_string_lossy().into_owned(),
        database_size_bytes: ctx.storage.size_bytes().await?,
        attachments_path: absolute_path(ctx.attachments_dir())
            .to_string_lossy()
            .into_owned(),
    })
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
