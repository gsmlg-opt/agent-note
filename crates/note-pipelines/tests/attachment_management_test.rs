mod support;

use note_core::{AttachmentStorageMetadata, NoteAttachment, ValidationError};
use note_embedding::StubEmbedder;
use note_pipelines::{
    chunk_hash, delete_note_attachment, drain_embedding_jobs, get_note, get_note_attachment,
    get_note_attachment_by_id, put_note_attachment, AttachmentMutationError, Context,
    DeleteNoteAttachmentResult, NoteMutationError,
};
use note_storage::{
    AttachmentOperationStatus, NewNote, StorageBackend, TransactionMode, UpsertNoteChunk,
};
use note_storage_turso::TursoStorage;
use std::sync::Arc;
use support::{event_log, test_context, ControlledAttachmentStore, EventLog, EventStorageBackend};

const NOTE_ID: &str = "attachment-note";
const OLD_TIMESTAMP: i64 = 1;
const NOTE_REVISION: i64 = 7;
const NOTE_CONTENT: &str = "stable note body";

fn attachment(
    id: &str,
    path: &str,
    mime: &str,
    description: &str,
    content: &[u8],
) -> NoteAttachment {
    NoteAttachment {
        id: id.into(),
        path: path.into(),
        mime: mime.into(),
        description: description.into(),
        content: content.to_vec(),
        storage: None,
    }
}

fn generated_attachment(
    id: &str,
    path: &str,
    object_key: &str,
    generation: &str,
    content: &[u8],
) -> NoteAttachment {
    NoteAttachment {
        storage: Some(AttachmentStorageMetadata {
            object_key: object_key.into(),
            storage_generation: generation.into(),
            size_bytes: content.len() as u64,
            checksum_sha256: "legacy-test-checksum".into(),
        }),
        ..attachment(id, path, "text/plain", "generated", content)
    }
}

async fn seed_note(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    note_id: &str,
    attachments: &[NoteAttachment],
) {
    seed_note_at(ctx, backend, note_id, attachments, OLD_TIMESTAMP).await;
}

