use crate::Context;
use note_core::{validate_system_config, SystemConfig};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_path: String,
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
    let attachments_path = match attachment_info.location {
        Some(location) if attachment_info.engine == "filesystem" => {
            absolute_path(Path::new(&location))
                .to_string_lossy()
                .into_owned()
        }
        Some(location) => location,
        None => attachment_info.engine,
    };
    Ok(SystemInfo {
        database_engine: info.engine,
        database_path: info
            .location
            .map(|path| path.to_string_lossy().into_owned()),
        database_size_bytes: info.size_bytes,
        attachments_path,
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
