mod support;

use note_attachments::{
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentMutation, PreparedAttachmentSet,
};
use note_core::{LabelKeyValidationError, LabelValueType, NoteAttachment};
use note_embedding::StubEmbedder;
use note_pipelines::{
    compute_tag, define_label_key, define_label_key_with_type, delete_note, drain_embedding_jobs,
    edit_note, get_note, get_note_attachment, get_note_metadata, import_json,
    list_deleted_note_summaries, list_label_keys, permanently_delete_note,
    purge_expired_deleted_notes, restore_notes, save_note, update_note, update_note_fields,
    update_system_config, Context, EditOp, NoteMutationError, RestoreNoteInput, SaveNoteInput,
    UpdateNoteFieldsInput, TRASH_RETENTION_SECONDS,
};
use note_storage::{NoteMutationResult, StorageBackend, TransactionMode};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use support::{
    event_log, test_context, ControlledAttachmentStore, EventLog, EventNotifier,
    EventStorageBackend,
};

use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};

type AttachmentObjects = Arc<Mutex<HashMap<(String, String), Vec<u8>>>>;

#[derive(Default)]
struct RecordingAttachmentStore {
    objects: AttachmentObjects,
    reads: Arc<Mutex<Vec<(String, String)>>>,
}

struct RecordingPreparedSet {
    note_id: String,
    metadata: Vec<NoteAttachment>,
    objects: AttachmentObjects,
    content: Vec<(String, Vec<u8>)>,
}

struct RecordingPreparedMutation {
    note_id: String,
    path: String,
    content: Option<Vec<u8>>,
    objects: AttachmentObjects,
}

struct PanicAttachmentStore;

#[async_trait::async_trait]
impl AttachmentStore for PanicAttachmentStore {
    async fn prepare(
        &self,
        _note_id: &str,
        _attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
        panic!("metadata-only workflows must not prepare attachments")
    }

    async fn prepare_put(
        &self,
        _note_id: &str,
        _attachment: &NoteAttachment,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        panic!("metadata-only workflows must not prepare an attachment put")
    }

    async fn prepare_delete(
        &self,
        _note_id: &str,
        _path: &str,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        panic!("metadata-only workflows must not prepare an attachment delete")
    }

    async fn read(&self, _note_id: &str, _path: &str) -> anyhow::Result<Vec<u8>> {
        panic!("metadata-only workflows must not read attachments")
    }

    async fn hydrate(
        &self,
        _note_id: &str,
        _attachments: &mut [NoteAttachment],
    ) -> anyhow::Result<()> {
        panic!("metadata-only workflows must not hydrate attachments")
    }