async fn seed_note_at(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    note_id: &str,
    attachments: &[NoteAttachment],
    timestamp: i64,
) {
    let store_info = ctx.attachments().info();
    if store_info.engine == "filesystem" {
        let root = std::path::PathBuf::from(store_info.location.unwrap());
        for attachment in attachments {
            let path = if let Some(storage) = &attachment.storage {
                root.join(&storage.object_key)
            } else {
                root.join(note_id)
                    .join(attachment.path.trim_start_matches("./"))
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &attachment.content).unwrap();
        }
    }
    let metadata = attachments
        .iter()
        .cloned()
        .map(|mut attachment| {
            attachment.content.clear();
            attachment
        })
        .collect::<Vec<_>>();
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_note(NewNote {
            id: note_id,
            title: "Attachment note",
            content: NOTE_CONTENT,
            attachments: &metadata,
            created_at: timestamp,
            updated_at: timestamp,
            note_revision: NOTE_REVISION,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction
        .insert_label_key_if_missing("status", "")
        .await
        .unwrap();
    transaction
        .attach_label(note_id, "status", "stable")
        .await
        .unwrap();
    let content_hash = chunk_hash(NOTE_CONTENT);
    transaction
        .upsert_note_chunk(UpsertNoteChunk {
            note_id,
            chunk_idx: 0,
            content_hash: &content_hash,
            content: NOTE_CONTENT,
            note_revision: NOTE_REVISION,
            status: "pending",
            updated_at: timestamp,
        })
        .await
        .unwrap();
    transaction
        .enqueue_embedding_job(
            note_id,
            0,
            &content_hash,
            NOTE_CONTENT,
            NOTE_REVISION,
            timestamp,
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

async fn stored_note(backend: &Arc<dyn StorageBackend>, note_id: &str) -> note_core::Note {
    backend
        .session()
        .await
        .unwrap()
        .get_note(note_id)
        .await
        .unwrap()
        .unwrap()
}

async fn controlled_context() -> (
    Context,
    Arc<dyn StorageBackend>,
    Arc<EventStorageBackend>,
    Arc<ControlledAttachmentStore>,
    EventLog,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let raw_backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("controlled.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let event_backend = Arc::new(EventStorageBackend::new(raw_backend, events.clone()));
    let backend: Arc<dyn StorageBackend> = event_backend.clone();
    let attachments = Arc::new(ControlledAttachmentStore::new(
        backend.clone(),
        events.clone(),
    ));
    let ctx = Context::new(backend.clone(), Arc::new(StubEmbedder), attachments.clone());
    (ctx, backend, event_backend, attachments, events, dir)
}

#[tokio::test]
async fn generated_attachment_reads_use_only_the_exact_object_key() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    let object_key = "notes/attachment-note/objects/generation-checksum";
    let generated = NoteAttachment {
        storage: Some(AttachmentStorageMetadata {
            object_key: object_key.into(),
            storage_generation: "generation".into(),
            size_bytes: 5,
            checksum_sha256: "checksum".into(),
        }),
        ..attachment(
            "generated-id",
            "user/path-must-not-be-read.bin",
            "application/octet-stream",
            "generated",
            b"ignored legacy bytes",
        )
    };
    seed_note(&ctx, &backend, NOTE_ID, &[generated]).await;
    attachments.set_object_content(object_key, &[0, 1, 2, 254, 255]);
    events.lock().unwrap().clear();

    let by_path = get_note_attachment(&ctx, NOTE_ID, "user/path-must-not-be-read.bin")
        .await
        .unwrap()
        .unwrap();
    let by_id = get_note_attachment_by_id(&ctx, NOTE_ID, "generated-id")
        .await
        .unwrap()
        .unwrap();
    let hydrated = get_note(&ctx, NOTE_ID).await.unwrap().unwrap();

    assert_eq!(by_path.content, vec![0, 1, 2, 254, 255]);
    assert_eq!(by_id.content, by_path.content);
    assert_eq!(hydrated.attachments[0].content, by_path.content);
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            format!("read_object:{object_key}"),
            format!("read_object:{object_key}"),
            format!("read_object:{object_key}"),
        ]
    );
}

#[tokio::test]
async fn legacy_attachment_reads_use_note_id_and_user_path() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment(
            "legacy-id",
            "legacy/user-path.bin",
            "application/octet-stream",
            "legacy",
            b"legacy bytes",
        )],
    )
    .await;
    attachments.set_read_content(NOTE_ID, "legacy/user-path.bin", &[0, 255, 3]);
    events.lock().unwrap().clear();

    get_note_attachment(&ctx, NOTE_ID, "legacy/user-path.bin")
        .await
        .unwrap()
        .unwrap();
    get_note_attachment_by_id(&ctx, NOTE_ID, "legacy-id")
        .await
        .unwrap()
        .unwrap();
    get_note(&ctx, NOTE_ID).await.unwrap().unwrap();

    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "read_legacy:attachment-note:legacy/user-path.bin",
            "read_legacy:attachment-note:legacy/user-path.bin",
            "read_legacy:attachment-note:legacy/user-path.bin",
        ]
    );
}

#[tokio::test]
async fn put_new_id_with_unique_normalized_path_creates_metadata_and_object() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;

    let result = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(
            "report",
            "./reports//today.txt",
            "text/plain",
            "daily report",
            b"today",
        ),
    )
    .await
    .unwrap();

    assert!(result.created);
    assert_eq!(result.attachment.id, "report");
    assert_eq!(result.attachment.path, "./reports//today.txt");
    assert!(result.attachment.content.is_empty());
    assert!(result.attachment.storage.is_some());
    let note = stored_note(&backend, NOTE_ID).await;
    assert_eq!(note.attachments, vec![result.attachment]);
    assert!(note.updated_at > OLD_TIMESTAMP);
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "report")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"today"
    );
}

