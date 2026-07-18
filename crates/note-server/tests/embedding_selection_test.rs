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
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_note-server"));
    command
        .arg(mode)
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
