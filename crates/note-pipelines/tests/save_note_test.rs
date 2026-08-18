mod support;

use note_attachments::{
    AttachmentStore, AttachmentStoreInfo, PreparedAttachmentMutation, PreparedAttachmentSet,
};
use note_core::{
    AttachmentStorageMetadata, LabelKeyValidationError, LabelValueType, NoteAttachment,
};
use note_embedding::StubEmbedder;
use note_pipelines::{
    compute_tag, define_label_key, define_label_key_with_type, delete_note, drain_embedding_jobs,
    edit_note, get_note, get_note_attachment, get_note_metadata, import_json,
    list_deleted_note_summaries, list_label_keys, permanently_delete_note,
    purge_expired_deleted_notes, restore_notes, save_note, update_note, update_note_fields,
    update_system_config, Context, EditOp, NoteMutationError, RestoreNoteInput, SaveNoteInput,
    UpdateNoteFieldsInput, TRASH_RETENTION_SECONDS,
};
use note_storage::{
    AttachmentOperationStatus, NewNote, NoteMutationResult, StorageBackend, TransactionMode,
};
use sha2::{Digest, Sha256};
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

fn regular_file_count(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                regular_file_count(&entry.path())
            } else {
                usize::from(entry.file_type().is_ok_and(|kind| kind.is_file()))
            }
        })
        .sum()
}

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
                    storage: attachment.storage.clone(),
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

async fn controlled_failure_context() -> (
    Context,
    Arc<dyn StorageBackend>,
    Arc<EventStorageBackend>,
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
    let event_backend = Arc::new(EventStorageBackend::new(
        raw_backend.clone(),
        events.clone(),
    ));
    let backend: Arc<dyn StorageBackend> = event_backend.clone();
    let attachments = Arc::new(ControlledAttachmentStore::new(
        backend.clone(),
        events.clone(),
    ));
    let ctx = Context::with_embedding_job_notifier(
        backend,
        Arc::new(StubEmbedder),
        note_embedding::EmbeddingBackendInfo::local_bge_m3(),
        Arc::new(EventNotifier::new(events.clone())),
        attachments.clone(),
    );
    (ctx, raw_backend, event_backend, attachments, events, dir)
}

fn one_attachment() -> Vec<NoteAttachment> {
    vec![NoteAttachment {
        id: "file".into(),
        path: "./file.txt".into(),
        mime: "text/plain".into(),
        description: String::new(),
        content: b"payload".to_vec(),
        storage: None,
    }]
}

#[tokio::test]
async fn save_uploads_generated_objects_before_begin_and_persists_verified_metadata() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(true).await;
    let bytes = vec![0, 159, 146, 150, 255];
    let checksum = format!("{:x}", Sha256::digest(&bytes));

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Generated".into(),
            content: "Object-first creation".into(),
            attachments: vec![NoteAttachment {
                id: "user-id-must-not-appear".into(),
                path: "private/user-path-must-not-appear.bin".into(),
                mime: "application/octet-stream".into(),
                description: "binary".into(),
                content: bytes.clone(),
                storage: None,
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let storage = note.attachments[0].storage.as_ref().unwrap();
    assert!(storage
        .object_key
        .starts_with(&format!("notes/{}/objects/", note.id)));
    assert!(!storage.object_key.contains("user-id-must-not-appear"));
    assert!(!storage.object_key.contains("user-path-must-not-appear"));
    assert!(storage.object_key.ends_with(&checksum[..16]));
    assert!(uuid::Uuid::parse_str(&storage.storage_generation).is_ok());
    assert_eq!(storage.size_bytes, bytes.len() as u64);
    assert_eq!(storage.checksum_sha256, checksum);
    assert_eq!(note.attachments[0].content, bytes);
    assert!(attachments.has_object(&storage.object_key));

    let stored = backend
        .session()
        .await
        .unwrap()
        .get_note(&note.id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.attachments[0].content.is_empty());
    assert_eq!(stored.attachments[0].storage, note.attachments[0].storage);

    let events = events.lock().unwrap().clone();
    let put = events
        .iter()
        .position(|event| event.starts_with("put_object:"))
        .unwrap();
    let begin = events.iter().position(|event| event == "begin").unwrap();
    assert!(put < begin);
    assert_eq!(events.last().map(String::as_str), Some("wake"));
}

#[tokio::test]
async fn object_write_failure_opens_no_transaction_persists_no_note_and_does_not_wake() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(true).await;
    attachments.fail_put_on_call(1);

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Rejected".into(),
            content: "Object write must finish first".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "attachment object write failed");
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_all_notes()
        .await
        .unwrap()
        .is_empty());
    let events = events.lock().unwrap().clone();
    assert!(events.iter().any(|event| event.starts_with("put_object:")));
    assert!(!events.iter().any(|event| event == "begin"));
    assert!(!events.iter().any(|event| event == "wake"));
}

