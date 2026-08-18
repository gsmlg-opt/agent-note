mod support;

use note_storage::{
    EmbeddingRepository, NotesRepository, RetrievalRepository, StorageErrorKind, UpsertNoteChunk,
};
use support::{fixture, insert_named_note, insert_test_note, unit};

#[tokio::test]
async fn title_search_matches_titles_but_not_body_text() {
    let fixture = fixture().await;
    insert_named_note(
        &fixture.session,
        "title-hit",
        "Rust ownership guide",
        "unrelated body",
    )
    .await;
    insert_named_note(
        &fixture.session,
        "body-only",
        "Other guide",
        "Rust ownership body",
    )
    .await;

    assert_eq!(
        fixture
            .session
            .title_search("Rust", 10, None)
            .await
            .unwrap(),
        vec!["title-hit"]
    );
    assert_eq!(
        fixture
            .session
            .title_search("Rust ownership", 10, None)
            .await
            .unwrap(),
        vec!["title-hit"]
    );
    assert!(fixture
        .session
        .title_search("unrelated", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .title_search("Rust", 0, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn title_search_tracks_updates_deletion_and_restoration() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "note", "Old heading", "body").await;
    fixture
        .session
        .update_note(note_storage::NoteUpdate {
            id: "note",
            title: "New heading",
            content: "body",
            attachments: &[],
            updated_at: 2,
            expected_revision: 1,
        })
        .await
        .unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert_eq!(
        observer.title_search("New", 10, None).await.unwrap(),
        vec!["note"]
    );
    assert!(observer
        .title_search("Old", 10, None)
        .await
        .unwrap()
        .is_empty());
    observer.soft_delete_note("note", 2, 3).await.unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert!(observer
        .title_search("New", 10, None)
        .await
        .unwrap()
        .is_empty());
    observer.restore_note("note", 3).await.unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert_eq!(
        observer.title_search("New", 10, None).await.unwrap(),
        vec!["note"]
    );
}

#[tokio::test]
async fn equal_title_scores_use_note_id_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tie-boundary.db");
    {
        let storage = note_storage_turso::TursoStorage::open(&path).await.unwrap();
        let session = storage.connect().await.unwrap();
        for id in ["b", "c", "a"] {
            insert_named_note(&session, id, "Shared heading", "body").await;
        }
    }
    let storage = note_storage_turso::TursoStorage::open(&path).await.unwrap();
    let observer = storage.connect().await.unwrap();

    assert_eq!(
        observer.title_search("Shared", 2, None).await.unwrap(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn title_search_treats_query_syntax_as_literal_term_separators() {
    let fixture = fixture().await;
    insert_named_note(
        &fixture.session,
        "unterminated",
        "Unterminated field guide",
        "body",
    )
    .await;
    insert_named_note(&fixture.session, "foo", "Foo reference", "body").await;
    insert_named_note(&fixture.session, "letters", "A B primer", "body").await;
    insert_named_note(
        &fixture.session,
        "operators",
        "AND OR NOT reference",
        "body",
    )
    .await;
    insert_named_note(&fixture.session, "unicode", "Café résumé", "body").await;

    for (query, expected_id) in [
        (r#""unterminated"#, "unterminated"),
        ("foo:", "foo"),
        ("a - b", "letters"),
        ("(foo", "foo"),
        ("AND OR NOT", "operators"),
        ("café / résumé", "unicode"),
    ] {
        let results = fixture.session.title_search(query, 10, None).await.unwrap();
        assert!(
            results.iter().any(|id| id == expected_id),
            "expected {query:?} to retrieve {expected_id:?}, got {results:?}"
        );
    }

    assert!(fixture
        .session
        .title_search(r#""():-"#, 10, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn title_search_caps_direct_callers_before_limit_conversion() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "rust", "Rust guide", "body").await;

    assert_eq!(
        fixture
            .session
            .title_search("Rust", usize::MAX, None)
            .await
            .unwrap(),
        vec!["rust"]
    );
}

#[tokio::test]
async fn allowed_ids_are_applied_before_title_and_dense_limits() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "allowed-single", "Needle", "allowed body").await;
    insert_named_note(
        &fixture.session,
        "allowed-repeated",
        "NEEDLE Needle",
        "allowed repeated body",
    )
    .await;
    insert_named_note(
        &fixture.session,
        "allowed-deleted",
        "Needle Needle Needle Needle",
        "deleted body",
    )
    .await;
    insert_named_note(
        &fixture.session,
        "excluded",
        "Needle Needle Needle",
        "excluded body",
    )
    .await;
    let mut farther = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    farther[0] = 0.8;
    farther[1] = 0.6;
    fixture
        .session
        .insert_chunk_embedding("allowed-single", 0, &farther)
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("allowed-repeated", 0, &unit(1))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("allowed-deleted", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("excluded", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .soft_delete_note("allowed-deleted", 1, 2)
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .title_search("Needle", 1, None)
            .await
            .unwrap(),
        vec!["excluded"]
    );
    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 1, None)
            .await
            .unwrap(),
        vec!["excluded"]
    );
    let allowed = vec![
        "allowed-single".to_string(),
        "allowed-repeated".to_string(),
        "allowed-single".to_string(),
        "allowed-deleted".to_string(),
        "allowed-repeated".to_string(),
    ];
    assert_eq!(
        fixture
            .session
            .title_search("needle", 2, Some(&allowed))
            .await
            .unwrap(),
        vec!["allowed-repeated", "allowed-single"]
    );
    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 1, Some(&allowed))
            .await
            .unwrap(),
        vec!["allowed-single"]
    );
}

#[tokio::test]
async fn dense_query_returns_nearest_neighbors() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    insert_test_note(&fixture.session, "note-2").await;

    let mut close = vec![1.0_f32; note_storage::EMBEDDING_DIMENSION];
    close[0] = 1.01;
    let mut far = vec![0.0_f32; note_storage::EMBEDDING_DIMENSION];
    far[0] = 1.0;
    fixture
        .session
        .insert_chunk_embedding("note-1", 0, &close)
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("note-2", 0, &far)
        .await
        .unwrap();

    let query = vec![1.0_f32; note_storage::EMBEDDING_DIMENSION];
    let ranked = fixture
        .session
        .dense_search(&query, 10, None)
        .await
        .unwrap();
    assert_eq!(ranked[0], "note-1");
}

#[tokio::test]
async fn dense_search_ranks_each_note_by_its_closest_chunk() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
    insert_test_note(&fixture.session, "b").await;
    fixture
        .session
        .insert_chunk_embedding("a", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("a", 1, &unit(3))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("b", 0, &unit(1))
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 2, None)
            .await
            .unwrap(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn dense_search_deduplicates_chunks_to_note_ids() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .insert_chunk_embedding("note-1", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("note-1", 1, &unit(1))
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 10, None)
            .await
            .unwrap(),
        vec!["note-1"]
    );
    assert!(fixture
        .session
        .chunk_embedding_exists("note-1", 0)
        .await
        .unwrap());
    assert!(!fixture
        .session
        .chunk_embedding_exists("note-1", 9)
        .await
        .unwrap());
}

