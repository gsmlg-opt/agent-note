use super::*;
use futures::{SinkExt, StreamExt};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::{RootCertStore, ServerConfig};
use sha2::Digest;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

pub(super) struct Certificates {
    pub(super) dir: tempfile::TempDir,
    pub(super) server: Arc<ServerConfig>,
    pub(super) profile: SyncConnection,
}
pub(super) fn certificates() -> Certificates {
    certificates_with_expiry(false)
}
fn certificates_with_expiry(expired: bool) -> Certificates {
    let dir = tempfile::tempdir().unwrap();
    let mut params = CertificateParams::new(vec![]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "note-sync test CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::from_params(&params, &key);
    let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&server_key, &issuer).unwrap();
    let mut params = CertificateParams::new(vec!["client.local".into()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "note-sync client");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    if expired {
        params.not_before = rcgen::date_time_ymd(2019, 1, 1);
        params.not_after = rcgen::date_time_ymd(2020, 1, 1);
    }
    let client_key = KeyPair::generate().unwrap();
    let client = params.signed_by(&client_key, &issuer).unwrap();
    std::fs::write(dir.path().join("server.pem"), cert.pem()).unwrap();
    std::fs::write(dir.path().join("server.key"), server_key.serialize_pem()).unwrap();
    std::fs::write(dir.path().join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.path().join("client.pem"), client.pem()).unwrap();
    std::fs::write(dir.path().join("client.key"), client_key.serialize_pem()).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .unwrap();
    let server = Arc::new(
        ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
            )
            .unwrap(),
    );
    let profile = SyncConnection {
        id: "local".into(),
        url: "wss://localhost:0/note-sync/websocket?vsn=2.0.0".into(),
        client_cert: dir.path().join("client.pem"),
        client_key: dir.path().join("client.key"),
        ca_cert: Some(dir.path().join("ca.pem")),
        max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
    };
    Certificates {
        dir,
        server,
        profile,
    }
}

#[tokio::test]
async fn mtls_socket_verifies_client_certificate_server_chain_and_hostname() {
    let mut certs = certificates();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    certs.profile.url = format!("wss://localhost:{port}/note-sync/websocket?vsn=2.0.0");
    let acceptor = TlsAcceptor::from(certs.server.clone());
    let peer = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            if let Ok(tls) = acceptor.accept(stream).await {
                let mut socket = tokio_tungstenite::accept_async(tls).await.unwrap();
                if let Some(Ok(message)) = socket.next().await {
                    socket.send(message).await.unwrap();
                }
            }
        }
    });
    let mut socket = connect_socket(&certs.profile).await.unwrap();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "probe".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap().into_text().unwrap(),
        "probe"
    );
    certs.profile.url = certs.profile.url.replace("localhost", "127.0.0.1");
    assert!(connect_socket(&certs.profile).await.is_err());
    peer.await.unwrap();
    assert!(certs.dir.path().exists());
}

#[test]
fn invalid_credentials_and_insecure_profiles_are_actionable_and_redacted() {
    let mut certs = certificates();
    certs.profile.client_key = certs.dir.path().join("private-secret-missing.key");
    let error = format!("{:#}", tls::client_config(&certs.profile).unwrap_err());
    assert!(error.contains("private key"));
    assert!(!error.contains("private-secret"));
    certs.profile.url = "ws://localhost/socket?vsn=2.0.0".into();
    assert!(certs.profile.validate().is_err());
    certs.profile.url = "wss://secret:password@localhost/socket?vsn=2.0.0".into();
    assert!(certs.profile.validate().is_err());
}