#[tokio::test]
async fn object_write_failure_preserves_the_sanitized_adapter_error_chain() {
    let (ctx, _backend, attachments, events, _dir) = controlled_context(false).await;
    attachments.fail_put_on_call(1);

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Adapter failure".into(),
            content: "Preserve the safe adapter cause".into(),
            attachments: vec![NoteAttachment {
                id: "secret-id".into(),
                path: "secret-path.bin".into(),
                mime: "application/octet-stream".into(),
                description: String::new(),
                content: b"super-secret-payload".to_vec(),
                storage: None,
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    let chain = format!("{error:#}");
    assert!(chain.contains("attachment object write failed"));
    assert!(chain.contains("controlled immutable put failure"));
    let object_key = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap()
        .to_string();
    assert!(!chain.contains(&object_key));
    assert!(!chain.contains("super-secret-payload"));
}

#[tokio::test]
async fn partial_multi_upload_failure_best_effort_deletes_every_generated_key() {
    let (ctx, _backend, attachments, events, _dir) = controlled_context(false).await;
    attachments.fail_put_on_call(2);
    let mut second = one_attachment().remove(0);
    second.id = "second".into();
    second.path = "second.bin".into();
    second.content = vec![0, 1, 2, 255];

    save_note(
        &ctx,
        SaveNoteInput {
            title: "Partial".into(),
            content: "Clean all attempted generated objects".into(),
            attachments: vec![one_attachment().remove(0), second],
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    let events = events.lock().unwrap().clone();
    let keys: Vec<&str> = events
        .iter()
        .filter_map(|event| event.strip_prefix("put_object:"))
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|key| !attachments.has_object(key)));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("delete_object:"))
            .count(),
        2
    );
}

#[tokio::test]
async fn returned_object_metadata_is_verified_before_opening_a_transaction() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(false).await;
    attachments.corrupt_next_put_return();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Unverified".into(),
            content: "Do not activate unverified metadata".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "attachment object verification failed");
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_all_notes()
        .await
        .unwrap()
        .is_empty());
    let events = events.lock().unwrap().clone();
    assert!(events
        .iter()
        .any(|event| event.starts_with("delete_object:")));
    assert!(!events.iter().any(|event| event == "begin"));
}

#[tokio::test]
async fn begin_and_repository_failures_delete_all_new_objects_and_keep_primary_errors() {
    for failure in ["begin", "insert_note"] {
        let (ctx, backend, event_backend, attachments, events, _dir) =
            controlled_failure_context().await;
        match failure {
            "begin" => event_backend.fail_next_begin(),
            _ => event_backend.fail_next_repository_call(failure),
        }

        let error = save_note(
            &ctx,
            SaveNoteInput {
                title: "Database failure".into(),
                content: "Generated object must become an orphan only if cleanup fails".into(),
                attachments: one_attachment(),
                labels: vec![],
            },
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains(match failure {
            "begin" => "controlled begin failure",
            _ => "controlled repository failure at insert_note",
        }));
        assert!(backend
            .session()
            .await
            .unwrap()
            .list_all_notes()
            .await
            .unwrap()
            .is_empty());
        let events = events.lock().unwrap().clone();
        let key = events
            .iter()
            .find_map(|event| event.strip_prefix("put_object:"))
            .unwrap();
        assert!(!attachments.has_object(key));
        assert!(events
            .iter()
            .any(|event| event == &format!("delete_object:{key}")));
        assert!(!events.iter().any(|event| event == "wake"));
    }
}

