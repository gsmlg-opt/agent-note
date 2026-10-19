pub mod api;
mod connections;
mod diagnostics;
mod progress;
pub use diagnostics::SyncDiagnostic;
pub mod phoenix;
mod runtime;
mod ssh;
pub mod tls;
mod transfer;
pub use connections::{ConnectionInput, ConnectionSummary, SaveError, SshInput, SshSummary};
pub use runtime::{connect_socket, run, SyncRuntime, SyncRuntimeStatus};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

pub const DEFAULT_MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;
fn default_max_blob_bytes() -> u64 {
    DEFAULT_MAX_BLOB_BYTES
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionMode {
    Direct,
    Ssh,
    #[default]
    Mtls,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshConnection {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub identity_file: PathBuf,
    pub known_hosts_file: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConnection {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub mode: ConnectionMode,
    pub ssh: Option<SshConnection>,
    #[serde(default)]
    pub client_cert: PathBuf,
    #[serde(default)]
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
            || !matches!(
                (self.mode, url.scheme()),
                (ConnectionMode::Direct, "ws" | "wss")
                    | (ConnectionMode::Ssh, "ws")
                    | (ConnectionMode::Mtls, "wss")
            )
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.query_pairs().any(|(k, v)| k == "vsn" && v == "2.0.0")
            || self.max_blob_bytes == 0
            || self.max_blob_bytes > DEFAULT_MAX_BLOB_BYTES
        {
            anyhow::bail!("sync connection requires an ID, an allowed ws/wss URL with vsn=2.0.0 and a 1..512MiB blob limit");
        }
        match self.mode {
            ConnectionMode::Mtls => anyhow::ensure!(
                !self.client_cert.as_os_str().is_empty() && !self.client_key.as_os_str().is_empty(),
                "mTLS requires client certificate and private key paths"
            ),
            ConnectionMode::Ssh => self
                .ssh
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("SSH connection settings are required"))?
                .validate()?,
            ConnectionMode::Direct => {}
        }
        Ok(())
    }
    pub(crate) fn resolve_paths(&mut self, base: &std::path::Path) {
        fn resolve(base: &std::path::Path, path: &mut PathBuf) {
            if !path.as_os_str().is_empty() && !path.is_absolute() {
                *path = base.join(&*path);
            }
        }
        resolve(base, &mut self.client_cert);
        resolve(base, &mut self.client_key);
        if let Some(path) = &mut self.ca_cert {
            resolve(base, path);
        }
        if let Some(ssh) = &mut self.ssh {
            resolve(base, &mut ssh.identity_file);
            resolve(base, &mut ssh.known_hosts_file);
        }
    }
    pub fn public_profile(&self) -> SyncProfile {
        SyncProfile {
            id: self.id.clone(),
            url: self.url.clone(),
        }
    }
}

impl SshConnection {
    fn validate(&self) -> anyhow::Result<()> {
        let host_valid = !self.host.is_empty()
            && self.host.len() <= 253
            && !self.host.starts_with('-')
            && self
                .host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-:_".contains(&c));
        let user_valid = !self.username.is_empty()
            && self.username.len() <= 128
            && !self.username.starts_with('-')
            && self
                .username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c));
        anyhow::ensure!(
            host_valid && user_valid && self.port != 0,
            "SSH requires a valid host, port and username"
        );
        anyhow::ensure!(
            !self.identity_file.as_os_str().is_empty()
                && !self.known_hosts_file.as_os_str().is_empty(),
            "SSH requires identity and known hosts file paths"
        );
        anyhow::ensure!([&self.identity_file, &self.known_hosts_file].iter().all(|path|
            path.to_str().is_some_and(|value| !value.chars().any(char::is_control)
                && !value.contains(['%', '$']))),
            "SSH credential paths must be valid text without control characters or expansion tokens");
        Ok(())
    }
    fn validate_files(&self) -> anyhow::Result<()> {
        self.validate()?;
        fn readable_file(path: &std::path::Path) -> bool {
            std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
                && std::fs::File::open(path).is_ok()
        }
        anyhow::ensure!(
            readable_file(&self.identity_file),
            "cannot read SSH identity file"
        );
        anyhow::ensure!(
            readable_file(&self.known_hosts_file),
            "cannot read SSH known hosts file"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
