mod support;

use note_attachments::FilesystemAttachmentStore;
use note_core::{Label, LabelKeyValidationError, LabelValueType, NoteAttachment, ValidationError};
use note_embedding::{EmbeddingBackendInfo, StubEmbedder};
use note_pipelines::{
    bulk_update_note_labels, chunk_hash, get_note, BulkUpdateNoteLabelsInput,
    BulkUpdateNoteLabelsResult, BulkUpdateNoteLabelsValidationError, Context, NoteMutationNotifier,
};
use note_storage::{
    EmbeddingJob, NewNote, NoteChunk, StorageBackend, TransactionMode, UpsertNoteChunk,
};
use std::sync::Arc;
use support::{event_log, test_context, EventLog, EventNotifier, EventStorageBackend};

const CREATED_AT: i64 = 10;
const UPDATED_AT: i64 = 20;
const NOTE_REVISION: i64 = 7;

struct MutationEventNotifier {
    events: EventLog,
}

impl NoteMutationNotifier for MutationEventNotifier {
    fn note_mutated(&self) {
        self.events.lock().unwrap().push("note_mutated".to_string());
    }
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
    non_project_labels: Vec<Label>,
    chunks: Vec<NoteChunk>,
    dense_search: Vec<String>,
    title_search: Vec<String>,
    pending_jobs: Vec<EmbeddingJob>,
}

async fn seed_note(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    id: &str,
    project: Option<&str>,
    deleted: bool,
) {
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
    let prepared = ctx.attachments().prepare(id, &attachments).await.unwrap();
    let metadata = prepared.metadata().to_vec();
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    for key in ["type", "project", "owner"] {
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
            created_at: CREATED_AT,
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
    if let Some(project) = project {
        transaction
            .attach_label(id, "project", project)
            .await
            .unwrap();
    }
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
    if deleted {
        transaction
            .soft_delete_note(id, NOTE_REVISION, 30)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
    prepared.publish().await.unwrap();
}

async fn preservation_snapshot(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    id: &str,
) -> NotePreservationSnapshot {
    let note = get_note(ctx, id).await.unwrap().unwrap();
    let non_project_labels = note
        .labels
        .iter()
        .filter(|label| label.key != "project")
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
        non_project_labels,
        chunks,
        dense_search,
        title_search,
        pending_jobs,
    }
}

fn assert_preserved_note_state(
    id: &str,
    before: &NotePreservationSnapshot,
    after: &NotePreservationSnapshot,
    timestamp_changed: bool,
) {
    assert_eq!(after.title, before.title, "{id} title");
    assert_eq!(after.content, before.content, "{id} content");
    assert_eq!(after.attachments, before.attachments, "{id} attachments");
    assert_eq!(after.created_at, before.created_at, "{id} created_at");
    assert_eq!(
        after.note_revision, before.note_revision,
        "{id} note_revision"
    );
    assert_eq!(
        after.non_project_labels, before.non_project_labels,
        "{id} unrelated labels"
    );
    assert_eq!(after.chunks, before.chunks, "{id} chunks");
    assert_eq!(after.dense_search, before.dense_search, "{id} dense search");
    assert_eq!(after.title_search, before.title_search, "{id} title search");
    assert_eq!(after.pending_jobs, before.pending_jobs, "{id} pending jobs");
    if timestamp_changed {
        assert!(after.updated_at > before.updated_at, "{id} updated_at");
    } else {
        assert_eq!(after.updated_at, before.updated_at, "{id} updated_at");
    }
}

fn traced_context(
    backend: Arc<dyn StorageBackend>,
    events: EventLog,
    dir: &tempfile::TempDir,
) -> (Context, Arc<EventStorageBackend>) {
    let traced = Arc::new(EventStorageBackend::new(backend, events.clone()));
    let ctx = Context::with_embedding_job_notifier(
        traced.clone(),
        Arc::new(StubEmbedder),
        EmbeddingBackendInfo::local_bge_m3(),
        Arc::new(EventNotifier::new(events)),
        Arc::new(FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        )),
    );
    (ctx, traced)
}

