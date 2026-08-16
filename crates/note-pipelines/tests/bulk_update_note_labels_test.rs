mod support;

use note_attachments::FilesystemAttachmentStore;
use note_core::{LabelKeyValidationError, LabelValueType, NoteAttachment, ValidationError};
use note_embedding::{EmbeddingBackendInfo, StubEmbedder};
use note_pipelines::{
    bulk_update_note_labels, get_note, BulkUpdateNoteLabelsInput, BulkUpdateNoteLabelsResult,
    BulkUpdateNoteLabelsValidationError, Context,
};
use note_storage::{NewNote, StorageBackend, TransactionMode};
use std::sync::Arc;
use support::{event_log, test_context, EventLog, EventNotifier, EventStorageBackend};

const CREATED_AT: i64 = 10;
const UPDATED_AT: i64 = 20;
const NOTE_REVISION: i64 = 7;

async fn seed_note(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    id: &str,
    project: Option<&str>,
    deleted: bool,
) {
    let attachments = vec![NoteAttachment {
        id: "source".into(),
        path: "source.txt".into(),
        mime: "text/plain".into(),
        description: "original payload".into(),
        content: format!("payload for {id}").into_bytes(),
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
            title: &format!("Title for {id}"),
            content: &format!("Content for {id}"),
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
    if deleted {
        transaction.soft_delete_note(id, 30).await.unwrap();
    }
    transaction.commit().await.unwrap();
    prepared.publish().await.unwrap();
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

    let add_before = get_note(&base_ctx, "add").await.unwrap().unwrap();
    let noop_before = get_note(&base_ctx, "noop").await.unwrap().unwrap();
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

    let add_after = get_note(&ctx, "add").await.unwrap().unwrap();
    assert_eq!(add_after.title, add_before.title);
    assert_eq!(add_after.content, add_before.content);
    assert_eq!(add_after.attachments, add_before.attachments);
    assert_eq!(add_after.created_at, add_before.created_at);
    assert!(add_after.updated_at > add_before.updated_at);
    assert_eq!(
        session.get_note_revision("add").await.unwrap(),
        Some(NOTE_REVISION)
    );

    let noop_after = get_note(&ctx, "noop").await.unwrap().unwrap();
    assert_eq!(noop_after.updated_at, noop_before.updated_at);
    assert_eq!(noop_after.attachments, noop_before.attachments);
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
    traced.override_matching_note_ids_for_update(vec!["scale".into(); 1_001]);

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
