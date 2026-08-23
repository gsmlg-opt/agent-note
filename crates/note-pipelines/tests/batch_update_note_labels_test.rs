mod support;

use note_attachments::FilesystemAttachmentStore;
use note_core::{Label, LabelKeyValidationError, LabelValueType, NoteAttachment};
use note_embedding::StubEmbedder;
use note_pipelines::{
    batch_update_note_labels, chunk_hash, get_note, BatchLabelAction, BatchNoteTarget,
    BatchNoteTargetsValidationError, BatchUpdateNoteLabelsInput, BatchUpdateNoteLabelsResult,
    Context, NoteMutationError, NoteMutationNotifier,
};
use note_storage::{
    EmbeddingJob, NewNote, NoteChunk, StorageBackend, TransactionMode, UpsertNoteChunk,
};
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

async fn attach_label(backend: &Arc<dyn StorageBackend>, id: &str, key: &str, value: &str) {
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_label_key_if_missing(key, "")
        .await
        .unwrap();
    transaction.attach_label(id, key, value).await.unwrap();
    transaction.commit().await.unwrap();
}

async fn note_updated_at(backend: &Arc<dyn StorageBackend>, id: &str) -> i64 {
    backend
        .session()
        .await
        .unwrap()
        .get_note(id)
        .await
        .unwrap()
        .unwrap()
        .updated_at
}

async fn labels_for(backend: &Arc<dyn StorageBackend>, id: &str) -> Vec<note_core::Label> {
    backend
        .session()
        .await
        .unwrap()
        .labels_for_note(id)
        .await
        .unwrap()
}

async fn catalog_contains(backend: &Arc<dyn StorageBackend>, key: &str) -> bool {
    backend
        .session()
        .await
        .unwrap()
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|label_key| label_key.key == key)
}

async fn catalog_types(backend: &Arc<dyn StorageBackend>) -> Vec<(String, LabelValueType)> {
    let mut catalog = backend
        .session()
        .await
        .unwrap()
        .list_label_keys()
        .await
        .unwrap()
        .into_iter()
        .map(|label_key| (label_key.key, label_key.value_type))
        .collect::<Vec<_>>();
    catalog.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    catalog
}

fn test_embedding() -> Vec<f32> {
    let mut embedding = vec![0.0; 1_024];
    embedding[0] = 1.0;
    embedding
}

#[derive(Debug)]
struct NotePreservationSnapshot {
    title: String,
    content: String,
    attachments: Vec<NoteAttachment>,
    created_at: i64,
    updated_at: i64,
    note_revision: Option<i64>,
    unrelated_labels: Vec<Label>,
    chunks: Vec<NoteChunk>,
    dense_search: Vec<String>,
    title_search: Vec<String>,
    pending_jobs: Vec<EmbeddingJob>,
}