#[tokio::test]
async fn updates_matching_active_notes_with_add_replace_and_exact_noop_semantics() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&base_ctx, &backend, "add", None, false).await;
    seed_note(&base_ctx, &backend, "replace", Some("old"), false).await;
    seed_note(&base_ctx, &backend, "noop", Some("new"), false).await;
    seed_note(&base_ctx, &backend, "deleted", Some("old"), true).await;

    let mut active_before = Vec::new();
    for id in ["add", "replace", "noop"] {
        active_before.push((id, preservation_snapshot(&base_ctx, &backend, id).await));
    }
    let deleted_labels_before = backend
        .session()
        .await
        .unwrap()
        .labels_for_note("deleted")
        .await
        .unwrap();
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);

    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("project".into(), "new".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        result,
        BulkUpdateNoteLabelsResult {
            matched: 3,
            updated: 2,
            unchanged: 1,
        }
    );

    let session = backend.session().await.unwrap();
    for id in ["add", "replace", "noop"] {
        let labels = session.labels_for_note(id).await.unwrap();
        assert!(labels
            .iter()
            .any(|label| label.key == "project" && label.value == "new"));
        assert!(labels
            .iter()
            .any(|label| label.key == "owner" && label.value == "protocols"));
    }
    assert_eq!(
        session.labels_for_note("deleted").await.unwrap(),
        deleted_labels_before
    );

    drop(session);
    for (id, before) in active_before {
        let after = preservation_snapshot(&ctx, &backend, id).await;
        assert_preserved_note_state(id, &before, &after, id != "noop");
    }
    assert_eq!(
        traced.repository_call_count("matching_note_ids_for_update"),
        1
    );
    for forbidden in [
        "update_note",
        "update_note_fields",
        "update_note_attachments",
        "clear_note_search_data",
        "clear_note_chunk_derived",
        "clear_note_chunks_from_derived",
        "upsert_note_chunk",
        "mark_note_chunk_status",
        "delete_note_chunks_from",
        "enqueue_embedding_job",
        "delete_stale_embedding_jobs_for_chunk",
        "delete_embedding_jobs_from_chunk",
        "insert_chunk_embedding",
    ] {
        assert_eq!(traced.repository_call_count(forbidden), 0, "{forbidden}");
    }
    assert_eq!(*events.lock().unwrap(), vec!["begin", "commit"]);
}

