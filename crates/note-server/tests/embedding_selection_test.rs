use note_server::org_offline::{ManifestDocument, ManifestWorkspace, WorkspaceManifest};
use note_storage::StorageBackend;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;
use wiremock::MockServer;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

fn write_remote_config(dir: &Path, server: &MockServer) -> std::path::PathBuf {
    let config_path = dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "openai"
base_url = "{}"
api_key_env = "UNSET_FOR_TEST"

[attachments]
engine = "filesystem"
path = "attachments"
"#,
            server.uri()
        ),
    )
    .unwrap();
    config_path
}

fn read_pipe<R>(mut reader: R) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(bytes)
    })
}

async fn finish_reader(mut reader: JoinHandle<std::io::Result<Vec<u8>>>) -> Vec<u8> {
    match tokio::time::timeout(REAP_TIMEOUT, &mut reader).await {
        Ok(result) => result
            .expect("subprocess output reader must not panic")
            .expect("read subprocess output"),
        Err(_) => {
            reader.abort();
            let _ = reader.await;
            panic!("subprocess output reader did not finish after process exit");
        }
    }
}

async fn run_offline_mode(dir: &Path, config_path: &Path, mode: &str, input: &[u8]) -> Output {
    run_offline_args(dir, config_path, &[mode], input).await
}

async fn run_offline_args(dir: &Path, config_path: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_note-server"));
    command
        .args(args)
        .current_dir(dir)
        .env("NOTE_CONFIG_PATH", config_path)
        .env_remove("UNSET_FOR_TEST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let stdout_reader = read_pipe(child.stdout.take().unwrap());
    let stderr_reader = read_pipe(child.stderr.take().unwrap());

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).await.unwrap();
    drop(stdin);

    let status = match tokio::time::timeout(PROCESS_TIMEOUT, child.wait()).await {
        Ok(result) => result.expect("wait for note-server subprocess"),
        Err(_) => {
            stdout_reader.abort();
            stderr_reader.abort();
            let kill_result = child.start_kill();
            let reap_result = tokio::time::timeout(REAP_TIMEOUT, child.wait()).await;
            let _ = stdout_reader.await;
            let _ = stderr_reader.await;
            panic!("note-server subprocess timed out; kill={kill_result:?}, reap={reap_result:?}");
        }
    };

    Output {
        status,
        stdout: finish_reader(stdout_reader).await,
        stderr: finish_reader(stderr_reader).await,
    }
}

fn snapshot_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn write_org_snapshot(dir: &Path) -> std::path::PathBuf {
    const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000001";
    const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000001";
    let snapshot = dir.join("snapshot");
    std::fs::create_dir(&snapshot).unwrap();
    std::fs::create_dir(snapshot.join("documents")).unwrap();
    let source = b"#+TITLE: Storage only\r\nOpaque syntax.\r\n";
    std::fs::write(
        snapshot.join(format!("documents/{DOCUMENT_ID}.org")),
        source,
    )
    .unwrap();
    let manifest = WorkspaceManifest {
        format_version: 1,
        workspace: ManifestWorkspace {
            id: WORKSPACE_ID.parse().unwrap(),
            slug: "storage-only".into(),
            display_name: "Storage only".into(),
            description: "No embedding or attachment startup".into(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: note_org::WorkspacePolicy::engineering_default(),
            revision: 1,
            archived_at: None,
        },
        documents: vec![ManifestDocument {
            id: DOCUMENT_ID.parse().unwrap(),
            path: "main.org".into(),
            revision: 1,
            content_hash: snapshot_hash(source),
            file: format!("documents/{DOCUMENT_ID}.org"),
        }],
    };
    std::fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    snapshot
}

#[tokio::test]
async fn import_does_not_resolve_embedding_secret_or_contact_remote_endpoint() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config_path = write_remote_config(dir.path(), &server);
    let input = br#"{
        "version": 2,
        "label_keys": [],
        "notes": [{
            "id": "offline-import",
            "title": "Offline import",
            "content": "queues an embedding job",
            "attachments": [],
            "created_at": 1000,
            "updated_at": 1000,
            "labels": []
        }]
    }"#;

    let output = run_offline_mode(dir.path(), &config_path, "--import", input).await;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("1 embedding jobs queued"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn export_does_not_resolve_embedding_secret_or_contact_remote_endpoint() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config_path = write_remote_config(dir.path(), &server);

    let output = run_offline_mode(dir.path(), &config_path, "--export", &[]).await;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let export: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(export["version"], 2);
    assert_eq!(export["label_keys"], serde_json::json!([]));
    assert_eq!(export["notes"], serde_json::json!([]));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn all_four_org_offline_modes_are_storage_only() {
    const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000001";
    const DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000001";
    const SECOND_DOCUMENT_ID: &str = "20000000-0000-4000-8000-000000000002";
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config_path = write_remote_config(dir.path(), &server);
    let snapshot = write_org_snapshot(dir.path());

    let import_workspace = run_offline_args(
        dir.path(),
        &config_path,
        &[
            "org",
            "import-workspace",
            "--input",
            snapshot.to_str().unwrap(),
            "--mode",
            "create",
            "--actor-id",
            "offline-test",
            "--operation-id",
            "create-workspace",
        ],
        &[],
    )
    .await;
    assert!(
        import_workspace.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import_workspace.stderr)
    );

    let exported = dir.path().join("exported");
    let export_workspace = run_offline_args(
        dir.path(),
        &config_path,
        &[
            "org",
            "export-workspace",
            "--workspace-id",
            WORKSPACE_ID,
            "--output",
            exported.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert!(export_workspace.status.success());

    let exported_document = dir.path().join("exported-document.org");
    let export_document = run_offline_args(
        dir.path(),
        &config_path,
        &[
            "org",
            "export-document",
            "--document-id",
            DOCUMENT_ID,
            "--output",
            exported_document.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert!(export_document.status.success());

    let second = dir.path().join("second.org");
    std::fs::write(&second, b"#+TITLE: Second\nOpaque.\n").unwrap();
    let import_document = run_offline_args(
        dir.path(),
        &config_path,
        &[
            "org",
            "import-document",
            "--workspace-id",
            WORKSPACE_ID,
            "--document-id",
            SECOND_DOCUMENT_ID,
            "--path",
            "second.org",
            "--input",
            second.to_str().unwrap(),
            "--mode",
            "create",
            "--actor-id",
            "offline-test",
            "--operation-id",
            "create-document",
        ],
        &[],
    )
    .await;
    assert!(import_document.status.success());

    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!dir.path().join("attachments").exists());
    let storage = note_storage_turso::TursoStorage::open(dir.path().join("notes.db"))
        .await
        .unwrap();
    assert!(storage
        .session()
        .await
        .unwrap()
        .list_notes(&[], None, None)
        .await
        .unwrap()
        .is_empty());
}
