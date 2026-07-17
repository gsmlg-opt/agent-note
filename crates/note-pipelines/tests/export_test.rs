mod support;

use note_core::NoteAttachment;
use note_pipelines::{
    define_label_key, delete_note, export_data, get_note, import_data, import_json, list_all_notes,
    list_deleted_note_summaries, list_label_keys, save_note, SaveNoteInput,
};
use support::test_context;

#[tokio::test]
async fn export_import_roundtrips_notes_and_label_keys() {
    let (source, _source_backend, _source_dir) = test_context().await;
    define_label_key(&source, "status", "Workflow status")
        .await
        .unwrap();
    let labeled = save_note(
        &source,
        SaveNoteInput {
            title: "Plan".into(),
            content: "Draft the release plan".into(),
            attachments: vec![
                NoteAttachment {
                    id: "meta".into(),
                    path: "./meta.json".into(),
                    mime: "application/json".into(),
                    description: "metadata".into(),
                    content: b"{}".to_vec(),
                },
                NoteAttachment {
                    id: "binary".into(),
                    path: "./blob.bin".into(),
                    mime: "application/octet-stream".into(),
                    description: "binary data".into(),
                    content: vec![0, 159, 146, 150],
                },
            ],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();
    let plain = save_note(
        &source,
        SaveNoteInput {
            title: "Scratch".into(),
            content: "Unlabeled thought".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&source, &labeled.id).await.unwrap();

    let data = export_data(&source).await.unwrap();
    assert_eq!(data.version, 2);
    assert_eq!(data.notes.len(), 2);
    let deleted_export = data
        .notes
        .iter()
        .find(|note| note.id == labeled.id)
        .unwrap();
    assert!(deleted_export.deleted_at.is_some());
    assert_eq!(deleted_export.attachments[0].content.as_deref(), Some("{}"));
    assert_eq!(
        deleted_export.attachments[0].content_base64.as_deref(),
        Some("e30=")
    );
    assert!(deleted_export.attachments[1].content.is_none());
    assert_eq!(
        deleted_export.attachments[1].content_base64.as_deref(),
        Some("AJ+Slg==")
    );

    let (target, _target_backend, target_dir) = test_context().await;
    let stats = import_data(&target, data).await.unwrap();
    assert_eq!(stats.notes_added, 2);
    assert_eq!(stats.notes_skipped, 0);
    assert_eq!(stats.label_keys_added, 1);
    assert!(stats.embedding_jobs_queued > 0);

    let keys = list_label_keys(&target).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].description, "Workflow status");
    assert_eq!(keys[0].value_type.as_str(), "text");

    assert!(get_note(&target, &labeled.id).await.unwrap().is_none());
    let deleted = list_deleted_note_summaries(&target).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, labeled.id);
    let restored_notes = list_all_notes(&target).await.unwrap();
    let restored_labeled = restored_notes
        .iter()
        .find(|note| note.id == labeled.id)
        .unwrap();
    assert_eq!(restored_labeled.title, labeled.title);
    assert_eq!(restored_labeled.content, labeled.content);
    assert_eq!(restored_labeled.created_at, labeled.created_at);
    assert_eq!(restored_labeled.updated_at, labeled.updated_at);
    assert_eq!(restored_labeled.attachments, labeled.attachments);
    assert_eq!(restored_labeled.labels.len(), 1);
    assert_eq!(restored_labeled.labels[0].key, "status");
    assert_eq!(restored_labeled.labels[0].value, "done");
    assert_eq!(restored_labeled.labels[0].description, "Workflow status");
    assert_eq!(restored_labeled.labels[0].value_type.as_str(), "text");
    assert!(restored_labeled.deleted_at.is_some());
    assert_eq!(
        std::fs::read_to_string(
            target_dir
                .path()
                .join("attachments")
                .join(&labeled.id)
                .join("meta.json")
        )
        .unwrap(),
        "{}"
    );

    let restored_plain = get_note(&target, &plain.id).await.unwrap().unwrap();
    assert_eq!(restored_plain.title, plain.title);
    assert_eq!(restored_plain.content, plain.content);
    assert_eq!(restored_plain.created_at, plain.created_at);
    assert_eq!(restored_plain.updated_at, plain.updated_at);
    assert!(restored_plain.labels.is_empty());
}

#[tokio::test]
async fn imports_version_one_text_attachments() {
    let (ctx, _backend, _dir) = test_context().await;
    let input = r#"{
        "version": 1,
        "label_keys": [],
        "notes": [{
            "id": "legacy-note",
            "title": "Legacy",
            "content": "Imported note",
            "attachments": [{
                "id": "text",
                "path": "legacy.txt",
                "mime": "text/plain",
                "description": "legacy attachment",
                "content": "legacy text"
            }],
            "created_at": 1000,
            "updated_at": 1000,
            "labels": []
        }]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();
    assert_eq!(stats.notes_added, 1);
    let note = get_note(&ctx, "legacy-note").await.unwrap().unwrap();
    assert_eq!(note.attachments[0].content, b"legacy text");
}

#[tokio::test]
async fn rejects_unsupported_export_versions() {
    let (ctx, _backend, _dir) = test_context().await;
    let error = import_json(&ctx, r#"{"version":3,"label_keys":[],"notes":[]}"#)
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "unsupported export version: 3");
}

#[tokio::test]
async fn invalid_version_two_attachment_rolls_back_the_import() {
    let (ctx, _backend, dir) = test_context().await;
    let input = r#"{
        "version": 2,
        "label_keys": [{"key":"status","description":"Workflow status","value_type":"text"}],
        "notes": [
            {
                "id": "valid-note",
                "title": "Valid",
                "content": "Would otherwise import",
                "attachments": [{
                    "id": "blob",
                    "path": "blob.bin",
                    "mime": "application/octet-stream",
                    "content_base64": "AP8="
                }],
                "created_at": 1000,
                "updated_at": 1000,
                "labels": [["status", "done"]]
            },
            {
                "id": "invalid-note",
                "title": "Invalid",
                "content": "Reject the whole import",
                "attachments": [{
                    "id": "text",
                    "path": "text.txt",
                    "mime": "text/plain",
                    "content": "text",
                    "content_base64": "b3RoZXI="
                }],
                "created_at": 1000,
                "updated_at": 1000,
                "labels": []
            }
        ]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("attachment content and content_base64 do not match"));
    assert!(list_all_notes(&ctx).await.unwrap().is_empty());
    assert!(list_label_keys(&ctx).await.unwrap().is_empty());

    let attachments_dir = dir.path().join("attachments");
    assert!(
        !attachments_dir.exists() || std::fs::read_dir(attachments_dir).unwrap().next().is_none()
    );
}
