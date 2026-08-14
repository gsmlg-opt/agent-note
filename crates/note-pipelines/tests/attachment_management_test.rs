mod support;

use note_core::{LabelValueType, NoteAttachment, ValidationError};
use note_embedding::StubEmbedder;
use note_pipelines::{
    chunk_hash, delete_note_attachment, drain_embedding_jobs, get_note_attachment_by_id,
    put_note_attachment, AttachmentMutationError, Context,
};
use note_storage::{NewNote, StorageBackend, TransactionMode, UpsertNoteChunk};
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
    let prepared = ctx
        .attachments()
        .prepare(note_id, attachments)
        .await
        .unwrap();
    let metadata = prepared.metadata().to_vec();
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
        .insert_label_key_with_type("status", "", LabelValueType::Text)
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
    prepared.publish().await.unwrap();
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
async fn put_new_id_with_unique_normalized_path_creates_metadata_and_object() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;

    let result = put_note_attachment(
        &ctx,
        NOTE_ID,
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
    assert!(delete_note_attachment(&ctx, NOTE_ID, " foo ")
        .await
        .unwrap());

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
        let error = put_note_attachment(&ctx, NOTE_ID, invalid)
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
            "read:attachment-note:old.txt",
            "race_read_paths:attachment-note:old.txt",
            "read:attachment-note:new.txt",
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
            "read:attachment-note:old.txt",
            "race_read_paths:attachment-note:old.txt",
            "read:attachment-note:new.txt",
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
        vec!["read:attachment-note:same.txt"]
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
            "read:attachment-note:first.txt",
            "race_read_paths:attachment-note:first.txt",
            "read:attachment-note:second.txt",
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

    assert!(delete_note_attachment(&ctx, NOTE_ID, "first")
        .await
        .unwrap());
    assert!(!delete_note_attachment(&ctx, NOTE_ID, "first")
        .await
        .unwrap());
    assert!(!delete_note_attachment(&ctx, "missing-note", "first")
        .await
        .unwrap());
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
    let (ctx, backend, dir) = test_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    tokio::fs::remove_file(dir.path().join("attachments").join(NOTE_ID).join("old.txt"))
        .await
        .unwrap();

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old").await.unwrap());
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
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
async fn put_advances_updated_at_when_the_stored_timestamp_is_the_current_second() {
    let (ctx, backend, _dir) = test_context().await;
    let current_second = chrono::Utc::now().timestamp();
    seed_note_at(&ctx, &backend, NOTE_ID, &[], current_second).await;

    put_note_attachment(
        &ctx,
        NOTE_ID,
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

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old").await.unwrap());

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

    assert!(delete_note_attachment(&ctx, NOTE_ID, "old").await.unwrap());

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
        attachment("target", "./target.txt", "text/plain", "", b"replacement"),
    )
    .await
    .unwrap();
    assert!(!sibling_path.exists());
    assert!(delete_note_attachment(&ctx, NOTE_ID, "target")
        .await
        .unwrap());
    assert!(!sibling_path.exists());
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "sibling"
    );
}

#[tokio::test]
async fn missing_note_contract_is_typed_for_put_and_idempotent_for_reads_and_delete() {
    let (ctx, _backend, _dir) = test_context().await;

    let error = put_note_attachment(
        &ctx,
        "missing-note",
        attachment("file", "file.txt", "text/plain", "", b"file"),
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<AttachmentMutationError>(),
        Some(&AttachmentMutationError::NoteNotFound(
            "missing-note".into()
        ))
    );
    assert_eq!(
        get_note_attachment_by_id(&ctx, "missing-note", "file")
            .await
            .unwrap(),
        None
    );
    assert!(!delete_note_attachment(&ctx, "missing-note", "file")
        .await
        .unwrap());
}

#[tokio::test]
async fn transaction_failure_aborts_put_and_preserves_primary_and_abort_context() {
    let (ctx, backend, event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();
    event_backend.fail_next_commit();
    attachments.fail_abort();

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("controlled commit failure"));
    assert!(message.contains("attachment mutation abort also failed"));
    assert!(message.contains("controlled mutation abort failure"));
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    let events = events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "prepare_put:attachment-note:new.txt",
            "begin",
            "commit_failed",
            "abort_put:attachment-note:new.txt",
        ]
    );
}

