use note_attachments::FilesystemAttachmentStore;
use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};
use note_embedding::StubEmbedder;
use note_mcp::{
    bulk_update_note_labels_tool, delete_note_attachment_tool, delete_note_tool, edit_note_tool,
    get_note_attachment_content_tool, get_note_tool, list_notes_tool, put_note_attachment_tool,
    read_note_lines_tool, save_note_tool, semantic_search_tool, update_note_tool,
    AttachmentMetadataData, BulkUpdateNoteLabelsToolInput, GetNoteAttachmentContentToolInput,
    NoteDetailData, NoteSummaryData, PutNoteAttachmentToolInput, PutNoteAttachmentToolOutput,
    SaveNoteToolInput, SemanticSearchToolInput, UpdateNoteToolInput,
};
use note_pipelines::{drain_embedding_jobs, update_system_config, Context, EditOp};
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use serde_json::json;
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
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    (ctx, backend, dir)
}

async fn save(ctx: &Context, title: &str, content: &str) -> String {
    save_note_tool(
        ctx,
        SaveNoteToolInput {
            title: title.into(),
            content: content.into(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .id
}

fn put_input(
    note_id: &str,
    expected_revision: i64,
    attachment_id: &str,
    path: &str,
    mime: &str,
    content: Vec<u8>,
) -> PutNoteAttachmentToolInput {
    PutNoteAttachmentToolInput {
        note_id: note_id.into(),
        expected_revision,
        attachment_id: attachment_id.into(),
        path: path.into(),
        mime: mime.into(),
        description: format!("{attachment_id} description"),
        content,
    }
}

#[test]
fn tool_input_deserialization_preserves_intended_compatibility() {
    assert!(serde_json::from_value::<SaveNoteToolInput>(json!({
        "title": "Title",
        "content": "Body"
    }))
    .is_ok());
    assert!(serde_json::from_value::<UpdateNoteToolInput>(json!({
        "id": "note-1",
        "expected_revision": 1,
        "title": "Title",
        "content": "Body"
    }))
    .is_ok());
    assert!(serde_json::from_value::<SemanticSearchToolInput>(json!({
        "query": "body",
        "limit": 5
    }))
    .is_ok());
    assert!(serde_json::from_value::<SaveNoteToolInput>(json!({
        "title": "Title",
        "content": "Body",
        "unknown": true
    }))
    .is_err());
    assert!(serde_json::from_value::<UpdateNoteToolInput>(json!({
        "id": "note-1",
        "expected_revision": 1,
        "title": "Title",
        "content": "Body",
        "unknown": true
    }))
    .is_err());
    assert!(serde_json::from_value::<UpdateNoteToolInput>(json!({
        "id": "note-1",
        "title": "Title",
        "content": "Body"
    }))
    .is_err());
    assert!(serde_json::from_value::<SemanticSearchToolInput>(json!({
        "query": "body",
        "limit": 5,
        "unknown": true
    }))
    .is_ok());
    assert!(
        serde_json::from_value::<GetNoteAttachmentContentToolInput>(json!({
            "note_id": "note-1",
            "attachment_id": "blob",
            "unknown": true
        }))
        .is_err()
    );
}

#[tokio::test]
async fn save_then_put_text_and_binary_attachments() {
    let (ctx, _backend, dir) = test_context().await;
    let note_id = save(&ctx, "Attachments", "Body").await;

    let text = put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            1,
            "text",
            "./text.txt",
            "text/plain",
            b"plain text\n".to_vec(),
        ),
    )
    .await
    .unwrap();
    let binary_bytes = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
    let binary = put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            2,
            "blob",
            "./blob.bin",
            "application/octet-stream",
            binary_bytes.clone(),
        ),
    )
    .await
    .unwrap();

    let PutNoteAttachmentToolOutput {
        created,
        attachment:
            AttachmentMetadataData {
                id,
                path,
                mime,
                description,
            },
        revision: _,
    } = text;
    assert!(created);
    assert_eq!(id, "text");
    assert_eq!(path, "./text.txt");
    assert_eq!(mime, "text/plain");
    assert_eq!(description, "text description");
    assert!(binary.created);
    assert_eq!(binary.attachment.id, "blob");
    assert_eq!(
        std::fs::read(
            dir.path()
                .join("attachments")
                .join(&note_id)
                .join("blob.bin")
        )
        .unwrap(),
        binary_bytes
    );

    let replacement_bytes = vec![3, 2, 1];
    let replacement = put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            3,
            "blob",
            "./blob.bin",
            "application/new-binary",
            replacement_bytes.clone(),
        ),
    )
    .await
    .unwrap();
    assert!(!replacement.created);
    assert_eq!(replacement.attachment.id, "blob");
    assert_eq!(replacement.attachment.mime, "application/new-binary");
    let selected = get_note_attachment_content_tool(
        &ctx,
        GetNoteAttachmentContentToolInput {
            note_id,
            attachment_id: "blob".into(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(selected.content, replacement_bytes);
}

#[tokio::test]
async fn get_update_list_and_line_read_do_not_require_attachment_content() {
    let (ctx, _backend, dir) = test_context().await;
    let note_id = save(&ctx, "Metadata", "First line").await;
    put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            1,
            "missing",
            "./missing.bin",
            "application/octet-stream",
            vec![1, 2, 3],
        ),
    )
    .await
    .unwrap();
    std::fs::remove_file(
        dir.path()
            .join("attachments")
            .join(&note_id)
            .join("missing.bin"),
    )
    .unwrap();

    let fetched = get_note_tool(&ctx, &note_id).await.unwrap().unwrap();
    assert_eq!(fetched.content, "First line");
    assert_eq!(fetched.attachments.len(), 1);
    assert_eq!(fetched.attachments[0].id, "missing");

    let updated = update_note_tool(
        &ctx,
        UpdateNoteToolInput {
            id: note_id.clone(),
            expected_revision: fetched.revision,
            title: "Metadata updated".into(),
            content: "Updated line".into(),
            labels: vec![("topic".into(), "rust".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.title, "Metadata updated");
    assert_eq!(updated.attachments, fetched.attachments);
    let NoteDetailData {
        id,
        title,
        content,
        attachments,
        labels,
        created_at,
        updated_at,
        revision: _,
    } = updated;
    assert_eq!(id, note_id);
    assert_eq!(title, "Metadata updated");
    assert_eq!(content, "Updated line");
    assert_eq!(attachments.len(), 1);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].key, "topic");
    assert_eq!(labels[0].value, "rust");
    assert!(created_at > 0);
    assert!(updated_at >= created_at);

    let lines = read_note_lines_tool(&ctx, &note_id).await.unwrap().unwrap();
    assert_eq!(lines.lines.len(), 1);
    assert_eq!(lines.lines[0].text, "Updated line");

    let listed = list_notes_tool(&ctx, None, None, None).await.unwrap();
    assert_eq!(listed.len(), 1);
    let NoteSummaryData {
        id,
        title,
        labels,
        created_at,
        updated_at,
        revision: _,
    } = listed.into_iter().next().unwrap();
    assert_eq!(id, note_id);
    assert_eq!(title, "Metadata updated");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].key, "topic");
    assert_eq!(labels[0].value, "rust");
    assert!(updated_at >= created_at);
}

