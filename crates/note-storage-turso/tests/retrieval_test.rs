mod support;

use note_storage::{
    EmbeddingRepository, NotesRepository, RetrievalRepository, StorageErrorKind, UpsertNoteChunk,
};
use support::{fixture, insert_test_note, unit};

#[tokio::test]
async fn sparse_query_ranks_by_matching_token_weight() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    insert_test_note(&fixture.session, "note-2").await;
    fixture
        .session
        .insert_chunk_sparse_weights("note-1", 0, &[(42, 0.9)])
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("note-2", 0, &[(42, 0.3)])
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .sparse_postings_query(&[42], 10)
            .await
            .unwrap(),
        vec!["note-1", "note-2"]
    );
}

#[tokio::test]
async fn sparse_query_ranks_by_best_matching_chunk_per_note() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    insert_test_note(&fixture.session, "note-2").await;
    fixture
        .session
        .insert_chunk_sparse_weights("note-1", 0, &[(42, 0.2)])
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("note-1", 1, &[(42, 0.8)])
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("note-2", 0, &[(42, 0.6), (7, 0.3)])
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .sparse_postings_query(&[42, 7], 10)
            .await
            .unwrap(),
        vec!["note-2", "note-1"]
    );
}

#[tokio::test]
async fn sparse_ties_use_note_id_order_and_respect_limits() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "b").await;
    insert_test_note(&fixture.session, "a").await;
    fixture
        .session
        .insert_chunk_sparse_weights("b", 0, &[(42, 1.0)])
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("a", 0, &[(42, 1.0)])
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .sparse_postings_query(&[42], 1)
            .await
            .unwrap(),
        vec!["a"]
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
    let ranked = fixture.session.dense_search(&query, 10).await.unwrap();
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
        fixture.session.dense_search(&unit(0), 2).await.unwrap(),
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
        fixture.session.dense_search(&unit(0), 10).await.unwrap(),
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
        fixture.session.dense_search(&unit(0), 2).await.unwrap(),
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
        .dense_search(&[0.0; 3], 10)
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
        .dense_search(&invalid_query, 10)
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
    fixture
        .session
        .insert_chunk_sparse_weights("a", 0, &[])
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("a", 0, &[(7, 1.0)])
        .await
        .unwrap();

    assert!(fixture
        .session
        .dense_search(&unit(0), 0)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .sparse_postings_query(&[], 10)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .sparse_postings_query(&[7], 0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deleting_a_note_cascades_chunks_jobs_and_retrieval() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
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
        .insert_chunk_sparse_weights("a", 0, &[(7, 1.0)])
        .await
        .unwrap();
    fixture
        .session
        .enqueue_embedding_job("a", 0, "hash", "content", 1, 1)
        .await
        .unwrap();
    fixture.session.soft_delete_note("a", 2).await.unwrap();
    fixture.session.permanently_delete_note("a").await.unwrap();

    assert!(fixture
        .session
        .list_note_chunks("a")
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .dense_search(&unit(0), 10)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .sparse_postings_query(&[7], 10)
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
