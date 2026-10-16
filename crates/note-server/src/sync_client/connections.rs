use super::{tls, ConnectionMode, SshConnection, SyncConnection};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use utoipa::ToSchema;

#[derive(Clone, Serialize, ToSchema)]
pub struct ConnectionSummary {
    pub id: String,
    pub url: String,
    pub max_blob_bytes: u64,
    pub mode: ConnectionMode,
    pub ssh: Option<SshSummary>,
    pub has_client_cert: bool,
    pub has_client_key: bool,
    pub has_ca_cert: bool,
}

impl From<&SyncConnection> for ConnectionSummary {
    fn from(connection: &SyncConnection) -> Self {
        Self {
            id: connection.id.clone(),
            url: connection.url.clone(),
            max_blob_bytes: connection.max_blob_bytes,
            mode: connection.mode,
            ssh: connection.ssh.as_ref().map(|ssh| SshSummary {
                host: ssh.host.clone(),
                port: ssh.port,
                username: ssh.username.clone(),
                has_identity_file: !ssh.identity_file.as_os_str().is_empty(),
                has_known_hosts_file: !ssh.known_hosts_file.as_os_str().is_empty(),
            }),
            has_client_cert: !connection.client_cert.as_os_str().is_empty(),
            has_client_key: !connection.client_key.as_os_str().is_empty(),
            has_ca_cert: connection.ca_cert.is_some(),
        }
    }
}

#[derive(Clone, Serialize, ToSchema)]
pub struct SshSummary {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub has_identity_file: bool,
    pub has_known_hosts_file: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SshInput {
    pub host: String,
    #[serde(default = "super::default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub identity_file: Option<String>,
    pub known_hosts_file: Option<String>,
}

// Credential paths are accepted only on this write DTO, never returned in status.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInput {
    pub id: String,
    pub url: String,
    pub mode: Option<ConnectionMode>,
    pub ssh: Option<SshInput>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub ca_cert: Option<String>,
    pub max_blob_bytes: u64,
}

#[derive(Debug)]
pub enum SaveError {
    Invalid(String),
    Persistence,
}

pub(super) struct Connections {
    path: Option<PathBuf>,
    base: PathBuf,
    current: watch::Sender<Vec<SyncConnection>>,
    save: Mutex<()>,
}

impl Default for Connections {
    fn default() -> Self {
        Self::memory(&[])
    }
}

impl Connections {
    pub(super) fn memory(initial: &[SyncConnection]) -> Self {
        Self {
            path: None,
            base: PathBuf::new(),
            current: watch::channel(initial.to_vec()).0,
            save: Mutex::new(()),
        }
    }

