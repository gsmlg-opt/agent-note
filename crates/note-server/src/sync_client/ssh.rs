//! OpenSSH owns authentication and remote forwarding; no local listener is opened.
use super::SshConnection;
use anyhow::{Context, Result};
use std::{
    io,
    pin::Pin,
    process::Stdio,
    task::{Context as TaskContext, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(super) struct SshStream {
    // kill_on_drop also covers cancellation during WebSocket/TLS handshakes.
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

impl SshStream {
    pub(super) fn connect(settings: &SshConnection, target: &reqwest::Url) -> Result<Self> {
        settings.validate_files()?;
        let host = target.host_str().context("SSH target host is missing")?;
        // URL parsing guarantees this is a host, not an OpenSSH option or command.
        let destination = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{}", target.port_or_known_default().unwrap())
        } else {
            format!("{host}:{}", target.port_or_known_default().unwrap())
        };
        // UserKnownHostsFile is parsed by OpenSSH as a configuration value even
        // though the whole argument is passed without a shell. Quote spaces and
        // disable token/environment expansion by rejecting those tokens below.
        let known_hosts = settings.known_hosts_file.to_str().unwrap();
        anyhow::ensure!(
            !known_hosts.contains(['%', '$']),
            "SSH known hosts path must not contain expansion tokens"
        );
        let known_hosts = known_hosts.replace('\\', "\\\\").replace('"', "\\\"");
        let mut command = Command::new("ssh");
        command
            .args([
                "-F",
                "/dev/null",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=2",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "LogLevel=ERROR",
            ])
            .arg("-o")
            .arg(format!("UserKnownHostsFile=\"{known_hosts}\""))
            .arg("-i")
            .arg(&settings.identity_file)
            .arg("-p")
            .arg(settings.port.to_string())
            .arg("-l")
            .arg(&settings.username)
            .arg("-W")
            .arg(destination)
            .arg("--")
            .arg(&settings.host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("cannot start SSH; install the OpenSSH client on the Agent Note server")?;
        let input = child.stdin.take().context("cannot open SSH input")?;
        let output = child.stdout.take().context("cannot open SSH output")?;
        Ok(Self {
            child,
            input,
            output,
        })
    }

    #[cfg(test)]
    pub(super) fn process_id(&self) -> u32 {
        self.child.id().unwrap()
    }
}

impl Drop for SshStream {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        // Tokio's child drop performs best-effort reaping after kill_on_drop.
    }
}
impl AsyncRead for SshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.output).poll_read(cx, buffer)
    }
}
impl AsyncWrite for SshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.input).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_client::{
        connect_socket, ConnectionMode, SyncConnection, DEFAULT_MAX_BLOB_BYTES,
    };
    use futures::{SinkExt, StreamExt};
    use std::{
        path::{Path, PathBuf},
        time::Duration,
    };
    use tokio::{net::TcpListener, time::timeout};

    struct Fixture {
        directory: tempfile::TempDir,
        daemon: Child,
        settings: SshConnection,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.daemon.start_kill();
        }
    }
    fn key(path: &Path) {
        assert!(std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .status()
            .unwrap()
            .success());
    }
    impl Fixture {
        async fn start() -> Self {
            let directory = tempfile::Builder::new()
                .prefix("agent-note ssh fixture ")
                .tempdir()
                .unwrap();
            let base = directory.path();
            key(&base.join("host"));
            key(&base.join("identity"));
            key(&base.join("wrong"));
            let username = String::from_utf8(
                std::process::Command::new("id")
                    .arg("-un")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let config = base.join("sshd_config");
            std::fs::write(&config, format!("Port {port}\nListenAddress 127.0.0.1\nHostKey \"{}/host\"\nPidFile \"{}/sshd.pid\"\nAuthorizedKeysFile \"{}/identity.pub\"\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nUsePAM no\nStrictModes no\nAllowUsers {username}\nAllowTcpForwarding yes\nPermitTTY no\nX11Forwarding no\nLogLevel ERROR\n", base.display(), base.display(), base.display())).unwrap();
            let known_hosts_file = base.join("known hosts");
            std::fs::write(
                &known_hosts_file,
                format!(
                    "[127.0.0.1]:{port} {}",
                    std::fs::read_to_string(base.join("host.pub")).unwrap()
                ),
            )
            .unwrap();
            let binary = ["/run/current-system/sw/bin/sshd", "/usr/sbin/sshd"]
                .into_iter()
                .find(|path| Path::new(path).exists())
                .expect("local sshd required");
            let log = std::fs::File::create(base.join("sshd.log")).unwrap();
            let daemon = Command::new(binary)
                .args(["-D", "-e", "-f"])
                .arg(config)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let settings = SshConnection {
                host: "127.0.0.1".into(),
                port,
                username,
                identity_file: base.join("identity"),
                known_hosts_file,
            };
            let mut fixture = Self {
                directory,
                daemon,
                settings,
            };
            timeout(Duration::from_secs(3), async {
                loop {
                    // Connecting observes readiness without competing for sshd's port.
                    if tokio::net::TcpStream::connect(("127.0.0.1", port))
                        .await
                        .is_ok()
                    {
                        break;
                    }
                    if let Some(status) = fixture.daemon.try_wait().unwrap() {
                        panic!(
                            "sshd exited {status}: {}",
                            std::fs::read_to_string(fixture.directory.path().join("sshd.log"))
                                .unwrap()
                        );
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fixture
        }
        fn profile(&self, port: u16) -> SyncConnection {
            SyncConnection {
                id: "ssh-test".into(),
                mode: ConnectionMode::Ssh,
                ssh: Some(self.settings.clone()),
                url: format!("ws://127.0.0.1:{port}/note-sync/websocket?vsn=2.0.0"),
                client_cert: PathBuf::new(),
                client_key: PathBuf::new(),
                ca_cert: None,
                max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
            }
        }
    }
    async fn reaped(pid: u32) {
        timeout(Duration::from_secs(3), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned SSH child must be killed and reaped");
    }

    #[test]
    fn rejects_ssh_option_control_and_expansion_injection() {
        let mut settings = SshConnection {
            host: "localhost".into(),
            port: 22,
            username: "gao".into(),
            identity_file: "/safe/key".into(),
            known_hosts_file: "/safe/known hosts".into(),
        };
        settings.validate().unwrap();
        for host in [
            "-oProxyCommand=evil",
            "localhost\nHost evil",
            "gao@host",
            "host name",
        ] {
            settings.host = host.into();
            assert!(settings.validate().is_err());
        }
        settings.host = "::1".into();
        settings.validate().unwrap();
        for user in ["-oCommand", "user\n", "user@host", "user space"] {
            settings.username = user.into();
            assert!(settings.validate().is_err());
        }
        settings.username = "gao".into();
        for path in ["/path/%h", "/path/${HOME}", "/path/\nsecret"] {
            settings.known_hosts_file = path.into();
            assert!(settings.validate().is_err());
            settings.known_hosts_file = "/safe/known hosts".into();
            settings.identity_file = path.into();
            assert!(settings.validate().is_err());
            settings.identity_file = "/safe/key".into();
        }
        let directory = tempfile::tempdir().unwrap();
        settings.identity_file = directory.path().into();
        settings.known_hosts_file = directory.path().into();
        assert!(settings.validate_files().is_err());
    }

    #[tokio::test]
    #[ignore = "requires local OpenSSH sshd/keygen and Linux /proc"]
    async fn real_openssh_stdio_websocket_transfer_auth_negatives_and_cleanup() {
        let fixture = Fixture::start().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     response| {
                        assert_eq!(
                            request.uri().path_and_query().unwrap().as_str(),
                            "/note-sync/websocket?vsn=2.0.0"
                        );
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let message = socket.next().await.unwrap().unwrap();
                socket.send(message).await.unwrap();
            }
        });
        let profile = fixture.profile(port);
        let target = reqwest::Url::parse(&profile.url).unwrap();
        let stream = SshStream::connect(profile.ssh.as_ref().unwrap(), &target).unwrap();
        let pid = stream.process_id();
        let (mut socket, _) = timeout(
            Duration::from_secs(5),
            tokio_tungstenite::client_async(&profile.url, stream),
        )
        .await
        .unwrap()
        .unwrap();
        let payload = vec![42; 150_000];
        socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                payload.clone().into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_data().as_ref(),
            payload
        );
        drop(socket);
        reaped(pid).await;
        // Public runtime path uses the same SSH stream and original URL handshake.
        let mut socket = connect_socket(&profile).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                "runtime".into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "runtime"
        );
        drop(socket);

        for wrong_host in [true, false] {
            let mut settings = fixture.settings.clone();
            if wrong_host {
                let path = fixture.directory.path().join("wrong known hosts");
                std::fs::write(
                    &path,
                    format!(
                        "[127.0.0.1]:{} {}",
                        settings.port,
                        std::fs::read_to_string(fixture.directory.path().join("wrong.pub"))
                            .unwrap()
                    ),
                )
                .unwrap();
                settings.known_hosts_file = path;
            } else {
                settings.identity_file = fixture.directory.path().join("wrong");
            }
            let stream = SshStream::connect(&settings, &target).unwrap();
            let pid = stream.process_id();
            assert!(timeout(
                Duration::from_secs(5),
                tokio_tungstenite::client_async(&profile.url, stream)
            )
            .await
            .unwrap()
            .is_err());
            reaped(pid).await;
        }
        peer.abort();
        assert!(matches!(peer.await, Err(error) if error.is_cancelled()));
        let mut missing = fixture.settings.clone();
        missing.identity_file = fixture.directory.path().join("private-secret-missing");
        let error = SshStream::connect(&missing, &target).err().unwrap();
        assert_eq!(error.to_string(), "cannot read SSH identity file");
        assert!(!format!("{error:#}").contains("private-secret"));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let blocked_profile = fixture.profile(listener.local_addr().unwrap().port());
        let stream = SshStream::connect(
            &fixture.settings,
            &reqwest::Url::parse(&blocked_profile.url).unwrap(),
        )
        .unwrap();
        let pid = stream.process_id();
        let handshake = tokio::spawn(async move {
            tokio_tungstenite::client_async(&blocked_profile.url, stream).await
        });
        let (_pending, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        handshake.abort();
        assert!(matches!(handshake.await, Err(error) if error.is_cancelled()));
        reaped(pid).await;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rejected_profile = fixture.profile(listener.local_addr().unwrap().port());
        let stream = SshStream::connect(
            &fixture.settings,
            &reqwest::Url::parse(&rejected_profile.url).unwrap(),
        )
        .unwrap();
        let pid = stream.process_id();
        let reject = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            tcp.read(&mut bytes).await.unwrap();
            tcp.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(timeout(
            Duration::from_secs(5),
            tokio_tungstenite::client_async(&rejected_profile.url, stream)
        )
        .await
        .unwrap()
        .is_err());
        reject.await.unwrap();
        reaped(pid).await;
        let mut fixture = fixture;
        fixture.daemon.kill().await.unwrap();
        fixture.daemon.wait().await.unwrap();
    }
}