#[tokio::test]
async fn put_existing_id_at_same_normalized_path_replaces_bytes_and_metadata() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment(
            "report",
            "./reports/today.txt",
            "text/plain",
            "old",
            b"old bytes",
        )],
    )
    .await;

    let result = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(
            "report",
            "reports/./today.txt",
            "application/octet-stream",
            "new",
            b"new bytes",
        ),
    )
    .await
    .unwrap();

    assert!(!result.created);
    assert_eq!(result.attachment.mime, "application/octet-stream");
    assert_eq!(result.attachment.description, "new");
    assert!(result.attachment.content.is_empty());
    let loaded = get_note_attachment_by_id(&ctx, NOTE_ID, "report")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded,
        NoteAttachment {
            content: b"new bytes".to_vec(),
            ..result.attachment
        }
    );
}

#[tokio::test]
async fn replacing_legacy_attachment_uses_explicit_legacy_cleanup_and_completes_intent() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment(
            "report",
            "legacy/report.txt",
            "text/plain",
            "old",
            b"old bytes",
        )],
    )
    .await;
    events.lock().unwrap().clear();

    put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(
            "report",
            "legacy/report.txt",
            "text/plain",
            "new",
            b"new bytes",
        ),
    )
    .await
    .unwrap();

    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event == "delete_legacy:attachment-note:legacy/report.txt"));
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].object_key, "legacy/report.txt");
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
}

#[tokio::test]
async fn identical_legacy_paths_on_two_notes_have_independent_cleanup_operations() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    for note_id in ["legacy-note-a", "legacy-note-b"] {
        seed_note(
            &ctx,
            &backend,
            note_id,
            &[attachment(
                "report",
                "report.txt",
                "text/plain",
                "old",
                note_id.as_bytes(),
            )],
        )
        .await;
        attachments.set_read_content(note_id, "report.txt", note_id.as_bytes());
    }
    events.lock().unwrap().clear();

    for note_id in ["legacy-note-a", "legacy-note-b"] {
        put_note_attachment(
            &ctx,
            note_id,
            NOTE_REVISION,
            attachment(
                "report",
                "report.txt",
                "text/plain",
                "new",
                format!("new-{note_id}").as_bytes(),
            ),
        )
        .await
        .unwrap();
    }

    for note_id in ["legacy-note-a", "legacy-note-b"] {
        let operations = backend
            .session()
            .await
            .unwrap()
            .list_attachment_operations_for_note(note_id)
            .await
            .unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].object_key, "report.txt");
        assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == &format!("delete_legacy:{note_id}:report.txt")));
    }
}

#[tokio::test]
async fn replacing_generated_attachment_activates_new_key_and_completes_old_cleanup() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    let old_key = "notes/attachment-note/objects/old-generation";
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[generated_attachment(
            "report",
            "report.txt",
            old_key,
            "old-generation",
            b"old",
        )],
    )
    .await;
    attachments.set_object_content(old_key, b"old");
    events.lock().unwrap().clear();

    let result = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("report", "report.txt", "text/plain", "new", b"new"),
    )
    .await
    .unwrap();

    let new_key = &result.attachment.storage.as_ref().unwrap().object_key;
    assert_ne!(new_key, old_key);
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0]
            .storage
            .as_ref()
            .unwrap()
            .object_key,
        *new_key
    );
    assert!(!attachments.has_object(old_key));
    assert!(attachments.has_object(new_key));
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].storage_generation, "old-generation");
    assert_eq!(operations[0].object_key, old_key);
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event == &format!("delete_object:{old_key}")));
}

