use note_embedding::StubEmbedder;
use note_mcp::{save_note_tool, semantic_search_tool, SaveNoteToolInput, SemanticSearchToolInput};
use note_pipelines::{drain_embedding_jobs, Context};
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
async fn save_note_tool_returns_an_id() {
    let (ctx, _dir) = test_context().await;
    let result = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Title".into(),
            content: "Content".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    assert!(!result.id.is_empty());
}

#[tokio::test]
async fn semantic_search_tool_finds_saved_note() {
    let (ctx, _dir) = test_context().await;
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Findable".into(),
            content: "unique searchable content".into(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = semantic_search_tool(
        &ctx,
        SemanticSearchToolInput {
            query: "unique searchable content".into(),
            limit: 5,
            label: None,
        },
    )
    .await
    .unwrap();

    assert!(results.iter().any(|r| r.title == "Findable"));
}

#[tokio::test]
async fn semantic_search_tool_filters_by_label() {
    let (ctx, _dir) = test_context().await;
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Rust".into(),
            content: "shared searchable content".into(),
            labels: vec![("topic".into(), "rust".into())],
        },
    )
    .await
    .unwrap();
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Ops".into(),
            content: "shared searchable content".into(),
            labels: vec![("topic".into(), "ops".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = semantic_search_tool(
        &ctx,
        SemanticSearchToolInput {
            query: "shared searchable content".into(),
            limit: 5,
            label: Some("topic=rust".into()),
        },
    )
    .await
    .unwrap();

    assert!(results.iter().any(|r| r.title == "Rust"));
    assert!(!results.iter().any(|r| r.title == "Ops"));
}