    async fn remove_note(&self, _note_id: &str) -> anyhow::Result<()> {
        panic!("metadata-only workflows must not remove attachments")
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "panic".into(),
            location: None,
        }
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentSet for RecordingPreparedSet {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let mut objects = self.objects.lock().unwrap();
        objects.retain(|(note_id, _), _| note_id != &self.note_id);
        for (path, content) in self.content {
            objects.insert((self.note_id.clone(), path), content);
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl PreparedAttachmentMutation for RecordingPreparedMutation {
    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let mut objects = self.objects.lock().unwrap();
        let key = (self.note_id, self.path);
        if let Some(content) = self.content {
            objects.insert(key, content);
        } else {
            objects.remove(&key);
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl AttachmentStore for RecordingAttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>> {
        Ok(Box::new(RecordingPreparedSet {
            note_id: note_id.to_string(),
            metadata: attachments
                .iter()
                .map(|attachment| NoteAttachment {
                    id: attachment.id.clone(),
                    path: attachment.path.clone(),
                    mime: attachment.mime.clone(),
                    description: attachment.description.clone(),
                    content: Vec::new(),
                })
                .collect(),
            objects: self.objects.clone(),
            content: attachments
                .iter()
                .map(|attachment| (attachment.path.clone(), attachment.content.clone()))
                .collect(),
        }))
    }

    async fn prepare_put(
        &self,
        note_id: &str,
        attachment: &NoteAttachment,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        Ok(Box::new(RecordingPreparedMutation {
            note_id: note_id.to_string(),
            path: attachment.path.clone(),
            content: Some(attachment.content.clone()),
            objects: self.objects.clone(),
        }))
    }

    async fn prepare_delete(
        &self,
        note_id: &str,
        path: &str,
    ) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>> {
        Ok(Box::new(RecordingPreparedMutation {
            note_id: note_id.to_string(),
            path: path.to_string(),
            content: None,
            objects: self.objects.clone(),
        }))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.reads
            .lock()
            .unwrap()
            .push((note_id.to_string(), path.to_string()));
        self.objects
            .lock()
            .unwrap()
            .get(&(note_id.to_string(), path.to_string()))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing test attachment"))
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        self.objects
            .lock()
            .unwrap()
            .retain(|(stored_note_id, _), _| stored_note_id != note_id);
        Ok(())
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "recording".into(),
            location: None,
        }
    }
}

async fn recording_context() -> (
    Context,
    Arc<dyn StorageBackend>,
    Arc<RecordingAttachmentStore>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments = Arc::new(RecordingAttachmentStore::default());
    let ctx = Context::new(backend.clone(), Arc::new(StubEmbedder), attachments.clone());
    (ctx, backend, attachments, dir)
}

async fn controlled_context(
    with_notifier: bool,
) -> (
    Context,
    Arc<dyn StorageBackend>,
    Arc<ControlledAttachmentStore>,
    EventLog,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let raw_backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let backend: Arc<dyn StorageBackend> = Arc::new(EventStorageBackend::new(
        raw_backend.clone(),
        events.clone(),
    ));
    let attachments = Arc::new(ControlledAttachmentStore::new(
        backend.clone(),
        events.clone(),
    ));
    let ctx = if with_notifier {
        Context::with_embedding_job_notifier(
            backend,
            Arc::new(StubEmbedder),
            note_embedding::EmbeddingBackendInfo::local_bge_m3(),
            Arc::new(EventNotifier::new(events.clone())),
            attachments.clone(),
        )
    } else {
        Context::new(backend, Arc::new(StubEmbedder), attachments.clone())
    };
    (ctx, raw_backend, attachments, events, dir)
}

fn one_attachment() -> Vec<NoteAttachment> {
    vec![NoteAttachment {
        id: "file".into(),
        path: "./file.txt".into(),
        mime: "text/plain".into(),
        description: String::new(),
        content: b"payload".to_vec(),
    }]
}

#[tokio::test]
async fn save_and_update_order_attachment_finalization_around_the_database_transaction() {
    let (ctx, _backend, _attachments, events, _dir) = controlled_context(true).await;

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Before".into(),
            content: "Initial content".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("prepare:{}:missing", note.id),
            "begin".into(),
            "commit".into(),
            format!("publish:{}:Before", note.id),
            "wake".into(),
        ]
    );

    events.lock().unwrap().clear();
    update_note(
        &ctx,
        &note.id,
        note.revision,
        SaveNoteInput {
            title: "After".into(),
            content: "Updated content".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("prepare:{}:Before", note.id),
            "begin".into(),
            "commit".into(),
            format!("publish:{}:After", note.id),
            "wake".into(),
        ]
    );
}

async fn enable_duplicate_series_rule(ctx: &Context) {
    update_system_config(
        ctx,
        &SystemConfig {
            category_labels: Vec::new(),
            duplicate_check: DuplicateCheckConfig {
                enabled: true,
                rules: vec![DuplicateCheckRule {
                    terms: vec![DuplicateCheckTerm {
                        key: "series".into(),
                        value: Some("same".into()),
                    }],
                }],
            },
        },
    )
    .await
    .unwrap();
}

async fn save_existing_duplicate(ctx: &Context) {
    save_note(
        ctx,
        SaveNoteInput {
            title: "Existing".into(),
            content: "Existing content".into(),
            attachments: one_attachment(),
            labels: vec![("series".into(), "same".into())],
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn save_transaction_failure_rolls_back_then_aborts_prepared_attachments() {
    let (ctx, _backend, _attachments, events, _dir) = controlled_context(false).await;
    enable_duplicate_series_rule(&ctx).await;
    save_existing_duplicate(&ctx).await;
    events.lock().unwrap().clear();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Duplicate".into(),
            content: "Duplicate content".into(),
            attachments: one_attachment(),
            labels: vec![("series".into(), "same".into())],
        },
    )
    .await
    .unwrap_err();

    assert!(error.downcast_ref::<DuplicateNoteError>().is_some());
    let events = events.lock().unwrap().clone();
    assert!(events[0].starts_with("prepare:"));
    assert!(events[0].ends_with(":missing"));
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "rollback");
    assert!(events[3].starts_with("abort:"));
}

#[tokio::test]
async fn attachment_publish_failure_keeps_the_committed_note_but_does_not_wake_the_worker() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(true).await;
    attachments.fail_publish();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Committed".into(),
            content: "Queue work only after publication".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "controlled publish failure");
    let events = events.lock().unwrap().clone();
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "commit");
    assert!(events[3].contains(":Committed"));
    assert!(!events.iter().any(|event| event == "wake"));
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_all_notes()
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn permanent_delete_and_purge_remove_attachments_only_after_database_deletion() {
    let (ctx, backend, _attachments, events, _dir) = controlled_context(false).await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete".into(),
            content: "Permanent deletion".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    assert!(delete_note(&ctx, &note.id, note.revision).await.unwrap());
    events.lock().unwrap().clear();

    assert!(
        note_pipelines::permanently_delete_note(&ctx, &note.id, note.revision + 1)
            .await
            .unwrap()
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec![format!("remove:{}:absent=true", note.id)]
    );
    events.lock().unwrap().clear();
    assert!(note_pipelines::permanently_delete_note(&ctx, "missing", 1)
        .await
        .is_err());
    assert!(events.lock().unwrap().is_empty());

    let expired = save_note(
        &ctx,
        SaveNoteInput {
            title: "Expired".into(),
            content: "Purge deletion".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let now = chrono::Utc::now().timestamp();
    backend
        .session()
        .await
        .unwrap()
        .soft_delete_note(&expired.id, 1, now.saturating_sub(TRASH_RETENTION_SECONDS))
        .await
        .unwrap();
    events.lock().unwrap().clear();

    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 1);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "begin".into(),
            "commit".into(),
            format!("remove:{}:absent=true", expired.id),
        ]
    );
}