/// Run with tests/fixtures/phoenix-sync/run.sh; launches actual Phoenix 1.8.15.
#[tokio::test]
#[ignore = "requires compiled Phoenix 1.8.15 fixture and Mix"]
async fn real_phoenix_mtls_inventory_transfers_and_durable_ack() {
    use note_attachments::FilesystemAttachmentStore;
    use note_core::sync::*;
    use note_embedding::StubEmbedder;
    use note_pipelines::{get_note, save_note, update_system_config, Context, SaveNoteInput};
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    let mut certs = certificates();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    certs.profile.url = format!("wss://localhost:{port}/note-sync/websocket?vsn=2.0.0");
    let storage = Arc::new(
        TursoStorage::open(certs.dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let ctx = Arc::new(Context::new(
        storage.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            certs.dir.path().join("attachments"),
        )),
    ));
    let local = save_note(
        &ctx,
        SaveNoteInput {
            title: "Agent Note outgoing".into(),
            content: "chunked content".repeat(12000),
            attachments: vec![note_core::NoteAttachment {
                id: "event".into(),
                path: "assets/data.bin".into(),
                mime: "application/octet-stream".into(),
                description: "fixture attachment".into(),
                content: vec![42; 150_000],
                storage: None,
            }],
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    update_system_config(
        &ctx,
        &note_core::SystemConfig {
            sync: SyncConfig {
                enabled: true,
                connection_profile: Some("local".into()),
                rules: vec![SyncRule {
                    id: "project".into(),
                    enabled: true,
                    selector: "project=bear_note".into(),
                }],
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let remote_id = uuid::Uuid::new_v4().to_string();
    let remote_blob = vec![81u8; 180_000];
    let blob_id = "event".to_string();
    std::fs::write(certs.dir.path().join(&blob_id), &remote_blob).unwrap();
    let clock = Hlc {
        millis: chrono::Utc::now().timestamp_millis(),
        counter: 1,
        node_id: "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa".into(),
    };
    let snapshot = SyncSnapshot {
        title: "GaoNote incoming".into(),
        content: "remote content".repeat(8000),
        labels: vec![SyncLabel {
            key: "project".into(),
            value: "bear_note".into(),
            description: "".into(),
            value_type: "text".into(),
        }],
        created_at: clock.millis / 1000,
        updated_at: clock.millis / 1000,
        deleted_at: None,
        attachments: vec![SyncAttachment {
            id: blob_id.clone(),
            path: "remote/binary.bin".into(),
            mime: "application/octet-stream".into(),
            description: "remote blob".into(),
            size_bytes: remote_blob.len() as u64,
            checksum_sha256: format!("{:x}", sha2::Sha256::digest(&remote_blob)),
        }],
    };
    let incoming = SyncEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        note_id: remote_id.clone(),
        generation: clock.clone(),
        control: clock.clone(),
        version: clock,
        kind: SyncEventKind::Attach,
        snapshot_hash: note_pipelines::sync::canonical_snapshot_hash(Some(&snapshot)).unwrap(),
        snapshot: Some(snapshot),
    };
    let inbound = certs.dir.path().join("inbound.json");
    std::fs::write(&inbound, serde_json::to_vec(&incoming).unwrap()).unwrap();
    let commit = certs.dir.path().join("commit.json");
    let local_commit = certs.dir.path().join(format!("commit.json.{}", local.id));
    let ack = certs.dir.path().join("ack.json");
    let log_path = certs.dir.path().join("phoenix.log");
    let lost_ack = certs.dir.path().join("lost-ack.marker");
    let pause_begin = certs.dir.path().join("pause-begin.marker");
    let scopes_file = certs.dir.path().join("scopes.jsonl");
    let log = std::fs::File::create(&log_path).unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phoenix-sync");
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut peer = Child(
        std::process::Command::new("mix")
            .args(["run", "--no-halt"])
            .current_dir(&fixture)
            .env("SYNC_FIXTURE_PORT", port.to_string())
            .env("SYNC_SERVER_CERT", certs.dir.path().join("server.pem"))
            .env("SYNC_SERVER_KEY", certs.dir.path().join("server.key"))
            .env("SYNC_CA_CERT", certs.dir.path().join("ca.pem"))
            .env("SYNC_INBOUND_JSON", &inbound)
            .env("SYNC_INBOUND_BLOBS", certs.dir.path())
            .env("SYNC_COMMIT_FILE", &commit)
            .env("SYNC_ACK_FILE", &ack)
            .env("SYNC_DROP_FIRST_ACK", &lost_ack)
            .env("SYNC_PAUSE_FIRST_BEGIN", &pause_begin)
            .env("SYNC_SCOPES_FILE", &scopes_file)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            peer.0.try_wait().unwrap().is_none(),
            "Phoenix fixture exited: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "Phoenix fixture failed to bind: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let direct = tokio_tungstenite::connect_async_tls_with_config(
        &certs.profile.url,
        None,
        true,
        Some(tokio_tungstenite::Connector::Rustls(
            tls::client_config(&certs.profile).unwrap(),
        )),
    )
    .await;
    assert!(
        direct.is_ok(),
        "actual Phoenix mTLS/WSS handshake failed: {:?}; fixture: {}",
        direct.as_ref().err(),
        std::fs::read_to_string(&log_path).unwrap()
    );
    drop(direct);
    let runtime = SyncRuntime::new(&[certs.profile.clone()]);
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(run(
        ctx.clone(),
        vec![certs.profile.clone()],
        runtime.clone(),
        shutdown,
    ));
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut retried = false;
        let mut changed_scope = false;
        loop {
            if pause_begin.exists() && !retried {
                assert!(
                    !storage
                        .session()
                        .await
                        .unwrap()
                        .list_sync_records("inflight", None, 100)
                        .await
                        .unwrap()
                        .is_empty(),
                    "fixture barrier must hold a DB lease"
                );
                let mut paused = note_pipelines::get_system_config(&ctx).await.unwrap();
                paused.sync.enabled = false;
                update_system_config(&ctx, &paused).await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        if storage
                            .session()
                            .await
                            .unwrap()
                            .list_sync_records("inflight", None, 100)
                            .await
                            .unwrap()
                            .is_empty()
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("pause must cancel the entire inventory transfer promptly");
                assert!(
                    !local_commit.exists(),
                    "paused initial transfer must not commit"
                );
                assert!(
                    note_pipelines::sync::sync_status(&ctx)
                        .await
                        .unwrap()
                        .pending_notes
                        > 0
                );
                std::fs::remove_file(&pause_begin).unwrap();
                paused.sync.enabled = true;
                update_system_config(&ctx, &paused).await.unwrap();
                runtime.retry();
                retried = true;
                continue;
            }
            if retried && pause_begin.exists() && !changed_scope {
                let mut changed = note_pipelines::get_system_config(&ctx).await.unwrap();
                changed.sync.rules[0].selector = "project".into();
                update_system_config(&ctx, &changed).await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        let lines = std::fs::read_to_string(&scopes_file).unwrap_or_default();
                        if lines.lines().last() == Some("[\"project\"]") {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("selector changes must cancel and rejoin during initial inventory");
                changed_scope = true;
            }
            if changed_scope
                && local_commit.exists()
                && ack.exists()
                && get_note(&ctx, &remote_id).await.unwrap().is_some()
                && note_pipelines::sync::sync_status(&ctx)
                    .await
                    .unwrap()
                    .pending_notes
                    == 0
            {
                break;
            }
            let state = runtime.status(&ctx).await.unwrap();
            if state.state == "error" && !lost_ack.exists() {
                panic!(
                    "Phoenix interoperability failed: {:?}; fixture: {}",
                    state.last_error,
                    std::fs::read_to_string(&log_path).unwrap()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    stop.send(true).unwrap();
    worker.await.unwrap();
    result.expect("Phoenix interoperability did not complete within 30 seconds");
    let committed: SyncEvent =
        serde_json::from_slice(&std::fs::read(&local_commit).unwrap()).unwrap();
    assert!(
        lost_ack.exists(),
        "fixture must persist then drop first ACK"
    );
    assert_eq!(committed.note_id, local.id);
    assert_eq!(
        std::fs::read_to_string(
            certs
                .dir
                .path()
                .join(format!("commit.json.{}.scopes", local.id))
        )
        .unwrap(),
        "[\"project\"]",
        "old selector session must never commit after scope change"
    );
    let outbound = committed.snapshot.as_ref().unwrap();
    assert_eq!(outbound.content, local.content);
    let received_blob = std::fs::read(certs.dir.path().join(format!(
        "commit.json.{}.{}",
        local.id, local.attachments[0].id
    )))
    .unwrap();
    assert_eq!(received_blob, vec![42u8; 150_000]);
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&received_blob)),
        outbound.attachments[0].checksum_sha256
    );
    let returned: SyncAck = serde_json::from_slice(&std::fs::read(ack).unwrap()).unwrap();
    assert_eq!(returned, incoming.acknowledgement());
    let received = get_note(&ctx, &remote_id).await.unwrap().unwrap();
    assert_eq!(received.title, "GaoNote incoming");
    assert_eq!(received.attachments[0].content, remote_blob);
    assert_eq!(
        note_pipelines::sync::sync_status(&ctx)
            .await
            .unwrap()
            .pending_notes,
        0
    );
    assert!(
        storage
            .session()
            .await
            .unwrap()
            .list_sync_records("inflight", None, 100)
            .await
            .unwrap()
            .is_empty(),
        "retry/reconnect/shutdown must reclaim every lease"
    );
    // Pause the next transfer after its durable lease, then stop the worker.
    std::fs::remove_file(&pause_begin).unwrap();
    let latest = note_pipelines::update_note(
        &ctx,
        &local.id,
        local.revision,
        SaveNoteInput {
            title: "updated before shutdown".into(),
            content: "latest pending version".into(),
            attachments: vec![],
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(run(
        ctx.clone(),
        vec![certs.profile.clone()],
        runtime.clone(),
        shutdown,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !pause_begin.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    assert!(
        storage
            .session()
            .await
            .unwrap()
            .list_sync_records("inflight", None, 100)
            .await
            .unwrap()
            .is_empty(),
        "interrupted transfer must not leave an orphan pin"
    );
    let pending = note_pipelines::sync::next_sync_event(&ctx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.snapshot.unwrap().title, latest.title);
    note_pipelines::sync::release_sync_event(&ctx, &pending.event_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn mtls_rejects_untrusted_server_and_missing_untrusted_expired_clients() {
    let mut certs = certificates();
    let other = certificates();
    let expired = certificates_with_expiry(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    certs.profile.url = format!(
        "wss://localhost:{}/note-sync/websocket?vsn=2.0.0",
        listener.local_addr().unwrap().port()
    );
    let servers = [
        certs.server.clone(),
        certs.server.clone(),
        expired.server.clone(),
        certs.server.clone(),
    ];
    let peer = tokio::spawn(async move {
        for server in servers {
            let acceptor = TlsAcceptor::from(server);
            let (stream, _) = listener.accept().await.unwrap();
            if let Ok(tls) = acceptor.accept(stream).await {
                // A TLS client may close after rejecting the server certificate.
                let _ = tokio_tungstenite::accept_async(tls).await;
            }
        }
    });
    let mut wrong_server = certs.profile.clone();
    wrong_server.ca_cert = other.profile.ca_cert.clone();
    assert!(connect_socket(&wrong_server).await.is_err());
    let mut wrong_client = certs.profile.clone();
    wrong_client.client_cert = other.profile.client_cert.clone();
    wrong_client.client_key = other.profile.client_key.clone();
    assert!(connect_socket(&wrong_client).await.is_err());
    let mut expired_client = expired.profile.clone();
    expired_client.url = certs.profile.url.clone();
    assert!(connect_socket(&expired_client).await.is_err());
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(certs.profile.ca_cert.as_ref().unwrap()).unwrap(),
    )) {
        roots.add(cert.unwrap()).unwrap();
    }
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let absent = tokio_tungstenite::connect_async_tls_with_config(
        &certs.profile.url,
        None,
        true,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(client))),
    )
    .await;
    assert!(absent.is_err());
    peer.await.unwrap();
}

#[tokio::test]
async fn missing_credentials_report_worker_error_while_notes_and_queue_remain_usable() {
    use note_pipelines::{get_note, save_note, update_system_config, Context, SaveNoteInput};
    let certs = certificates();
    let storage = Arc::new(
        note_storage_turso::TursoStorage::open(certs.dir.path().join("app.db"))
            .await
            .unwrap(),
    );
    let ctx = Arc::new(Context::new(
        storage,
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(note_attachments::FilesystemAttachmentStore::new(
            certs.dir.path().join("attachments"),
        )),
    ));
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "still usable".into(),
            content: "offline credentials".into(),
            attachments: vec![],
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap();
    update_system_config(
        &ctx,
        &note_core::SystemConfig {
            sync: note_core::sync::SyncConfig {
                enabled: true,
                connection_profile: Some("local".into()),
                rules: vec![note_core::sync::SyncRule {
                    id: "project".into(),
                    enabled: true,
                    selector: "project=bear_note".into(),
                }],
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut profile = certs.profile.clone();
    profile.client_key = certs.dir.path().join("secret-path.key");
    let runtime = SyncRuntime::new(&[profile.clone()]);
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(run(ctx.clone(), vec![profile], runtime.clone(), shutdown));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if runtime.status(&ctx).await.unwrap().state == "error" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let state = runtime.status(&ctx).await.unwrap();
    assert!(state.last_error.as_ref().unwrap().contains("private key"));
    assert!(!state.last_error.as_ref().unwrap().contains("secret-path"));
    assert_eq!(state.pending_notes, 1);
    assert_eq!(
        get_note(&ctx, &note.id).await.unwrap().unwrap().title,
        "still usable"
    );
    assert!(save_note(
        &ctx,
        SaveNoteInput {
            title: "new offline note".into(),
            content: "writes remain available".into(),
            attachments: vec![],
            labels: vec![]
        }
    )
    .await
    .is_ok());
    stop.send(true).unwrap();
    worker.await.unwrap();
}

struct PhoenixPeer {
    child: std::process::Child,
    log: std::path::PathBuf,
}
impl Drop for PhoenixPeer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn start_phoenix(
    certs: &Certificates,
    options: &[(&str, std::path::PathBuf)],
) -> PhoenixPeer {
    let log = certs.dir.path().join("review-phoenix.log");
    let output = std::fs::File::create(&log).unwrap();
    let port = reqwest::Url::parse(&certs.profile.url)
        .unwrap()
        .port()
        .unwrap();
    let mut command = std::process::Command::new("mix");
    command
        .args(["run", "--no-halt"])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phoenix-sync"),
        )
        .env("SYNC_FIXTURE_PORT", port.to_string())
        .env("SYNC_SERVER_CERT", certs.dir.path().join("server.pem"))
        .env("SYNC_SERVER_KEY", certs.dir.path().join("server.key"))
        .env("SYNC_CA_CERT", certs.dir.path().join("ca.pem"))
        .env("SYNC_COMMIT_FILE", certs.dir.path().join("committed"))
        .env("SYNC_ACK_FILE", certs.dir.path().join("ack"))
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    for (key, value) in options {
        command.env(key, value);
    }
    let mut peer = PhoenixPeer {
        child: command.spawn().unwrap(),
        log,
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            assert!(
                peer.child.try_wait().unwrap().is_none(),
                "Phoenix failed: {}",
                std::fs::read_to_string(&peer.log).unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    peer
}

#[tokio::test]
#[ignore = "requires compiled Phoenix 1.8.15 fixture and Mix"]
async fn real_phoenix_rejected_note_does_not_starve_inventory_across_reconnect() {
    use note_core::sync::*;
    use note_pipelines::{save_note, update_note, update_system_config, Context, SaveNoteInput};
    let mut certs = certificates();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    certs.profile.url = format!("wss://localhost:{port}/note-sync/websocket?vsn=2.0.0");
    let storage = Arc::new(
        note_storage_turso::TursoStorage::open(certs.dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let ctx = Arc::new(Context::new(
        storage,
        Arc::new(note_embedding::StubEmbedder),
        Arc::new(note_attachments::FilesystemAttachmentStore::new(
            certs.dir.path().join("attachments"),
        )),
    ));
    let mut notes = vec![];
    for title in ["first valid note", "second valid note"] {
        notes.push(
            save_note(
                &ctx,
                SaveNoteInput {
                    title: title.into(),
                    content: "valid payload".into(),
                    attachments: vec![],
                    labels: vec![("project".into(), "bear_note".into())],
                },
            )
            .await
            .unwrap(),
        );
    }
    notes.sort_by(|a, b| a.id.cmp(&b.id));
    let rejected = &notes[0];
    let accepted = &notes[1];
    update_system_config(
        &ctx,
        &note_core::SystemConfig {
            sync: SyncConfig {
                enabled: true,
                connection_profile: Some("local".into()),
                rules: vec![SyncRule {
                    id: "project".into(),
                    enabled: true,
                    selector: "project=bear_note".into(),
                }],
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rejections = certs.dir.path().join("rejections");
    let reconnect = certs.dir.path().join("reconnect");
    let scopes = certs.dir.path().join("scopes");
    let _peer = start_phoenix(
        &certs,
        &[
            ("SYNC_REJECT_NOTE_ID", rejected.id.clone().into()),
            ("SYNC_REJECTIONS_FILE", rejections.clone()),
            ("SYNC_RECONNECT_ONCE", reconnect.clone()),
            ("SYNC_SCOPES_FILE", scopes.clone()),
        ],
    )
    .await;
    let runtime = SyncRuntime::new(&[certs.profile.clone()]);
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(run(
        ctx.clone(),
        vec![certs.profile.clone()],
        runtime.clone(),
        shutdown,
    ));
    let accepted_path = certs.dir.path().join(format!("committed.{}", accepted.id));
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let joins = std::fs::read_to_string(&scopes)
                .unwrap_or_default()
                .lines()
                .count();
            let state = runtime.status(&ctx).await.unwrap();
            if joins >= 2
                && state.state == "connected"
                && accepted_path.exists()
                && state.pending_notes == 1
                && state.blocked_notes.len() == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("per-note rejection must allow later inventory and reconnect");
    assert_eq!(
        std::fs::read_to_string(&rejections)
            .unwrap()
            .lines()
            .count(),
        1,
        "same rejected event must not retry after automatic reconnect"
    );
    let blocked = runtime.status(&ctx).await.unwrap().blocked_notes;
    assert_eq!(blocked[0].note_id, rejected.id);
    assert_eq!(blocked[0].reason, "catalog_conflict");
    assert!(!certs
        .dir
        .path()
        .join(format!("committed.{}", rejected.id))
        .exists());
    update_note(
        &ctx,
        &accepted.id,
        accepted.revision,
        SaveNoteInput {
            title: "later queued version".into(),
            content: "valid payload after inventory".into(),
            attachments: vec![],
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let committed = std::fs::read(&accepted_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<SyncEvent>(&bytes).ok());
            if committed
                .and_then(|event| event.snapshot)
                .is_some_and(|snapshot| snapshot.title == "later queued version")
                && runtime.status(&ctx).await.unwrap().pending_notes == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a rejected pending note must not starve later queued notes");
    let changed = update_note(
        &ctx,
        &rejected.id,
        rejected.revision,
        SaveNoteInput {
            title: "new rejected version".into(),
            content: "updated valid note".into(),
            attachments: vec![],
            labels: vec![("project".into(), "bear_note".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let attempts = std::fs::read_to_string(&rejections).unwrap();
            let blocked = runtime.status(&ctx).await.unwrap().blocked_notes;
            if attempts.lines().count() >= 2
                && blocked
                    .first()
                    .is_some_and(|note| Some(note.event_id.as_str()) == attempts.lines().last())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runtime.status(&ctx).await.unwrap().blocked_notes[0].note_id,
        changed.id
    );
    runtime.retry();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(&rejections)
                .unwrap()
                .lines()
                .count()
                >= 3
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        note_pipelines::sync::sync_status(&ctx)
            .await
            .unwrap()
            .pending_notes,
        1
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
}