#[tokio::test]
async fn commit_acknowledgement_failure_retains_active_metadata_and_generated_objects() {
    let (ctx, backend, event_backend, attachments, events, _dir) =
        controlled_failure_context().await;
    event_backend.fail_next_commit_acknowledgement();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Committed despite acknowledgement".into(),
            content: "The commit outcome is unknown to the caller".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    let chain = format!("{error:#}");
    assert!(chain.contains("sentinel commit acknowledgement failure"));
    assert!(chain.contains("commit outcome is unknown"));
    assert!(chain.contains("generated attachment objects retained"));
    let note = backend
        .session()
        .await
        .unwrap()
        .list_all_notes()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    assert!(attachments.has_object(&object_key));
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            format!("put_object:{object_key}"),
            "begin".into(),
            "commit_ack_failed".into(),
        ]
    );
}

#[tokio::test]
async fn save_and_full_update_publish_objects_before_begin_and_cleanup_after_commit() {
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
            format!(
                "put_object:{}",
                note.attachments[0].storage.as_ref().unwrap().object_key
            ),
            "begin".into(),
            "commit".into(),
            "wake".into(),
        ]
    );

    events.lock().unwrap().clear();
    let updated = update_note(
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

    let new_key = &updated.attachments[0].storage.as_ref().unwrap().object_key;
    let old_key = &note.attachments[0].storage.as_ref().unwrap().object_key;

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("put_object:{new_key}"),
            "begin".into(),
            "commit".into(),
            format!("delete_object:{old_key}"),
            "wake".into(),
        ]
    );
}

async fn enable_duplicate_series_rule(ctx: &Context) {
    update_system_config(
        ctx,
        &SystemConfig {
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
async fn save_transaction_failure_rolls_back_then_deletes_generated_objects() {
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
    assert!(events[0].starts_with("put_object:"));
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "rollback");
    assert!(events[3].starts_with("delete_object:"));
}

#[tokio::test]
async fn save_does_not_use_the_legacy_prepared_publish_path() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(true).await;
    attachments.fail_publish();

    save_note(
        &ctx,
        SaveNoteInput {
            title: "Committed".into(),
            content: "Queue work only after publication".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let events = events.lock().unwrap().clone();
    assert!(events[0].starts_with("put_object:"));
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "commit");
    assert_eq!(events[3], "wake");
    assert!(!events.iter().any(|event| event.starts_with("prepare:")));
    assert!(!events.iter().any(|event| event.starts_with("publish:")));
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
async fn permanent_delete_and_purge_enqueue_durable_cleanup_after_database_deletion() {
    let (ctx, backend, attachments, events, _dir) = controlled_context(false).await;
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
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", &format!("delete_object:{object_key}")]
    );
    assert!(!attachments.has_object(&object_key));
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].object_key, object_key);
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
    events.lock().unwrap().clear();
    assert!(note_pipelines::permanently_delete_note(&ctx, "missing", 1)
        .await
        .is_err());
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);

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
    let expired_key = expired.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", &format!("delete_object:{expired_key}")]
    );
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(&expired.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn permanent_delete_cleanup_failure_keeps_committed_success_pending_for_retry() {
    let (ctx, backend, attachments, _events, _dir) = controlled_context(false).await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete".into(),
            content: "Durable cleanup".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    attachments.fail_delete_object();

    assert!(permanently_delete_note(&ctx, &note.id, note.revision + 1)
        .await
        .unwrap());
    assert!(!backend
        .session()
        .await
        .unwrap()
        .note_exists(&note.id)
        .await
        .unwrap());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status, AttachmentOperationStatus::Pending);
    assert!(attachments.has_object(&operations[0].object_key));
}

