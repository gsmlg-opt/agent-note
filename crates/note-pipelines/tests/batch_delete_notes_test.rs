mod support;

use note_attachments::FilesystemAttachmentStore;
use note_core::NoteAttachment;
use note_embedding::StubEmbedder;
use note_pipelines::{
    batch_delete_notes, chunk_hash, get_note, list_deleted_note_summaries, BatchDeleteNotesInput,
    BatchDeleteNotesResult, BatchNoteTarget, BatchNoteTargetsValidationError, Context,
    NoteMutationError,
};
use note_storage::{NewNote, StorageBackend, TransactionMode, UpsertNoteChunk};
use std::sync::Arc;
use support::{event_log, EventLog, EventStorageBackend};

const NOTE_REVISION: i64 = 1;

fn target(id: &str, expected_revision: i64) -> BatchNoteTarget {
    BatchNoteTarget {
        id: id.into(),
        expected_revision,
    }
}

fn traced_context(
    backend: Arc<dyn StorageBackend>,
    events: EventLog,
    dir: &tempfile::TempDir,
) -> (Context, Arc<EventStorageBackend>) {
    let traced = Arc::new(EventStorageBackend::new(backend, events));
    (
        Context::new(
            traced.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        ),
        traced,
    )
}

fn test_embedding() -> Vec<f32> {
    let mut embedding = vec![0.0; 1_024];
    embedding[0] = 1.0;
    embedding
}

async fn seed_rich_note(backend: &Arc<dyn StorageBackend>, id: &str) {
    let title = format!("Search title for {id}");
    let content = format!("Search content for {id}");
    let attachments = [NoteAttachment {
        id: "source".into(),
        path: "source.txt".into(),
        mime: "text/plain".into(),
        description: "original payload".into(),
        content: vec![],
        storage: None,
    }];
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_label_key_if_missing("project", "")
        .await
        .unwrap();
    transaction
        .insert_note(NewNote {
            id,
            title: &title,
            content: &content,
            attachments: &attachments,
            created_at: 10,
            updated_at: 10,
            note_revision: NOTE_REVISION,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction
        .attach_label(id, "project", "agent-note")
        .await
        .unwrap();
    let content_hash = chunk_hash(&content);
    transaction
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: id,
            chunk_idx: 0,
            content_hash: &content_hash,
            content: &content,
            note_revision: NOTE_REVISION,
            status: "ready",
            updated_at: 10,
        })
        .await
        .unwrap();
    transaction
        .insert_chunk_embedding(id, 0, &test_embedding())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

async fn assert_active_with_search_data(backend: &Arc<dyn StorageBackend>, id: &str) {
    let session = backend.session().await.unwrap();
    let note = session.get_note(id).await.unwrap().unwrap();
    assert_eq!(note.revision, NOTE_REVISION);
    assert_eq!(session.list_note_chunks(id).await.unwrap().len(), 1);
    let allowed_ids = [id.to_string()];
    assert_eq!(
        session
            .dense_search(&test_embedding(), 10, Some(&allowed_ids))
            .await
            .unwrap(),
        vec![id]
    );
    assert_eq!(
        session
            .title_search("Search", 10, Some(&allowed_ids))
            .await
            .unwrap(),
        vec![id]
    );
}

#[tokio::test]
async fn deletes_two_explicit_active_notes_atomically() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "first").await;
    seed_rich_note(&backend, "second").await;
    let (ctx, traced) = traced_context(backend.clone(), event_log(), &dir);

    let result = batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("second", NOTE_REVISION),
                target("first", NOTE_REVISION),
            ],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        BatchDeleteNotesResult {
            requested: 2,
            deleted: 2,
        }
    );
    assert!(get_note(&ctx, "first").await.unwrap().is_none());
    assert!(get_note(&ctx, "second").await.unwrap().is_none());
    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 2);
    for id in ["first", "second"] {
        let summary = deleted.iter().find(|note| note.id == id).unwrap();
        assert_eq!(summary.revision, NOTE_REVISION + 1);
        assert!(summary
            .labels
            .iter()
            .any(|label| label.key == "project" && label.value == "agent-note"));

        let session = backend.session().await.unwrap();
        let snapshot = session
            .get_deleted_note_snapshot(id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.revision, NOTE_REVISION + 1);
        assert_eq!(snapshot.attachments.len(), 1);
        assert!(session
            .labels_for_note(id)
            .await
            .unwrap()
            .iter()
            .any(|label| { label.key == "project" && label.value == "agent-note" }));
        assert!(session.list_note_chunks(id).await.unwrap().is_empty());
        let allowed_ids = [id.to_string()];
        assert!(session
            .dense_search(&test_embedding(), 10, Some(&allowed_ids))
            .await
            .unwrap()
            .is_empty());
        assert!(session
            .title_search("Search", 10, Some(&allowed_ids))
            .await
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        traced.repository_call_count("active_note_revisions_for_update"),
        1
    );
}