#[tokio::test]
async fn abort_failure_adds_context_without_hiding_the_primary_transaction_error() {
    let (ctx, _backend, attachments, events, _dir) = controlled_context(false).await;
    enable_duplicate_series_rule(&ctx).await;
    save_existing_duplicate(&ctx).await;
    events.lock().unwrap().clear();
    attachments.fail_abort();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Duplicate".into(),
            content: "Duplicate content".into(),
            attachments: one_attachment(),
            labels: vec![("series".into(), "same".into())],
        },
    )
    .await
    .unwrap_err();

    let chain = format!("{error:#}");
    assert!(chain.contains("attachment abort also failed"));
    assert!(chain.contains("controlled abort failure"));
    assert!(error
        .chain()
        .any(|cause| cause.downcast_ref::<DuplicateNoteError>().is_some()));
    let events = events.lock().unwrap();
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "rollback");
    assert!(events[3].starts_with("abort:"));
}

#[tokio::test]
async fn get_note_hydrates_attachments_through_the_injected_store() {
    let (ctx, _backend, attachments, _dir) = recording_context().await;
    import_json(
        &ctx,
        r#"{
            "version": 2,
            "label_keys": [],
            "notes": [{
                "id": "note-1",
                "title": "Recorded",
                "content": "Attachment indirection",
                "attachments": [{
                    "id": "file",
                    "path": "./file.txt",
                    "mime": "text/plain",
                    "content": "payload"
                }],
                "created_at": 1,
                "updated_at": 1,
                "labels": []
            }]
        }"#,
    )
    .await
    .unwrap();
    attachments.reads.lock().unwrap().clear();

    let note = get_note(&ctx, "note-1").await.unwrap().unwrap();

    assert_eq!(note.attachments[0].content, b"payload");
    assert_eq!(
        *attachments.reads.lock().unwrap(),
        vec![("note-1".into(), "./file.txt".into())]
    );
}

#[tokio::test]
async fn get_note_metadata_returns_content_and_unhydrated_attachment_metadata() {
    let (write_ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &write_ctx,
        SaveNoteInput {
            title: "Metadata".into(),
            content: "Metadata-only content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: "proof metadata".into(),
                content: b"payload".to_vec(),
            }],
            labels: vec![("topic".into(), "metadata".into())],
        },
    )
    .await
    .unwrap();
    let ctx = Context::new(
        backend,
        Arc::new(StubEmbedder),
        Arc::new(PanicAttachmentStore),
    );

    let note = get_note_metadata(&ctx, &saved.id).await.unwrap().unwrap();

    assert_eq!(note.content, "Metadata-only content");
    assert_eq!(note.labels, saved.labels);
    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].id, "proof");
    assert_eq!(note.attachments[0].path, "./proof.txt");
    assert_eq!(note.attachments[0].mime, "text/plain");
    assert_eq!(note.attachments[0].description, "proof metadata");
    assert!(note.attachments[0].content.is_empty());
}