#[tokio::test]
async fn line_edit_preserves_attachment_metadata_without_reading_bytes() {
    let (ctx, _backend, dir) = test_context().await;
    let note_id = save(&ctx, "Line edit", "First line").await;
    put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            1,
            "blob",
            "./blob.bin",
            "application/octet-stream",
            vec![0, 1, 2],
        ),
    )
    .await
    .unwrap();
    std::fs::remove_file(
        dir.path()
            .join("attachments")
            .join(&note_id)
            .join("blob.bin"),
    )
    .unwrap();
    let read = read_note_lines_tool(&ctx, &note_id).await.unwrap().unwrap();

    let edited = edit_note_tool(
        &ctx,
        &note_id,
        read.revision,
        &read.tag,
        vec![EditOp::InsertTail {
            lines: vec!["Second line".into()],
        }],
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(edited.lines.len(), 2);
    let fetched = get_note_tool(&ctx, &note_id).await.unwrap().unwrap();
    assert_eq!(fetched.content, "First line\nSecond line");
    assert_eq!(fetched.attachments[0].id, "blob");
}

#[tokio::test]
async fn selected_attachment_content_read_does_not_read_siblings() {
    let (ctx, _backend, dir) = test_context().await;
    let note_id = save(&ctx, "Selected read", "Body").await;
    put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            1,
            "selected",
            "./selected.txt",
            "text/plain",
            b"selected bytes".to_vec(),
        ),
    )
    .await
    .unwrap();
    put_note_attachment_tool(
        &ctx,
        put_input(
            &note_id,
            2,
            "missing-sibling",
            "./missing.bin",
            "application/octet-stream",
            vec![9, 8, 7],
        ),
    )
    .await
    .unwrap();
    std::fs::remove_file(
        dir.path()
            .join("attachments")
            .join(&note_id)
            .join("missing.bin"),
    )
    .unwrap();

    let selected = get_note_attachment_content_tool(
        &ctx,
        GetNoteAttachmentContentToolInput {
            note_id,
            attachment_id: "selected".into(),
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(selected.attachment.id, "selected");
    assert_eq!(selected.content, b"selected bytes");
}

#[tokio::test]
async fn delete_attachment_is_idempotent_and_leaves_siblings_intact() {
    let (ctx, _backend, _dir) = test_context().await;
    let note_id = save(&ctx, "Delete attachment", "Body").await;
    for (id, path, content) in [
        ("remove", "./remove.txt", b"remove".to_vec()),
        ("keep", "./keep.txt", b"keep".to_vec()),
    ] {
        put_note_attachment_tool(
            &ctx,
            put_input(
                &note_id,
                if id == "remove" { 1 } else { 2 },
                id,
                path,
                "text/plain",
                content,
            ),
        )
        .await
        .unwrap();
    }

    assert!(delete_note_attachment_tool(&ctx, &note_id, "remove", 3)
        .await
        .unwrap());
    assert!(!delete_note_attachment_tool(&ctx, &note_id, "remove", 4)
        .await
        .unwrap());
    let sibling = get_note_attachment_content_tool(
        &ctx,
        GetNoteAttachmentContentToolInput {
            note_id: note_id.clone(),
            attachment_id: "keep".into(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sibling.content, b"keep");
    let fetched = get_note_tool(&ctx, &note_id).await.unwrap().unwrap();
    assert_eq!(fetched.attachments.len(), 1);
    assert_eq!(fetched.attachments[0].id, "keep");
}

#[tokio::test]
async fn delete_note_tool_soft_deletes_once_and_hides_the_note() {
    let (ctx, backend, _dir) = test_context().await;
    let note_id = save(&ctx, "Delete through MCP", "Content").await;

    assert!(delete_note_tool(&ctx, &note_id, 1).await.unwrap());
    assert!(delete_note_tool(&ctx, &note_id, 1).await.is_err());
    assert!(list_notes_tool(&ctx, None, None, None)
        .await
        .unwrap()
        .is_empty());
    let session = backend.session().await.unwrap();
    assert!(session.note_exists(&note_id).await.unwrap());
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
async fn semantic_search_returns_summary_labels_and_timestamps() {
    let (ctx, _backend, _dir) = test_context().await;
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Findable".into(),
            content: "unique searchable content".into(),
            labels: vec![("topic".into(), "rust".into())],
        },
    )
    .await
    .unwrap();
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Excluded".into(),
            content: "unique searchable content".into(),
            labels: vec![("topic".into(), "ops".into())],
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
            label: Some("topic=rust".into()),
        },
    )
    .await
    .unwrap();
    assert!(!results.iter().any(|result| result.title == "Excluded"));
    let result = results
        .into_iter()
        .find(|result| result.title == "Findable")
        .unwrap();

    assert_eq!(result.labels.len(), 1);
    assert_eq!(result.labels[0].key, "topic");
    assert_eq!(result.labels[0].value, "rust");
    assert!(result.created_at > 0);
    assert!(result.updated_at >= result.created_at);
}

