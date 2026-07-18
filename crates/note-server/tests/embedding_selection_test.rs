use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use wiremock::MockServer;

#[tokio::test]
async fn import_does_not_resolve_embedding_secret_or_contact_remote_endpoint() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
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

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_note-server"))
        .arg("--import")
        .current_dir(dir.path())
        .env("NOTE_CONFIG_PATH", &config_path)
        .env_remove("UNSET_FOR_TEST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{
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
            }"#,
        )
        .await
        .unwrap();

    let output = child.wait_with_output().await.unwrap();
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