#[tokio::test]
async fn get_note_attachment_reads_only_the_selected_object() {
    let (ctx, _backend, attachments, _dir) = recording_context().await;
    import_json(
        &ctx,
        r#"{
            "version": 2,
            "label_keys": [],
            "notes": [{
                "id": "note-1",
                "title": "Recorded",
                "content": "Attachment indirection",
                "attachments": [
                    {"id":"first","path":"./file.txt","mime":"text/plain","content":"payload"},
                    {"id":"second","path":"./sibling.txt","mime":"text/plain","content":"sibling"}
                ],
                "created_at": 1,
                "updated_at": 1,
                "labels": []
            }]
        }"#,
    )
    .await
    .unwrap();
    attachments.reads.lock().unwrap().clear();

    let attachment = get_note_attachment(&ctx, "note-1", "file.txt")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(attachment.path, "./file.txt");
    assert_eq!(attachment.content, b"payload");
    assert_eq!(
        *attachments.reads.lock().unwrap(),
        vec![("note-1".into(), "./file.txt".into())]
    );
    assert!(get_note_attachment(&ctx, "note-1", "missing.txt")
        .await
        .unwrap()
        .is_none());
    assert!(get_note_attachment(&ctx, "missing-note", "file.txt")
        .await
        .unwrap()
        .is_none());
    assert!(get_note_attachment(&ctx, "note-1", "../file.txt")
        .await
        .unwrap()
        .is_none());
    assert!(get_note_attachment(&ctx, "note-1", "")
        .await
        .unwrap()
        .is_none());
    assert_eq!(attachments.reads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn get_note_attachment_matches_forward_and_backslash_separators() {
    let (ctx, _backend, attachments, _dir) = recording_context().await;
    import_json(
        &ctx,
        r#"{
            "version": 2,
            "label_keys": [],
            "notes": [
                {
                    "id":"forward-note",
                    "title":"Forward",
                    "content":"Forward storage path",
                    "attachments":[{"id":"file","path":"./dir/file.txt","mime":"text/plain","content":"forward"}],
                    "created_at":1,
                    "updated_at":1,
                    "labels":[]
                },
                {
                    "id":"back-note",
                    "title":"Back",
                    "content":"Backslash storage path",
                    "attachments":[{"id":"file","path":".\\dir\\file.txt","mime":"text/plain","content":"back"}],
                    "created_at":1,
                    "updated_at":1,
                    "labels":[]
                }
            ]
        }"#,
    )
    .await
    .unwrap();
    attachments.reads.lock().unwrap().clear();

    let forward = get_note_attachment(&ctx, "forward-note", r"dir\file.txt")
        .await
        .unwrap()
        .unwrap();
    let back = get_note_attachment(&ctx, "back-note", "dir/file.txt")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(forward.content, b"forward");
    assert_eq!(back.content, b"back");
    assert_eq!(
        *attachments.reads.lock().unwrap(),
        vec![
            ("forward-note".into(), "./dir/file.txt".into()),
            ("back-note".into(), r".\dir\file.txt".into()),
        ]
    );
}

#[tokio::test]
async fn saves_and_returns_a_persisted_note() {
    let (ctx, _backend, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.title, "My note");
    assert_eq!(note.labels.len(), 1);
    assert_eq!(note.labels[0].key, "status");
    assert!(!note.id.is_empty());
}

// Auto-create: a label key that isn't in the catalog yet is created on save (docs/design.md §6),
// not rejected. The save succeeds and the key afterwards exists in the catalog.
#[tokio::test]
async fn unknown_label_key_is_auto_created() {
    let (ctx, _backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("project".into(), "alpha".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.labels.len(), 1);
    assert_eq!(note.labels[0].key, "project");
    assert_eq!(note.labels[0].value, "alpha");

    // The previously-unknown key now exists in the catalog (auto-created, empty description).
    let keys = list_label_keys(&ctx).await.unwrap();
    assert!(keys.iter().any(|k| k.key == "project"));
}

#[tokio::test]
async fn selector_reserved_label_key_is_not_auto_created_on_save() {
    let (ctx, _backend, _dir) = test_context().await;
    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("project$name".into(), "alpha".into())],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<LabelKeyValidationError>(),
        Some(&LabelKeyValidationError::ReservedCharacter('$'))
    );
    assert!(list_label_keys(&ctx).await.unwrap().is_empty());
}

