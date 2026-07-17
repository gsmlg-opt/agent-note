use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};
use note_embedding::StubEmbedder;
use note_mcp::{
    delete_note_tool, edit_note_tool, get_note_tool, list_notes_tool, read_note_lines_tool,
    save_note_tool, semantic_search_tool, update_note_tool, AttachmentData, SaveNoteToolInput,
    SemanticSearchToolInput, UpdateNoteToolInput,
};
use note_pipelines::{drain_embedding_jobs, update_system_config, Context, EditOp};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::Arc;
use tempfile::TempDir;

async fn test_context() -> (Context, Arc<dyn StorageBackend>, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        dir.path().join("attachments"),
    );
    (ctx, backend, dir)
}

#[tokio::test]
async fn save_note_tool_returns_an_id() {
    let (ctx, _backend, _dir) = test_context().await;
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
async fn binary_attachment_roundtrips_through_save_get_update_and_list() {
    let (ctx, _backend, dir) = test_context().await;
    let bytes = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
    let attachment = AttachmentData {
        id: "blob".into(),
        path: "./blob.bin".into(),
        mime: "application/octet-stream".into(),
        description: "raw bytes".into(),
        content: bytes.clone(),
    };
    let saved = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Binary".into(),
            content: "First line".into(),
            attachments: vec![attachment.clone()],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read(
            dir.path()
                .join("attachments")
                .join(&saved.id)
                .join("blob.bin")
        )
        .unwrap(),
        bytes
    );
    let fetched = get_note_tool(&ctx, &saved.id).await.unwrap().unwrap();
    assert_eq!(fetched.attachments, vec![attachment.clone()]);

    let updated = update_note_tool(
        &ctx,
        UpdateNoteToolInput {
            id: saved.id.clone(),
            title: "Binary updated".into(),
            content: "Updated body".into(),
            attachments: vec![attachment.clone()],
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.attachments, vec![attachment.clone()]);

    let listed = list_notes_tool(&ctx, None, None, None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].attachments, vec![attachment]);
}

#[tokio::test]
async fn line_edit_preserves_binary_attachment_bytes() {
    let (ctx, _backend, dir) = test_context().await;
    let bytes = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
    let saved = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Binary edit".into(),
            content: "First line".into(),
            attachments: vec![AttachmentData {
                id: "blob".into(),
                path: "./blob.bin".into(),
                mime: "application/octet-stream".into(),
                description: String::new(),
                content: bytes.clone(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let read = read_note_lines_tool(&ctx, &saved.id)
        .await
        .unwrap()
        .unwrap();

    let edited = edit_note_tool(
        &ctx,
        &saved.id,
        &read.tag,
        vec![EditOp::InsertTail {
            lines: vec!["Second line".into()],
        }],
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(edited.lines.len(), 2);
    assert_eq!(edited.lines[1].text, "Second line");
    let fetched = get_note_tool(&ctx, &saved.id).await.unwrap().unwrap();
    assert_eq!(fetched.content, "First line\nSecond line");
    assert_eq!(fetched.attachments[0].content, bytes);
    assert_eq!(
        std::fs::read(
            dir.path()
                .join("attachments")
                .join(&saved.id)
                .join("blob.bin")
        )
        .unwrap(),
        fetched.attachments[0].content
    );
}

#[tokio::test]
async fn delete_note_tool_soft_deletes_once_and_hides_the_note() {
    let (ctx, backend, _dir) = test_context().await;
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
    let session = backend.session().await.unwrap();
    assert!(session.note_exists(&note.id).await.unwrap());
}

#[tokio::test]
async fn save_note_tool_honors_duplicate_check_config() {
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