    pub(super) fn open(
        initial: &[SyncConnection],
        path: PathBuf,
        base: PathBuf,
    ) -> anyhow::Result<Self> {
        let profiles = match std::fs::read(&path) {
            Ok(bytes) => {
                let mut profiles: Vec<SyncConnection> = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("saved sync connections are invalid"))?;
                let mut ids = std::collections::HashSet::new();
                for profile in &mut profiles {
                    profile.validate()?;
                    anyhow::ensure!(
                        ids.insert(profile.id.clone()),
                        "saved sync connections are invalid"
                    );
                    profile.resolve_paths(&base);
                }
                profiles
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut ids = std::collections::HashSet::new();
                for profile in initial {
                    profile.validate()?;
                    anyhow::ensure!(
                        ids.insert(profile.id.clone()),
                        "sync connection IDs must be unique"
                    );
                }
                initial.to_vec()
            }
            Err(_) => anyhow::bail!("saved sync connections cannot be read"),
        };
        Ok(Self {
            path: Some(path),
            base,
            current: watch::channel(profiles).0,
            save: Mutex::new(()),
        })
    }

    pub(super) fn snapshot(&self) -> Vec<SyncConnection> {
        self.current.borrow().clone()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<Vec<SyncConnection>> {
        self.current.subscribe()
    }

    pub(super) async fn update(
        self: &Arc<Self>,
        input: ConnectionInput,
    ) -> Result<ConnectionSummary, SaveError> {
        let store = self.clone();
        // The blocking operation owns the entire commit. Cancelling an HTTP request
        // cannot separate a successful rename from publishing its runtime snapshot.
        tokio::task::spawn_blocking(move || store.save(input))
            .await
            .map_err(|_| SaveError::Persistence)?
    }

    fn save(&self, input: ConnectionInput) -> Result<ConnectionSummary, SaveError> {
        let _guard = self.save.lock().map_err(|_| SaveError::Persistence)?;
        let mut profiles = self.snapshot();
        let existing = profiles.iter().position(|profile| profile.id == input.id);
        let before = existing.map(|index| &profiles[index]);
        let mode = input
            .mode
            .or(before.map(|profile| profile.mode))
            .unwrap_or_default();
        let ssh = if mode == ConnectionMode::Ssh {
            let old = before
                .filter(|profile| profile.mode == ConnectionMode::Ssh)
                .and_then(|profile| profile.ssh.as_ref());
            Some(match input.ssh {
                Some(ssh) => SshConnection {
                    host: ssh.host,
                    port: ssh.port,
                    username: ssh.username,
                    identity_file: credential_path(
                        &self.base,
                        ssh.identity_file,
                        old.map(|ssh| &ssh.identity_file),
                    )?,
                    known_hosts_file: credential_path(
                        &self.base,
                        ssh.known_hosts_file,
                        old.map(|ssh| &ssh.known_hosts_file),
                    )?,
                },
                None => old.cloned().ok_or_else(|| {
                    SaveError::Invalid("SSH settings and credential paths are required".into())
                })?,
            })
        } else {
            None
        };
        let uses_tls = reqwest::Url::parse(&input.url).is_ok_and(|url| url.scheme() == "wss");
        let mtls_before = before.filter(|profile| profile.mode == ConnectionMode::Mtls);
        let profile = SyncConnection {
            mode,
            ssh,
            id: input.id,
            url: input.url,
            client_cert: if mode == ConnectionMode::Mtls {
                credential_path(
                    &self.base,
                    input.client_cert,
                    mtls_before.map(|profile| &profile.client_cert),
                )?
            } else {
                PathBuf::new()
            },
            client_key: if mode == ConnectionMode::Mtls {
                credential_path(
                    &self.base,
                    input.client_key,
                    mtls_before.map(|profile| &profile.client_key),
                )?
            } else {
                PathBuf::new()
            },
            ca_cert: if !uses_tls {
                None
            } else {
                match input.ca_cert {
                    None => before.and_then(|profile| profile.ca_cert.clone()),
                    Some(path) if path.trim().is_empty() => None,
                    Some(path) => Some(resolve(&self.base, Path::new(&path))),
                }
            },
            max_blob_bytes: input.max_blob_bytes,
        };
        profile
            .validate()
            .map_err(|error| SaveError::Invalid(error.to_string()))?;
        if uses_tls {
            tls::client_config(&profile).map_err(|error| SaveError::Invalid(error.to_string()))?;
        }
        if let Some(ssh) = &profile.ssh {
            ssh.validate_files()
                .map_err(|error| SaveError::Invalid(error.to_string()))?;
        }
        let summary = ConnectionSummary::from(&profile);
        if let Some(index) = existing {
            profiles[index] = profile;
        } else {
            profiles.push(profile);
        }
        let path = self.path.as_ref().ok_or(SaveError::Persistence)?;
        persist(path, &profiles).map_err(|_| SaveError::Persistence)?;
        // No fallible operation follows the durable replacement.
        self.current.send_replace(profiles);
        // The rename is the commit point. A directory-sync failure must not turn
        // a committed update into a reported failure or leave runtime state old.
        #[cfg(unix)]
        if std::fs::File::open(path.parent().unwrap())
            .and_then(|parent| parent.sync_all())
            .is_err()
        {
            eprintln!(
                "warning: sync connections saved, but the storage directory could not be synced"
            );
        }
        Ok(summary)
    }
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() || path.is_absolute() {
        path.into()
    } else {
        base.join(path)
    }
}

