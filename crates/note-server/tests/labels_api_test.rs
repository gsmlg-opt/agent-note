use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, list_label_keys, Context};
use note_storage::Storage;
use std::sync::Arc;
use tempfile::TempDir;

async fn test_context() -> (Context, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    (
        Context::with_attachment_dir(
            Arc::new(storage),
            Arc::new(StubEmbedder),
            dir.path().join("attachments"),
        ),
        dir,
    )
}

#[tokio::test]
async fn define_then_list_labels_via_pipelines() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();
    let keys = list_label_keys(&ctx).await.unwrap();
    assert!(keys.iter().any(|k| k.key == "status"));
}