#[tokio::test]
async fn selector_reserved_label_key_is_not_auto_created_on_update() {
    let (ctx, _backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let error = update_note(
        &ctx,
        &note.id,
        note.revision,
        SaveNoteInput {
            title: "Updated note".into(),
            content: "Updated content".into(),
            attachments: vec![],
            labels: vec![("project$name".into(), "alpha".into())],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<LabelKeyValidationError>(),
        Some(&LabelKeyValidationError::ReservedCharacter('$'))
    );
    assert!(list_label_keys(&ctx).await.unwrap().is_empty());
}

#[tokio::test]
async fn existing_cataloged_reserved_label_key_remains_attachable() {
    let (ctx, backend, _dir) = test_context().await;
    backend
        .session()
        .await
        .unwrap()
        .insert_label_key_with_type("project$name", "", LabelValueType::Text)
        .await
        .unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Legacy label".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("project$name".into(), "alpha".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.labels[0].key, "project$name");
    assert_eq!(note.labels[0].value, "alpha");
}

#[tokio::test]
async fn typed_label_value_is_validated_on_save() {
    let (ctx, _backend, _dir) = test_context().await;
    define_label_key_with_type(&ctx, "priority", "Priority score", LabelValueType::Number)
        .await
        .unwrap();

    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("priority".into(), "high".into())],
        },
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn save_persists_attachments() {
    let (ctx, backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![NoteAttachment {
                id: "meta".into(),
                path: "./meta.json".into(),
                mime: "application/json".into(),
                description: "metadata".into(),
                content: b"{}".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].path, "./meta.json");

    let session = backend.session().await.unwrap();
    let stored = session.get_note(&note.id).await.unwrap().unwrap();
    assert_eq!(stored.attachments[0].description, "metadata");
    assert!(stored.attachments[0].content.is_empty());

    let attachment_path = _dir
        .path()
        .join("attachments")
        .join(&note.id)
        .join("meta.json");
    assert_eq!(std::fs::read_to_string(attachment_path).unwrap(), "{}");

    let hydrated = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(hydrated.attachments[0].content, b"{}");
}

#[tokio::test]
async fn save_and_get_preserve_binary_attachment_bytes() {
    let (ctx, _backend, dir) = test_context().await;
    let bytes = vec![0, 159, 146, 150, 255];
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Binary attachment".into(),
            content: "Contains arbitrary bytes".into(),
            attachments: vec![NoteAttachment {
                id: "blob".into(),
                path: "blob.bin".into(),
                mime: "application/octet-stream".into(),
                description: String::new(),
                content: bytes.clone(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read(
            dir.path()
                .join("attachments")
                .join(&note.id)
                .join("blob.bin")
        )
        .unwrap(),
        bytes
    );
    let hydrated = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(hydrated.attachments[0].content, bytes);
}

#[tokio::test]
async fn delete_and_restore_preserve_note_data_and_requeue_embeddings() {
    let (ctx, backend, dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete me".into(),
            content: "Searchable content".into(),
            attachments: vec![NoteAttachment {
                id: "meta".into(),
                path: "./meta.json".into(),
                mime: "application/json".into(),
                description: "metadata".into(),
                content: b"{}".to_vec(),
            }],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    assert!(delete_note(&ctx, &note.id, note.revision).await.unwrap());
    assert!(delete_note(&ctx, &note.id, note.revision).await.is_err());
    assert!(get_note(&ctx, &note.id).await.unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join("attachments")
                .join(&note.id)
                .join("meta.json")
        )
        .unwrap(),
        "{}"
    );

    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, note.id);
    assert_eq!(deleted[0].labels.len(), 1);

    let session = backend.session().await.unwrap();
    assert_eq!(session.labels_for_note(&note.id).await.unwrap().len(), 1);
    assert!(session.list_note_chunks(&note.id).await.unwrap().is_empty());
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());

    assert!(restore_notes(
        &ctx,
        &[
            RestoreNoteInput {
                id: note.id.clone(),
                expected_revision: note.revision + 1
            },
            RestoreNoteInput {
                id: note.id.clone(),
                expected_revision: note.revision + 1
            }
        ]
    )
    .await
    .unwrap());
    let restored = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(restored.labels.len(), 1);
    assert_eq!(restored.attachments[0].content, b"{}");
    let observer = backend.session().await.unwrap();
    assert_eq!(observer.get_note_revision(&note.id).await.unwrap(), Some(3));
    assert!(list_deleted_note_summaries(&ctx).await.unwrap().is_empty());
    assert!(!observer
        .list_note_chunks(&note.id)
        .await
        .unwrap()
        .is_empty());
    assert!(!observer
        .claim_pending_embedding_jobs(10, 2)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn restore_batch_rolls_back_when_any_note_is_not_in_trash() {
    let (ctx, backend, _dir) = test_context().await;
    let deleted = save_note(
        &ctx,
        SaveNoteInput {
            title: "Deleted".into(),
            content: "Deleted content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let active = save_note(
        &ctx,
        SaveNoteInput {
            title: "Active".into(),
            content: "Active content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &deleted.id, deleted.revision)
        .await
        .unwrap();

    assert!(restore_notes(
        &ctx,
        &[
            RestoreNoteInput {
                id: deleted.id.clone(),
                expected_revision: deleted.revision + 1
            },
            RestoreNoteInput {
                id: active.id,
                expected_revision: active.revision
            }
        ]
    )
    .await
    .is_err());
    assert!(get_note(&ctx, &deleted.id).await.unwrap().is_none());
    let session = backend.session().await.unwrap();
    assert!(session
        .list_note_chunks(&deleted.id)
        .await
        .unwrap()
        .is_empty());
    let pending = session.claim_pending_embedding_jobs(10, 1).await.unwrap();
    assert!(pending.iter().all(|job| job.note_id != deleted.id));
}

#[tokio::test]
async fn lifecycle_writes_report_stale_revisions_and_restore_batch_is_atomic() {
    let (ctx, backend, _dir) = test_context().await;
    let first = save_note(
        &ctx,
        SaveNoteInput {
            title: "First".into(),
            content: "first".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let second = save_note(
        &ctx,
        SaveNoteInput {
            title: "Second".into(),
            content: "second".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let error = delete_note(&ctx, &first.id, first.revision - 1)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            current_revision: 1,
            ..
        })
    ));
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_note(&first.id)
        .await
        .unwrap()
        .is_some());

    delete_note(&ctx, &first.id, first.revision).await.unwrap();
    delete_note(&ctx, &second.id, second.revision)
        .await
        .unwrap();
    let error = restore_notes(
        &ctx,
        &[
            RestoreNoteInput {
                id: first.id.clone(),
                expected_revision: first.revision + 1,
            },
            RestoreNoteInput {
                id: second.id.clone(),
                expected_revision: second.revision,
            },
        ],
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision { note_id, .. }) if note_id == &second.id
    ));
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_note(&first.id)
        .await
        .unwrap()
        .is_none());
    assert!(backend
        .session()
        .await
        .unwrap()
        .get_note(&second.id)
        .await
        .unwrap()
        .is_none());

    let error = permanently_delete_note(&ctx, &first.id, first.revision)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            current_revision: 2,
            ..
        })
    ));
    assert!(backend
        .session()
        .await
        .unwrap()
        .note_exists(&first.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn purge_removes_notes_at_the_ninety_day_boundary_and_their_attachments() {
    let (ctx, backend, dir) = test_context().await;
    let expired = save_note(
        &ctx,
        SaveNoteInput {
            title: "Expired".into(),
            content: "Content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                content: b"expired".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let retained = save_note(
        &ctx,
        SaveNoteInput {
            title: "Retained".into(),
            content: "Content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                content: b"retained".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let now = 1_800_000_000;
    let cutoff = now - TRASH_RETENTION_SECONDS;
    let unit = backend.begin(TransactionMode::Immediate).await.unwrap();
    assert_eq!(
        unit.soft_delete_note(&expired.id, 1, cutoff).await.unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
    assert_eq!(
        unit.soft_delete_note(&retained.id, 1, cutoff + 1)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
    unit.commit().await.unwrap();

    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 1);
    let observer = backend.session().await.unwrap();
    assert!(!observer.note_exists(&expired.id).await.unwrap());
    assert!(observer.note_exists(&retained.id).await.unwrap());
    assert!(!dir.path().join("attachments").join(&expired.id).exists());
    assert!(dir.path().join("attachments").join(&retained.id).exists());

    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, retained.id);
    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 0);
}

#[tokio::test]
async fn duplicate_attachment_id_is_rejected() {
    let (ctx, _backend, _dir) = test_context().await;
    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![
                NoteAttachment {
                    id: "meta".into(),
                    path: "./meta.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: b"{}".to_vec(),
                },
                NoteAttachment {
                    id: "meta".into(),
                    path: "./other.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: b"{}".to_vec(),
                },
            ],
            labels: vec![],
        },
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn empty_title_is_rejected() {
    let (ctx, _backend, _dir) = test_context().await;
    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await;
    assert!(result.is_err());
}

fn duplicate_config(terms: &[(&str, Option<&str>)]) -> SystemConfig {
    SystemConfig {
        category_labels: Vec::new(),
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: terms
                    .iter()
                    .map(|(key, value)| DuplicateCheckTerm {
                        key: (*key).to_string(),
                        value: value.map(str::to_string),
                    })
                    .collect(),
            }],
        },
    }
}

#[tokio::test]
async fn duplicate_check_is_disabled_by_default() {
    let (ctx, _backend, _dir) = test_context().await;
    for title in ["First", "Second"] {
        save_note(
            &ctx,
            SaveNoteInput {
                title: title.into(),
                content: "Content".into(),
                attachments: vec![],
                labels: vec![
                    ("skill-name".into(), "zddi-hooks".into()),
                    ("version".into(), "1.0.0".into()),
                ],
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn duplicate_rule_rejects_create_with_the_same_composite_labels() {
    let (ctx, backend, dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Existing".into(),
            content: "Existing content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
                ("channel".into(), "stable".into()),
            ],
        },
    )
    .await
    .unwrap();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Duplicate".into(),
            content: "Duplicate content".into(),
            attachments: vec![NoteAttachment {
                id: "metadata".into(),
                path: "./metadata.json".into(),
                mime: "application/json".into(),
                description: String::new(),
                content: b"{}".to_vec(),
            }],
            labels: vec![
                ("version".into(), "1.0.0".into()),
                ("skill-name".into(), "zddi-hooks".into()),
            ],
        },
    )
    .await
    .unwrap_err();

    let duplicate = error.downcast_ref::<DuplicateNoteError>().unwrap();
    assert_eq!(
        duplicate.labels,
        vec![
            ("skill-name".to_string(), "zddi-hooks".to_string()),
            ("version".to_string(), "1.0.0".to_string()),
        ]
    );
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 1);
    assert_eq!(
        session
            .claim_pending_embedding_jobs(10, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("attachments"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn duplicate_rule_allows_missing_or_different_terms_and_scopes_fixed_values() {
    let (ctx, _backend, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[
            ("kind", Some("skill")),
            ("skill-name", None),
            ("version", None),
        ]),
    )
    .await
    .unwrap();

    for (title, labels) in [
        (
            "Existing",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        ),
        (
            "Different version",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "2.0.0".into()),
            ],
        ),
        (
            "Different scope",
            vec![
                ("kind".into(), "note".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        ),
        (
            "Missing version",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
            ],
        ),
    ] {
        save_note(
            &ctx,
            SaveNoteInput {
                title: title.into(),
                content: "Content".into(),
                attachments: vec![],
                labels,
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_duplicate_creates_allow_only_one_note() {
    let (ctx, backend, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    let input = || SaveNoteInput {
        title: "Concurrent".into(),
        content: "Content".into(),
        attachments: vec![],
        labels: vec![
            ("skill-name".into(), "zddi-hooks".into()),
            ("version".into(), "1.0.0".into()),
        ],
    };

    let (first, second) = tokio::join!(save_note(&ctx, input()), save_note(&ctx, input()));
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let error = first.err().or_else(|| second.err()).unwrap();
    assert!(error.downcast_ref::<DuplicateNoteError>().is_some());
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_different_values_can_share_a_new_label_key() {
    let (ctx, backend, _dir) = test_context().await;
    update_system_config(&ctx, &duplicate_config(&[("race-key", None)]))
        .await
        .unwrap();
    let input = |value: &str| SaveNoteInput {
        title: format!("Value {value}"),
        content: "Content".into(),
        attachments: vec![],
        labels: vec![("race-key".into(), value.into())],
    };

    let (first, second) = tokio::join!(save_note(&ctx, input("a")), save_note(&ctx, input("b")));
    first.unwrap();
    second.unwrap();
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 2);
}

#[tokio::test]
async fn duplicate_check_does_not_apply_to_updates() {
    let (ctx, _backend, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    let first = save_note(
        &ctx,
        SaveNoteInput {
            title: "First".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        },
    )
    .await
    .unwrap();
    let second = save_note(
        &ctx,
        SaveNoteInput {
            title: "Second".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "other".into()),
                ("version".into(), "2.0.0".into()),
            ],
        },
    )
    .await
    .unwrap();

    let updated = update_note(
        &ctx,
        &second.id,
        second.revision,
        SaveNoteInput {
            title: "Second".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.id, second.id);
    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn successful_save_persists_note_chunks_and_embedding_jobs() {
    let (ctx, backend, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let stored = session.get_note(&note.id).await.unwrap().unwrap();
    assert_eq!(stored.title, "My note");
    assert_eq!(stored.labels.len(), 1);
    assert_eq!(stored.labels[0].key, "status");

    assert!(!session.list_note_chunks(&note.id).await.unwrap().is_empty());
    assert!(!session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());
}

#[tokio::test]
async fn embedding_worker_populates_dense_recall_table_from_queue() {
    let (ctx, backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 1);

    let dense = ctx.embedder.embed("Some content").await.unwrap();
    let session = backend.session().await.unwrap();
    assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());
    assert!(session
        .dense_search(&dense, 10, None)
        .await
        .unwrap()
        .contains(&note.id));
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn update_only_queues_embedding_when_content_hash_changes() {
    let (ctx, backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Stable content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    update_note(
        &ctx,
        &note.id,
        note.revision,
        SaveNoteInput {
            title: "Renamed".into(),
            content: "Stable content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    let session = backend.session().await.unwrap();
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());

    update_note(
        &ctx,
        &note.id,
        note.revision + 1,
        SaveNoteInput {
            title: "Renamed".into(),
            content: "Changed content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        session
            .claim_pending_embedding_jobs(10, 2)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());
}

#[tokio::test]
async fn field_only_update_preserves_attachments_and_synchronizes_content_embeddings() {
    let (write_ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &write_ctx,
        SaveNoteInput {
            title: "Before".into(),
            content: "Original content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: "preserved metadata".into(),
                content: b"payload".to_vec(),
            }],
            labels: vec![("status".into(), "draft".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&write_ctx, 10).await.unwrap();
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(PanicAttachmentStore),
    );

    let updated = update_note_fields(
        &ctx,
        &saved.id,
        UpdateNoteFieldsInput {
            expected_revision: saved.revision,
            title: "After".into(),
            content: "Changed content".into(),
            labels: vec![("status".into(), "published".into())],
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(updated.title, "After");
    assert_eq!(updated.content, "Changed content");
    assert_eq!(updated.labels[0].value, "published");
    assert_eq!(updated.attachments.len(), 1);
    assert_eq!(updated.attachments[0].description, "preserved metadata");
    assert!(updated.attachments[0].content.is_empty());
    let session = backend.session().await.unwrap();
    assert_eq!(session.get_note_revision(&saved.id).await.unwrap(), Some(2));
    let chunks = session.list_note_chunks(&saved.id).await.unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].content, "Changed content");
    assert_eq!(chunks[0].note_revision, 2);
    let jobs = session
        .claim_pending_embedding_jobs(10, chrono::Utc::now().timestamp())
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].content, "Changed content");
    assert_eq!(jobs[0].note_revision, 2);
}

#[tokio::test]
async fn stale_full_update_rolls_back_all_note_state_and_does_not_wake() {
    let (ctx, backend, _attachments, events, _dir) = controlled_context(true).await;
    let saved = save_note(
        &ctx,
        SaveNoteInput {
            title: "Before".into(),
            content: "original".into(),
            attachments: one_attachment(),
            labels: vec![("status".into(), "draft".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();
    events.lock().unwrap().clear();

    let error = update_note(
        &ctx,
        &saved.id,
        saved.revision - 1,
        SaveNoteInput {
            title: "After".into(),
            content: "changed".into(),
            attachments: vec![],
            labels: vec![("status".into(), "published".into())],
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            expected_revision: 0,
            current_revision: 1,
            ..
        })
    ));
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_note(&saved.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.title, "Before");
    assert_eq!(stored.content, "original");
    assert_eq!(stored.labels[0].value, "draft");
    assert_eq!(stored.attachments.len(), 1);
    assert_eq!(stored.revision, 1);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session.list_note_chunks(&saved.id).await.unwrap()[0].content,
        "original"
    );
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    let logged = events.lock().unwrap().clone();
    assert!(logged.iter().any(|event| event == "rollback"));
    assert!(!logged.iter().any(|event| event == "wake"));
}

#[tokio::test]
async fn edit_rejects_stale_revision_even_when_the_content_tag_matches() {
    let (ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &ctx,
        SaveNoteInput {
            title: "Editable".into(),
            content: "current".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let current = update_note_fields(
        &ctx,
        &saved.id,
        UpdateNoteFieldsInput {
            expected_revision: saved.revision,
            title: saved.title.clone(),
            content: saved.content.clone(),
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    let error = edit_note(
        &ctx,
        &saved.id,
        saved.revision,
        &compute_tag(&current.content),
        &[EditOp::InsertTail {
            lines: vec!["never".into()],
        }],
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            expected_revision: 1,
            current_revision: 2,
            ..
        })
    ));
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_note(&saved.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "current"
    );
}

#[tokio::test]
async fn edit_note_preserves_attachment_metadata_without_attachment_io() {
    let (write_ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &write_ctx,
        SaveNoteInput {
            title: "Editable".into(),
            content: "first\nsecond".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: "preserved metadata".into(),
                content: b"payload".to_vec(),
            }],
            labels: vec![("status".into(), "draft".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&write_ctx, 10).await.unwrap();
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(PanicAttachmentStore),
    );

    let edited = edit_note(
        &ctx,
        &saved.id,
        saved.revision,
        &compute_tag(&saved.content),
        &[EditOp::InsertTail {
            lines: vec!["third".into()],
        }],
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(edited.content, "first\nsecond\nthird");
    assert_eq!(edited.title, saved.title);
    assert_eq!(edited.labels, saved.labels);
    assert_eq!(edited.attachments.len(), 1);
    assert_eq!(edited.attachments[0].description, "preserved metadata");
    assert!(edited.attachments[0].content.is_empty());
    let session = backend.session().await.unwrap();
    assert_eq!(session.get_note_revision(&saved.id).await.unwrap(), Some(2));
    let jobs = session
        .claim_pending_embedding_jobs(10, chrono::Utc::now().timestamp())
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].content, "first\nsecond\nthird");
    assert_eq!(jobs[0].note_revision, 2);
}

#[tokio::test]
async fn edit_note_keeps_the_stale_tag_error_without_attachment_io() {
    let (write_ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &write_ctx,
        SaveNoteInput {
            title: "Editable".into(),
            content: "current".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                content: b"payload".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(PanicAttachmentStore),
    );

    let error = edit_note(
        &ctx,
        &saved.id,
        saved.revision,
        &compute_tag("stale"),
        &[EditOp::InsertTail {
            lines: vec!["never written".into()],
        }],
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleContentTag {
            note_id,
            current_tag,
            ..
        }) if note_id == &saved.id && current_tag == &compute_tag("current")
    ));
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_note(&saved.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.content, "current");
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_note_revision(&saved.id)
            .await
            .unwrap(),
        Some(1)
    );
}

#[tokio::test]
async fn failed_write_rolls_back_the_note_and_embedding_job() {
    let (ctx, backend, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![
                ("status".into(), "done".into()),
                ("status".into(), "wip".into()), // duplicate key -> PK violation mid-transaction
            ],
        },
    )
    .await;
    assert!(result.is_err(), "duplicate label key must fail the save");

    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 0);
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
}
