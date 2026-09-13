mod support;

use async_trait::async_trait;
use note_attachments::{AttachmentStore, AttachmentStoreInfo, FilesystemAttachmentStore};
use note_core::{AttachmentStorageMetadata, Note, NoteAttachment};
use note_embedding::StubEmbedder;
use note_pipelines::{
    capture_note_export, freeze_note_export, hydrate_note_export, plan_note_export_assets,
    CapturedNoteExport, Context, ExportAssetKind, NoteExportError, NoteExportLimits,
};
use note_storage::{
    AttachmentMetadataUpdate, NewNote, NoteFieldsUpdate, StorageBackend, TransactionMode,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn attachment(id: &str, path: &str, mime: &str) -> NoteAttachment {
    NoteAttachment {
        id: id.into(),
        path: path.into(),
        mime: mime.into(),
        description: String::new(),
        content: Vec::new(),
        storage: Some(AttachmentStorageMetadata {
            object_key: format!("immutable/{id}"),
            storage_generation: format!("generation-{id}"),
            size_bytes: 1,
            checksum_sha256: "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
                .into(),
        }),
    }
}

async fn seed_note(
    backend: &Arc<dyn StorageBackend>,
    content: &str,
    attachments: &[NoteAttachment],
) {
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_note(NewNote {
            id: "export-note",
            title: "Captured title",
            content,
            attachments,
            created_at: 11,
            updated_at: 12,
            note_revision: 7,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction
        .insert_label_key_if_missing("status", "")
        .await
        .unwrap();
    transaction
        .attach_label("export-note", "status", "captured")
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[derive(Clone, Copy)]
enum ReadRace {
    None,
    Edit,
    Label,
    ReplaceAttachment,
    Delete,
    EditThenFail,
}

struct RecordingStore {
    backend: Arc<dyn StorageBackend>,
    objects: Mutex<HashMap<String, Vec<u8>>>,
    reads: Mutex<Vec<String>>,
    race: Mutex<ReadRace>,
}

impl RecordingStore {
    fn new(backend: Arc<dyn StorageBackend>) -> Self {
        Self {
            backend,
            objects: Mutex::new(HashMap::new()),
            reads: Mutex::new(Vec::new()),
            race: Mutex::new(ReadRace::None),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) {
        self.objects
            .lock()
            .unwrap()
            .insert(key.into(), bytes.into());
    }

    fn set_race(&self, race: ReadRace) {
        *self.race.lock().unwrap() = race;
    }

    async fn run_race(&self, race: ReadRace) {
        if matches!(race, ReadRace::None) {
            return;
        }
        let session = self.backend.session().await.unwrap();
        let note = session.get_note("export-note").await.unwrap().unwrap();
        if matches!(race, ReadRace::Delete) {
            session
                .soft_delete_note("export-note", note.revision, 99)
                .await
                .unwrap();
        } else if matches!(race, ReadRace::Label) {
            session
                .set_note_label("export-note", "status", "changed")
                .await
                .unwrap();
            session
                .advance_note_updated_at("export-note", 99)
                .await
                .unwrap();
        } else if matches!(race, ReadRace::ReplaceAttachment) {
            let replacement = generated("image", "image.png", "key-8", b"newer");
            session
                .update_note_attachments(AttachmentMetadataUpdate {
                    id: "export-note",
                    expected_revision: note.revision,
                    attachments: &[replacement],
                    updated_at: 99,
                })
                .await
                .unwrap();
        } else {
            session
                .update_note_fields(NoteFieldsUpdate {
                    id: "export-note",
                    expected_revision: note.revision,
                    title: "Changed during hydration",
                    content: &note.content,
                    updated_at: 99,
                })
                .await
                .unwrap();
        }
    }
}

#[async_trait]
impl AttachmentStore for RecordingStore {
    async fn read_object_bounded(
        &self,
        object_key: &str,
        max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        self.reads.lock().unwrap().push(object_key.into());
        let race = std::mem::replace(&mut *self.race.lock().unwrap(), ReadRace::None);
        self.run_race(race).await;
        if matches!(race, ReadRace::EditThenFail) {
            anyhow::bail!("controlled missing object");
        }
        let bytes = self
            .objects
            .lock()
            .unwrap()
            .get(object_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("controlled missing object"))?;
        if bytes.len() as u64 > max_bytes {
            anyhow::bail!("attachment object exceeds bounded read limit");
        }
        Ok(bytes)
    }

    async fn read_legacy(&self, _note_id: &str, _path: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("unbounded reads must not be used")
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
    Arc<RecordingStore>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("export.db"))
            .await
            .unwrap(),
    );
    let store = Arc::new(RecordingStore::new(backend.clone()));
    let ctx = Context::new(backend.clone(), Arc::new(StubEmbedder), store.clone());
    (ctx, backend, store, dir)
}

fn generated(id: &str, path: &str, key: &str, bytes: &[u8]) -> NoteAttachment {
    NoteAttachment {
        id: id.into(),
        path: path.into(),
        mime: "image/png".into(),
        description: String::new(),
        content: vec![],
        storage: Some(AttachmentStorageMetadata {
            object_key: key.into(),
            storage_generation: format!("generation-{id}"),
            size_bytes: bytes.len() as u64,
            checksum_sha256: format!("{:x}", Sha256::digest(bytes)),
        }),
    }
}

fn captured(content: &str, attachments: Vec<NoteAttachment>) -> CapturedNoteExport {
    CapturedNoteExport::from_note(Note {
        id: "export-note".into(),
        title: "Export".into(),
        content: content.into(),
        attachments,
        labels: vec![],
        created_at: 1,
        updated_at: 2,
        revision: 7,
        deleted_at: None,
    })
    .unwrap()
}

#[test]
fn planning_deduplicates_markdown_reference_and_html_images_without_selecting_links() {
    let snapshot = captured(
        r#"![inline](images/a%20b.png)
[not an image](images/unused.png)
![reference][logo]

[logo]: images/%E7%8C%AB%23%3F%25.png

<img alt="quoted" src="images/a%20b.png"><img src='https://example.test/remote.png'>

![data](data:image/png;base64,YQ==)"#,
        vec![
            attachment("space", "images/a b.png", "image/png"),
            attachment("unicode", "images/猫#?%.png", "image/png"),
            attachment("unused", "images/unused.png", "image/png"),
        ],
    );

    let plan = plan_note_export_assets(&snapshot).unwrap();

    assert_eq!(
        plan.assets()
            .iter()
            .map(|asset| asset.attachment().id.as_str())
            .collect::<Vec<_>>(),
        ["space", "unicode"]
    );
    assert_eq!(plan.omissions().len(), 1);
    assert_eq!(plan.omissions()[0].kind, ExportAssetKind::External);
    assert_eq!(plan.data_images(), ["data:image/png;base64,YQ=="]);
}

#[test]
fn planning_reports_missing_and_non_image_local_destinations() {
    let non_image = captured(
        "![bad](files/report.pdf)",
        vec![attachment("pdf", "files/report.pdf", "application/pdf")],
    );
    assert!(matches!(
        plan_note_export_assets(&non_image),
        Err(NoteExportError::AssetInvalid { .. })
    ));

    let missing = captured("![missing](images/missing.png)", vec![]);
    assert!(matches!(
        plan_note_export_assets(&missing),
        Err(NoteExportError::AssetMissing { .. })
    ));
}

#[test]
fn planning_decodes_once_and_prefers_an_exact_literal_percent_path() {
    let snapshot = captured(
        "![exact](images/raw%20.png) ![encoded](images/encoded%2Fsegment.png) ![percent](images/literal%2520.png)",
        vec![
            attachment("exact", "images/raw%20.png", "image/png"),
            attachment("decoded-shadow", "images/raw .png", "image/png"),
            attachment("segment", "images/encoded/segment.png", "image/png"),
            attachment("percent", "images/literal%20.png", "image/png"),
        ],
    );

    let plan = plan_note_export_assets(&snapshot).unwrap();
    assert_eq!(
        plan.assets()
            .iter()
            .map(|asset| asset.attachment().id.as_str())
            .collect::<Vec<_>>(),
        ["exact", "segment", "percent"]
    );
}

#[tokio::test]
async fn capture_rejects_stale_revision_before_any_asset_read() {
    let (ctx, backend, store, _dir) = recording_context().await;
    seed_note(
        &backend,
        "![image](image.png)",
        &[generated("image", "image.png", "key-7", b"a")],
    )
    .await;

    assert_eq!(
        capture_note_export(&ctx, "export-note", 6)
            .await
            .unwrap_err(),
        NoteExportError::StaleRevision {
            expected: 6,
            current: 7,
        }
    );
    assert!(store.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn freeze_reads_only_captured_referenced_keys_and_remains_frozen_after_edit() {
    let (ctx, backend, store, _dir) = recording_context().await;
    let referenced = generated("image", "image.png", "captured-key", b"image");
    let unused = generated("unused", "unused.png", "unused-key", b"unused");
    seed_note(&backend, "![image](image.png)", &[referenced, unused]).await;
    store.put("captured-key", b"image");
    store.put("unused-key", b"unused");

    let frozen = freeze_note_export(&ctx, "export-note", 7, NoteExportLimits::default())
        .await
        .unwrap();
    assert_eq!(frozen.note.title, "Captured title");
    assert_eq!(frozen.note.labels[0].value, "captured");
    assert_eq!(frozen.assets[0].bytes, b"image");
    assert_eq!(*store.reads.lock().unwrap(), ["captured-key"]);

    let session = backend.session().await.unwrap();
    session
        .update_note_fields(NoteFieldsUpdate {
            id: "export-note",
            expected_revision: 7,
            title: "Later title",
            content: "later body",
            updated_at: 100,
        })
        .await
        .unwrap();
    assert_eq!(frozen.note.title, "Captured title");
    assert_eq!(frozen.note.content, "![image](image.png)");
}

#[tokio::test]
async fn hydration_reports_edit_and_delete_races_from_final_confirmation() {
    for (race, expected) in [
        (
            ReadRace::Edit,
            NoteExportError::StaleRevision {
                expected: 7,
                current: 8,
            },
        ),
        (
            ReadRace::Label,
            NoteExportError::StaleRevision {
                expected: 7,
                current: 7,
            },
        ),
        (
            ReadRace::ReplaceAttachment,
            NoteExportError::StaleRevision {
                expected: 7,
                current: 8,
            },
        ),
        (ReadRace::Delete, NoteExportError::NoteNotFound),
        (
            ReadRace::EditThenFail,
            NoteExportError::StaleRevision {
                expected: 7,
                current: 8,
            },
        ),
    ] {
        let (ctx, backend, store, _dir) = recording_context().await;
        seed_note(
            &backend,
            "![image](image.png)",
            &[generated("image", "image.png", "key-7", b"image")],
        )
        .await;
        store.put("key-7", b"image");
        let captured = capture_note_export(&ctx, "export-note", 7).await.unwrap();
        let plan = plan_note_export_assets(&captured).unwrap();
        store.set_race(race);

        assert_eq!(
            hydrate_note_export(&ctx, captured, plan, NoteExportLimits::default())
                .await
                .unwrap_err(),
            expected
        );
    }
}

#[tokio::test]
async fn legacy_images_use_bounded_legacy_reads_without_fabricating_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("attachments");
    std::fs::create_dir_all(root.join("export-note/images")).unwrap();
    std::fs::write(root.join("export-note/images/legacy.png"), b"legacy").unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("legacy.db"))
            .await
            .unwrap(),
    );
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(root)),
    );
    let legacy = NoteAttachment {
        storage: None,
        content: vec![],
        ..attachment("legacy", "images/legacy.png", "image/png")
    };
    seed_note(&backend, "![legacy](images/legacy.png)", &[legacy]).await;

    let frozen = freeze_note_export(&ctx, "export-note", 7, NoteExportLimits::default())
        .await
        .unwrap();
    assert_eq!(frozen.assets[0].bytes, b"legacy");
    assert!(frozen.assets[0].attachment.storage.is_none());
}

#[tokio::test]
async fn limits_reject_captured_metadata_before_reading_any_object() {
    let (ctx, backend, store, _dir) = recording_context().await;
    seed_note(
        &backend,
        "![a](a.png) ![b](b.png)",
        &[
            generated("a", "a.png", "key-a", b"12345"),
            generated("b", "b.png", "key-b", b"67890"),
        ],
    )
    .await;
    let captured = capture_note_export(&ctx, "export-note", 7).await.unwrap();
    let plan = plan_note_export_assets(&captured).unwrap();
    let limits = NoteExportLimits {
        max_combined_asset_bytes: 9,
        ..NoteExportLimits::default()
    };

    assert_eq!(
        hydrate_note_export(&ctx, captured, plan, limits)
            .await
            .unwrap_err(),
        NoteExportError::CombinedAssetLimitExceeded
    );
    assert!(store.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn generated_asset_size_checksum_missing_and_unsupported_are_typed() {
    for (bytes, expected) in [
        (
            Some(b"longer".as_slice()),
            NoteExportError::AssetSizeMismatch {
                attachment_id: "image".into(),
            },
        ),
        (
            Some(b"other".as_slice()),
            NoteExportError::AssetChecksumMismatch {
                attachment_id: "image".into(),
            },
        ),
        (
            None,
            NoteExportError::AssetMissing {
                destination: "image.png".into(),
            },
        ),
    ] {
        let (ctx, backend, store, _dir) = recording_context().await;
        seed_note(
            &backend,
            "![image](image.png)",
            &[generated("image", "image.png", "key", b"image")],
        )
        .await;
        if let Some(bytes) = bytes {
            store.put("key", bytes);
        }
        assert_eq!(
            freeze_note_export(&ctx, "export-note", 7, NoteExportLimits::default())
                .await
                .unwrap_err(),
            expected
        );
    }

    struct UnsupportedStore;
    #[async_trait]
    impl AttachmentStore for UnsupportedStore {
        async fn read_legacy(&self, _note_id: &str, _path: &str) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("unbounded reads must not be used")
        }
        fn info(&self) -> AttachmentStoreInfo {
            AttachmentStoreInfo {
                engine: "unsupported".into(),
                location: None,
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("unsupported.db"))
            .await
            .unwrap(),
    );
    seed_note(
        &backend,
        "![image](image.png)",
        &[generated("image", "image.png", "key", b"image")],
    )
    .await;
    let ctx = Context::new(backend, Arc::new(StubEmbedder), Arc::new(UnsupportedStore));
    assert_eq!(
        freeze_note_export(&ctx, "export-note", 7, NoteExportLimits::default())
            .await
            .unwrap_err(),
        NoteExportError::UnsupportedBoundedRead
    );
}

#[tokio::test]
async fn live_postgres_capture_contract_or_exact_environment_skip() {
    let Some(database) = support::PgAcceptanceDatabase::provision(
        "live_postgres_capture_contract_or_exact_environment_skip",
    )
    .await
    else {
        return;
    };
    let storage = Arc::new(
        note_storage_pg::PgStorage::connect(database.url(), 2)
            .await
            .unwrap(),
    );
    let backend: Arc<dyn StorageBackend> = storage.clone();
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    seed_note(&backend, "no images", &[]).await;

    let captured = capture_note_export(&ctx, "export-note", 7).await.unwrap();
    assert_eq!(captured.note().revision, 7);

    storage.close().await;
    database.cleanup().await.unwrap();
}