#[tokio::test]
async fn transaction_failure_aborts_delete_without_publishing_or_removing_metadata() {
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

    let error = delete_note_attachment(&ctx, NOTE_ID, "old")
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("controlled commit failure"));
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "old"
    );
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "prepare_delete:attachment-note:old.txt",
            "begin",
            "commit_failed",
            "abort_delete:attachment-note:old.txt",
        ]
    );
}

#[tokio::test]
async fn put_and_delete_publish_only_after_metadata_commit() {
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
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap();
    assert!(delete_note_attachment(&ctx, NOTE_ID, "old").await.unwrap());

    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "prepare_put:attachment-note:new.txt",
            "begin",
            "commit",
            "publish_put:attachment-note:new.txt",
            "prepare_delete:attachment-note:old.txt",
            "begin",
            "commit",
            "publish_delete:attachment-note:old.txt",
        ]
    );
}

#[tokio::test]
async fn put_publication_failure_reports_committed_active_metadata_without_aborting() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(&ctx, &backend, NOTE_ID, &[]).await;
    events.lock().unwrap().clear();
    attachments.fail_publish();

    let error = put_note_attachment(
        &ctx,
        NOTE_ID,
        attachment("new", "new.txt", "text/plain", "", b"new"),
    )
    .await
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("attachment metadata is committed"));
    assert!(message.contains("object publication failed"));
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].id,
        "new"
    );
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "prepare_put:attachment-note:new.txt",
            "begin",
            "commit",
            "publish_put:attachment-note:new.txt",
        ]
    );
}

#[tokio::test]
async fn delete_publication_failure_reports_committed_deletion_and_possible_orphan() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
    seed_note(
        &ctx,
        &backend,
        NOTE_ID,
        &[attachment("old", "old.txt", "text/plain", "", b"old")],
    )
    .await;
    events.lock().unwrap().clear();
    attachments.fail_publish();

    let error = delete_note_attachment(&ctx, NOTE_ID, "old")
        .await
        .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("metadata deletion is committed"));
    assert!(message.contains("physical cleanup failed"));
    assert!(message.contains("orphan"));
    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "prepare_delete:attachment-note:old.txt",
            "begin",
            "commit",
            "publish_delete:attachment-note:old.txt",
        ]
    );
}

#[tokio::test]
async fn delete_retries_once_when_the_path_changes_under_its_prepared_lock() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
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
    attachments.race_attachment_path_on_delete(NOTE_ID, "moving", "second.txt");

    assert!(delete_note_attachment(&ctx, NOTE_ID, "moving")
        .await
        .unwrap());

    assert!(stored_note(&backend, NOTE_ID).await.attachments.is_empty());
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "prepare_delete:attachment-note:first.txt",
            "race_delete_path:attachment-note:moving:second.txt",
            "begin",
            "rollback",
            "abort_delete:attachment-note:first.txt",
            "prepare_delete:attachment-note:second.txt",
            "begin",
            "commit",
            "publish_delete:attachment-note:second.txt",
        ]
    );
}

#[tokio::test]
async fn delete_rejects_a_second_path_change_without_publishing_the_wrong_object() {
    let (ctx, backend, _event_backend, attachments, events, _dir) = controlled_context().await;
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
    attachments.race_attachment_path_on_delete(NOTE_ID, "moving", "second.txt");
    attachments.race_attachment_path_on_delete(NOTE_ID, "moving", "third.txt");

    let error = delete_note_attachment(&ctx, NOTE_ID, "moving")
        .await
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<AttachmentMutationError>(),
        Some(&AttachmentMutationError::AttachmentPathChange {
            attachment_id: "moving".into()
        })
    );
    assert_eq!(
        stored_note(&backend, NOTE_ID).await.attachments[0].path,
        "third.txt"
    );
    let events = events.lock().unwrap().clone();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("abort_delete:"))
            .count(),
        2
    );
    assert!(events
        .iter()
        .all(|event| !event.starts_with("publish_delete:")));
}
