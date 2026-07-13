use note_core::NoteAttachment;
use note_embedding::StubEmbedder;
use note_pipelines::{
    define_label_key, export_data, get_note, import_data, list_label_keys, save_note, Context,
    SaveNoteInput,
};
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
async fn export_import_roundtrips_notes_and_label_keys() {
    let (source, _source_dir) = test_context().await;
    define_label_key(&source, "status", "Workflow status")
        .await
        .unwrap();
    let labeled = save_note(
        &source,
        SaveNoteInput {
            title: "Plan".into(),
            content: "Draft the release plan".into(),
            attachments: vec![NoteAttachment {
                id: "meta".into(),
                path: "./meta.json".into(),
                mime: "application/json".into(),
                description: "metadata".into(),
                content: "{}".into(),
            }],
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

    let data = export_data(&source).await.unwrap();
    assert_eq!(data.version, 1);
    assert_eq!(data.notes.len(), 2);

    let (target, _target_dir) = test_context().await;
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

    let restored_labeled = get_note(&target, &labeled.id).await.unwrap().unwrap();
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

    let restored_plain = get_note(&target, &plain.id).await.unwrap().unwrap();
    assert_eq!(restored_plain.title, plain.title);
    assert_eq!(restored_plain.content, plain.content);
    assert_eq!(restored_plain.created_at, plain.created_at);
    assert_eq!(restored_plain.updated_at, plain.updated_at);
    assert!(restored_plain.labels.is_empty());
}