#[tokio::test]
async fn bulk_update_note_labels_tool_delegates_matching_and_set_semantics() {
    let (ctx, _backend, _dir) = test_context().await;
    let added = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Add project".into(),
            content: "Body".into(),
            labels: vec![
                ("type".into(), "ietf-rfc".into()),
                ("owner".into(), "protocols".into()),
            ],
        },
    )
    .await
    .unwrap()
    .id;
    let replaced = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Replace project".into(),
            content: "Body".into(),
            labels: vec![
                ("type".into(), "ietf-rfc".into()),
                ("project".into(), "old".into()),
                ("owner".into(), "protocols".into()),
            ],
        },
    )
    .await
    .unwrap()
    .id;
    save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Unmatched".into(),
            content: "Body".into(),
            labels: vec![("type".into(), "other".into())],
        },
    )
    .await
    .unwrap();

    let output = bulk_update_note_labels_tool(
        &ctx,
        BulkUpdateNoteLabelsToolInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("project".into(), "IETF-RFC".into())],
            remove: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(output.matched, 2);
    assert_eq!(output.updated, 2);
    assert_eq!(output.unchanged, 0);
    for id in [added, replaced] {
        let note = get_note_tool(&ctx, &id).await.unwrap().unwrap();
        assert!(note
            .labels
            .iter()
            .any(|label| label.key == "project" && label.value == "IETF-RFC"));
        assert!(note
            .labels
            .iter()
            .any(|label| label.key == "owner" && label.value == "protocols"));
    }
}

#[tokio::test]
async fn bulk_update_note_labels_tool_delegates_remove_semantics() {
    let (ctx, _backend, _dir) = test_context().await;
    let id = save_note_tool(
        &ctx,
        SaveNoteToolInput {
            title: "Remove type".into(),
            content: "Body".into(),
            labels: vec![
                ("type".into(), "ietf-rfc".into()),
                ("owner".into(), "protocols".into()),
            ],
        },
    )
    .await
    .unwrap()
    .id;

    let output = bulk_update_note_labels_tool(
        &ctx,
        BulkUpdateNoteLabelsToolInput {
            selector: "type=ietf-rfc".into(),
            set: vec![],
            remove: vec!["type".into()],
        },
    )
    .await
    .unwrap();

    assert_eq!(output.matched, 1);
    assert_eq!(output.updated, 1);
    assert_eq!(output.unchanged, 0);
    let note = get_note_tool(&ctx, &id).await.unwrap().unwrap();
    assert!(!note.labels.iter().any(|label| label.key == "type"));
    assert!(note
        .labels
        .iter()
        .any(|label| label.key == "owner" && label.value == "protocols"));
}