async fn seed_rich_note(ctx: &Context, backend: &Arc<dyn StorageBackend>, id: &str, source: &str) {
    let title = format!("Title for {id}");
    let content = format!("Content for {id}");
    let attachments = vec![NoteAttachment {
        id: "source".into(),
        path: "source.txt".into(),
        mime: "text/plain".into(),
        description: "original payload".into(),
        content: format!("payload for {id}").into_bytes(),
        storage: None,
    }];
    let path = std::path::PathBuf::from(ctx.attachments().info().location.unwrap())
        .join(id)
        .join("source.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &attachments[0].content).unwrap();
    let metadata = attachments
        .iter()
        .cloned()
        .map(|mut attachment| {
            attachment.content.clear();
            attachment
        })
        .collect::<Vec<_>>();
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    for key in ["type", "owner", "source"] {
        transaction
            .insert_label_key_if_missing(key, "")
            .await
            .unwrap();
    }
    transaction
        .insert_note(NewNote {
            id,
            title: &title,
            content: &content,
            attachments: &metadata,
            created_at: 10,
            updated_at: UPDATED_AT,
            note_revision: NOTE_REVISION,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction
        .attach_label(id, "type", "ietf-rfc")
        .await
        .unwrap();
    transaction
        .attach_label(id, "owner", "protocols")
        .await
        .unwrap();
    transaction
        .attach_label(id, "source", source)
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
            updated_at: UPDATED_AT,
        })
        .await
        .unwrap();
    transaction
        .insert_chunk_embedding(id, 0, &test_embedding())
        .await
        .unwrap();
    transaction
        .enqueue_embedding_job(id, 0, &content_hash, &content, NOTE_REVISION, UPDATED_AT)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

async fn preservation_snapshot(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    id: &str,
    excluded_label_keys: &[&str],
) -> NotePreservationSnapshot {
    let note = get_note(ctx, id).await.unwrap().unwrap();
    let unrelated_labels = note
        .labels
        .iter()
        .filter(|label| !excluded_label_keys.contains(&label.key.as_str()))
        .cloned()
        .collect();
    let session = backend.session().await.unwrap();
    let allowed_ids = [id.to_string()];
    let note_revision = session.get_note_revision(id).await.unwrap();
    let chunks = session.list_note_chunks(id).await.unwrap();
    let dense_search = session
        .dense_search(&test_embedding(), 10, Some(&allowed_ids))
        .await
        .unwrap();
    let title_search = session
        .title_search("Title", 10, Some(&allowed_ids))
        .await
        .unwrap();
    drop(session);
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    let pending_jobs = transaction
        .claim_pending_embedding_jobs(100, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.note_id == id)
        .collect();
    transaction.rollback().await.unwrap();

    NotePreservationSnapshot {
        title: note.title,
        content: note.content,
        attachments: note.attachments,
        created_at: note.created_at,
        updated_at: note.updated_at,
        note_revision,
        unrelated_labels,
        chunks,
        dense_search,
        title_search,
        pending_jobs,
    }
}

fn assert_preserved_note_state(
    before: &NotePreservationSnapshot,
    after: &NotePreservationSnapshot,
) {
    assert_eq!(after.title, before.title);
    assert_eq!(after.content, before.content);
    assert_eq!(after.attachments, before.attachments);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.note_revision, before.note_revision);
    assert_eq!(after.unrelated_labels, before.unrelated_labels);
    assert_eq!(after.chunks, before.chunks);
    assert_eq!(after.dense_search, before.dense_search);
    assert_eq!(after.title_search, before.title_search);
    assert_eq!(after.pending_jobs, before.pending_jobs);
    assert!(after.updated_at > before.updated_at);
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

    for action in [
        BatchLabelAction::Add {
            key: "bad=key".into(),
            value: "value".into(),
        },
        BatchLabelAction::Update {
            from_key: "bad=key".into(),
            key: "destination".into(),
            value: "value".into(),
        },
        BatchLabelAction::Update {
            from_key: "source".into(),
            key: "bad=key".into(),
            value: "value".into(),
        },
        BatchLabelAction::Remove {
            key: "bad=key".into(),
        },
    ] {
        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes: vec![target("does-not-matter", NOTE_REVISION)],
                action,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.downcast_ref(),
            Some(&LabelKeyValidationError::ReservedCharacter('='))
        );
    }
    assert_eq!(*events.lock().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn same_key_update_replaces_value_and_replay_is_a_timestamp_noop() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "same-key", Some("old")).await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));
    let input = BatchUpdateNoteLabelsInput {
        notes: vec![target("same-key", NOTE_REVISION)],
        action: BatchLabelAction::Update {
            from_key: "project".into(),
            key: "project".into(),
            value: "new".into(),
        },
    };

    let changed = batch_update_note_labels(&ctx, input.clone()).await.unwrap();
    assert_eq!(
        changed,
        BatchUpdateNoteLabelsResult {
            requested: 1,
            updated: 1,
            unchanged: 0,
        }
    );
    let changed_at = backend
        .session()
        .await
        .unwrap()
        .get_note("same-key")
        .await
        .unwrap()
        .unwrap()
        .updated_at;
    assert!(backend
        .session()
        .await
        .unwrap()
        .labels_for_note("same-key")
        .await
        .unwrap()
        .iter()
        .any(|label| label.key == "project" && label.value == "new"));

    let replay = batch_update_note_labels(&ctx, input).await.unwrap();
    assert_eq!(
        replay,
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
            .get_note("same-key")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        changed_at
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", "note_mutated", "begin", "rollback"]
    );
    drop(base_ctx);
}

#[tokio::test]
async fn rename_updates_only_targets_with_the_source_and_replaces_destination_collisions() {
    let (base_ctx, backend, dir) = test_context().await;
    for id in ["source-only", "collision", "missing"] {
        seed_note(&backend, id, None).await;
    }
    attach_label(&backend, "source-only", "source", "one").await;
    attach_label(&backend, "collision", "source", "two").await;
    attach_label(&backend, "collision", "destination", "old").await;
    let (ctx, _traced) = traced_context(backend.clone(), event_log(), &dir);

    let result = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![
                target("source-only", NOTE_REVISION),
                target("collision", NOTE_REVISION),
                target("missing", NOTE_REVISION),
            ],
            action: BatchLabelAction::Update {
                from_key: "source".into(),
                key: "destination".into(),
                value: "requested".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        BatchUpdateNoteLabelsResult {
            requested: 3,
            updated: 2,
            unchanged: 1,
        }
    );
    for id in ["source-only", "collision"] {
        let labels = labels_for(&backend, id).await;
        assert!(!labels.iter().any(|label| label.key == "source"));
        assert!(labels
            .iter()
            .any(|label| label.key == "destination" && label.value == "requested"));
    }
    assert!(!labels_for(&backend, "missing")
        .await
        .iter()
        .any(|label| label.key == "destination"));
    drop(base_ctx);
}