#[tokio::test]
async fn dense_search_excludes_soft_deleted_notes_and_restore_reuses_vectors() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "active").await;
    insert_test_note(&fixture.session, "deleted").await;
    fixture
        .session
        .insert_chunk_embedding("active", 0, &unit(1))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("deleted", 0, &unit(0))
        .await
        .unwrap();

    fixture
        .session
        .soft_delete_note("deleted", 1, 2)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 10, None)
            .await
            .unwrap(),
        vec!["active"]
    );

    fixture.session.restore_note("deleted", 2).await.unwrap();
    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 10, None)
            .await
            .unwrap(),
        vec!["deleted", "active"]
    );
}

#[tokio::test]
async fn equal_dense_distances_use_note_id_order() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "b").await;
    insert_test_note(&fixture.session, "a").await;
    fixture
        .session
        .insert_chunk_embedding("b", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("a", 0, &unit(0))
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .dense_search(&unit(0), 2, None)
            .await
            .unwrap(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn vectors_must_have_1024_finite_components() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;

    let error = fixture
        .session
        .insert_chunk_embedding("a", 0, &[0.0; 3])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    let error = fixture
        .session
        .dense_search(&[0.0; 3], 10, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);

    let mut invalid_insert = unit(0);
    invalid_insert[1] = f32::NAN;
    let error = fixture
        .session
        .insert_chunk_embedding("a", 0, &invalid_insert)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    let mut invalid_query = unit(0);
    invalid_query[1] = f32::INFINITY;
    let error = fixture
        .session
        .dense_search(&invalid_query, 10, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
}

#[tokio::test]
async fn empty_inputs_and_zero_limits_return_no_results() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
    fixture
        .session
        .insert_chunk_embedding("a", 0, &unit(0))
        .await
        .unwrap();

    assert!(fixture
        .session
        .dense_search(&unit(0), 0, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deleting_a_note_cascades_chunks_jobs_and_retrieval() {
    let fixture = fixture().await;
    insert_named_note(
        &fixture.session,
        "a",
        "Distinctive cascade heading",
        "content",
    )
    .await;
    fixture
        .session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "a",
            chunk_idx: 0,
            content_hash: "hash",
            content: "content",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("a", 0, &unit(0))
        .await
        .unwrap();
    fixture
        .session
        .enqueue_embedding_job("a", 0, "hash", "content", 1, 1)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .session
            .title_search("cascade", 10, None)
            .await
            .unwrap(),
        vec!["a"]
    );

    fixture.session.soft_delete_note("a", 1, 2).await.unwrap();
    fixture
        .session
        .permanently_delete_note("a", 2)
        .await
        .unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert!(observer
        .title_search("cascade", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .list_note_chunks("a")
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .dense_search(&unit(0), 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .claim_pending_embedding_jobs(10, 3)
        .await
        .unwrap()
        .is_empty());
}