#[tokio::test]
async fn permanent_delete_commit_acknowledgement_failure_retains_object_with_durable_operation() {
    let (ctx, backend, event_backend, attachments, _events, _dir) =
        controlled_failure_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete".into(),
            content: "Ambiguous commit".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    event_backend.fail_next_commit_acknowledgement();

    assert!(permanently_delete_note(&ctx, &note.id, note.revision + 1)
        .await
        .is_err());
    assert!(attachments.has_object(&object_key));
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(&note.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn permanent_delete_enqueues_generated_and_legacy_snapshots_before_removing_the_note() {
    let (ctx, backend, attachments, _events, _dir) = controlled_context(false).await;
    let generated_key = "notes/snapshot/objects/generation-checksum";
    let captured = vec![
        NoteAttachment {
            id: "generated".into(),
            path: "generated.bin".into(),
            mime: "application/octet-stream".into(),
            description: String::new(),
            content: vec![],
            storage: Some(AttachmentStorageMetadata {
                object_key: generated_key.into(),
                storage_generation: "generation".into(),
                size_bytes: 3,
                checksum_sha256: "checksum".into(),
            }),
        },
        NoteAttachment {
            id: "legacy".into(),
            path: "./legacy.txt".into(),
            mime: "text/plain".into(),
            description: String::new(),
            content: vec![],
            storage: None,
        },
    ];
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: "snapshot",
            title: "Snapshot",
            content: "body",
            attachments: &captured,
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: Some(1),
        })
        .await
        .unwrap();
    attachments.set_object_content(generated_key, b"new");
    attachments.set_read_content("snapshot", "legacy.txt", b"old");

    assert!(permanently_delete_note(&ctx, "snapshot", 1).await.unwrap());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note("snapshot")
        .await
        .unwrap();
    assert_eq!(operations.len(), 2);
    assert_eq!(
        operations
            .iter()
            .map(|operation| operation.object_key.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from([generated_key, "legacy.txt"])
    );
    assert!(operations
        .iter()
        .all(|operation| operation.status == AttachmentOperationStatus::Completed));
}

#[tokio::test]
async fn stale_delete_and_cleanup_enqueue_failure_preserve_note_objects_and_operations() {
    let (ctx, backend, event_backend, attachments, _events, _dir) =
        controlled_failure_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete".into(),
            content: "Rollback cleanup".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();

    assert!(permanently_delete_note(&ctx, &note.id, note.revision)
        .await
        .is_err());
    assert!(backend
        .session()
        .await
        .unwrap()
        .note_exists(&note.id)
        .await
        .unwrap());
    assert!(attachments.has_object(&object_key));
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap()
        .is_empty());

    event_backend.fail_next_repository_call("insert_attachment_operation");
    assert!(permanently_delete_note(&ctx, &note.id, note.revision + 1)
        .await
        .is_err());
    assert!(backend
        .session()
        .await
        .unwrap()
        .note_exists(&note.id)
        .await
        .unwrap());
    assert!(attachments.has_object(&object_key));
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn already_missing_physical_object_completes_permanent_delete_cleanup() {
    let (ctx, backend, attachments, _events, _dir) = controlled_context(false).await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete".into(),
            content: "Already gone".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    attachments.delete_object(&object_key).await.unwrap();

    assert!(permanently_delete_note(&ctx, &note.id, note.revision + 1)
        .await
        .unwrap());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap();
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
}

#[tokio::test]
async fn soft_delete_and_restore_preserve_attachment_objects_without_cleanup_operations() {
    let (ctx, backend, attachments, _events, _dir) = controlled_context(false).await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Restore".into(),
            content: "Keep attachments".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let object_key = note.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    delete_note(&ctx, &note.id, note.revision).await.unwrap();
    assert!(attachments.has_object(&object_key));
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap()
        .is_empty());

    assert!(restore_notes(
        &ctx,
        &[RestoreNoteInput {
            id: note.id.clone(),
            expected_revision: note.revision + 1
        }]
    )
    .await
    .unwrap());
    assert_eq!(
        get_note_attachment(&ctx, &note.id, "file.txt")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"payload"
    );
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&note.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn purge_rolls_back_all_notes_on_cleanup_enqueue_failure_then_records_exact_operations() {
    let (ctx, backend, event_backend, attachments, _events, _dir) =
        controlled_failure_context().await;
    let mut notes = Vec::new();
    for title in ["First", "Second"] {
        let note = save_note(
            &ctx,
            SaveNoteInput {
                title: title.into(),
                content: "Expired".into(),
                attachments: one_attachment(),
                labels: vec![],
            },
        )
        .await
        .unwrap();
        notes.push(note);
    }
    let now = 1_900_000_000;
    for note in &notes {
        backend
            .session()
            .await
            .unwrap()
            .soft_delete_note(&note.id, note.revision, now - TRASH_RETENTION_SECONDS)
            .await
            .unwrap();
    }

    event_backend.fail_next_repository_call("insert_attachment_operation");
    assert!(purge_expired_deleted_notes(&ctx, now).await.is_err());
    for note in &notes {
        assert!(backend
            .session()
            .await
            .unwrap()
            .note_exists(&note.id)
            .await
            .unwrap());
        assert!(backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(&note.id)
            .await
            .unwrap()
            .is_empty());
        assert!(attachments.has_object(&note.attachments[0].storage.as_ref().unwrap().object_key));
    }

    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 2);
    for note in &notes {
        let operations = backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(&note.id)
            .await
            .unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(
            operations[0].object_key,
            note.attachments[0].storage.as_ref().unwrap().object_key
        );
    }
}

