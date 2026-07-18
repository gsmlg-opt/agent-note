mod support;

use note_pipelines::{
    drain_embedding_jobs, save_note, search_notes, search_notes_filtered, SaveNoteInput,
};
use std::sync::Arc;
use support::test_context;

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

    let results = search_notes_filtered(&ctx, "Shared", 10, Some("topic=rust".into()))
        .await
        .unwrap();

    assert!(results.iter().any(|r| r.note.title == "Shared Rust title"));
    assert!(!results.iter().any(|r| r.note.title == "Shared Ops title"));
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
