#[allow(dead_code)]
mod support;

use note_storage::{
    NewNote, NoteUpdate, NotesRepository, RetrievalRepository, StorageErrorKind,
    EMBEDDING_DIMENSION,
};
use note_storage_pg::{PgSession, PgStorage};
use sqlx::AssertSqlSafe;
use std::sync::OnceLock;
use support::{configured_url_or_skip, TestDatabase};
use tokio::sync::{Mutex, MutexGuard};

const TITLE_SEARCH_SQL: &str = include_str!("../src/title_search.sql");

async fn real_database_test_guard() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(())).lock().await
}

fn unit(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; EMBEDDING_DIMENSION];
    vector[index] = 1.0;
    vector
}

async fn insert_note(session: &PgSession, id: &str, title: &str, content: &str) {
    session
        .insert_note(NewNote {
            id,
            title,
            content,
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn allowed_ids_are_applied_before_title_and_dense_limits() {
    let Some(admin_url) =
        configured_url_or_skip("allowed_ids_are_applied_before_title_and_dense_limits")
    else {
        return;
    };
    let _guard = real_database_test_guard().await;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();

    insert_note(&session, "allowed", "Needle", "allowed body").await;
    insert_note(
        &session,
        "excluded",
        "Needle Needle Needle",
        "excluded body",
    )
    .await;
    session
        .insert_chunk_embedding("allowed", 0, &unit(1))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("excluded", 0, &unit(0))
        .await
        .unwrap();

    assert_eq!(
        session.title_search("Needle", 1, None).await.unwrap(),
        vec!["excluded"]
    );
    assert_eq!(
        session.dense_search(&unit(0), 1, None).await.unwrap(),
        vec!["excluded"]
    );
    let allowed = vec!["allowed".to_string()];
    assert_eq!(
        session
            .title_search("Needle", 1, Some(&allowed))
            .await
            .unwrap(),
        allowed
    );
    assert_eq!(
        session
            .dense_search(&unit(0), 1, Some(&allowed))
            .await
            .unwrap(),
        allowed
    );

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up allowed retrieval test database");
}

#[tokio::test]
async fn exact_dense_retrieval_validates_vectors_and_filters_active_notes() {
    let Some(admin_url) =
        configured_url_or_skip("exact_dense_retrieval_validates_vectors_and_filters_active_notes")
    else {
        return;
    };
    let _guard = real_database_test_guard().await;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();

    insert_note(&session, "validation", "Validation", "body").await;
    for length in [EMBEDDING_DIMENSION - 1, EMBEDDING_DIMENSION + 1] {
        let insert_error = session
            .insert_chunk_embedding("validation", 0, &vec![0.0; length])
            .await
            .unwrap_err();
        assert_eq!(insert_error.kind(), StorageErrorKind::Operation);
        let query_error = session
            .dense_search(&vec![0.0; length], 10, None)
            .await
            .unwrap_err();
        assert_eq!(query_error.kind(), StorageErrorKind::Operation);
    }
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut vector = unit(0);
        vector[1] = invalid;
        let insert_error = session
            .insert_chunk_embedding("validation", 0, &vector)
            .await
            .unwrap_err();
        assert_eq!(insert_error.kind(), StorageErrorKind::Operation);
        let query_error = session.dense_search(&vector, 10, None).await.unwrap_err();
        assert_eq!(query_error.kind(), StorageErrorKind::Operation);
    }

    for id in ["a", "b", "c"] {
        insert_note(&session, id, id, "body").await;
    }
    let mut diagonal = vec![0.0; EMBEDDING_DIMENSION];
    diagonal[0] = 0.8;
    diagonal[1] = 0.6;
    session
        .insert_chunk_embedding("a", 0, &unit(1))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("a", 1, &unit(0))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("b", 0, &diagonal)
        .await
        .unwrap();
    session
        .insert_chunk_embedding("c", 0, &diagonal)
        .await
        .unwrap();

    assert_eq!(
        session.dense_search(&unit(0), 10, None).await.unwrap(),
        vec!["a", "b", "c"]
    );
    assert!(session
        .dense_search(&unit(0), 0, None)
        .await
        .unwrap()
        .is_empty());
    if let Some(unsupported_limit) = usize::try_from(i64::MAX)
        .ok()
        .and_then(|limit| limit.checked_add(1))
    {
        assert_eq!(
            session
                .dense_search(&unit(0), unsupported_limit, None)
                .await
                .unwrap_err()
                .kind(),
            StorageErrorKind::Operation
        );
    }
    session
        .insert_chunk_embedding("b", 0, &unit(1))
        .await
        .unwrap();
    assert_eq!(
        session.dense_search(&unit(0), 10, None).await.unwrap(),
        vec!["a", "c", "b"]
    );
    session
        .insert_chunk_embedding("b", 0, &diagonal)
        .await
        .unwrap();
    session.soft_delete_note("a", 2).await.unwrap();
    assert_eq!(
        session.dense_search(&unit(0), 10, None).await.unwrap(),
        vec!["b", "c"]
    );
    session.restore_note("a", 2).await.unwrap();
    assert_eq!(
        session.dense_search(&unit(0), 10, None).await.unwrap(),
        vec!["a", "b", "c"]
    );

    drop(session);
    let inspection_pool = database.inspect_pool().await;
    let ann_indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexdef
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND tablename = 'note_chunk_embeddings'
           AND (lower(indexdef) LIKE '%hnsw%' OR lower(indexdef) LIKE '%ivfflat%')",
    )
    .fetch_all(&inspection_pool)
    .await
    .unwrap();
    assert!(
        ann_indexes.is_empty(),
        "unexpected ANN indexes: {ann_indexes:?}"
    );
    inspection_pool.close().await;

    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up exact retrieval test database");
}

#[tokio::test]
async fn title_fts_is_literal_title_only_active_and_uses_gin() {
    let Some(admin_url) =
        configured_url_or_skip("title_fts_is_literal_title_only_active_and_uses_gin")
    else {
        return;
    };
    let _guard = real_database_test_guard().await;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();

    insert_note(&session, "title", "Rust ownership guide", "unrelated body").await;
    insert_note(
        &session,
        "body-only",
        "Other guide",
        "Rust ownership appears only in the body",
    )
    .await;
    insert_note(&session, "alpha", "Alpha reference", "body").await;
    insert_note(&session, "beta", "Beta reference", "body").await;

    assert_eq!(
        session.title_search("rust", 10, None).await.unwrap(),
        vec!["title"]
    );
    assert!(session
        .title_search("appears", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session
            .title_search("(alpha / beta):", 10, None)
            .await
            .unwrap(),
        vec!["alpha", "beta"]
    );
    assert_eq!(
        session
            .title_search(r#"alpha & !beta | gamma:* <-> '("#, 10, None)
            .await
            .unwrap(),
        vec!["alpha", "beta"]
    );
    assert!(session
        .title_search("   ", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .title_search(r#""():-!"#, 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .title_search("alpha", 0, None)
        .await
        .unwrap()
        .is_empty());
    if let Some(unsupported_limit) = usize::try_from(i64::MAX)
        .ok()
        .and_then(|limit| limit.checked_add(1))
    {
        assert_eq!(
            session
                .title_search("alpha", unsupported_limit, None)
                .await
                .unwrap_err()
                .kind(),
            StorageErrorKind::Operation
        );
    }

    session
        .update_note(NoteUpdate {
            id: "title",
            title: "New heading",
            content: "unrelated body",
            attachments: &[],
            updated_at: 2,
            note_revision: 2,
        })
        .await
        .unwrap();
    assert!(session
        .title_search("ownership", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session.title_search("new", 10, None).await.unwrap(),
        vec!["title"]
    );
    session.soft_delete_note("title", 3).await.unwrap();
    assert!(session
        .title_search("new", 10, None)
        .await
        .unwrap()
        .is_empty());
    session.restore_note("title", 3).await.unwrap();
    assert_eq!(
        session.title_search("new", 10, None).await.unwrap(),
        vec!["title"]
    );

    insert_note(&session, "tie-b", "Shared ranking heading", "body").await;
    insert_note(&session, "tie-a", "Shared ranking heading", "body").await;
    assert_eq!(
        session.title_search("shared", 2, None).await.unwrap(),
        vec!["tie-a", "tie-b"]
    );

    drop(session);
    let inspection_pool = database.inspect_pool().await;
    sqlx::query(
        "INSERT INTO notes (
             id, title, content, created_at, updated_at, note_revision, deleted_at
         )
         SELECT 'distractor-' || value, 'unrelated heading ' || value, 'body', value, value, 1, NULL
         FROM generate_series(1, 5000) AS value",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    let mut transaction = inspection_pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let explain_sql = format!("EXPLAIN (COSTS OFF)\n{TITLE_SEARCH_SQL}");
    let plan: Vec<String> = sqlx::query_scalar(AssertSqlSafe(explain_sql))
        .bind("'new'")
        .bind(Option::<Vec<String>>::None)
        .bind(10_i64)
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
    let rendered = plan.join("\n");
    assert!(
        rendered.contains("Bitmap Index Scan on idx_notes_title_fts")
            || rendered.contains("Index Scan using idx_notes_title_fts"),
        "expected title GIN index in plan:\n{rendered}"
    );
    transaction.rollback().await.unwrap();
    inspection_pool.close().await;

    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up title retrieval test database");
}