#[tokio::test]
async fn standalone_attachment_ids_use_one_trimmed_canonical_identity() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("foo", "foo.txt", "text/plain", "old", b"old")],
    )
    .await;

    let replaced = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(" foo ", "./foo.txt", "text/plain", "new", b"new"),
    )
    .await
    .unwrap();

    assert!(!replaced.created);
    assert_eq!(replaced.attachment.id, "foo");
    assert_eq!(stored_note(&backend, NOTE_ID).await.attachments.len(), 1);
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, " foo ")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"new"
    );
    assert!(
        delete_note_attachment(&ctx, NOTE_ID, " foo ", NOTE_REVISION + 1)
            .await
            .unwrap()
    );

    let (legacy_ctx, legacy_backend, _dir) = test_context().await;
    seed_note(
        &legacy_ctx,
        &legacy_backend,
        "legacy-id-note",
        &[attachment(
            " legacy ",
            "legacy.txt",
            "text/plain",
            "old",
            b"old",
        )],
    )
    .await;
    let legacy = put_note_attachment(
        &legacy_ctx,
        "legacy-id-note",
        NOTE_REVISION,
        attachment("legacy", "./legacy.txt", "text/plain", "new", b"new"),
    )
    .await
    .unwrap();
    assert!(!legacy.created);
    assert_eq!(legacy.attachment.id, " legacy ");
    assert_eq!(
        stored_note(&legacy_backend, "legacy-id-note")
            .await
            .attachments[0]
            .id,
        " legacy "
    );
}

#[tokio::test]
async fn a_new_standalone_attachment_stores_its_trimmed_canonical_id() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;

    let result = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(" new ", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap();

    assert!(result.created);
    assert_eq!(result.attachment.id, "new");
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "new"
    );
}

#[tokio::test]
async fn put_rejects_path_changes_and_normalized_path_collisions_without_mutation() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("first", "./files/first.txt", "text/plain", "", b"first"),
            attachment("second", "files/second.txt", "text/plain", "", b"second"),
        ],
    )
    .await;
    let before = stored_note(&backend, NOTE_ID).await;

    let changed_path = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("first", "files/moved.txt", "text/plain", "", b"changed"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        changed_path.downcast_ref::<AttachmentMutationError>(),
        Some(&AttachmentMutationError::AttachmentPathChange {
            attachment_id: "first".into()
        })
    );

    let collision = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment(
            "third",
            "./files//second.txt",
            "text/plain",
            "",
            b"collision",
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(
        collision.downcast_ref::<AttachmentMutationError>(),
        Some(&AttachmentMutationError::AttachmentPathCollision {
            path: "files/second.txt".into()
        })
    );

    assert_eq!(stored_note(&backend, NOTE_ID).await, before);
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "first")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"first"
    );
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "second")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"second"
    );
}

#[tokio::test]
async fn put_reports_stale_revision_before_errors_derived_from_newer_attachment_state() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("first", "first.txt", "text/plain", "", b"first"),
            attachment("second", "second.txt", "text/plain", "", b"second"),
        ],
    )
    .await;

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION - 1,
        attachment("first", "second.txt", "text/plain", "", b"stale"),
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::StaleRevision {
            note_id: NOTE_ID.into(),
            expected_revision: 6,
            current_revision: NOTE_REVISION,
        })
    );
    assert_eq!(stored_note(&backend, NOTE_ID).await.revision, NOTE_REVISION);
}