#[tokio::test]
async fn update_with_every_source_missing_is_a_catalog_and_notification_noop() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "missing-source", None).await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));

    let result = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("missing-source", NOTE_REVISION)],
            action: BatchLabelAction::Update {
                from_key: "source".into(),
                key: "unknown-destination".into(),
                value: "value".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(result.updated, 0);
    assert_eq!(result.unchanged, 1);
    assert!(!catalog_contains(&backend, "unknown-destination").await);
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
    drop(base_ctx);
}

#[tokio::test]
async fn remove_changes_only_present_associations_and_replay_is_timestamp_stable() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "present", None).await;
    seed_note(&backend, "absent", None).await;
    attach_label(&backend, "present", "remove-me", "value").await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));
    let input = BatchUpdateNoteLabelsInput {
        notes: vec![
            target("present", NOTE_REVISION),
            target("absent", NOTE_REVISION),
        ],
        action: BatchLabelAction::Remove {
            key: "remove-me".into(),
        },
    };

    let changed = batch_update_note_labels(&ctx, input.clone()).await.unwrap();
    assert_eq!(changed.updated, 1);
    assert_eq!(changed.unchanged, 1);
    assert!(!labels_for(&backend, "present")
        .await
        .iter()
        .any(|label| label.key == "remove-me"));
    assert!(catalog_contains(&backend, "remove-me").await);
    let changed_at = note_updated_at(&backend, "present").await;

    let replay = batch_update_note_labels(&ctx, input).await.unwrap();
    assert_eq!(replay.updated, 0);
    assert_eq!(replay.unchanged, 2);
    assert_eq!(note_updated_at(&backend, "present").await, changed_at);
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", "note_mutated", "begin", "rollback"]
    );
    drop(base_ctx);
}

#[tokio::test]
async fn eligible_invalid_typed_destination_rolls_back_every_target() {
    let (base_ctx, backend, dir) = test_context().await;
    for id in ["typed-a", "typed-b"] {
        seed_note(&backend, id, None).await;
        attach_label(&backend, id, "source", "value").await;
    }
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_label_key_with_type("number", "", LabelValueType::Number)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let labels_before = labels_for(&backend, "typed-a").await;
    let timestamp_before = note_updated_at(&backend, "typed-a").await;
    let (ctx, _traced) = traced_context(backend.clone(), event_log(), &dir);

    let error = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![
                target("typed-a", NOTE_REVISION),
                target("typed-b", NOTE_REVISION),
            ],
            action: BatchLabelAction::Update {
                from_key: "source".into(),
                key: "number".into(),
                value: "not-a-number".into(),
            },
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.downcast_ref::<note_core::ValidationError>(),
        Some(&note_core::ValidationError::InvalidLabelValue {
            key: "number".into(),
            value: "not-a-number".into(),
            value_type: LabelValueType::Number,
        })
    );
    assert_eq!(labels_for(&backend, "typed-a").await, labels_before);
    assert_eq!(note_updated_at(&backend, "typed-a").await, timestamp_before);
    assert!(labels_for(&backend, "typed-b")
        .await
        .iter()
        .any(|label| label.key == "source"));
    drop(base_ctx);
}

#[tokio::test]
async fn ineligible_invalid_typed_destinations_are_rejected_without_mutation() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "ineligible", None).await;
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_label_key_with_type("number", "", LabelValueType::Number)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    attach_label(&backend, "ineligible", "number", "1").await;

    for action in [
        BatchLabelAction::Update {
            from_key: "source".into(),
            key: "number".into(),
            value: "not-a-number".into(),
        },
        BatchLabelAction::Add {
            key: "number".into(),
            value: "not-a-number".into(),
        },
    ] {
        let labels_before = labels_for(&backend, "ineligible").await;
        let timestamp_before = note_updated_at(&backend, "ineligible").await;
        let catalog_before = catalog_types(&backend).await;
        let events = event_log();
        let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);

        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes: vec![target("ineligible", NOTE_REVISION)],
                action,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.downcast_ref::<note_core::ValidationError>(),
            Some(&note_core::ValidationError::InvalidLabelValue {
                key: "number".into(),
                value: "not-a-number".into(),
                value_type: LabelValueType::Number,
            })
        );
        assert_eq!(labels_for(&backend, "ineligible").await, labels_before);
        assert_eq!(
            note_updated_at(&backend, "ineligible").await,
            timestamp_before
        );
        assert_eq!(catalog_types(&backend).await, catalog_before);
        assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
    }
    drop(base_ctx);
}