#[tokio::test]
async fn rejects_invalid_request_shapes_before_beginning_a_transaction() {
    let (_base_ctx, backend, dir) = test_context().await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend, events.clone(), &dir);
    let cases = [
        (
            "   ",
            vec![("project".into(), "new".into())],
            BulkUpdateNoteLabelsValidationError::EmptySelector,
            "selector must not be empty",
        ),
        (
            "type=x&&status=y",
            vec![("project".into(), "new".into())],
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
            "label selector is malformed",
        ),
        (
            "&type=x",
            vec![("project".into(), "new".into())],
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
            "label selector is malformed",
        ),
        (
            "type=x&",
            vec![("project".into(), "new".into())],
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
            "label selector is malformed",
        ),
        (
            "&",
            vec![("project".into(), "new".into())],
            BulkUpdateNoteLabelsValidationError::MalformedSelector,
            "label selector is malformed",
        ),
        (
            "type=x",
            vec![],
            BulkUpdateNoteLabelsValidationError::EmptySet,
            "at least one label assignment is required",
        ),
        (
            "type=x",
            vec![
                ("project".into(), "one".into()),
                ("project".into(), "two".into()),
            ],
            BulkUpdateNoteLabelsValidationError::DuplicateKey("project".into()),
            "duplicate label assignment key: project",
        ),
    ];

    for (selector, set, expected, message) in cases {
        let error = bulk_update_note_labels(
            &ctx,
            BulkUpdateNoteLabelsInput {
                selector: selector.into(),
                set,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), message);
        assert_eq!(
            error
                .downcast_ref::<BulkUpdateNoteLabelsValidationError>()
                .unwrap(),
            &expected
        );
        assert!(events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn rejects_invalid_selector_and_target_keys_before_beginning_a_transaction() {
    let (_base_ctx, backend, dir) = test_context().await;
    let events = event_log();
    let (ctx, _traced) = traced_context(backend, events.clone(), &dir);

    for input in [
        BulkUpdateNoteLabelsInput {
            selector: "project$name=old".into(),
            set: vec![("project".into(), "new".into())],
        },
        BulkUpdateNoteLabelsInput {
            selector: "project=old".into(),
            set: vec![("target$key".into(), "new".into())],
        },
    ] {
        let error = bulk_update_note_labels(&ctx, input).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<LabelKeyValidationError>(),
            Some(&LabelKeyValidationError::ReservedCharacter('$'))
        );
        assert!(events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn zero_matches_roll_back_without_creating_target_catalog_keys() {
    for selector in ["type~=[", "missing=value"] {
        let (base_ctx, backend, dir) = test_context().await;
        seed_note(&base_ctx, &backend, "candidate", None, false).await;
        let events = event_log();
        let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);

        let result = bulk_update_note_labels(
            &ctx,
            BulkUpdateNoteLabelsInput {
                selector: selector.into(),
                set: vec![("new-target".into(), "value".into())],
            },
        )
        .await
        .unwrap();

        assert_eq!(
            result,
            BulkUpdateNoteLabelsResult {
                matched: 0,
                updated: 0,
                unchanged: 0,
            }
        );
        assert_eq!(
            traced.repository_call_count("matching_note_ids_for_update"),
            1
        );
        assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
        assert!(!backend
            .session()
            .await
            .unwrap()
            .list_label_keys()
            .await
            .unwrap()
            .iter()
            .any(|label_key| label_key.key == "new-target"));
    }
}

#[tokio::test]
async fn creates_missing_target_catalog_keys_as_text_when_notes_match() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, "matched", None, false).await;

    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("new-key".into(), "value".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(result.matched, 1);
    assert_eq!(result.updated, 1);
    let session = backend.session().await.unwrap();
    let key = session
        .list_label_keys()
        .await
        .unwrap()
        .into_iter()
        .find(|key| key.key == "new-key")
        .unwrap();
    assert_eq!(key.description, "");
    assert_eq!(key.value_type, LabelValueType::Text);
    assert!(session
        .labels_for_note("matched")
        .await
        .unwrap()
        .iter()
        .any(|label| label.key == "new-key" && label.value == "value"));
}

#[tokio::test]
async fn rejects_values_invalid_for_the_final_catalog_types() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, "typed", None, false).await;
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    for (key, value_type) in [
        ("number", LabelValueType::Number),
        ("version", LabelValueType::Version),
        ("date", LabelValueType::Date),
        ("datetime", LabelValueType::DateTime),
        ("time", LabelValueType::Time),
    ] {
        transaction
            .insert_label_key_with_type(key, "", value_type)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    for (key, value, value_type) in [
        ("number", "not-a-number", LabelValueType::Number),
        ("version", "1.2.beta", LabelValueType::Version),
        ("date", "07/09/2026", LabelValueType::Date),
        ("datetime", "not-a-datetime", LabelValueType::DateTime),
        ("time", "25:00", LabelValueType::Time),
    ] {
        let error = bulk_update_note_labels(
            &ctx,
            BulkUpdateNoteLabelsInput {
                selector: "type=ietf-rfc".into(),
                set: vec![(key.into(), value.into())],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ValidationError>(),
            Some(&ValidationError::InvalidLabelValue {
                key: key.into(),
                value: value.into(),
                value_type,
            })
        );
    }

    let labels = backend
        .session()
        .await
        .unwrap()
        .labels_for_note("typed")
        .await
        .unwrap();
    assert!(!labels.iter().any(|label| {
        ["number", "version", "date", "datetime", "time"].contains(&label.key.as_str())
    }));
}

#[tokio::test]
async fn updates_all_1001_matches_without_a_page_cap() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&base_ctx, &backend, "scale", Some("done"), false).await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend, events, &dir);
    let expected_ids: Vec<String> = (0..1_001)
        .map(|index| format!("synthetic-{index:04}"))
        .collect();
    traced.override_bulk_label_noops(expected_ids.clone());

    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "project=done".into(),
            set: vec![("project".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(result.matched, 1_001);
    assert_eq!(result.updated, 0);
    assert_eq!(result.unchanged, 1_001);
    assert_eq!(traced.repository_call_count("set_note_label"), 1_001);
    assert_eq!(traced.set_note_label_note_ids(), expected_ids);
}

#[tokio::test]
async fn retains_the_fixed_target_ids_when_an_assignment_changes_the_selector() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, "fixed-a", Some("old"), false).await;
    seed_note(&ctx, &backend, "fixed-b", Some("old"), false).await;

    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "project=old".into(),
            set: vec![("project".into(), "new".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(result.matched, 2);
    assert_eq!(result.updated, 2);
    for id in ["fixed-a", "fixed-b"] {
        assert!(backend
            .session()
            .await
            .unwrap()
            .labels_for_note(id)
            .await
            .unwrap()
            .iter()
            .any(|label| label.key == "project" && label.value == "new"));
    }
}

#[tokio::test]
async fn multiple_assignments_count_once_and_exact_noops_preserve_the_timestamp() {
    let (ctx, backend, _dir) = test_context().await;
    seed_note(&ctx, &backend, "multiple", Some("old"), false).await;

    let first = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![
                ("project".into(), "new".into()),
                ("status".into(), "ready".into()),
            ],
        },
    )
    .await
    .unwrap();
    assert_eq!(first.updated, 1);
    let changed_timestamp = backend
        .session()
        .await
        .unwrap()
        .get_note("multiple")
        .await
        .unwrap()
        .unwrap()
        .updated_at;
    assert!(changed_timestamp > UPDATED_AT);

    let second = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![
                ("project".into(), "new".into()),
                ("status".into(), "ready".into()),
            ],
        },
    )
    .await
    .unwrap();
    assert_eq!(second.updated, 0);
    assert_eq!(second.unchanged, 1);
    assert_eq!(
        backend
            .session()
            .await
            .unwrap()
            .get_note("multiple")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        changed_timestamp
    );
}

