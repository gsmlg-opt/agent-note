#[allow(dead_code)]
mod support;

use note_core::{
    parse_label_selectors, DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm,
    LabelValueType, NoteAttachment, SystemConfig,
};
use note_storage::{
    EmbeddingRepository, LabelRepository, NewNote, NoteUpdate, NotesRepository, SettingsRepository,
    StorageErrorKind, TransactionMode, UpsertNoteChunk,
};
use note_storage_pg::PgStorage;
use serde_json::Value;
use support::{configured_url_or_skip, TestDatabase};

#[tokio::test]
async fn notes_and_labels_follow_repository_semantics() {
    let Some(admin_url) = configured_url_or_skip("notes_and_labels_follow_repository_semantics")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage
        .connect_session()
        .await
        .expect("connect PostgreSQL session");

    let attachment = NoteAttachment {
        id: "meta".into(),
        path: "meta.json".into(),
        mime: "application/json".into(),
        description: "metadata".into(),
        content: b"must not be stored".to_vec(),
    };
    session
        .insert_note(NewNote {
            id: "pg-jsonb",
            title: "JSON metadata",
            content: "active body",
            attachments: std::slice::from_ref(&attachment),
            created_at: 100,
            updated_at: 101,
            note_revision: 4,
            deleted_at: None,
        })
        .await
        .expect("insert active note");
    session
        .insert_note(NewNote {
            id: "deleted",
            title: "Deleted",
            content: "deleted body",
            attachments: &[],
            created_at: 50,
            updated_at: 51,
            note_revision: 2,
            deleted_at: Some(700),
        })
        .await
        .expect("insert deleted note");

    assert_eq!(
        session.get_note_revision("pg-jsonb").await.unwrap(),
        Some(4)
    );
    assert_eq!(session.get_note_revision("deleted").await.unwrap(), None);
    assert!(session.note_exists("pg-jsonb").await.unwrap());
    assert!(session.note_exists("deleted").await.unwrap());
    assert_eq!(
        session.get_note_content("pg-jsonb").await.unwrap(),
        Some("active body".into())
    );
    assert_eq!(session.get_note_content("deleted").await.unwrap(), None);

    let note = session.get_note("pg-jsonb").await.unwrap().unwrap();
    assert_eq!(note.title, "JSON metadata");
    assert_eq!(note.content, "active body");
    assert_eq!(note.created_at, 100);
    assert_eq!(note.updated_at, 101);
    assert_eq!(note.deleted_at, None);
    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].id, "meta");
    assert_eq!(note.attachments[0].path, "meta.json");
    assert_eq!(note.attachments[0].mime, "application/json");
    assert_eq!(note.attachments[0].description, "metadata");
    assert!(note.attachments[0].content.is_empty());

    let inspection_pool = database.inspect_pool().await;
    let (kind, path, stored): (Option<String>, Option<String>, Value) = sqlx::query_as(
        "SELECT jsonb_typeof(attachments), attachments->0->>'path', attachments
         FROM notes WHERE id = 'pg-jsonb'",
    )
    .fetch_one(&inspection_pool)
    .await
    .expect("inspect attachment JSONB representation");
    assert_eq!(kind.as_deref(), Some("array"));
    assert_eq!(path.as_deref(), Some("meta.json"));
    let object = stored
        .as_array()
        .and_then(|items| items.first())
        .and_then(Value::as_object)
        .expect("stored attachment object");
    assert!(!object.contains_key("content"));
    assert!(!object.contains_key("content_base64"));

    let updated_attachment = NoteAttachment {
        id: "updated".into(),
        path: "updated.txt".into(),
        mime: "text/plain".into(),
        description: "updated metadata".into(),
        content: b"also not stored".to_vec(),
    };
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "pg-jsonb",
                title: "Updated",
                content: "updated body",
                attachments: std::slice::from_ref(&updated_attachment),
                updated_at: 110,
                note_revision: 5,
            })
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "deleted",
                title: "Must not change",
                content: "must not change",
                attachments: &[],
                updated_at: 800,
                note_revision: 9,
            })
            .await
            .unwrap(),
        0
    );
    let note = session.get_note("pg-jsonb").await.unwrap().unwrap();
    assert_eq!(note.title, "Updated");
    assert_eq!(note.content, "updated body");
    assert_eq!(note.created_at, 100);
    assert_eq!(note.updated_at, 110);
    assert_eq!(note.attachments[0].id, "updated");
    assert!(note.attachments[0].content.is_empty());

    session
        .insert_label_key_with_type("priority", "Priority", LabelValueType::Number)
        .await
        .unwrap();
    session
        .insert_label_key_with_type("status", "Status", LabelValueType::Text)
        .await
        .unwrap();
    session
        .insert_label_key("active-only", "Active marker")
        .await
        .unwrap();
    session
        .update_label_key("priority", "Updated priority")
        .await
        .unwrap();
    session
        .insert_label_key_if_missing("priority", "Must not replace")
        .await
        .unwrap();
    session
        .update_label_key_with_type("status", "Version status", LabelValueType::Version)
        .await
        .unwrap();
    let keys = session.list_label_keys().await.unwrap();
    assert_eq!(
        keys.iter()
            .map(|key| (key.key.as_str(), key.description.as_str(), key.value_type))
            .collect::<Vec<_>>(),
        vec![
            ("active-only", "Active marker", LabelValueType::Text),
            ("priority", "Updated priority", LabelValueType::Number),
            ("status", "Version status", LabelValueType::Version),
        ]
    );
    session
        .update_label_key_with_type("status", "Status", LabelValueType::Text)
        .await
        .unwrap();

    let duplicate_key = session
        .insert_label_key("priority", "Duplicate")
        .await
        .unwrap_err();
    assert_eq!(duplicate_key.kind(), StorageErrorKind::Constraint);
    let unknown_key = session
        .attach_label("pg-jsonb", "missing", "value")
        .await
        .unwrap_err();
    assert_eq!(unknown_key.kind(), StorageErrorKind::Operation);
    assert_eq!(unknown_key.to_string(), "unknown label key: missing");

    for (key, value) in [
        ("priority", "2"),
        ("status", "ready"),
        ("active-only", "yes"),
    ] {
        session.attach_label("pg-jsonb", key, value).await.unwrap();
    }
    let labels = session.labels_for_note("pg-jsonb").await.unwrap();
    assert_eq!(labels.len(), 3);
    assert!(labels.iter().any(|label| {
        label.key == "priority"
            && label.value == "2"
            && label.description == "Updated priority"
            && label.value_type == LabelValueType::Number
    }));
    assert_eq!(
        session
            .get_note("pg-jsonb")
            .await
            .unwrap()
            .unwrap()
            .labels
            .into_iter()
            .map(|label| label.key)
            .collect::<Vec<_>>(),
        vec!["active-only", "priority", "status"]
    );
    let duplicate_attachment = session
        .attach_label("pg-jsonb", "status", "other")
        .await
        .unwrap_err();
    assert_eq!(duplicate_attachment.kind(), StorageErrorKind::Constraint);

    let expected = Some("pg-jsonb".to_string());
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("status".into(), "ready".into()),
                ("priority".into(), "2".into()),
            ])
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("priority".into(), "2".into()),
                ("status".into(), "ready".into()),
            ])
            .await
            .unwrap(),
        Some("pg-jsonb".into())
    );
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("priority".into(), "2".into()),
                ("priority".into(), "2".into()),
            ])
            .await
            .unwrap(),
        None
    );
    assert_eq!(session.find_note_with_labels(&[]).await.unwrap(), None);

    for (id, created_at, priority, status) in [
        ("numeric-low", 400, "1", "ready"),
        ("numeric-ten", 350, "10", "ready"),
        ("newer", 300, "5", "ready"),
        ("middle", 200, "3", "ready"),
        ("other", 150, "9", "blocked"),
    ] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content: id,
                attachments: &[],
                created_at,
                updated_at: created_at,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        session
            .attach_label(id, "priority", priority)
            .await
            .unwrap();
        session.attach_label(id, "status", status).await.unwrap();
    }

    let selectors = parse_label_selectors("priority>=2&status=ready");
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 4);
    assert_eq!(
        session
            .list_notes(&selectors, Some(2), Some(1))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["newer", "middle"]
    );
    assert_eq!(
        session
            .list_note_summaries(&selectors, Some(1), Some(1))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["newer"]
    );
    let expected_unbounded = vec!["numeric-ten", "newer", "middle", "pg-jsonb"];
    assert_eq!(
        session
            .list_notes(&selectors, Some(-1), Some(-10))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        expected_unbounded
    );
    assert_eq!(
        session
            .list_note_summaries(&selectors, Some(-1), Some(-10))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["numeric-ten", "newer", "middle", "pg-jsonb"]
    );
    assert_eq!(
        session
            .list_notes(&[], Some(-1), Some(-10))
            .await
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        session
            .list_note_summaries(&[], Some(-1), Some(-10))
            .await
            .unwrap()
            .len(),
        6
    );
    assert_eq!(session.list_all_notes().await.unwrap().len(), 7);

    for id in ["tie-c", "tie-a", "tie-b"] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content: id,
                attachments: &[],
                created_at: 1_000,
                updated_at: 1_000,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        session
            .list_notes(&[], Some(2), Some(0))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["tie-a", "tie-b"]
    );
    assert_eq!(
        session
            .list_notes(&[], Some(1), Some(2))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["tie-c"]
    );
    assert_eq!(
        session
            .list_note_summaries(&[], Some(3), Some(0))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["tie-a", "tie-b", "tie-c"]
    );
    for id in ["tie-a", "tie-b", "tie-c"] {
        session.soft_delete_note(id, 450).await.unwrap();
        session.permanently_delete_note(id).await.unwrap();
    }

    for index in 0..5 {
        let id = format!("selector-noise-{index}");
        session
            .insert_note(NewNote {
                id: &id,
                title: &id,
                content: &id,
                attachments: &[],
                created_at: 900 - index,
                updated_at: 900 - index,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        session.attach_label(&id, "priority", "9").await.unwrap();
        session
            .attach_label(&id, "status", "blocked")
            .await
            .unwrap();
    }
    assert_eq!(
        session
            .list_notes(&selectors, Some(1), Some(0))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["numeric-ten"]
    );
    assert_eq!(
        session
            .list_note_summaries(&selectors, Some(1), Some(1))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["newer"]
    );
    for index in 0..5 {
        let id = format!("selector-noise-{index}");
        session.soft_delete_note(&id, 450).await.unwrap();
        session.permanently_delete_note(&id).await.unwrap();
    }

    assert_eq!(session.soft_delete_note("pg-jsonb", 500).await.unwrap(), 1);
    assert_eq!(session.soft_delete_note("pg-jsonb", 501).await.unwrap(), 0);
    assert!(session.get_note("pg-jsonb").await.unwrap().is_none());
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("pg-jsonb")
            .await
            .unwrap(),
        Some(("updated body".into(), 5))
    );
    let deleted_summaries = session.list_deleted_note_summaries().await.unwrap();
    assert_eq!(deleted_summaries[0].id, "deleted");
    assert_eq!(deleted_summaries[1].id, "pg-jsonb");
    assert_eq!(deleted_summaries[1].deleted_at, Some(500));
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "pg-jsonb",
                title: "Must not update",
                content: "Must not update",
                attachments: &[],
                updated_at: 600,
                note_revision: 9,
            })
            .await
            .unwrap(),
        0
    );
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 3);
    assert_eq!(
        session
            .find_note_with_labels(&[("active-only".into(), "yes".into())])
            .await
            .unwrap(),
        None
    );
    assert!(session
        .label_note_counts()
        .await
        .unwrap()
        .contains(&("active-only".into(), 0)));
    assert_eq!(session.restore_note("pg-jsonb", 6).await.unwrap(), 1);
    assert_eq!(session.restore_note("pg-jsonb", 7).await.unwrap(), 0);
    assert_eq!(
        session.get_note_revision("pg-jsonb").await.unwrap(),
        Some(6)
    );

    for (id, deleted_at) in [
        ("expired-b", 700),
        ("expired-a", 700),
        ("expired-old", 699),
        ("expired-new", 701),
    ] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content: id,
                attachments: &[],
                created_at: 1,
                updated_at: 1,
                note_revision: 1,
                deleted_at: Some(deleted_at),
            })
            .await
            .unwrap();
    }
    assert_eq!(
        session.list_expired_deleted_note_ids(700).await.unwrap(),
        vec!["expired-old", "deleted", "expired-a", "expired-b"]
    );

    assert_eq!(session.permanently_delete_note("newer").await.unwrap(), 0);
    sqlx::query(
        "INSERT INTO note_chunks (
             note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
         ) VALUES
             ('pg-jsonb', 0, 'zero', 'zero', 6, 'ready', 1),
             ('pg-jsonb', 1, 'one', 'one', 6, 'ready', 1),
             ('pg-jsonb', 2, 'two', 'two', 6, 'ready', 1)",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding)
         VALUES
             ('pg-jsonb', 0, array_fill(0::real, ARRAY[1024])::vector),
             ('pg-jsonb', 1, array_fill(0::real, ARRAY[1024])::vector),
             ('pg-jsonb', 2, array_fill(0::real, ARRAY[1024])::vector)",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO embedding_jobs (
             note_id, chunk_idx, chunk_hash, content, note_revision,
             status, attempts, created_at, updated_at
         ) VALUES ('pg-jsonb', 0, 'zero', 'zero', 6, 'pending', 0, 1, 1)",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    session
        .clear_note_chunk_derived("pg-jsonb", 0)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint
             FROM note_chunk_embeddings
             WHERE note_id = 'pg-jsonb'",
        )
        .fetch_one(&inspection_pool)
        .await
        .unwrap(),
        2
    );
    session
        .clear_note_chunks_from_derived("pg-jsonb", 2)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint
             FROM note_chunk_embeddings
             WHERE note_id = 'pg-jsonb'",
        )
        .fetch_one(&inspection_pool)
        .await
        .unwrap(),
        1
    );
    session.clear_note_search_data("pg-jsonb").await.unwrap();
    for table in ["note_chunk_embeddings", "embedding_jobs", "note_chunks"] {
        let query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*)::bigint FROM {table} WHERE note_id = 'pg-jsonb'"
        )));
        assert_eq!(query.fetch_one(&inspection_pool).await.unwrap(), 0);
    }

    sqlx::query(
        "INSERT INTO note_chunks (
             note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
         ) VALUES ('other', 0, 'other', 'other', 1, 'ready', 1)",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    session.soft_delete_note("other", 800).await.unwrap();
    assert_eq!(session.permanently_delete_note("other").await.unwrap(), 1);
    assert!(session.labels_for_note("other").await.unwrap().is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM note_chunks WHERE note_id = 'other'",
        )
        .fetch_one(&inspection_pool)
        .await
        .unwrap(),
        0
    );

    assert_eq!(
        session
            .list_active_note_sources()
            .await
            .unwrap()
            .into_iter()
            .map(|source| (source.id, source.content, source.note_revision))
            .collect::<Vec<_>>(),
        vec![
            ("middle".into(), "middle".into(), 1),
            ("newer".into(), "newer".into(), 1),
            ("numeric-low".into(), "numeric-low".into(), 1),
            ("numeric-ten".into(), "numeric-ten".into(), 1),
            ("pg-jsonb".into(), "updated body".into(), 6),
        ]
    );

    session.clear_note_labels("pg-jsonb").await.unwrap();
    assert!(session
        .labels_for_note("pg-jsonb")
        .await
        .unwrap()
        .is_empty());
    session.delete_label_key("active-only").await.unwrap();
    assert!(!session
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "active-only"));

    inspection_pool.close().await;
    drop(session);
    database
        .cleanup(Some(&storage))
        .await
        .expect("clean isolated PostgreSQL test database");
}