#[tokio::test]
async fn update_preflight_rejects_missing_or_stale_targets_before_any_mutation() {
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
        attach_label(&backend, "sibling", "source", "value").await;
        let events = event_log();
        let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);

        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes: vec![bad_target, target("sibling", NOTE_REVISION)],
                action: BatchLabelAction::Update {
                    from_key: "source".into(),
                    key: "destination".into(),
                    value: "value".into(),
                },
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.downcast_ref(), Some(&expected_error));
        assert!(!catalog_contains(&backend, "destination").await);
        assert!(labels_for(&backend, "sibling")
            .await
            .iter()
            .any(|label| label.key == "source"));
        assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
        drop(base_ctx);
    }
}

#[tokio::test]
async fn remove_preflight_rejects_missing_or_stale_targets_before_any_mutation() {
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
        attach_label(&backend, "sibling", "remove-me", "value").await;
        let events = event_log();
        let (ctx, _traced) = traced_context(backend.clone(), events.clone(), &dir);

        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes: vec![bad_target, target("sibling", NOTE_REVISION)],
                action: BatchLabelAction::Remove {
                    key: "remove-me".into(),
                },
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.downcast_ref(), Some(&expected_error));
        assert!(labels_for(&backend, "sibling")
            .await
            .iter()
            .any(|label| label.key == "remove-me"));
        assert!(catalog_contains(&backend, "remove-me").await);
        assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
        drop(base_ctx);
    }
}

#[tokio::test]
async fn repository_failure_after_update_writes_rolls_back_labels_catalog_timestamp_and_notification(
) {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "rollback", None).await;
    attach_label(&backend, "rollback", "source", "old").await;
    let labels_before = labels_for(&backend, "rollback").await;
    let timestamp_before = note_updated_at(&backend, "rollback").await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));
    traced.fail_next_repository_call("advance_note_updated_at");

    let error = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("rollback", NOTE_REVISION)],
            action: BatchLabelAction::Update {
                from_key: "source".into(),
                key: "new-destination".into(),
                value: "new".into(),
            },
        },
    )
    .await
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("controlled repository failure at advance_note_updated_at"));
    assert_eq!(labels_for(&backend, "rollback").await, labels_before);
    assert_eq!(
        note_updated_at(&backend, "rollback").await,
        timestamp_before
    );
    assert!(!catalog_contains(&backend, "new-destination").await);
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
    drop(base_ctx);
}

#[tokio::test]
async fn commit_failure_after_remove_restores_association_timestamp_and_notification() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&backend, "commit-failure", None).await;
    attach_label(&backend, "commit-failure", "remove-me", "value").await;
    let labels_before = labels_for(&backend, "commit-failure").await;
    let timestamp_before = note_updated_at(&backend, "commit-failure").await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationNotifier(events.clone())));
    traced.fail_next_commit();

    let error = batch_update_note_labels(
        &ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("commit-failure", NOTE_REVISION)],
            action: BatchLabelAction::Remove {
                key: "remove-me".into(),
            },
        },
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("controlled commit failure"));
    assert_eq!(labels_for(&backend, "commit-failure").await, labels_before);
    assert_eq!(
        note_updated_at(&backend, "commit-failure").await,
        timestamp_before
    );
    assert_eq!(*events.lock().unwrap(), vec!["begin", "commit_failed"]);
    drop(base_ctx);
}

#[tokio::test]
async fn rename_and_remove_preserve_note_payload_and_derived_state() {
    let (base_ctx, backend, _dir) = test_context().await;
    seed_rich_note(&base_ctx, &backend, "rename", "old").await;
    seed_rich_note(&base_ctx, &backend, "remove", "old").await;
    let rename_before =
        preservation_snapshot(&base_ctx, &backend, "rename", &["source", "destination"]).await;
    let remove_before = preservation_snapshot(&base_ctx, &backend, "remove", &["source"]).await;

    let rename = batch_update_note_labels(
        &base_ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("rename", NOTE_REVISION)],
            action: BatchLabelAction::Update {
                from_key: "source".into(),
                key: "destination".into(),
                value: "new".into(),
            },
        },
    )
    .await
    .unwrap();
    let remove = batch_update_note_labels(
        &base_ctx,
        BatchUpdateNoteLabelsInput {
            notes: vec![target("remove", NOTE_REVISION)],
            action: BatchLabelAction::Remove {
                key: "source".into(),
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(rename.updated, 1);
    assert_eq!(remove.updated, 1);
    assert_preserved_note_state(
        &rename_before,
        &preservation_snapshot(&base_ctx, &backend, "rename", &["source", "destination"]).await,
    );
    assert_preserved_note_state(
        &remove_before,
        &preservation_snapshot(&base_ctx, &backend, "remove", &["source"]).await,
    );
    assert!(labels_for(&backend, "rename")
        .await
        .iter()
        .any(|label| label.key == "destination" && label.value == "new"));
    assert!(!labels_for(&backend, "remove")
        .await
        .iter()
        .any(|label| label.key == "source"));
}
