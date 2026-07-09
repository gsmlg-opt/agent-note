use note_embedding::StubEmbedder;
use note_pipelines::{drain_embedding_jobs, save_note, search_notes, Context, SaveNoteInput};
use note_storage::Storage;
use std::sync::Arc;
use tempfile::TempDir;

async fn test_context() -> (Context, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    (Context::new(Arc::new(storage), Arc::new(StubEmbedder)), dir)
}

#[tokio::test]
async fn saved_note_is_findable_via_pipelines_directly() {
    let (ctx, _dir) = test_context().await;
    save_note(
        &ctx,
        SaveNoteInput {
            title: "T".into(),
            content: "C unique".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();
    let results = search_notes(&ctx, "C unique", 5).await.unwrap();
    assert!(!results.is_empty());
}
