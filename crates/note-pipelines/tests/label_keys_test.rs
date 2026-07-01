use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, list_label_keys, Context};
use note_storage::Storage;
use std::sync::Arc;
use tempfile::TempDir;

// Returns the TempDir guard alongside the Context so the caller keeps it alive:
// dropping it deletes the DB directory and later connect() calls fail with SQLITE_CANTOPEN.
async fn test_context() -> (Context, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));
    (ctx, dir)
}

#[tokio::test]
async fn define_then_list_roundtrips() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();
    let keys = list_label_keys(&ctx).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
}

#[tokio::test]
async fn empty_description_is_rejected() {
    let (ctx, _dir) = test_context().await;
    let result = define_label_key(&ctx, "status", "").await;
    assert!(result.is_err());
}