#[tokio::test]
async fn missing_target_rolls_back_without_deleting_valid_sibling() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "active").await;
    let (ctx, _traced) = traced_context(backend.clone(), event_log(), &dir);

    let error = batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("active", NOTE_REVISION),
                target("missing", NOTE_REVISION),
            ],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref(),
        Some(&NoteMutationError::NotFound("missing".into()))
    );
    assert_active_with_search_data(&backend, "active").await;
}

#[tokio::test]
async fn deleted_target_rolls_back_without_deleting_valid_sibling() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "active").await;
    seed_rich_note(&backend, "deleted").await;
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .soft_delete_note("deleted", NOTE_REVISION, 20)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let (ctx, _traced) = traced_context(backend.clone(), event_log(), &dir);

    let error = batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("active", NOTE_REVISION),
                target("deleted", NOTE_REVISION),
            ],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref(),
        Some(&NoteMutationError::NotFound("deleted".into()))
    );
    assert_active_with_search_data(&backend, "active").await;
}

#[tokio::test]
async fn stale_target_rolls_back_without_deleting_valid_sibling() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "alpha").await;
    seed_rich_note(&backend, "beta").await;
    let (ctx, _traced) = traced_context(backend.clone(), event_log(), &dir);

    let error = batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("beta", NOTE_REVISION),
                target("alpha", NOTE_REVISION + 1),
            ],
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref(),
        Some(&NoteMutationError::StaleRevision {
            note_id: "alpha".into(),
            expected_revision: NOTE_REVISION + 1,
            current_revision: NOTE_REVISION,
        })
    );
    assert_active_with_search_data(&backend, "alpha").await;
    assert_active_with_search_data(&backend, "beta").await;
}

#[tokio::test]
async fn rejects_invalid_target_lists_before_starting_a_transaction() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend, events.clone(), &dir);

    for (notes, expected) in [
        (vec![], BatchNoteTargetsValidationError::Empty),
        (
            vec![target("same", 1), target("same", 2)],
            BatchNoteTargetsValidationError::DuplicateId("same".into()),
        ),
        (
            vec![target("   ", 1)],
            BatchNoteTargetsValidationError::BlankId,
        ),
        (
            (0..1_001)
                .map(|index| target(&format!("note-{index}"), 1))
                .collect(),
            BatchNoteTargetsValidationError::TooMany { maximum: 1_000 },
        ),
    ] {
        let error = batch_delete_notes(&ctx, BatchDeleteNotesInput { notes })
            .await
            .unwrap_err();
        assert_eq!(error.downcast_ref(), Some(&expected));
    }

    assert_eq!(*events.lock().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn clear_search_failure_after_first_delete_rolls_back_note_revision_and_search_data() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "first").await;
    seed_rich_note(&backend, "second").await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    traced.fail_next_repository_call("clear_note_search_data");

    assert!(batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("second", NOTE_REVISION),
                target("first", NOTE_REVISION)
            ],
        },
    )
    .await
    .is_err());

    assert_active_with_search_data(&backend, "first").await;
    assert_active_with_search_data(&backend, "second").await;
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
}

#[tokio::test]
async fn commit_failure_wrapper_rolls_back_all_deleted_note_state() {
    let (_base_ctx, backend, dir) = support::test_context().await;
    seed_rich_note(&backend, "first").await;
    seed_rich_note(&backend, "second").await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    traced.fail_next_commit();

    assert!(batch_delete_notes(
        &ctx,
        BatchDeleteNotesInput {
            notes: vec![
                target("first", NOTE_REVISION),
                target("second", NOTE_REVISION)
            ],
        },
    )
    .await
    .is_err());

    assert_active_with_search_data(&backend, "first").await;
    assert_active_with_search_data(&backend, "second").await;
    assert_eq!(*events.lock().unwrap(), vec!["begin", "commit_failed"]);
}