#[tokio::test]
async fn put_rejects_invalid_id_mime_and_paths_before_mutation() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();
    let cases = [
        (
            attachment("", "file.txt", "text/plain", "", b"x"),
            ValidationError::EmptyAttachmentId,
        ),
        (
            attachment("file", "file.txt", "", "", b"x"),
            ValidationError::EmptyAttachmentMime,
        ),
        (
            attachment("file", "", "text/plain", "", b"x"),
            ValidationError::EmptyAttachmentPath,
        ),
        (
            attachment("file", ".", "text/plain", "", b"x"),
            ValidationError::EmptyAttachmentPath,
        ),
        (
            attachment("file", "./", "text/plain", "", b"x"),
            ValidationError::EmptyAttachmentPath,
        ),
        (
            attachment("file", "/etc/passwd", "text/plain", "", b"x"),
            ValidationError::InvalidAttachmentPath("/etc/passwd".into()),
        ),
        (
            attachment("file", "../secret", "text/plain", "", b"x"),
            ValidationError::InvalidAttachmentPath("../secret".into()),
        ),
        (
            attachment("file", "https://example.test/file", "text/plain", "", b"x"),
            ValidationError::InvalidAttachmentPath("https://example.test/file".into()),
        ),
    ];

    for (invalid, expected) in cases {
        let error = put_note_attachment(&ctx, NOTE_ID, NOTE_REVISION, invalid)
            .await
            .unwrap_err();
        assert_eq!(error.downcast_ref::<ValidationError>(), Some(&expected));
    }

    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn get_by_id_reads_only_the_selected_object() {
    let (ctx, backend, dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("selected", "selected.txt", "text/plain", "", b"selected"),
            attachment("missing", "missing.txt", "text/plain", "", b"sibling"),
        ],
    )
    .await;
    tokio::fs::remove_file(
        dir.path()
            .join("attachments")
            .join(NOTE_ID)
            .join("missing.txt"),
    )
    .await
    .unwrap();

    let selected = get_note_attachment_by_id(&ctx, NOTE_ID, "selected")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(selected.content, b"selected");
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "absent")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn get_by_id_retries_when_the_old_path_is_reassigned_to_another_id() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("target", "old.txt", "text/plain", "", b""),
            attachment("other", "other.txt", "text/plain", "", b""),
        ],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.set_read_content(NOTE_ID, "old.txt", b"wrong bytes");
    attachments.set_read_content(NOTE_ID, "new.txt", b"right bytes");
    attachments.race_attachment_paths_after_read(
        NOTE_ID,
        "old.txt",
        &[("target", "new.txt"), ("other", "old.txt")],
    );

    let loaded = get_note_attachment_by_id(&ctx, NOTE_ID, " target ")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(loaded.id, "target");
    assert_eq!(loaded.path, "new.txt");
    assert_eq!(loaded.content, b"right bytes");
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "read_legacy:attachment-note:old.txt",
            "race_read_paths:attachment-note:old.txt",
            "read_legacy:attachment-note:new.txt",
        ]
    );
}

#[tokio::test]
async fn get_by_id_retries_a_missing_old_object_when_metadata_moved_to_a_new_path() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("target", "old.txt", "text/plain", "", b""),
            attachment("other", "other.txt", "text/plain", "", b""),
        ],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.fail_read_once(NOTE_ID, "old.txt", "controlled old object not found");
    attachments.set_read_content(NOTE_ID, "new.txt", b"new bytes");
    attachments.race_attachment_paths_after_read(
        NOTE_ID,
        "old.txt",
        &[("target", "new.txt"), ("other", "old.txt")],
    );

    let loaded = get_note_attachment_by_id(&ctx, NOTE_ID, "target")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(loaded.path, "new.txt");
    assert_eq!(loaded.content, b"new bytes");
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "read_legacy:attachment-note:old.txt",
            "race_read_paths:attachment-note:old.txt",
            "read_legacy:attachment-note:new.txt",
        ]
    );
}

#[tokio::test]
async fn get_by_id_preserves_a_read_failure_when_metadata_still_maps_to_the_same_path() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("target", "same.txt", "text/plain", "", b"")],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.fail_read_once(NOTE_ID, "same.txt", "sentinel unchanged-path read failure");

    let error = get_note_attachment_by_id(&ctx, NOTE_ID, "target")
        .await
        .unwrap_err();

    assert_eq!(format!("{error:#}"), "sentinel unchanged-path read failure");
    assert_eq!(
        events.lock().unwrap().clone(),
        vec!["read_legacy:attachment-note:same.txt"]
    );
}

