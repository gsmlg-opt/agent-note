pub mod api;
pub mod phoenix;
mod runtime;
pub mod tls;
mod transfer;
pub use runtime::{connect_socket, run, SyncRuntime, SyncRuntimeStatus};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

pub const DEFAULT_MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;
fn default_max_blob_bytes() -> u64 {
    DEFAULT_MAX_BLOB_BYTES
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConnection {
    pub id: String,
    pub url: String,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    pub ca_cert: Option<PathBuf>,
    #[serde(default = "default_max_blob_bytes")]
    pub max_blob_bytes: u64,
}
impl std::fmt::Debug for SyncConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncConnection")
            .field("id", &self.id)
            .field("url", &self.url)
            .field("credentials", &"<redacted>")
            .field("max_blob_bytes", &self.max_blob_bytes)
            .finish()
    }
}
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncProfile {
    pub id: String,
    pub url: String,
}
impl SyncConnection {
    pub fn validate(&self) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| anyhow::anyhow!("sync connection URL is invalid"))?;
        if self.id.trim().is_empty()
            || self.id.len() > 128
            || url.scheme() != "wss"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.query_pairs().any(|(k, v)| k == "vsn" && v == "2.0.0")
            || self.max_blob_bytes == 0
            || self.max_blob_bytes > DEFAULT_MAX_BLOB_BYTES
        {
            anyhow::bail!("sync connection requires unique ID, wss URL with vsn=2.0.0 and a 1..512MiB blob limit");
        }
        Ok(())
    }
    pub fn public_profile(&self) -> SyncProfile {
        SyncProfile {
            id: self.id.clone(),
            url: self.url.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
