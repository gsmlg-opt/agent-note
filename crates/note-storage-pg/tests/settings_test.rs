#[allow(dead_code)]
mod support;

use note_storage::{SettingsRepository, StorageErrorKind};
use note_storage_pg::PgStorage;
use serde_json::Value;
use std::error::Error;
use support::{configured_url_or_skip, TestDatabase};

#[tokio::test]
async fn sync_records_are_atomic_and_deleted_notes_remain_visible() {
    use note_storage::{NewNote, StorageBackend};
    use std::sync::Arc;

    let Some((database, storage)) =
        storage_for("sync_records_are_atomic_and_deleted_notes_remain_visible").await
    else {
        return;
    };
    let storage = Arc::new(storage);
    note_storage_contract_tests::run_sync_records(storage.clone()).await;
    let session = storage.session().await.unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    session
        .put_sync_note(NewNote {
            id,
            title: "In trash",
            content: "Retain the deletion snapshot",
            attachments: &[],
            created_at: 1,
            updated_at: 2,
            note_revision: 3,
            deleted_at: Some(20),
        })
        .await
        .unwrap();
    assert!(session.get_note(id).await.unwrap().is_none());
    let snapshot = session.get_sync_note(id).await.unwrap().unwrap();
    assert_eq!(snapshot.deleted_at, Some(20));
    assert_eq!(snapshot.revision, 3);
    assert_eq!(
        session.list_sync_note_ids(None, 10).await.unwrap(),
        vec![id]
    );
    drop(session);
    database.cleanup(Some(storage.as_ref())).await.unwrap();
}

async fn storage_for(test_name: &str) -> Option<(TestDatabase, PgStorage)> {
    let admin_url = configured_url_or_skip(test_name)?;
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    Some((database, storage))
}

#[tokio::test]
async fn embedding_fingerprint_defaults_to_none_and_persists() {
    let Some((database, storage)) =
        storage_for("embedding_fingerprint_defaults_to_none_and_persists").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();

    assert_eq!(session.get_embedding_fingerprint().await.unwrap(), None);
    session
        .set_embedding_fingerprint("bge-m3:1024")
        .await
        .unwrap();
    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
    let inspection_pool = database.inspect_pool().await;
    let stored: Value =
        sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'embedding_fingerprint'")
            .fetch_one(&inspection_pool)
            .await
            .unwrap();
    assert_eq!(stored, Value::String("bge-m3:1024".into()));
    inspection_pool.close().await;

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up fingerprint persistence test database");
}

#[tokio::test]
async fn embedding_fingerprint_rejects_blank_values() {
    let Some((database, storage)) = storage_for("embedding_fingerprint_rejects_blank_values").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();

    let error = session
        .set_embedding_fingerprint(" \t\n")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    assert_eq!(session.get_embedding_fingerprint().await.unwrap(), None);

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up blank fingerprint test database");
}

#[tokio::test]
async fn malformed_embedding_fingerprint_json_is_a_safe_operation_error() {
    let Some((database, storage)) =
        storage_for("malformed_embedding_fingerprint_json_is_a_safe_operation_error").await
    else {
        return;
    };
    let session = storage.connect_session().await.unwrap();
    let inspection_pool = database.inspect_pool().await;
    sqlx::query(
        "INSERT INTO app_settings (key, value)
         VALUES ('embedding_fingerprint', $1)",
    )
    .bind(Value::Number(42.into()))
    .execute(&inspection_pool)
    .await
    .unwrap();
    inspection_pool.close().await;

    let error = session.get_embedding_fingerprint().await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    assert!(error.source().is_none());
    let message = error.to_string();
    assert!(!message.contains(&database.url));
    assert!(!message.contains("postgres:postgres"));

    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean up malformed fingerprint test database");
}
