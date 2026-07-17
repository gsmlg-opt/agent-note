use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, list_label_keys, Context};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::Arc;
use tempfile::TempDir;

async fn test_context() -> (Context, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    (
        Context::new(
            storage,
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