#[tokio::test]
async fn get_by_id_returns_a_typed_conflict_after_a_second_path_reassignment() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("target", "first.txt", "text/plain", "", b""),
            attachment("other", "other.txt", "text/plain", "", b""),
        ],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.set_read_content(NOTE_ID, "first.txt", b"first wrong bytes");
    attachments.set_read_content(NOTE_ID, "second.txt", b"second wrong bytes");
    attachments.race_attachment_paths_after_read(
        NOTE_ID,
        "first.txt",
        &[("target", "second.txt"), ("other", "first.txt")],
    );
    attachments.race_attachment_paths_after_read(
        NOTE_ID,
        "second.txt",
        &[("target", "third.txt"), ("other", "second.txt")],
    );

    let error = get_note_attachment_by_id(&ctx, NOTE_ID, "target")
        .await
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<AttachmentMutationError>(),
        Some(&AttachmentMutationError::AttachmentPathChange {
            attachment_id: "target".into()
        })
    );
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "read_legacy:attachment-note:first.txt",
            "race_read_paths:attachment-note:first.txt",
            "read_legacy:attachment-note:second.txt",
            "race_read_paths:attachment-note:second.txt",
        ]
    );
}

#[tokio::test]
async fn delete_removes_only_selected_metadata_and_object_and_is_idempotent() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("first", "first.txt", "text/plain", "", b"first"),
            attachment("second", "second.txt", "text/plain", "", b"second"),
        ],
    )
    .await;

    assert!(
        delete_note_attachment(&ctx, NOTE_ID, "first", NOTE_REVISION)
            .await
            .unwrap()
    );
    assert!(
        !delete_note_attachment(&ctx, NOTE_ID, "first", NOTE_REVISION + 1)
            .await
            .unwrap()
    );
    let error = delete_note_attachment(&ctx, "missing-note", "first", NOTE_REVISION)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::NotFound("missing-note".into()))
    );
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "first")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        get_note_attachment_by_id(&ctx, NOTE_ID, "second")
            .await
            .unwrap()
            .unwrap()
            .content,
        b"second"
    );
    let note = stored_note(&backend, NOTE_ID).await;
    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].id, "second");
    assert!(note.updated_at > OLD_TIMESTAMP);
}

#[tokio::test]
async fn delete_succeeds_when_the_selected_physical_object_is_already_missing() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[generated_attachment(
            "old",
            "old.txt",
            "notes/attachment-note/objects/already-missing",
            "already-missing",
            b"old",
        )],
    )
    .await;

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION)
        .await
        .unwrap());
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].storage_generation, "already-missing");
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
}

#[tokio::test]
async fn put_advances_revision_once_and_preserves_chunks_labels_and_embedding_queue() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 1);
    let observer = backend.session().await.unwrap();
    let chunks_before = observer.list_note_chunks(NOTE_ID).await.unwrap();
    let labels_before = observer.labels_for_note(NOTE_ID).await.unwrap();

    put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap();

    assert_eq!(
        observer.get_note_revision(NOTE_ID).await.unwrap(),
        Some(NOTE_REVISION + 1)
    );
    assert_eq!(
        observer.list_note_chunks(NOTE_ID).await.unwrap(),
        chunks_before
    );
    assert_eq!(
        observer.labels_for_note(NOTE_ID).await.unwrap(),
        labels_before
    );
    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 0);
}

#[tokio::test]
async fn stale_put_rolls_back_metadata_and_deletes_the_new_generation() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION - 1,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            expected_revision: 6,
            current_revision: NOTE_REVISION,
            ..
        })
    ));
    let stored = stored_note(&backend, NOTE_ID).await;
    assert_eq!(stored.revision, NOTE_REVISION);
    assert!(stored.attachments.is_empty());
    let logged = events.lock().unwrap().clone();
    let object_key = logged[0]
        .strip_prefix("put_object:")
        .expect("first event is immutable publication");
    assert!(logged.iter().any(|event| event == "rollback"));
    assert!(logged
        .iter()
        .any(|event| event == &format!("delete_object:{object_key}")));
    assert!(!logged
        .iter()
        .any(|event| event.starts_with("delete_legacy:")));
}

#[tokio::test]
async fn put_advances_updated_at_when_the_stored_timestamp_is_the_current_second() {
    let (ctx, backend, _dir) = test_context().await;
    let current_second = chrono::Utc::now().timestamp();
    seed_note_at(&ctx, &backend, NOTE_ID, &[], current_second).await;

    put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap();

    assert!(stored_note(&backend, NOTE_ID).await.updated_at > current_second);
}

