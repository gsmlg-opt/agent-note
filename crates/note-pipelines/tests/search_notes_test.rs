mod support;

use note_attachments::{AttachmentStore, AttachmentStoreInfo};
use note_core::{NoteAttachment, NoteListItem};
use note_embedding::StubEmbedder;
use note_pipelines::{
    drain_embedding_jobs, save_note, search_notes, search_notes_filtered, SaveNoteInput,
};
use note_storage::{NewNote, TransactionMode};
use std::sync::Arc;
use support::test_context;

struct PanicAttachmentStore;

#[async_trait::async_trait]
impl AttachmentStore for PanicAttachmentStore {
    async fn read_legacy(&self, _note_id: &str, _path: &str) -> anyhow::Result<Vec<u8>> {
        panic!("search must not read attachments")
    }

    fn info(&self) -> AttachmentStoreInfo {
        AttachmentStoreInfo {
            engine: "panic".into(),
            location: None,
        }
    }
}

struct PanicEmbedder;

#[async_trait::async_trait]
impl note_embedding::Embedder for PanicEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<note_embedding::DenseVector> {
        panic!("blank retrieval must not call the embedder")
    }
}

#[tokio::test]
async fn blank_query_returns_no_results_without_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn note_storage::StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
        note_attachments::FilesystemAttachmentStore::new(dir.path().join("attachments")),
    );
    let ctx = note_pipelines::Context::new(storage, Arc::new(PanicEmbedder), attachments);

    assert!(search_notes(&ctx, " \n\t", 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn malformed_filters_are_rejected_before_blank_or_zero_limit_shortcuts() {
    let (ctx, _backend, _dir) = test_context().await;
    for (query, limit) in [("", 10), ("anything", 0)] {
        let error = search_notes_filtered(&ctx, query, limit, Some("~==secret".into()))
            .await
            .unwrap_err();
        assert!(error
            .downcast_ref::<note_core::LabelSelectorParseError>()
            .is_some());
    }
}

#[tokio::test]
async fn empty_label_collection_returns_without_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn note_storage::StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
        note_attachments::FilesystemAttachmentStore::new(dir.path().join("attachments")),
    );
    let ctx = note_pipelines::Context::new(storage, Arc::new(PanicEmbedder), attachments);

    let results = search_notes_filtered(&ctx, "needle", 10, Some("missing=value".into()))
        .await
        .unwrap();

    assert!(results.is_empty());
}

#[tokio::test]
async fn search_returns_saved_notes_with_fused_scores() {
    let (ctx, _backend, _dir) = test_context().await;
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Rust ownership".into(),
            content: "Ownership and borrowing in Rust".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Grocery list".into(),
            content: "Milk eggs bread".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = search_notes(&ctx, "Ownership and borrowing in Rust", 10)
        .await
        .unwrap();

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.note.title == "Rust ownership"));
    for r in &results {
        assert!(r.score > 0.0);
    }
}