#[tokio::test]
async fn cleanup_failure_adds_safe_context_without_hiding_the_primary_transaction_error() {
    let (ctx, _backend, attachments, events, _dir) = controlled_context(false).await;
    enable_duplicate_series_rule(&ctx).await;
    save_existing_duplicate(&ctx).await;
    events.lock().unwrap().clear();
    attachments.fail_delete_object();

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
    assert!(chain.contains("generated attachment cleanup also failed"));
    assert!(chain.contains("safe orphan may remain"));
    assert!(!chain.contains("notes/"));
    assert!(error
        .chain()
        .any(|cause| cause.downcast_ref::<DuplicateNoteError>().is_some()));
    let events = events.lock().unwrap();
    assert!(events[0].starts_with("put_object:"));
    assert_eq!(events[1], "begin");
    assert_eq!(events[2], "rollback");
    assert!(events[3].starts_with("delete_object:"));
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
                storage: None,
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
                storage: None,
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
        .join(&note.attachments[0].storage.as_ref().unwrap().object_key);
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
                storage: None,
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
                .join(&note.attachments[0].storage.as_ref().unwrap().object_key)
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
                storage: None,
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
                .join(&note.attachments[0].storage.as_ref().unwrap().object_key)
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
async fn purge_removes_notes_at_the_ninety_day_boundary() {
    let (ctx, backend, _dir) = test_context().await;
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
                storage: None,
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
                storage: None,
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
                    storage: None,
                },
                NoteAttachment {
                    id: "meta".into(),
                    path: "./other.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: b"{}".to_vec(),
                    storage: None,
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
                storage: None,
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
    assert_eq!(regular_file_count(&dir.path().join("attachments")), 0);
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
                storage: None,
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
            attachments: one_attachment(),
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
    let old_key = stored.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.starts_with("put_object:")));
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.starts_with("delete_object:") && !event.ends_with(&old_key)));
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
    assert!(session
        .list_attachment_operations_for_note(&saved.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn full_update_cleanup_enqueue_failure_rolls_back_metadata_and_new_objects() {
    let (ctx, backend, event_backend, attachments, events, _dir) =
        controlled_failure_context().await;
    let saved = save_note(
        &ctx,
        SaveNoteInput {
            title: "Before".into(),
            content: "original".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let old_key = saved.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    events.lock().unwrap().clear();
    event_backend.fail_next_repository_call("insert_attachment_operation");

    let error = update_note(
        &ctx,
        &saved.id,
        saved.revision,
        SaveNoteInput {
            title: "After".into(),
            content: "changed".into(),
            attachments: one_attachment(),
            labels: vec![],
        },
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}")
        .contains("controlled repository failure at insert_attachment_operation"));
    let stored = backend
        .session()
        .await
        .unwrap()
        .get_note(&saved.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.title, "Before");
    assert_eq!(stored.revision, saved.revision);
    assert_eq!(
        stored.attachments[0].storage.as_ref().unwrap().object_key,
        old_key
    );
    assert!(attachments.has_object(&old_key));
    let logged = events.lock().unwrap().clone();
    let new_key = logged
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(!attachments.has_object(new_key));
    assert!(logged.iter().any(|event| event == "rollback"));
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(&saved.id)
        .await
        .unwrap()
        .is_empty());
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
                storage: None,
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
                storage: None,
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
