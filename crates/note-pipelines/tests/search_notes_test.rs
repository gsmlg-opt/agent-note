mod support;

use note_pipelines::{
    drain_embedding_jobs, save_note, search_notes, search_notes_filtered, SaveNoteInput,
};
use support::test_context;

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
            title: "Rust ownership".into(),
            content: "Shared searchable content".into(),
            attachments: vec![],
            labels: vec![("topic".into(), "rust".into())],
        },
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Ops checklist".into(),
            content: "Shared searchable content".into(),
            attachments: vec![],
            labels: vec![("topic".into(), "ops".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = search_notes_filtered(
        &ctx,
        "Shared searchable content",
        10,
        Some("topic=rust".into()),
    )
    .await
    .unwrap();

    assert!(results.iter().any(|r| r.note.title == "Rust ownership"));
    assert!(!results.iter().any(|r| r.note.title == "Ops checklist"));
}