#[tokio::test]
async fn search_returns_note_summaries_without_attachment_io() {
    let (write_ctx, backend, _dir) = test_context().await;
    let saved = save_note(
        &write_ctx,
        SaveNoteInput {
            title: "Metadata search".into(),
            content: "Find the metadata-only result".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: "search proof".into(),
                content: b"payload".to_vec(),
                storage: None,
            }],
            labels: vec![("topic".into(), "metadata".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&write_ctx, 10).await.unwrap();
    let ctx = note_pipelines::Context::new(
        backend,
        Arc::new(StubEmbedder),
        Arc::new(PanicAttachmentStore),
    );

    let results = search_notes(&ctx, "Metadata search", 10).await.unwrap();
    let result = results
        .iter()
        .find(|result| result.note.id == saved.id)
        .unwrap();

    fn assert_summary_type(_: &NoteListItem) {}
    assert_summary_type(&result.note);
    assert_eq!(result.note.labels, saved.labels);
    assert_eq!(result.note.created_at, saved.created_at);
    assert_eq!(result.note.updated_at, saved.updated_at);
    assert_eq!(result.note.deleted_at, saved.deleted_at);
}

#[tokio::test]
async fn search_filters_results_by_label() {
    let (ctx, _backend, _dir) = test_context().await;
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Shared Rust title".into(),
            content: "Alpha body".into(),
            attachments: vec![],
            labels: vec![("topic".into(), "rust".into())],
        },
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Shared Ops title".into(),
            content: "Beta body".into(),
            attachments: vec![],
            labels: vec![("topic".into(), "ops".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = search_notes_filtered(&ctx, "Shared", 10, Some("topic~=^RU.T$".into()))
        .await
        .unwrap();

    assert!(results.iter().any(|r| r.note.title == "Shared Rust title"));
    assert!(!results.iter().any(|r| r.note.title == "Shared Ops title"));
}

#[tokio::test]
async fn filtered_search_cannot_be_crowded_out_before_title_limit() {
    let (ctx, backend, _dir) = test_context().await;
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_label_key("scope", "Search visibility")
        .await
        .unwrap();
    transaction
        .insert_note(NewNote {
            id: "allowed-note",
            title: "Needle",
            content: "Irrelevant body",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    transaction
        .attach_label("allowed-note", "scope", "allowed")
        .await
        .unwrap();
    for index in 0..129 {
        let id = format!("excluded-{index:03}");
        transaction
            .insert_note(NewNote {
                id: &id,
                title: "Needle Needle Needle",
                content: "Irrelevant body",
                attachments: &[],
                created_at: index + 2,
                updated_at: index + 2,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        transaction
            .attach_label(&id, "scope", "excluded")
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let results = search_notes_filtered(&ctx, "Needle", 1, Some("scope=allowed".into()))
        .await
        .unwrap();

    assert_eq!(
        results
            .iter()
            .map(|result| result.note.id.as_str())
            .collect::<Vec<_>>(),
        vec!["allowed-note"]
    );
}

#[tokio::test]
async fn title_only_match_is_retrieved_without_body_embeddings() {
    let (ctx, _backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Quasar handbook".into(),
            content: "Unrelated body text".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let results = search_notes(&ctx, "Quasar", 10).await.unwrap();

    assert_eq!(results[0].note.id, note.id);
}

#[tokio::test]
async fn search_applies_the_saved_minimum_score_inclusively() {
    let (ctx, backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Quasar threshold handbook".into(),
            content: "Unrelated body text".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    {
        let session = backend.session().await.unwrap();
        let mut config = session.get_system_config().await.unwrap();
        config.search.minimum_score = 0.0;
        session.set_system_config(&config).await.unwrap();
    }

    let baseline = search_notes(&ctx, "Quasar threshold", 10).await.unwrap();
    let score = baseline
        .iter()
        .find(|result| result.note.id == note.id)
        .unwrap()
        .score;
    {
        let session = backend.session().await.unwrap();
        let mut config = session.get_system_config().await.unwrap();
        config.search.minimum_score = score;
        session.set_system_config(&config).await.unwrap();
    }

    let inclusive = search_notes(&ctx, "Quasar threshold", 10).await.unwrap();
    assert_eq!(inclusive[0].note.id, note.id);
    {
        let session = backend.session().await.unwrap();
        let mut config = session.get_system_config().await.unwrap();
        config.search.minimum_score = f32::from_bits(score.to_bits() + 1);
        session.set_system_config(&config).await.unwrap();
    }

    let excluded = search_notes(&ctx, "Quasar threshold", 10).await.unwrap();
    assert!(excluded.is_empty());
}

#[tokio::test]
async fn rank_one_title_match_outranks_rank_one_content_match() {
    let (ctx, _backend, _dir) = test_context().await;
    let title_match = save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle handbook".into(),
            content: "Alpha unrelated body".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Other handbook".into(),
            content: "Needle".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = search_notes(&ctx, "Needle", 1).await.unwrap();

    assert_eq!(results[0].note.id, title_match.id);
}

#[tokio::test]
async fn fusion_overfetches_candidates_before_applying_requested_limit() {
    let (ctx, _backend, _dir) = test_context().await;
    let both_channels = save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle handbook".into(),
            content: "Needle".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle Needle Needle handbook".into(),
            content: "Alpha unrelated body".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let results = search_notes(&ctx, "Needle", 1).await.unwrap();

    assert_eq!(results[0].note.id, both_channels.id);
}

#[tokio::test]
async fn filtered_fusion_overfetches_within_allowed_collection() {
    let (ctx, _backend, _dir) = test_context().await;
    let both_channels = save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle handbook".into(),
            content: "Needle".into(),
            attachments: vec![],
            labels: vec![("scope".into(), "allowed".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle Needle Needle handbook".into(),
            content: "Alpha unrelated body".into(),
            attachments: vec![],
            labels: vec![("scope".into(), "allowed".into())],
        },
    )
    .await
    .unwrap();

    let results = search_notes_filtered(&ctx, "Needle", 1, Some("scope=allowed".into()))
        .await
        .unwrap();

    assert_eq!(results[0].note.id, both_channels.id);
}

#[tokio::test]
async fn huge_limit_is_bounded_before_storage_retrieval() {
    let (ctx, _backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Quasar handbook".into(),
            content: "Unrelated body text".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let results = search_notes(&ctx, "Quasar", usize::MAX).await.unwrap();

    assert_eq!(results[0].note.id, note.id);
}
