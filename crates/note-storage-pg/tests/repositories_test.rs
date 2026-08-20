#[allow(dead_code)]
mod support;

use note_core::{
    parse_label_selectors, AttachmentStorageMetadata, DuplicateCheckConfig, DuplicateCheckRule,
    DuplicateCheckTerm, LabelValueType, NoteAttachment, SystemConfig,
};
use note_storage::{
    EmbeddingRepository, LabelRepository, NewNote, NoteChunk, NoteMutationResult, NoteUpdate,
    NotesRepository, SettingsRepository, StorageErrorKind, TransactionMode, UpsertNoteChunk,
};
use note_storage_pg::PgStorage;
use serde_json::Value;
use support::{configured_url_or_skip, TestDatabase};

#[tokio::test]
async fn filtered_pages_and_counts_use_resolved_value_arrays() {
    let Some(admin_url) =
        configured_url_or_skip("filtered_pages_and_counts_use_resolved_value_arrays")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 2)
        .await
        .expect("connect PostgreSQL storage");
    let session = storage
        .connect_session()
        .await
        .expect("connect PostgreSQL session");

    session
        .insert_label_key_with_type("status", "Status", LabelValueType::Text)
        .await
        .unwrap();
    session
        .insert_label_key_with_type("version", "Version", LabelValueType::Version)
        .await
        .unwrap();

    for (index, version) in ["1.9.0", "1.10.0", "1.99.0", "2.0.0", "2.1.0"]
        .into_iter()
        .enumerate()
    {
        let id = format!("blocked-{index}");
        let created_at = 200 + index as i64;
        session
            .insert_note(NewNote {
                id: &id,
                title: &id,
                content: &id,
                attachments: &[],
                created_at,
                updated_at: created_at,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        session
            .attach_label(&id, "status", "blocked")
            .await
            .unwrap();
        session.attach_label(&id, "version", version).await.unwrap();
    }

    session
        .insert_note(NewNote {
            id: "ready",
            title: "ready",
            content: "ready",
            attachments: &[],
            created_at: 100,
            updated_at: 100,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    session
        .attach_label("ready", "status", "ready")
        .await
        .unwrap();

    let ready_selector = parse_label_selectors("status=ready");
    assert_eq!(
        session
            .list_note_summaries(&ready_selector, Some(1), Some(0))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["ready"]
    );
    assert_eq!(session.count_notes(&ready_selector).await.unwrap(), 1);

    let version_selectors = parse_label_selectors("version>=1.10.0&version<2.0.0");
    assert_eq!(
        session
            .list_note_summaries(&version_selectors, Some(1), Some(1))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec!["blocked-1"]
    );
    assert_eq!(session.count_notes(&version_selectors).await.unwrap(), 2);

    let invalid_regex = parse_label_selectors("status~=[");
    assert!(session
        .list_note_summaries(&invalid_regex, Some(10), Some(0))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(session.count_notes(&invalid_regex).await.unwrap(), 0);

    drop(session);
    database.cleanup(Some(&storage)).await.unwrap();
}

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
        storage: Some(AttachmentStorageMetadata {
            object_key: "notes/pg-jsonb/meta.json/generation-1".into(),
            storage_generation: "generation-1".into(),
            size_bytes: 18,
            checksum_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        }),
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
    assert_eq!(note.attachments[0].storage, attachment.storage);

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
    assert_eq!(
        object["object_key"],
        "notes/pg-jsonb/meta.json/generation-1"
    );
    assert_eq!(object["storage_generation"], "generation-1");
    assert_eq!(object["size_bytes"], 18);
    assert_eq!(
        object["checksum_sha256"],
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    );

    let updated_attachment = NoteAttachment {
        id: "updated".into(),
        path: "updated.txt".into(),
        mime: "text/plain".into(),
        description: "updated metadata".into(),
        content: b"also not stored".to_vec(),
        storage: Some(AttachmentStorageMetadata {
            object_key: "notes/pg-jsonb/updated.txt/generation-2".into(),
            storage_generation: "generation-2".into(),
            size_bytes: 15,
            checksum_sha256: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                .into(),
        }),
    };
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "pg-jsonb",
                title: "Updated",
                content: "updated body",
                attachments: std::slice::from_ref(&updated_attachment),
                updated_at: 110,
                expected_revision: 4,
            })
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 5
        }
    );
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "deleted",
                title: "Must not change",
                content: "must not change",
                attachments: &[],
                updated_at: 800,
                expected_revision: 9,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    let note = session.get_note("pg-jsonb").await.unwrap().unwrap();
    assert_eq!(note.title, "Updated");
    assert_eq!(note.content, "updated body");
    assert_eq!(note.created_at, 100);
    assert_eq!(note.updated_at, 110);
    assert_eq!(note.attachments[0].id, "updated");
    assert!(note.attachments[0].content.is_empty());
    assert_eq!(note.attachments[0].storage, updated_attachment.storage);

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
        session.soft_delete_note(id, 1, 450).await.unwrap();
        session.permanently_delete_note(id, 2).await.unwrap();
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
        session.soft_delete_note(&id, 1, 450).await.unwrap();
        session.permanently_delete_note(&id, 2).await.unwrap();
    }

    assert_eq!(
        session.soft_delete_note("pg-jsonb", 5, 500).await.unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 6
        }
    );
    assert_eq!(
        session.soft_delete_note("pg-jsonb", 6, 501).await.unwrap(),
        NoteMutationResult::NotFound
    );
    assert!(session.get_note("pg-jsonb").await.unwrap().is_none());
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("pg-jsonb")
            .await
            .unwrap(),
        Some(("updated body".into(), 6))
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
                expected_revision: 6,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
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
    assert_eq!(
        session.restore_note("pg-jsonb", 6).await.unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 7
        }
    );
    assert_eq!(
        session.restore_note("pg-jsonb", 7).await.unwrap(),
        NoteMutationResult::NotFound
    );
    assert_eq!(
        session.get_note_revision("pg-jsonb").await.unwrap(),
        Some(7)
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

    assert_eq!(
        session.permanently_delete_note("newer", 1).await.unwrap(),
        NoteMutationResult::NotFound
    );
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
    session.soft_delete_note("other", 1, 800).await.unwrap();
    assert_eq!(
        session.permanently_delete_note("other", 2).await.unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
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
            ("pg-jsonb".into(), "updated body".into(), 7),
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

    sqlx::query(
        r#"UPDATE notes SET attachments = '[{"id":"legacy","path":"legacy.txt","mime":"text/plain","description":"Legacy"}]'::jsonb WHERE id = $1"#,
    )
    .bind("malformed")
    .execute(&inspection_pool)
    .await
    .unwrap();
    let legacy = session.get_note("malformed").await.unwrap().unwrap();
    assert_eq!(legacy.attachments[0].storage, None);

    sqlx::query(
        r#"UPDATE notes SET attachments = '[{"id":"partial","path":"partial.txt","mime":"text/plain","description":"Partial","object_key":"notes/malformed/partial.txt/generation-1"}]'::jsonb WHERE id = $1"#,
    )
    .bind("malformed")
    .execute(&inspection_pool)
    .await
    .unwrap();
    let error = session.get_note("malformed").await.unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    sqlx::query("UPDATE notes SET deleted_at = 1 WHERE id = 'malformed'")
        .execute(&inspection_pool)
        .await
        .unwrap();
    let error = session
        .get_deleted_note_snapshot("malformed")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
    sqlx::query("UPDATE notes SET deleted_at = NULL WHERE id = 'malformed'")
        .execute(&inspection_pool)
        .await
        .unwrap();

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
        chunks,
        vec![
            NoteChunk {
                note_id: "complete".into(),
                chunk_idx: 0,
                content_hash: "complete-0".into(),
                content: "content-0".into(),
                note_revision: 1,
                status: "embedded".into(),
                updated_at: 10,
            },
            NoteChunk {
                note_id: "complete".into(),
                chunk_idx: 1,
                content_hash: "complete-1".into(),
                content: "content-1".into(),
                note_revision: 1,
                status: "embedded".into(),
                updated_at: 11,
            },
        ]
    );
    for expected in &chunks {
        assert_eq!(
            session
                .get_note_chunk("complete", expected.chunk_idx)
                .await
                .unwrap(),
            Some(expected.clone())
        );
    }
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
    let tied_updated_at = 34;
    let second = session
        .claim_pending_embedding_jobs(1, tied_updated_at)
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
    session
        .enqueue_embedding_job("stale", 2, "dashboard-tie", "dashboard tie", 2, 41)
        .await
        .unwrap();
    let tied_processing = session
        .claim_pending_embedding_jobs(1, tied_updated_at)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(second.id < tied_processing.id);
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
            .delete_embedding_job(tied_processing.id)
            .await
            .unwrap(),
        1
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
        category_labels: Vec::new(),
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: vec![
                    DuplicateCheckTerm {
                        key: "skill-name".into(),
                        value: None,
                    },
                    DuplicateCheckTerm {
                        key: "kind".into(),
                        value: Some("skill".into()),
                    },
                ],
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_status_never_mixes_cross_session_snapshots() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;
    use tokio::time::{timeout, Duration};

    let Some(admin_url) =
        configured_url_or_skip("dashboard_status_never_mixes_cross_session_snapshots")
    else {
        return;
    };
    let database = TestDatabase::create(&admin_url).await;
    database.provision_vector().await;
    let storage = PgStorage::connect(&database.url, 4)
        .await
        .expect("connect PostgreSQL storage");
    let reader = storage.connect_session().await.unwrap();
    reader
        .insert_note(NewNote {
            id: "dashboard-snapshot",
            title: "Dashboard snapshot",
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    reader
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "dashboard-snapshot",
            chunk_idx: 0,
            content_hash: "snapshot",
            content: "content",
            note_revision: 1,
            status: "embedded",
            updated_at: 1,
        })
        .await
        .unwrap();
    reader
        .enqueue_embedding_job("dashboard-snapshot", 0, "snapshot", "content", 1, 1)
        .await
        .unwrap();

    let writer_pool = database.inspect_pool().await;
    sqlx::query("UPDATE embedding_jobs SET status = 'failed' WHERE note_id = $1")
        .bind("dashboard-snapshot")
        .execute(&writer_pool)
        .await
        .unwrap();
    let initial = reader.embedding_dashboard_status().await.unwrap();
    assert_eq!(
        (initial.embedded_note_count, initial.processing_note),
        (1, None)
    );

    let start = Arc::new(Barrier::new(2));
    let stop = Arc::new(AtomicBool::new(false));
    let transitions = Arc::new(AtomicUsize::new(0));
    let writer = {
        let start = start.clone();
        let stop = stop.clone();
        let transitions = transitions.clone();
        tokio::spawn(async move {
            start.wait().await;
            let mut embedded = true;
            while !stop.load(Ordering::Acquire) {
                embedded = !embedded;
                let mut transaction = writer_pool.begin().await.unwrap();
                sqlx::query("UPDATE note_chunks SET status = $1 WHERE note_id = $2")
                    .bind(if embedded { "embedded" } else { "pending" })
                    .bind("dashboard-snapshot")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                sqlx::query(
                    "UPDATE embedding_jobs
                     SET status = $1, updated_at = updated_at + 1
                     WHERE note_id = $2",
                )
                .bind(if embedded { "failed" } else { "processing" })
                .bind("dashboard-snapshot")
                .execute(&mut *transaction)
                .await
                .unwrap();
                transaction.commit().await.unwrap();
                transitions.fetch_add(1, Ordering::Release);
                tokio::task::yield_now().await;
            }
            writer_pool.close().await;
        })
    };

    start.wait().await;
    let transitions_before_reads = transitions.load(Ordering::Acquire);
    let mut mixed_snapshot = None;
    let mut saw_embedded = false;
    let mut saw_processing = false;
    for _ in 0..2_000 {
        let status = reader.embedding_dashboard_status().await.unwrap();
        let processing_id = status.processing_note.map(|processing| processing.id);
        match (status.embedded_note_count, processing_id.as_deref()) {
            (1, None) => saw_embedded = true,
            (0, Some("dashboard-snapshot")) => saw_processing = true,
            _ if mixed_snapshot.is_none() => {
                mixed_snapshot = Some((status.embedded_note_count, processing_id));
            }
            _ => {}
        }
    }
    let transitions_after_reads = transitions.load(Ordering::Acquire);
    stop.store(true, Ordering::Release);
    timeout(Duration::from_secs(10), writer)
        .await
        .expect("writer stops after reader completes")
        .expect("writer task succeeds");
    assert!(
        transitions_after_reads > transitions_before_reads,
        "writer must commit state transitions during dashboard reads"
    );
    assert!(
        saw_embedded && saw_processing,
        "reader must observe both coherent dashboard states"
    );
    assert_eq!(
        mixed_snapshot, None,
        "dashboard combined different database snapshots"
    );

    drop(reader);
    database.cleanup(Some(&storage)).await.unwrap();
}