#[tokio::test]
async fn delete_advances_revision_once_and_preserves_chunks_labels_and_embedding_queue() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 1);
    let observer = backend.session().await.unwrap();
    let chunks_before = observer.list_note_chunks(NOTE_ID).await.unwrap();
    let labels_before = observer.labels_for_note(NOTE_ID).await.unwrap();

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION)
        .await
        .unwrap());

    assert_eq!(
        observer.get_note_revision(NOTE_ID).await.unwrap(),
        Some(NOTE_REVISION + 1)
    );
    assert_eq!(
        observer.list_note_chunks(NOTE_ID).await.unwrap(),
        chunks_before
    );
    assert_eq!(
        observer.labels_for_note(NOTE_ID).await.unwrap(),
        labels_before
    );
    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 0);
}

#[tokio::test]
async fn delete_advances_updated_at_when_the_stored_timestamp_is_the_current_second() {
    let (ctx, backend, _dir) = test_context().await;
    let current_second = chrono::Utc::now().timestamp();
    seed_note_at(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
        current_second,
    )
    .await;

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION)
        .await
        .unwrap());

    assert!(stored_note(&backend, NOTE_ID).await.updated_at > current_second);
}

#[tokio::test]
async fn put_and_delete_do_not_hydrate_or_rewrite_sibling_objects() {
    let (ctx, backend, dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[
            attachment("target", "target.txt", "text/plain", "", b"target"),
            attachment("sibling", "sibling.txt", "text/plain", "", b"sibling"),
        ],
    )
    .await;
    let sibling_path = dir
        .path()
        .join("attachments")
        .join(NOTE_ID)
        .join("sibling.txt");
    tokio::fs::remove_file(&sibling_path).await.unwrap();

    put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("target", "./target.txt", "text/plain", "", b"replacement"),
    )
    .await
    .unwrap();
    assert!(!sibling_path.exists());
    assert!(
        delete_note_attachment(&ctx, NOTE_ID, "target", NOTE_REVISION + 1)
            .await
            .unwrap()
    );
    assert!(!sibling_path.exists());
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "sibling"
    );
}

#[tokio::test]
async fn missing_note_contract_is_typed_for_mutations_and_idempotent_for_reads() {
    let (ctx, _backend, _dir) = test_context().await;

    let error = put_note_attachment(
        &ctx,
        "missing-note",
        NOTE_REVISION,
        attachment("file", "file.txt", "text/plain", "", b"file"),
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::NotFound("missing-note".into()))
    );
    assert_eq!(
        get_note_attachment_by_id(&ctx, "missing-note", "file")
            .await
            .unwrap(),
        None
    );
    let error = delete_note_attachment(&ctx, "missing-note", "file", NOTE_REVISION)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<NoteMutationError>(),
        Some(&NoteMutationError::NotFound("missing-note".into()))
    );
}

#[tokio::test]
async fn ambiguous_put_commit_failure_retains_the_new_generation() {
    let (ctx, backend, event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();
    event_backend.fail_next_commit();

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("controlled commit failure"));
    assert!(message.contains("generated object retained"));
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let events = events.lock().unwrap().clone();
    assert!(events[0].starts_with("put_object:"));
    assert_eq!(&events[1..], ["begin", "commit_failed"]);
    let object_key = events[0].strip_prefix("put_object:").unwrap();
    assert!(attachments.has_object(object_key));
}

