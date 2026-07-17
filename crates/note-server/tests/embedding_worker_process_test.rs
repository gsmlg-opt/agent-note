use note_embedding::{
    connect_client, EmbedInput, EmbedRequest, HandshakeRequest, ProcessWorkerConfig,
    ProcessWorkerRuntime, DEFAULT_MAX_RESPONSE_BYTES,
};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tarpc::context;
use tokio::process::{Child, Command};

#[tokio::test]
async fn embedding_worker_child_process_embeds_text() {
    let ipc_name = test_ipc_name("child");
    // Internal workers do not own application storage, so their early-return path must stay ahead
    // of runtime database configuration and validation.
    let mut child = spawn_worker(&ipc_name, &[("NOTE_DB_ENGINE", "pg")]).await;
    let client = connect_with_retry(&ipc_name).await;

    let handshake = client
        .handshake(context::current(), HandshakeRequest::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handshake.model.embedding_dimension, 1024);

    let response = client
        .embed(
            context::current(),
            EmbedRequest {
                request_id: "integration".into(),
                deadline_unix_ms: None,
                max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
                inputs: vec![EmbedInput {
                    job_id: 1,
                    text: "hello from parent".into(),
                }],
            },
        )
        .await
        .unwrap()
        .unwrap();

    let vectors = response.outputs[0].result.as_ref().unwrap();
    assert_eq!(vectors.dense.len(), 1024);
    assert!(!vectors.sparse.is_empty());

    let _ = client.shutdown(context::current()).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = child.start_kill();
}

#[tokio::test]
async fn process_supervisor_restarts_after_worker_crash() {
    let dir = tempfile::tempdir().unwrap();
    let crash_file = dir.path().join("crash-once");
    let mut config = ProcessWorkerConfig::from_env().unwrap();
    config.worker_exe = note_server_bin();
    config.ipc_name = test_ipc_name("restart");
    config.max_restarts = 2;
    config.connect_timeout = Duration::from_secs(5);
    config.request_timeout = Duration::from_secs(5);
    config.backoff_initial = Duration::from_millis(50);
    config.backoff_max = Duration::from_millis(200);
    config.extra_env.push((
        "NOTE_EMBEDDING_WORKER_CRASH_ONCE_FILE".to_string(),
        crash_file.display().to_string(),
    ));

    let runtime = ProcessWorkerRuntime::start(config).await.unwrap();
    let embedder = runtime.embedder();

    let _ = embedder.embed("first request may trigger crash").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (dense, sparse) = embedder
        .embed("second request after restart")
        .await
        .unwrap();

    assert_eq!(dense.len(), 1024);
    assert!(!sparse.is_empty());
    assert!(crash_file.exists());

    runtime.shutdown().await;
}

async fn spawn_worker(ipc_name: &str, extra_env: &[(&str, &str)]) -> Child {
    let mut command = Command::new(note_server_bin());
    command
        .arg("--internal-role")
        .arg("embedding-worker")
        .arg("--ipc-name")
        .arg(ipc_name)
        .env_remove("NOTE_MODEL_PATH");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.spawn().unwrap()
}

async fn connect_with_retry(ipc_name: &str) -> note_embedding::EmbeddingRpcClient {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match connect_client(ipc_name).await {
            Ok(client) => return client,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("connect worker {ipc_name}: {error:#}"),
        }
    }
}

fn note_server_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_note-server"))
}

fn test_ipc_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("agent-note-test-{prefix}-{}-{nanos}", std::process::id())
}