#[tokio::test]
async fn malformed_attachment_json_is_an_operation_error() {
    let Some(admin_url) = configured_url_or_skip("malformed_attachment_json_is_an_operation_error")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 2)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();
    session
        .insert_note(NewNote {
            id: "malformed",
            title: "Malformed",
            content: "body",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();

    let inspection_pool = database.inspect_pool().await;
    sqlx::query("UPDATE notes SET attachments = '{\"not\":\"an array\"}'::jsonb WHERE id = $1")
        .bind("malformed")
        .execute(&inspection_pool)
        .await
        .unwrap();
    let selector = parse_label_selectors("missing=value");
    assert!(session
        .list_notes(&selector, Some(0), Some(0))
        .await
        .unwrap()
        .is_empty());
    let error = session.get_note("malformed").await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);

    session
        .insert_label_key("malformed-type", "Malformed type")
        .await
        .unwrap();
    session
        .attach_label("malformed", "malformed-type", "value")
        .await
        .unwrap();
    sqlx::query("UPDATE notes SET attachments = '[]'::jsonb WHERE id = 'malformed'")
        .execute(&inspection_pool)
        .await
        .unwrap();
    sqlx::query("UPDATE label_keys SET value_type = 'unknown' WHERE key = 'malformed-type'")
        .execute(&inspection_pool)
        .await
        .unwrap();
    let error = session.list_label_keys().await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    assert!(session
        .list_note_summaries(&selector, Some(0), Some(0))
        .await
        .unwrap()
        .is_empty());
    let error = session.get_note("malformed").await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);

    inspection_pool.close().await;
    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn embedding_chunks_jobs_dashboard_and_settings_follow_repository_semantics() {
    let Some(admin_url) = configured_url_or_skip(
        "embedding_chunks_jobs_dashboard_and_settings_follow_repository_semantics",
    ) else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();

    for (id, title, revision, deleted_at) in [
        ("complete", "Complete", 1, None),
        ("partial", "Partial", 1, None),
        ("stale", "Stale", 2, None),
        ("deleted", "Deleted", 1, Some(99)),
        ("processing", "Processing", 1, None),
    ] {
        session
            .insert_note(NewNote {
                id,
                title,
                content: "content",
                attachments: &[],
                created_at: 1,
                updated_at: 1,
                note_revision: revision,
                deleted_at,
            })
            .await
            .unwrap();
    }

    for (note_id, chunk_idx, revision, status) in [
        ("complete", 1, 1, "embedded"),
        ("complete", 0, 1, "embedded"),
        ("partial", 0, 1, "embedded"),
        ("partial", 1, 1, "pending"),
        ("stale", 0, 1, "embedded"),
        ("deleted", 0, 1, "embedded"),
        ("processing", 0, 1, "pending"),
    ] {
        session
            .upsert_note_chunk(UpsertNoteChunk {
                note_id,
                chunk_idx,
                content_hash: &format!("{note_id}-{chunk_idx}"),
                content: &format!("content-{chunk_idx}"),
                note_revision: revision,
                status,
                updated_at: 10 + chunk_idx,
            })
            .await
            .unwrap();
    }

    let chunks = session.list_note_chunks("complete").await.unwrap();
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.chunk_idx)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(chunks[0].content_hash, "complete-0");
    let dashboard = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(dashboard.embedded_note_count, 1);
    assert_eq!(dashboard.processing_note, None);
    assert_eq!(
        session
            .mark_note_chunk_status("complete", 0, "wrong", 1, "pending", 20)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .mark_note_chunk_status("complete", 0, "complete-0", 2, "pending", 20)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .mark_note_chunk_status("complete", 0, "complete-0", 1, "pending", 20)
            .await
            .unwrap(),
        1
    );
    let chunk = session
        .get_note_chunk("complete", 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chunk.status, "pending");
    assert_eq!(chunk.updated_at, 20);
    assert_eq!(
        session
            .delete_note_chunks_from("complete", 1)
            .await
            .unwrap(),
        1
    );

    let dashboard = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(dashboard.embedded_note_count, 0);
    assert_eq!(dashboard.processing_note, None);

    session
        .enqueue_embedding_job("processing", 0, "processing-0", "original", 1, 30)
        .await
        .unwrap();
    session
        .enqueue_embedding_job("processing", 0, "processing-0", "replacement", 2, 31)
        .await
        .unwrap();
    session
        .enqueue_embedding_job("processing", 1, "processing-1", "second", 1, 29)
        .await
        .unwrap();
    assert!(session
        .claim_pending_embedding_jobs(0, 32)
        .await
        .unwrap()
        .is_empty());
    #[cfg(target_pointer_width = "64")]
    {
        let error = session
            .claim_pending_embedding_jobs(usize::MAX, 32)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
    }
    let first = session
        .claim_pending_embedding_jobs(1, 33)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.chunk_idx, 1);
    assert_eq!(first.attempts, 1);
    let second = session
        .claim_pending_embedding_jobs(1, 34)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(second.chunk_idx, 0);
    assert_eq!(second.content, "replacement");
    assert_eq!(second.note_revision, 2);

    session
        .enqueue_embedding_job(
            "processing",
            0,
            "processing-0",
            "must not replace processing",
            3,
            35,
        )
        .await
        .unwrap();
    assert!(session
        .claim_pending_embedding_jobs(1, 36)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session
            .fail_embedding_job(first.id, first.attempts, 2, "retry", 37)
            .await
            .unwrap(),
        1
    );
    let retry = session
        .claim_pending_embedding_jobs(1, 38)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(retry.id, first.id);
    assert_eq!(retry.attempts, 2);
    assert_eq!(
        session
            .fail_embedding_job(retry.id, retry.attempts, 2, "terminal", 39)
            .await
            .unwrap(),
        1
    );
    session
        .enqueue_embedding_job("partial", 2, "dashboard", "dashboard", 1, 40)
        .await
        .unwrap();
    let oldest_processing = session
        .claim_pending_embedding_jobs(1, 32)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let processing = session.embedding_dashboard_status().await.unwrap();
    assert_eq!(processing.processing_note.unwrap().id, "partial");
    assert_eq!(
        session
            .delete_embedding_job(oldest_processing.id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .embedding_dashboard_status()
            .await
            .unwrap()
            .processing_note
            .unwrap()
            .id,
        "processing"
    );
    assert_eq!(
        session
            .delete_embedding_jobs_from_chunk("processing", 0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .embedding_dashboard_status()
            .await
            .unwrap()
            .processing_note
            .unwrap()
            .id,
        "processing"
    );
    assert_eq!(
        session.requeue_processing_embedding_jobs(40).await.unwrap(),
        1
    );
    let reclaimed = session
        .claim_pending_embedding_jobs(1, 41)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(reclaimed.id, second.id);
    assert_eq!(reclaimed.content, "replacement");
    assert_eq!(session.delete_embedding_job(reclaimed.id).await.unwrap(), 1);
    assert_eq!(session.delete_embedding_job(reclaimed.id).await.unwrap(), 0);

    session
        .enqueue_embedding_job("processing", 2, "old", "old", 1, 42)
        .await
        .unwrap();
    session
        .enqueue_embedding_job("processing", 2, "current", "current", 1, 43)
        .await
        .unwrap();
    let old = session
        .claim_pending_embedding_jobs(1, 44)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(old.content_hash, "old");
    assert_eq!(
        session
            .delete_stale_embedding_jobs_for_chunk("processing", 2, "current")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session.requeue_processing_embedding_jobs(45).await.unwrap(),
        1
    );
    assert_eq!(
        session
            .delete_stale_embedding_jobs_for_chunk("processing", 2, "current")
            .await
            .unwrap(),
        1
    );
    let current = session
        .claim_pending_embedding_jobs(1, 46)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(current.content_hash, "current");
    assert_eq!(session.delete_embedding_job(current.id).await.unwrap(), 1);
    assert_eq!(
        session
            .delete_embedding_jobs_from_chunk("processing", 0)
            .await
            .unwrap(),
        0
    );

    for chunk_idx in 1..=2 {
        session
            .upsert_note_chunk(UpsertNoteChunk {
                note_id: "processing",
                chunk_idx,
                content_hash: &format!("derived-{chunk_idx}"),
                content: "derived content",
                note_revision: 1,
                status: "embedded",
                updated_at: 50,
            })
            .await
            .unwrap();
    }
    let inspection_pool = database.inspect_pool().await;
    for chunk_idx in 0..=2 {
        sqlx::query(
            "INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding)
             VALUES ($1, $2, $3)",
        )
        .bind("processing")
        .bind(chunk_idx)
        .bind(pgvector::Vector::from(vec![
            0.0;
            note_storage::EMBEDDING_DIMENSION
        ]))
        .execute(&inspection_pool)
        .await
        .unwrap();
        assert!(session
            .chunk_embedding_exists("processing", chunk_idx)
            .await
            .unwrap());
    }
    session
        .clear_note_chunk_derived("processing", 0)
        .await
        .unwrap();
    session
        .clear_note_chunks_from_derived("processing", 2)
        .await
        .unwrap();
    assert!(!session
        .chunk_embedding_exists("processing", 0)
        .await
        .unwrap());
    assert!(session
        .chunk_embedding_exists("processing", 1)
        .await
        .unwrap());
    assert!(!session
        .chunk_embedding_exists("processing", 2)
        .await
        .unwrap());
    session
        .enqueue_embedding_job("processing", 2, "clear", "clear", 1, 51)
        .await
        .unwrap();
    session.clear_note_search_data("processing").await.unwrap();
    assert!(session
        .list_note_chunks("processing")
        .await
        .unwrap()
        .is_empty());
    assert!(!session
        .chunk_embedding_exists("processing", 1)
        .await
        .unwrap());
    assert!(session
        .claim_pending_embedding_jobs(10, 52)
        .await
        .unwrap()
        .is_empty());

    assert_eq!(
        session.get_system_config().await.unwrap(),
        SystemConfig::default()
    );
    let config = SystemConfig {
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: vec![DuplicateCheckTerm {
                    key: "kind".into(),
                    value: Some("note".into()),
                }],
            }],
        },
    };
    session.set_system_config(&config).await.unwrap();
    assert_eq!(session.get_system_config().await.unwrap(), config);

    sqlx::query(
        "UPDATE app_settings
         SET value = '{\"duplicate_check\":{\"enabled\":\"not-a-boolean\"}}'::jsonb
         WHERE key = 'system_config'",
    )
    .execute(&inspection_pool)
    .await
    .unwrap();
    let error = session.get_system_config().await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);

    inspection_pool.close().await;
    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}

