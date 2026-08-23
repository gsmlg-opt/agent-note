mod support;

use note_attachments::FilesystemAttachmentStore;
use note_core::{LabelKeyValidationError, LabelValueType};
use note_embedding::StubEmbedder;
use note_pipelines::{
    batch_update_note_labels, BatchLabelAction, BatchNoteTarget, BatchNoteTargetsValidationError,
    BatchUpdateNoteLabelsInput, BatchUpdateNoteLabelsResult, Context, NoteMutationError,
    NoteMutationNotifier,
};
use note_storage::{NewNote, StorageBackend, TransactionMode};
use std::sync::Arc;
use support::{event_log, test_context, EventLog, EventStorageBackend};

const NOTE_REVISION: i64 = 7;
const UPDATED_AT: i64 = 20;

struct MutationNotifier(EventLog);

impl NoteMutationNotifier for MutationNotifier {
    fn note_mutated(&self) {
        self.0.lock().unwrap().push("note_mutated".into());
    }
}

fn target(id: &str, expected_revision: i64) -> BatchNoteTarget {
    BatchNoteTarget {
        id: id.into(),
        expected_revision,
    }
}

async fn seed_note(backend: &Arc<dyn StorageBackend>, id: &str, project: Option<&str>) {
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    for key in ["type", "project"] {
        transaction
            .insert_label_key_if_missing(key, "")
            .await
            .unwrap();
    }
    transaction
        .insert_note(NewNote {
            id,
            title: id,
            content: id,
            attachments: &[],
            created_at: 10,
            updated_at: UPDATED_AT,
            note_revision: NOTE_REVISION,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction.attach_label(id, "type", "note").await.unwrap();
    if let Some(value) = project {
        transaction
            .attach_label(id, "project", value)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
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

#[tokio::test]
async fn rejects_invalid_target_lists_before_starting_a_transaction() {
    let (base_ctx, backend, dir) = test_context().await;
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
        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes,
                action: BatchLabelAction::Add {
                    key: "project".into(),
                    value: "agent-note".into(),
                },
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.downcast_ref(), Some(&expected));
    }

    assert_eq!(*events.lock().unwrap(), Vec::<String>::new());
    drop(base_ctx);
}

#[tokio::test]
async fn adds_only_missing_labels_for_explicit_targets_regardless_of_input_order() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "already", Some("existing")).await;
    seed_note(&backend, "missing", None).await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));

    let result = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![
                target("missing", NOTE_REVISION),
                target("already", NOTE_REVISION),
            ],
            action: BatchLabelAction::Add {
                key: "project".into(),
                value: "agent-note".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        BatchUpdateNoteLabelsResult {
            requested: 2,
            updated: 1,
            unchanged: 1,
        }
    );
    let session = backend.session().await.unwrap();
    let already = session.labels_for_note("already").await.unwrap();
    let missing = session.labels_for_note("missing").await.unwrap();
    assert!(already
        .iter()
        .any(|label| label.key == "project" && label.value == "existing"));
    assert!(missing
        .iter()
        .any(|label| label.key == "project" && label.value == "agent-note"));
    assert_eq!(
        traced.repository_call_count("active_note_revisions_for_update"),
        1
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", "note_mutated"]
    );
    drop(base_ctx);
}

#[tokio::test]
async fn creates_a_missing_catalog_key_as_text_only_for_eligible_targets() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "candidate", None).await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend.clone(), events, &dir);

    let result = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("candidate", NOTE_REVISION)],
            action: BatchLabelAction::Add {
                key: "custom".into(),
                value: "value".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(result.updated, 1);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session
            .list_label_keys()
            .await
            .unwrap()
            .into_iter()
            .find(|label_key| label_key.key == "custom")
            .unwrap()
            .value_type,
        LabelValueType::Text
    );
    assert!(session
        .labels_for_note("candidate")
        .await
        .unwrap()
        .iter()
        .any(|label| label.key == "custom" && label.value == "value"));
    drop(base_ctx);
}

#[tokio::test]
async fn all_existing_additions_are_a_timestamp_and_notification_noop() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "existing", Some("keep")).await;
    let before = backend
        .session()
        .await
        .unwrap()
        .get_note("existing")
        .await
        .unwrap()
        .unwrap()
        .updated_at;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));

    let result = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("existing", NOTE_REVISION)],
            action: BatchLabelAction::Add {
                key: "project".into(),
                value: "ignored".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        BatchUpdateNoteLabelsResult {
            requested: 1,
            updated: 0,
            unchanged: 1,
        }
    );
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_note("existing")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        before
    );
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
    drop(base_ctx);
}

#[tokio::test]
async fn missing_and_stale_targets_roll_back_before_catalog_or_label_mutations() {
    for (bad_target, expected_error) in [
        (
            target("missing", NOTE_REVISION),
            NoteMutationError::NotFound("missing".into()),
        ),
        (
            target("valid", NOTE_REVISION + 1),
            NoteMutationError::StaleRevision {
                note_id: "valid".into(),
                expected_revision: NOTE_REVISION + 1,
                current_revision: NOTE_REVISION,
            },
        ),
    ] {
        let (base_ctx, backend, dir) = test_context().await;
        seed_note(&backend, "valid", None).await;
        seed_note(&backend, "sibling", None).await;
        let events = event_log();
        let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);

        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes: vec![bad_target, target("sibling", NOTE_REVISION)],
                action: BatchLabelAction::Add {
                    key: "new-key".into(),
                    value: "value".into(),
                },
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.downcast_ref(), Some(&expected_error));
        let session = backend.session().await.unwrap();
        assert!(!session
            .list_label_keys()
            .await
            .unwrap()
            .iter()
            .any(|label_key| label_key.key == "new-key"));
        for id in ["valid", "sibling"] {
            assert!(!session
                .labels_for_note(id)
                .await
                .unwrap()
                .iter()
                .any(|label| label.key == "new-key"));
        }
        assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
        drop(base_ctx);
    }
}

#[tokio::test]
async fn rejects_invalid_label_keys_before_starting_a_transaction() {
    let (_base_ctx, backend, dir) = test_context().await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend, events.clone(), &dir);

    let error = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("does-not-matter", NOTE_REVISION)],
            action: BatchLabelAction::Add {
                key: "bad=key".into(),
                value: "value".into(),
            },
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref(),
        Some(&LabelKeyValidationError::ReservedCharacter('='))
    );
    assert_eq!(*events.lock().unwrap(), Vec::<String>::new());
}