#[tokio::test]
async fn repository_failure_after_label_writes_rolls_back_labels_catalog_and_timestamp() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&base_ctx, &backend, "rollback", Some("old"), false).await;
    let before = backend
        .session()
        .await
        .unwrap()
        .get_note("rollback")
        .await
        .unwrap()
        .unwrap();
    let labels_before = backend
        .session()
        .await
        .unwrap()
        .labels_for_note("rollback")
        .await
        .unwrap();
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    traced.fail_next_repository_call("advance_note_updated_at");

    let error = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![
                ("project".into(), "new".into()),
                ("created-in-failed-transaction".into(), "value".into()),
            ],
        },
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("controlled repository failure at advance_note_updated_at"));
    assert_eq!(*events.lock().unwrap(), vec!["begin", "rollback"]);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session.labels_for_note("rollback").await.unwrap(),
        labels_before
    );
    assert_eq!(
        session
            .get_note("rollback")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        before.updated_at
    );
    assert!(!session
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "created-in-failed-transaction"));
}

#[tokio::test]
async fn commit_failure_returns_no_result_and_leaves_no_partial_state() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&base_ctx, &backend, "commit-failure", Some("old"), false).await;
    let before = backend
        .session()
        .await
        .unwrap()
        .get_note("commit-failure")
        .await
        .unwrap()
        .unwrap();
    let labels_before = backend
        .session()
        .await
        .unwrap()
        .labels_for_note("commit-failure")
        .await
        .unwrap();
    let events = event_log();
    let (ctx, traced) = traced_context(backend.clone(), events.clone(), &dir);
    traced.fail_next_commit();

    let error = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![
                ("project".into(), "new".into()),
                ("commit-created-key".into(), "value".into()),
            ],
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("controlled commit failure"));
    assert_eq!(*events.lock().unwrap(), vec!["begin", "commit_failed"]);
    let session = backend.session().await.unwrap();
    assert_eq!(
        session.labels_for_note("commit-failure").await.unwrap(),
        labels_before
    );
    assert_eq!(
        session
            .get_note("commit-failure")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        before.updated_at
    );
    assert!(!session
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "commit-created-key"));
}

#[tokio::test]
async fn mutation_notifier_runs_once_after_changed_commit_and_not_for_other_outcomes() {
    let (base_ctx, backend, dir) = test_context().await;
    seed_note(&base_ctx, &backend, "notifier", Some("old"), false).await;
    let events = event_log();
    let (ctx, traced) = traced_context(backend, events.clone(), &dir);
    let ctx = ctx.with_note_mutation_notifier(Arc::new(MutationEventNotifier {
        events: events.clone(),
    }));

    let changed = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("project".into(), "new".into())],
        },
    )
    .await
    .unwrap();
    assert_eq!(changed.updated, 1);
    assert_eq!(
        *events.lock().unwrap(),
        vec!["begin", "commit", "note_mutated"]
    );

    let noop = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("project".into(), "new".into())],
        },
    )
    .await
    .unwrap();
    assert_eq!(noop.updated, 0);

    let zero = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=missing".into(),
            set: vec![("project".into(), "new".into())],
        },
    )
    .await
    .unwrap();
    assert_eq!(zero.matched, 0);

    traced.fail_next_commit();
    let error = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: "type=ietf-rfc".into(),
            set: vec![("project".into(), "commit-failed".into())],
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("controlled commit failure"));
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "begin",
            "commit",
            "note_mutated",
            "begin",
            "commit",
            "begin",
            "rollback",
            "begin",
            "commit_failed",
        ]
    );
}
