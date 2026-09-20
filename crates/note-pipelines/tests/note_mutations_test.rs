mod support;

use note_attachments::{
    AttachmentStore, AttachmentStoreInfo, DeleteObjectOutcome, FilesystemAttachmentStore,
    PutObjectRequest, StoredObject,
};
use note_core::{ContentPatch, ContentUpdate, LabelValueType, NoteAttachment, ValidationError};
use note_embedding::StubEmbedder;
use note_pipelines::{
    delete_note, drain_embedding_jobs, get_note, get_note_metadata, patch_note, replace_note,
    save_note, Context, EmbeddingJobNotifier, NoteMutationError, NoteMutationNotifier,
    PatchNoteInput, ReplacePatchError, SaveNoteInput,
};
use note_storage::{
    BackendInfo, StorageBackend, StorageResult, StorageSession, StorageTransaction, TransactionMode,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use support::{event_log, ControlledAttachmentStore, EventNotifier, EventStorageBackend};

#[derive(Default)]
struct CountingNotifier(AtomicUsize);

impl CountingNotifier {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl NoteMutationNotifier for CountingNotifier {
    fn note_mutated(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

enum BeginRace {
    DeleteLabelKey(String),
    ChangeLabelType {
        key: String,
        value_type: LabelValueType,
    },
}

struct BeginRaceStorageBackend {
    inner: Arc<dyn StorageBackend>,
    race: Mutex<Option<BeginRace>>,
}

impl BeginRaceStorageBackend {
    fn new(inner: Arc<dyn StorageBackend>) -> Self {
        Self {
            inner,
            race: Mutex::new(None),
        }
    }

    fn arm(&self, race: BeginRace) {
        *self.race.lock().unwrap() = Some(race);
    }
}

#[async_trait::async_trait]
impl StorageBackend for BeginRaceStorageBackend {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
        self.inner.session().await
    }

    async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>> {
        let race = self.race.lock().unwrap().take();
        if let Some(race) = race {
            let session = self.inner.session().await?;
            match race {
                BeginRace::DeleteLabelKey(key) => session.delete_label_key(&key).await?,
                BeginRace::ChangeLabelType { key, value_type } => {
                    session
                        .update_label_key_with_type(&key, "", value_type)
                        .await?
                }
            }
            drop(session);
        }
        self.inner.begin(mode).await
    }

    async fn info(&self) -> StorageResult<BackendInfo> {
        self.inner.info().await
    }
}

struct LabelTypeRaceAttachmentStore {
    inner: FilesystemAttachmentStore,
    backend: Arc<dyn StorageBackend>,
    armed: AtomicBool,
}

impl LabelTypeRaceAttachmentStore {
    fn new(root: std::path::PathBuf, backend: Arc<dyn StorageBackend>) -> Self {
        Self {
            inner: FilesystemAttachmentStore::new(root),
            backend,
            armed: AtomicBool::new(false),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl AttachmentStore for LabelTypeRaceAttachmentStore {
    async fn put_immutable(&self, request: PutObjectRequest) -> anyhow::Result<StoredObject> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.backend
                .session()
                .await?
                .update_label_key_with_type("score", "", LabelValueType::Number)
                .await?;
        }
        self.inner.put_immutable(request).await
    }

    async fn read_object(&self, object_key: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.read_object(object_key).await
    }

    async fn delete_object(&self, object_key: &str) -> anyhow::Result<DeleteObjectOutcome> {
        self.inner.delete_object(object_key).await
    }

    async fn read_legacy(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.read_legacy(note_id, path).await
    }

    fn info(&self) -> AttachmentStoreInfo {
        self.inner.info()
    }
}

struct FailGetNoteOnWakeNotifier {
    backend: Arc<EventStorageBackend>,
    armed: AtomicBool,
}

impl FailGetNoteOnWakeNotifier {
    fn new(backend: Arc<EventStorageBackend>) -> Self {
        Self {
            backend,
            armed: AtomicBool::new(false),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}

impl EmbeddingJobNotifier for FailGetNoteOnWakeNotifier {
    fn wake(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.backend.fail_next_repository_call("get_note");
        }
    }
}

struct Rig {
    ctx: Context,
    backend: Arc<dyn StorageBackend>,
    traced: Arc<EventStorageBackend>,
    attachments: Arc<ControlledAttachmentStore>,
    events: support::EventLog,
    mutations: Arc<CountingNotifier>,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let raw: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let traced = Arc::new(EventStorageBackend::new(raw, events.clone()));
    let backend: Arc<dyn StorageBackend> = traced.clone();
    let attachments = Arc::new(ControlledAttachmentStore::new(
        backend.clone(),
        events.clone(),
    ));
    let mutations = Arc::new(CountingNotifier::default());
    let ctx = Context::with_embedding_job_notifier(
        backend.clone(),
        Arc::new(StubEmbedder),
        note_embedding::EmbeddingBackendInfo::local_bge_m3(),
        Arc::new(EventNotifier::new(events.clone())),
        attachments.clone(),
    )
    .with_note_mutation_notifier(mutations.clone());
    Rig {
        ctx,
        backend,
        traced,
        attachments,
        events,
        mutations,
        _dir: dir,
    }
}

fn attachment(id: &str, path: &str, bytes: &[u8]) -> NoteAttachment {
    NoteAttachment {
        id: id.into(),
        path: path.into(),
        mime: "text/plain".into(),
        description: format!("{id} attachment"),
        content: bytes.to_vec(),
        storage: None,
    }
}

fn input(
    title: &str,
    content: &str,
    attachments: Vec<NoteAttachment>,
    labels: &[(&str, &str)],
) -> SaveNoteInput {
    SaveNoteInput {
        title: title.into(),
        content: content.into(),
        attachments,
        labels: labels
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    }
}

async fn seed(rig: &Rig) -> note_core::Note {
    save_note(
        &rig.ctx,
        input(
            "Original",
            "alpha\nbeta\ngamma\n",
            vec![attachment("old", "docs/old.txt", b"old bytes")],
            &[("state", "old"), ("kind", "note")],
        ),
    )
    .await
    .unwrap()
}

fn assert_stale(error: &anyhow::Error, id: &str, expected: i64, current: i64) {
    assert_eq!(
        error.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::StaleRevision {
            note_id: id.into(),
            expected_revision: expected,
            current_revision: current,
        })
    );
}

#[tokio::test]
async fn replace_changes_every_writable_field_with_one_revision_increment() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let mutation = replace_note(
        &rig.ctx,
        &original.id,
        original.revision,
        input(
            "Replacement",
            "replacement body",
            vec![attachment("new", "new.txt", b"new bytes")],
            &[("state", "new")],
        ),
    )
    .await
    .unwrap();

    assert_eq!(mutation.id, original.id);
    assert_eq!(mutation.revision, original.revision + 1);
    assert!(mutation.changed);
    let saved = get_note(&rig.ctx, &original.id).await.unwrap().unwrap();
    assert_eq!(saved.title, "Replacement");
    assert_eq!(saved.content, "replacement body");
    assert_eq!(saved.attachments[0].id, "new");
    assert_eq!(saved.attachments[0].content, b"new bytes");
    assert_eq!(saved.labels.len(), 1);
    assert_eq!(saved.labels[0].key, "state");
    assert_eq!(saved.labels[0].value, "new");
}

#[tokio::test]
async fn patch_changes_fields_independently_and_combines_contextual_content() {
    let rig = rig().await;
    let original = seed(&rig).await;

    let title = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Title only".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let content = patch_note(
        &rig.ctx,
        &original.id,
        title.revision,
        PatchNoteInput {
            content: Some(ContentUpdate::Replace("one\ntwo\nthree\n".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let attachments = patch_note(
        &rig.ctx,
        &original.id,
        content.revision,
        PatchNoteInput {
            attachments: Some(vec![attachment("new", "new.txt", b"new")]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let labels = patch_note(
        &rig.ctx,
        &original.id,
        attachments.revision,
        PatchNoteInput {
            labels: Some(vec![("state".into(), "patched".into())]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let combined = patch_note(
        &rig.ctx,
        &original.id,
        labels.revision,
        PatchNoteInput {
            title: Some("Combined".into()),
            content: Some(ContentUpdate::ApplyPatch(ContentPatch {
                apply_patch: "@@\n one\n-two\n+TWO\n three\n".into(),
            })),
            attachments: Some(vec![attachment("final", "final.txt", b"final")]),
            labels: Some(vec![("state".into(), "final".into())]),
        },
    )
    .await
    .unwrap();

    assert_eq!(combined.revision, original.revision + 5);
    let saved = get_note(&rig.ctx, &original.id).await.unwrap().unwrap();
    assert_eq!(saved.title, "Combined");
    assert_eq!(saved.content, "one\nTWO\nthree\n");
    assert_eq!(saved.attachments[0].id, "final");
    assert_eq!(saved.labels[0].value, "final");
}

#[tokio::test]
async fn omitted_fields_preserve_and_explicit_empty_collections_clear() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let title = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Preserve the rest".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let preserved = get_note(&rig.ctx, &original.id).await.unwrap().unwrap();
    assert_eq!(preserved.content, original.content);
    assert_eq!(preserved.attachments[0].content, b"old bytes");
    assert_eq!(preserved.labels, original.labels);

    patch_note(
        &rig.ctx,
        &original.id,
        title.revision,
        PatchNoteInput {
            attachments: Some(vec![]),
            labels: Some(vec![]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let cleared = get_note(&rig.ctx, &original.id).await.unwrap().unwrap();
    assert!(cleared.attachments.is_empty());
    assert!(cleared.labels.is_empty());
}

#[tokio::test]
async fn reordered_labels_and_logically_identical_attachments_are_noops() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let stored_before = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    let object_key = stored_before.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();

    let mutation = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            attachments: Some(vec![attachment("old", r"./docs\old.txt", b"old bytes")]),
            labels: Some(vec![
                ("kind".into(), "note".into()),
                ("state".into(), "old".into()),
            ]),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(!mutation.changed);
    assert_eq!(mutation.revision, original.revision);
    let stored_after = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored_after.attachments[0]
            .storage
            .as_ref()
            .unwrap()
            .object_key,
        object_key
    );

    let changed = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Only the title changed".into()),
            attachments: Some(vec![attachment("old", "docs/old.txt", b"old bytes")]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(changed.changed);
    let stored_changed = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored_changed.attachments[0]
            .storage
            .as_ref()
            .unwrap()
            .object_key,
        object_key
    );
}

#[tokio::test]
async fn semantic_noop_preserves_all_committed_and_derived_state() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let session = rig.backend.session().await.unwrap();
    let stored_before = session.get_note(&original.id).await.unwrap().unwrap();
    let chunks_before = session.list_note_chunks(&original.id).await.unwrap();
    let dashboard_before = session.embedding_dashboard_status().await.unwrap();
    let title_before = session.title_search("Original", 10, None).await.unwrap();
    drop(session);
    rig.events.lock().unwrap().clear();

    let mutation = replace_note(
        &rig.ctx,
        &original.id,
        original.revision,
        input(
            "Original",
            "alpha\nbeta\ngamma\n",
            vec![attachment("old", "docs/old.txt", b"old bytes")],
            &[("kind", "note"), ("state", "old")],
        ),
    )
    .await
    .unwrap();

    let session = rig.backend.session().await.unwrap();
    let stored_after = session.get_note(&original.id).await.unwrap().unwrap();
    assert!(!mutation.changed);
    assert_eq!(stored_after.revision, stored_before.revision);
    assert_eq!(stored_after.updated_at, stored_before.updated_at);
    assert_eq!(stored_after.labels, stored_before.labels);
    assert_eq!(stored_after.attachments, stored_before.attachments);
    assert_eq!(
        session.list_note_chunks(&original.id).await.unwrap(),
        chunks_before
    );
    assert_eq!(
        session.embedding_dashboard_status().await.unwrap(),
        dashboard_before
    );
    assert_eq!(
        session.title_search("Original", 10, None).await.unwrap(),
        title_before
    );
    assert_eq!(rig.mutations.count(), 0);
    let events = rig.events.lock().unwrap();
    assert!(events[0].starts_with("read_object:"));
    assert_eq!(&events[1..], ["begin", "commit"]);
}

#[tokio::test]
async fn noop_reconciles_labels_deleted_after_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let raw: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let racing = Arc::new(BeginRaceStorageBackend::new(raw));
    let backend: Arc<dyn StorageBackend> = racing.clone();
    let mutations = Arc::new(CountingNotifier::default());
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    )
    .with_note_mutation_notifier(mutations.clone());
    let original = save_note(
        &ctx,
        input("Original", "Body", vec![], &[("state", "current")]),
    )
    .await
    .unwrap();
    racing.arm(BeginRace::DeleteLabelKey("state".into()));

    let mutation = patch_note(
        &ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            labels: Some(vec![("state".into(), "current".into())]),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(mutation.changed);
    assert_eq!(mutation.revision, original.revision + 1);
    let after = get_note_metadata(&ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.revision, original.revision + 1);
    assert_eq!(
        after
            .labels
            .iter()
            .map(|label| (label.key.clone(), label.value.clone()))
            .collect::<Vec<_>>(),
        [("state".to_string(), "current".to_string())]
    );
    assert_eq!(mutations.count(), 1);
}

#[tokio::test]
async fn noop_revalidates_label_types_changed_after_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let raw: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let racing = Arc::new(BeginRaceStorageBackend::new(raw));
    let backend: Arc<dyn StorageBackend> = racing.clone();
    let mutations = Arc::new(CountingNotifier::default());
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    )
    .with_note_mutation_notifier(mutations.clone());
    let original = save_note(
        &ctx,
        input("Original", "Body", vec![], &[("score", "not-a-number")]),
    )
    .await
    .unwrap();
    let before = get_note_metadata(&ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    racing.arm(BeginRace::ChangeLabelType {
        key: "score".into(),
        value_type: LabelValueType::Number,
    });

    let error = patch_note(
        &ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            labels: Some(vec![("score".into(), "not-a-number".into())]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<ValidationError>(),
        Some(ValidationError::InvalidLabelValue {
            key,
            value,
            value_type: LabelValueType::Number,
        }) if key == "score" && value == "not-a-number"
    ));
    let after = get_note_metadata(&ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.title, before.title);
    assert_eq!(after.content, before.content);
    assert_eq!(after.attachments, before.attachments);
    assert_eq!(
        after
            .labels
            .iter()
            .map(|label| (&label.key, &label.value))
            .collect::<Vec<_>>(),
        before
            .labels
            .iter()
            .map(|label| (&label.key, &label.value))
            .collect::<Vec<_>>()
    );
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.updated_at, before.updated_at);
    let score = backend
        .session()
        .await
        .unwrap()
        .list_label_keys()
        .await
        .unwrap()
        .into_iter()
        .find(|label_key| label_key.key == "score")
        .unwrap();
    assert_eq!(score.value_type, LabelValueType::Number);
    assert_eq!(mutations.count(), 0);
}

#[tokio::test]
async fn stale_revision_fails_even_for_noop() {
    let rig = rig().await;
    let original = seed(&rig).await;
    rig.events.lock().unwrap().clear();
    let error = patch_note(
        &rig.ctx,
        &original.id,
        original.revision + 1,
        PatchNoteInput {
            title: Some(original.title.clone()),
            attachments: Some(vec![attachment("old", "docs/old.txt", b"old bytes")]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_stale(
        &error,
        &original.id,
        original.revision + 1,
        original.revision,
    );
    assert!(rig.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_revision_and_empty_patch_fail_before_writes() {
    let rig = rig().await;
    for error in [
        patch_note(
            &rig.ctx,
            "missing",
            0,
            PatchNoteInput {
                title: Some("value".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err(),
        patch_note(&rig.ctx, "missing", 1, PatchNoteInput::default())
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(
            error.downcast_ref::<ReplacePatchError>(),
            Some(ReplacePatchError::InvalidRequest(_))
        ));
    }
    assert!(rig.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_and_inactive_notes_return_typed_not_found() {
    let rig = rig().await;
    let missing = patch_note(
        &rig.ctx,
        "missing",
        1,
        PatchNoteInput {
            title: Some("value".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        missing.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::NotFound("missing".into()))
    );

    let original = seed(&rig).await;
    delete_note(&rig.ctx, &original.id, original.revision)
        .await
        .unwrap();
    let inactive = patch_note(
        &rig.ctx,
        &original.id,
        original.revision + 1,
        PatchNoteInput {
            title: Some("value".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        inactive.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::NotFound(original.id))
    );
}

#[tokio::test]
async fn failed_later_content_hunk_saves_nothing() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let before = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    let error = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Must roll back".into()),
            content: Some(ContentUpdate::ApplyPatch(ContentPatch {
                apply_patch: "@@\n-alpha\n+ALPHA\n@@\n-missing\n+MISSING\n".into(),
            })),
            attachments: Some(vec![attachment("new", "new.txt", b"new")]),
            labels: Some(vec![("new-key".into(), "new-value".into())]),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReplacePatchError>(),
        Some(ReplacePatchError::PatchFailed(_))
    ));
    assert_eq!(
        get_note_metadata(&rig.ctx, &original.id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(!rig
        .backend
        .session()
        .await
        .unwrap()
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "new-key"));
}

#[tokio::test]
async fn invalid_inputs_and_publication_failure_preserve_note_and_catalog() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let before = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();

    let invalid_label = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            labels: Some(vec![("bad=key".into(), "value".into())]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(invalid_label
        .downcast_ref::<note_core::LabelKeyValidationError>()
        .is_some());
    let invalid_attachment = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            attachments: Some(vec![attachment("bad", "../bad.txt", b"bad")]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        invalid_attachment.downcast_ref::<ValidationError>(),
        Some(ValidationError::InvalidAttachmentPath(_))
    ));
    rig.attachments.fail_put_on_call(2);
    let publication = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            attachments: Some(vec![attachment("new", "new.txt", b"new")]),
            labels: Some(vec![("uncommitted".into(), "value".into())]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(publication.to_string(), "attachment object write failed");
    assert_eq!(
        get_note_metadata(&rig.ctx, &original.id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(!rig
        .backend
        .session()
        .await
        .unwrap()
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "uncommitted" || key.key == "bad=key"));
}

#[tokio::test]
async fn storage_failure_after_publication_cleans_new_objects_and_keeps_old_generation() {
    let rig = rig().await;
    let original = seed(&rig).await;
    let before = get_note_metadata(&rig.ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    let old_key = before.attachments[0]
        .storage
        .as_ref()
        .unwrap()
        .object_key
        .clone();
    rig.events.lock().unwrap().clear();
    rig.traced.fail_next_repository_call("update_note");

    let error = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            attachments: Some(vec![attachment("new", "new.txt", b"new")]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("controlled repository failure at update_note"));
    assert_eq!(
        get_note_metadata(&rig.ctx, &original.id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(rig.attachments.has_object(&old_key));
    let events = rig.events.lock().unwrap().clone();
    let new_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(!rig.attachments.has_object(new_key));
    assert!(events.iter().any(|event| event == "rollback"));
    assert!(events
        .iter()
        .any(|event| event == &format!("delete_object:{new_key}")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_revision_requests_have_one_change_and_one_conflict() {
    let rig = Arc::new(rig().await);
    let original = seed(&rig).await;
    let revision = original.revision;
    let first_rig = rig.clone();
    let first_id = original.id.clone();
    let first = tokio::spawn(async move {
        patch_note(
            &first_rig.ctx,
            &first_id,
            revision,
            PatchNoteInput {
                title: Some("First".into()),
                ..Default::default()
            },
        )
        .await
    });
    let second_rig = rig.clone();
    let second_id = original.id.clone();
    let second = tokio::spawn(async move {
        patch_note(
            &second_rig.ctx,
            &second_id,
            revision,
            PatchNoteInput {
                title: Some("Second".into()),
                ..Default::default()
            },
        )
        .await
    });
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().is_ok_and(|value| value.changed))
            .count(),
        1
    );
    let error = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert_stale(error, &original.id, revision, revision + 1);
    assert_eq!(rig.mutations.count(), 1);
}

#[tokio::test]
async fn changed_mutation_notifies_once_and_wakes_only_for_queued_embedding_work() {
    let rig = rig().await;
    let original = seed(&rig).await;
    drain_embedding_jobs(&rig.ctx, 10).await.unwrap();
    rig.events.lock().unwrap().clear();

    let title = patch_note(
        &rig.ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Title changed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rig.mutations.count(), 1);
    assert!(!rig
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event == "wake"));

    rig.events.lock().unwrap().clear();
    patch_note(
        &rig.ctx,
        &original.id,
        title.revision,
        PatchNoteInput {
            content: Some(ContentUpdate::Replace("brand new content".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rig.mutations.count(), 2);
    assert_eq!(
        rig.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.as_str() == "wake")
            .count(),
        1
    );
}

#[tokio::test]
async fn in_transaction_label_type_validation_rejects_a_publication_race() {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments = Arc::new(LabelTypeRaceAttachmentStore::new(
        dir.path().join("attachments"),
        backend.clone(),
    ));
    let ctx = Context::new(backend.clone(), Arc::new(StubEmbedder), attachments.clone());
    let original = save_note(
        &ctx,
        input(
            "Original",
            "Body",
            vec![attachment("old", "old.txt", b"old")],
            &[("score", "not-a-number")],
        ),
    )
    .await
    .unwrap();
    let before = get_note_metadata(&ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    attachments.arm();

    let error = patch_note(
        &ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Must not commit".into()),
            attachments: Some(vec![attachment("new", "new.txt", b"new")]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<ValidationError>(),
        Some(ValidationError::InvalidLabelValue {
            key,
            value,
            value_type: LabelValueType::Number,
        }) if key == "score" && value == "not-a-number"
    ));
    let after = get_note_metadata(&ctx, &original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.title, before.title);
    assert_eq!(after.content, before.content);
    assert_eq!(after.attachments, before.attachments);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(
        after
            .labels
            .iter()
            .map(|label| (&label.key, &label.value))
            .collect::<Vec<_>>(),
        before
            .labels
            .iter()
            .map(|label| (&label.key, &label.value))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn patch_acknowledges_and_notifies_without_a_post_commit_metadata_read() {
    let dir = tempfile::tempdir().unwrap();
    let raw: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let traced = Arc::new(EventStorageBackend::new(raw, events));
    let backend: Arc<dyn StorageBackend> = traced.clone();
    let wake = Arc::new(FailGetNoteOnWakeNotifier::new(traced.clone()));
    let mutations = Arc::new(CountingNotifier::default());
    let ctx = Context::with_embedding_job_notifier(
        backend.clone(),
        Arc::new(StubEmbedder),
        note_embedding::EmbeddingBackendInfo::local_bge_m3(),
        wake.clone(),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    )
    .with_note_mutation_notifier(mutations.clone());
    let original = save_note(&ctx, input("Original", "Body", vec![], &[]))
        .await
        .unwrap();
    wake.arm();

    let mutation = patch_note(
        &ctx,
        &original.id,
        original.revision,
        PatchNoteInput {
            title: Some("Committed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(mutation.changed);
    assert_eq!(mutation.revision, original.revision + 1);
    assert_eq!(mutations.count(), 1);
    assert_eq!(traced.repository_call_count("get_note"), 3);
    let injected = backend
        .session()
        .await
        .unwrap()
        .get_note(&original.id)
        .await
        .unwrap_err();
    assert_eq!(
        injected.to_string(),
        "controlled repository failure at get_note"
    );
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_note(&original.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Committed"
    );
}

#[tokio::test]
async fn basic_mutations_work_with_the_filesystem_attachment_store() {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let ctx = Context::new(
        backend,
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    let saved = save_note(&ctx, input("Title", "Body", vec![], &[]))
        .await
        .unwrap();
    let mutation = patch_note(
        &ctx,
        &saved.id,
        saved.revision,
        PatchNoteInput {
            title: Some("Changed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(mutation.changed);
}