fn credential_path(
    base: &Path,
    supplied: Option<String>,
    existing: Option<&PathBuf>,
) -> Result<PathBuf, SaveError> {
    match supplied {
        Some(value) if !value.trim().is_empty() => Ok(resolve(base, Path::new(&value))),
        None if existing.is_some() => Ok(existing.unwrap().clone()),
        _ => Err(SaveError::Invalid(
            "credential file paths are required; omit unchanged paths when editing".into(),
        )),
    }
}

fn persist(path: &Path, profiles: &[SyncConnection]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(temporary.as_file_mut(), profiles)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_client::tests::certificates;
    use std::time::Duration;

    #[test]
    fn missing_sidecar_validates_initial_catalog() {
        let certs = certificates();
        let path = certs.dir.path().join("missing.json");
        let mut invalid = certs.profile.clone();
        invalid.max_blob_bytes = 0;
        for initial in [vec![invalid], vec![certs.profile.clone(); 2]] {
            assert!(Connections::open(&initial, path.clone(), certs.dir.path().into()).is_err());
        }
    }

    fn input(profile: &SyncConnection) -> ConnectionInput {
        ConnectionInput {
            id: profile.id.clone(),
            mode: Some(profile.mode),
            ssh: None,
            url: profile.url.clone(),
            client_cert: Some(profile.client_cert.to_str().unwrap().into()),
            client_key: Some(profile.client_key.to_str().unwrap().into()),
            ca_cert: profile
                .ca_cert
                .as_ref()
                .map(|path| path.to_str().unwrap().into()),
            max_blob_bytes: profile.max_blob_bytes,
        }
    }

    #[test]
    fn malformed_or_unreadable_sidecar_never_falls_back_to_toml() {
        let certs = certificates();
        let path = certs.dir.path().join("connections.json");
        for bytes in [
            "private-secret-invalid-json".to_owned(),
            serde_json::to_string(&[&certs.profile, &certs.profile]).unwrap(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let error = Connections::open(
                &[certs.profile.clone()],
                path.clone(),
                certs.dir.path().into(),
            )
            .err()
            .unwrap();
            assert!(!format!("{error:#}").contains("private-secret"));
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let error = Connections::open(&[certs.profile.clone()], path, certs.dir.path().into())
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "saved sync connections cannot be read");
    }

    #[tokio::test]
    async fn replacement_cannot_be_lost_between_snapshot_and_watch() {
        let certs = certificates();
        let store = Arc::new(
            Connections::open(
                &[],
                certs.dir.path().join("connections.json"),
                certs.dir.path().into(),
            )
            .unwrap(),
        );
        let mut receiver = store.subscribe();
        assert!(receiver.borrow_and_update().is_empty());
        store.update(input(&certs.profile)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*receiver.borrow_and_update(), store.snapshot());
        let mut updated = input(&certs.profile);
        updated.max_blob_bytes = 1024;
        store.update(updated).await.unwrap();
        let receiver = store.subscribe();
        assert_eq!(receiver.borrow()[0].max_blob_bytes, 1024);
    }

    #[tokio::test]
    async fn cancelling_the_request_cannot_cancel_a_started_persistence_commit() {
        let certs = certificates();
        let path = certs.dir.path().join("connections.json");
        let store =
            Arc::new(Connections::open(&[], path.clone(), certs.dir.path().into()).unwrap());
        let mut receiver = store.subscribe();
        let guard = store.save.lock().unwrap();
        let mut save = Box::pin(store.update(input(&certs.profile)));
        assert!(futures::poll!(save.as_mut()).is_pending());
        drop(save);
        drop(guard);
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let restarted = Connections::open(&[], path, certs.dir.path().into()).unwrap();
        assert_eq!(restarted.snapshot(), store.snapshot());
        assert_eq!(store.snapshot()[0].id, certs.profile.id);
    }

    #[tokio::test]
    async fn concurrent_creates_preserve_both_profiles() {
        let certs = certificates();
        let store = Arc::new(
            Connections::open(
                &[],
                certs.dir.path().join("connections.json"),
                certs.dir.path().into(),
            )
            .unwrap(),
        );
        let mut second = input(&certs.profile);
        second.id = "second".into();
        let (first, second) =
            tokio::join!(store.update(input(&certs.profile)), store.update(second));
        first.unwrap();
        second.unwrap();
        assert_eq!(store.snapshot().len(), 2);
    }
    #[tokio::test]
    async fn modes_persist_restart_redact_and_do_not_reuse_inappropriate_credentials() {
        let certs = certificates();
        let path = certs.dir.path().join("connections.json");
        let base = certs.dir.path().to_path_buf();
        let store = Arc::new(Connections::open(&[], path.clone(), base.clone()).unwrap());
        store.update(input(&certs.profile)).await.unwrap();
        let mut edit = input(&certs.profile);
        edit.mode = Some(ConnectionMode::Direct);
        edit.url = "ws://localhost:6222/note-sync/websocket?vsn=2.0.0".into();
        store.update(edit).await.unwrap();
        let restarted = Connections::open(&[], path.clone(), base.clone()).unwrap();
        let direct = restarted.snapshot().remove(0);
        assert_eq!(direct.mode, ConnectionMode::Direct);
        assert!(direct.client_cert.as_os_str().is_empty());
        assert!(direct.client_key.as_os_str().is_empty());
        assert!(direct.ca_cert.is_none());
        let omitted_mode: ConnectionInput = serde_json::from_value(serde_json::json!({
            "id":"local", "url":"ws://localhost:6223/note-sync/websocket?vsn=2.0.0", "max_blob_bytes":1024
        })).unwrap();
        store.update(omitted_mode).await.unwrap();
        assert_eq!(store.snapshot()[0].mode, ConnectionMode::Direct);
        let mtls_without_files: ConnectionInput = serde_json::from_value(serde_json::json!({
            "id":"local", "url":certs.profile.url, "mode":"mtls", "max_blob_bytes":1024
        }))
        .unwrap();
        assert!(matches!(
            store.update(mtls_without_files).await,
            Err(SaveError::Invalid(_))
        ));
        assert_eq!(store.snapshot()[0].mode, ConnectionMode::Direct);
        // These files represent server-provisioned SSH credentials, whose content
        // is validated by OpenSSH when establishing a tunnel.
        std::fs::write(base.join("ssh identity"), "server-only identity").unwrap();
        std::fs::write(base.join("known hosts"), "server-only host pins").unwrap();
        let ssh_input = |identity: Option<&str>, hosts: Option<&str>| -> ConnectionInput {
            serde_json::from_value(serde_json::json!({
                "id":"local", "url":"ws://127.0.0.1:4000/note-sync/websocket?vsn=2.0.0", "mode":"ssh",
                "max_blob_bytes":1024, "ssh":{"host":"test.internal", "username":"gao",
                    "identity_file":identity, "known_hosts_file":hosts}
            })).unwrap()
        };
        let summary = store
            .update(ssh_input(Some("ssh identity"), Some("known hosts")))
            .await
            .unwrap();
        let public = serde_json::to_string(&summary).unwrap();
        assert!(!public.contains("ssh identity"));
        assert!(!public.contains("known hosts"));
        assert!(!public.contains("server-only"));
        assert!(summary.ssh.unwrap().has_identity_file);
        store.update(ssh_input(None, None)).await.unwrap();
        let restarted = Connections::open(&[], path, base.clone()).unwrap();
        assert_eq!(restarted.snapshot(), store.snapshot());
        let ssh = restarted.snapshot().remove(0).ssh.unwrap();
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.identity_file, base.join("ssh identity"));
        assert_eq!(ssh.known_hosts_file, base.join("known hosts"));
        let mut direct = input(&certs.profile);
        direct.mode = Some(ConnectionMode::Direct);
        store.update(direct).await.unwrap();
        assert!(store.snapshot()[0].ssh.is_none());
        assert!(store.snapshot()[0].client_cert.as_os_str().is_empty());
        assert!(matches!(
            store.update(ssh_input(None, None)).await,
            Err(SaveError::Invalid(_))
        ));
    }
}