#[tokio::test]
async fn put_cleanup_enqueue_failure_rolls_back_metadata_and_new_generation() {
    let (ctx, backend, event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "old", b"old")],
    )
    .await;
    events.lock().unwrap().clear();
    event_backend.fail_next_repository_call("insert_attachment_operation");

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("old", "old.txt", "text/plain", "new", b"new"),
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}")
        .contains("controlled repository failure at insert_attachment_operation"));
    let stored = stored_note(&backend, NOTE_ID).await;
    assert_eq!(stored.revision, NOTE_REVISION);
    assert_eq!(stored.attachments[0].description, "old");
    let logged = events.lock().unwrap().clone();
    let new_key = logged[0].strip_prefix("put_object:").unwrap();
    assert!(!attachments.has_object(new_key));
    assert!(logged.iter().any(|event| event == "rollback"));
    assert!(backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn transaction_failure_rolls_back_delete_metadata_and_cleanup_intent() {
    let (ctx, backend, event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    events.lock().unwrap().clear();
    event_backend.fail_next_commit();

    let error = delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION)
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("controlled commit failure"));
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "old"
    );
    assert_eq!(
        events.lock().unwrap().clone(),
        vec!["begin", "commit_failed"]
    );
}

#[tokio::test]
async fn put_is_object_first_and_delete_cleanup_runs_after_commit() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    events.lock().unwrap().clear();

    put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap();
    assert!(
        delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION + 1)
            .await
            .unwrap()
    );

    let events = events.lock().unwrap().clone();
    assert!(events[0].starts_with("put_object:"));
    assert_eq!(
        &events[1..],
        [
            "begin",
            "commit",
            "begin",
            "commit",
            "delete_legacy:attachment-note:old.txt",
        ]
    );
}

#[tokio::test]
async fn put_object_failure_occurs_before_begin_and_never_activates_metadata() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();
    attachments.fail_put_on_call(1);

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        NOTE_REVISION,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("controlled immutable put failure"));
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let events = events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("put_object:"));
}

#[tokio::test]
async fn delete_cleanup_failure_returns_success_and_leaves_a_safe_pending_retry() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.fail_delete_object();

    let result = delete_note_attachment(&ctx, NOTE_ID, "old", NOTE_REVISION)
        .await
        .unwrap();

    assert_eq!(result, DeleteNoteAttachmentResult::Applied { revision: 8 });
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status, AttachmentOperationStatus::Pending);
    assert_eq!(
        operations[0].last_error.as_deref(),
        Some("attachment cleanup failed; retry pending")
    );
    assert_eq!(operations[0].lease_owner, None);
    assert_eq!(operations[0].lease_expires_at, None);
    assert_eq!(
        events.lock().unwrap().clone(),
        vec!["begin", "commit", "delete_legacy:attachment-note:old.txt",]
    );
}

#[tokio::test]
async fn delete_checks_revision_inside_the_metadata_transaction() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment(
            "moving",
            "first.txt",
            "text/plain",
            "",
            b"bytes",
        )],
    )
    .await;
    events.lock().unwrap().clear();
    let error = delete_note_attachment(&ctx, NOTE_ID, "moving", NOTE_REVISION - 1)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<NoteMutationError>(),
        Some(NoteMutationError::StaleRevision {
            expected_revision: 6,
            current_revision: NOTE_REVISION,
            ..
        })
    ));

    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].path,
        "first.txt"
    );
    assert_eq!(events.lock().unwrap().clone(), vec!["begin", "rollback"]);
}

#[tokio::test]
async fn replaying_a_completed_delete_mutation_is_harmless() {
    let (ctx, backend, _event_backend, _attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment(
            "moving",
            "first.txt",
            "text/plain",
            "",
            b"bytes",
        )],
    )
    .await;
    events.lock().unwrap().clear();
    let first = delete_note_attachment(&ctx, NOTE_ID, "moving", NOTE_REVISION)
        .await
        .unwrap();
    let replay = delete_note_attachment(&ctx, NOTE_ID, "moving", NOTE_REVISION + 1)
        .await
        .unwrap();

    assert_eq!(first, DeleteNoteAttachmentResult::Applied { revision: 8 });
    assert_eq!(replay, DeleteNoteAttachmentResult::Absent);
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let operations = backend
        .session()
        .await
        .unwrap()
        .list_attachment_operations_for_note(NOTE_ID)
        .await
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status, AttachmentOperationStatus::Completed);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.starts_with("delete_legacy:"))
            .count(),
        1
    );
}
