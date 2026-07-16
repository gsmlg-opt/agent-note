use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};
use note_embedding::StubEmbedder;
use note_mcp::{
    delete_note_tool, list_notes_tool, save_note_tool, semantic_search_tool, SaveNoteToolInput,
    SemanticSearchToolInput,
};
use note_pipelines::{drain_embedding_jobs, update_system_config, Context};
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
async fn save_note_tool_returns_an_id() {
    let (ctx, _dir) = test_context().await;
    let result = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Title".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    assert!(!result.id.is_empty());
}

#[tokio::test]
async fn delete_note_tool_soft_deletes_once_and_hides_the_note() {
    let (ctx, _dir) = test_context().await;
    let note = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Delete through MCP".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert!(delete_note_tool(&ctx, &note.id).await.unwrap());
    assert!(!delete_note_tool(&ctx, &note.id).await.unwrap());
    assert!(list_notes_tool(&ctx, None, None, None)
        .await
        .unwrap()
        .is_empty());
    assert!(
        note_storage::note_exists(&ctx.storage.connect().unwrap(), &note.id)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn save_note_tool_honors_duplicate_check_config() {
    let (ctx, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &SystemConfig {
            duplicate_check: DuplicateCheckConfig {
                enabled: true,
                rules: vec![DuplicateCheckRule {
                    terms: vec![
                        DuplicateCheckTerm {
                            key: "skill-name".into(),
                            value: None,
                        },
                        DuplicateCheckTerm {
                            key: "version".into(),
                            value: None,
                        },
                    ],
                }],
            },
        },
    )
    .await
    .unwrap();
    let input = || SaveNoteToolInput {
        title: "Skill".into(),
        content: "Content".into(),
        attachments: vec![],
        labels: vec![
            ("skill-name".into(), "zddi-hooks".into()),
            ("version".into(), "1.0.0".into()),
        ],
    };

    save_note_tool(&ctx, input()).await.unwrap();
    let error = save_note_tool(&ctx, input()).await.unwrap_err();
    assert!(error.downcast_ref::<DuplicateNoteError>().is_some());
}

#[tokio::test]
async fn semantic_search_tool_finds_saved_note() {
    let (ctx, _dir) = test_context().await;
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Findable".into(),
            content: "unique searchable content".into(),
            attachments: vec![],
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
            attachments: vec![],
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
            attachments: vec![],
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