#[tokio::test]
async fn concurrent_embedding_claims_are_disjoint_and_complete() {
    let Some(admin_url) =
        configured_url_or_skip("concurrent_embedding_claims_are_disjoint_and_complete")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage.connect_session().await.unwrap();
    session
        .insert_note(NewNote {
            id: "claim-concurrently",
            title: "Concurrent claims",
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    for chunk_idx in 0..10 {
        session
            .enqueue_embedding_job(
                "claim-concurrently",
                chunk_idx,
                &format!("hash-{chunk_idx}"),
                &format!("content-{chunk_idx}"),
                1,
                10,
            )
            .await
            .unwrap();
    }
    drop(session);

    let first = storage
        .begin_session(TransactionMode::Deferred)
        .await
        .unwrap();
    let second = storage
        .begin_session(TransactionMode::Deferred)
        .await
        .unwrap();
    let (first_claim, second_claim) = tokio::join!(
        first.claim_pending_embedding_jobs(5, 20),
        second.claim_pending_embedding_jobs(5, 20)
    );
    let first_claim = first_claim.unwrap();
    let second_claim = second_claim.unwrap();
    assert_eq!(first_claim.len(), 5);
    assert_eq!(second_claim.len(), 5);
    let first_ids = first_claim
        .iter()
        .map(|job| job.id)
        .collect::<std::collections::HashSet<_>>();
    let second_ids = second_claim
        .iter()
        .map(|job| job.id)
        .collect::<std::collections::HashSet<_>>();
    assert!(first_ids.is_disjoint(&second_ids));
    assert_eq!(first_ids.union(&second_ids).count(), 10);

    first.commit().await.unwrap();
    second.commit().await.unwrap();
    database.cleanup(Some(&storage)).await.unwrap();
}
