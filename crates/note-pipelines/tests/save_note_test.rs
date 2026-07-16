use note_core::{LabelValueType, NoteAttachment};
use note_embedding::StubEmbedder;
use note_pipelines::{
    define_label_key, define_label_key_with_type, delete_note, drain_embedding_jobs, get_note,
    list_deleted_note_summaries, list_label_keys, purge_expired_deleted_notes, restore_notes,
    save_note, update_note, update_system_config, Context, SaveNoteInput, TRASH_RETENTION_SECONDS,
};
use note_storage::Storage;
use std::sync::Arc;
use tempfile::TempDir;

use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};

// Returns the TempDir guard alongside the Context so the caller keeps it alive:
// dropping it deletes the DB directory and later connect() calls fail with SQLITE_CANTOPEN.
async fn test_context() -> (Context, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let ctx = Context::with_attachment_dir(
        Arc::new(storage),
        Arc::new(StubEmbedder),
        dir.path().join("attachments"),
    );
    (ctx, dir)
}

#[tokio::test]
async fn saves_and_returns_a_persisted_note() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.title, "My note");
    assert_eq!(note.labels.len(), 1);
    assert_eq!(note.labels[0].key, "status");
    assert!(!note.id.is_empty());
}

// Auto-create: a label key that isn't in the catalog yet is created on save (docs/design.md §6),
// not rejected. The save succeeds and the key afterwards exists in the catalog.
#[tokio::test]
async fn unknown_label_key_is_auto_created() {
    let (ctx, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("project".into(), "alpha".into())],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.labels.len(), 1);
    assert_eq!(note.labels[0].key, "project");
    assert_eq!(note.labels[0].value, "alpha");

    // The previously-unknown key now exists in the catalog (auto-created, empty description).
    let keys = list_label_keys(&ctx).await.unwrap();
    assert!(keys.iter().any(|k| k.key == "project"));
}

#[tokio::test]
async fn typed_label_value_is_validated_on_save() {
    let (ctx, _dir) = test_context().await;
    define_label_key_with_type(&ctx, "priority", "Priority score", LabelValueType::Number)
        .await
        .unwrap();

    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("priority".into(), "high".into())],
        },
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn save_persists_attachments() {
    let (ctx, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![NoteAttachment {
                id: "meta".into(),
                path: "./meta.json".into(),
                mime: "application/json".into(),
                description: "metadata".into(),
                content: b"{}".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].path, "./meta.json");

    let conn = ctx.storage.connect().unwrap();
    let stored = note_storage::get_note(&conn, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.attachments[0].description, "metadata");
    assert!(stored.attachments[0].content.is_empty());

    let attachment_path = _dir
        .path()
        .join("attachments")
        .join(&note.id)
        .join("meta.json");
    assert_eq!(std::fs::read_to_string(attachment_path).unwrap(), "{}");

    let hydrated = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(hydrated.attachments[0].content, b"{}");
}

#[tokio::test]
async fn save_and_get_preserve_binary_attachment_bytes() {
    let (ctx, dir) = test_context().await;
    let bytes = vec![0, 159, 146, 150, 255];
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Binary attachment".into(),
            content: "Contains arbitrary bytes".into(),
            attachments: vec![NoteAttachment {
                id: "blob".into(),
                path: "blob.bin".into(),
                mime: "application/octet-stream".into(),
                description: String::new(),
                content: bytes.clone(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read(
            dir.path()
                .join("attachments")
                .join(&note.id)
                .join("blob.bin")
        )
        .unwrap(),
        bytes
    );
    let hydrated = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(hydrated.attachments[0].content, bytes);
}

#[tokio::test]
async fn delete_and_restore_preserve_note_data_and_requeue_embeddings() {
    let (ctx, dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Delete me".into(),
            content: "Searchable content".into(),
            attachments: vec![NoteAttachment {
                id: "meta".into(),
                path: "./meta.json".into(),
                mime: "application/json".into(),
                description: "metadata".into(),
                content: b"{}".to_vec(),
            }],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    assert!(delete_note(&ctx, &note.id).await.unwrap());
    assert!(!delete_note(&ctx, &note.id).await.unwrap());
    assert!(get_note(&ctx, &note.id).await.unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join("attachments")
                .join(&note.id)
                .join("meta.json")
        )
        .unwrap(),
        "{}"
    );

    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, note.id);
    assert_eq!(deleted[0].labels.len(), 1);

    let conn = ctx.storage.connect().unwrap();
    assert_eq!(count_rows(&conn, "note_labels", &note.id).await, 1);
    assert_eq!(count_rows(&conn, "note_chunks", &note.id).await, 0);
    assert_eq!(count_rows(&conn, "embedding_jobs", &note.id).await, 0);
    assert_eq!(
        count_rows(&conn, "note_chunk_embeddings", &note.id).await,
        0
    );
    assert_eq!(count_rows(&conn, "note_chunk_sparse", &note.id).await, 0);

    assert!(restore_notes(&ctx, &[note.id.clone(), note.id.clone()])
        .await
        .unwrap());
    let restored = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(restored.labels.len(), 1);
    assert_eq!(restored.attachments[0].content, b"{}");
    assert_eq!(
        note_storage::get_note_revision(&conn, &note.id)
            .await
            .unwrap(),
        Some(2)
    );
    assert!(list_deleted_note_summaries(&ctx).await.unwrap().is_empty());
    assert!(count_rows(&conn, "note_chunks", &note.id).await > 0);
    assert!(count_rows(&conn, "embedding_jobs", &note.id).await > 0);
}

#[tokio::test]
async fn restore_batch_rolls_back_when_any_note_is_not_in_trash() {
    let (ctx, _dir) = test_context().await;
    let deleted = save_note(
        &ctx,
        SaveNoteInput {
            title: "Deleted".into(),
            content: "Deleted content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let active = save_note(
        &ctx,
        SaveNoteInput {
            title: "Active".into(),
            content: "Active content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &deleted.id).await.unwrap();

    assert!(!restore_notes(&ctx, &[deleted.id.clone(), active.id])
        .await
        .unwrap());
    assert!(get_note(&ctx, &deleted.id).await.unwrap().is_none());
    let conn = ctx.storage.connect().unwrap();
    assert_eq!(count_rows(&conn, "note_chunks", &deleted.id).await, 0);
    assert_eq!(count_rows(&conn, "embedding_jobs", &deleted.id).await, 0);
}

#[tokio::test]
async fn purge_removes_notes_at_the_ninety_day_boundary_and_their_attachments() {
    let (ctx, dir) = test_context().await;
    let expired = save_note(
        &ctx,
        SaveNoteInput {
            title: "Expired".into(),
            content: "Content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                content: b"expired".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    let retained = save_note(
        &ctx,
        SaveNoteInput {
            title: "Retained".into(),
            content: "Content".into(),
            attachments: vec![NoteAttachment {
                id: "proof".into(),
                path: "./proof.txt".into(),
                mime: "text/plain".into(),
                description: String::new(),
                content: b"retained".to_vec(),
            }],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&ctx, &expired.id).await.unwrap();
    delete_note(&ctx, &retained.id).await.unwrap();

    let now = 1_800_000_000;
    let cutoff = now - TRASH_RETENTION_SECONDS;
    let conn = ctx.storage.connect().unwrap();
    conn.execute(
        "UPDATE notes SET deleted_at = ?2 WHERE id = ?1",
        libsql::params![expired.id.as_str(), cutoff],
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE notes SET deleted_at = ?2 WHERE id = ?1",
        libsql::params![retained.id.as_str(), cutoff + 1],
    )
    .await
    .unwrap();

    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 1);
    assert!(!note_storage::note_exists(&conn, &expired.id).await.unwrap());
    assert!(note_storage::note_exists(&conn, &retained.id)
        .await
        .unwrap());
    assert!(!dir.path().join("attachments").join(&expired.id).exists());
    assert!(dir.path().join("attachments").join(&retained.id).exists());

    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, retained.id);
    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 0);
}

#[tokio::test]
async fn duplicate_attachment_id_is_rejected() {
    let (ctx, _dir) = test_context().await;
    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![
                NoteAttachment {
                    id: "meta".into(),
                    path: "./meta.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: b"{}".to_vec(),
                },
                NoteAttachment {
                    id: "meta".into(),
                    path: "./other.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: b"{}".to_vec(),
                },
            ],
            labels: vec![],
        },
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn empty_title_is_rejected() {
    let (ctx, _dir) = test_context().await;
    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await;
    assert!(result.is_err());
}

fn duplicate_config(terms: &[(&str, Option<&str>)]) -> SystemConfig {
    SystemConfig {
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: terms
                    .iter()
                    .map(|(key, value)| DuplicateCheckTerm {
                        key: (*key).to_string(),
                        value: value.map(str::to_string),
                    })
                    .collect(),
            }],
        },
    }
}

#[tokio::test]
async fn duplicate_check_is_disabled_by_default() {
    let (ctx, _dir) = test_context().await;
    for title in ["First", "Second"] {
        save_note(
            &ctx,
            SaveNoteInput {
                title: title.into(),
                content: "Content".into(),
                attachments: vec![],
                labels: vec![
                    ("skill-name".into(), "zddi-hooks".into()),
                    ("version".into(), "1.0.0".into()),
                ],
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn duplicate_rule_rejects_create_with_the_same_composite_labels() {
    let (ctx, dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Existing".into(),
            content: "Existing content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
                ("channel".into(), "stable".into()),
            ],
        },
    )
    .await
    .unwrap();

    let error = save_note(
        &ctx,
        SaveNoteInput {
            title: "Duplicate".into(),
            content: "Duplicate content".into(),
            attachments: vec![NoteAttachment {
                id: "metadata".into(),
                path: "./metadata.json".into(),
                mime: "application/json".into(),
                description: String::new(),
                content: b"{}".to_vec(),
            }],
            labels: vec![
                ("version".into(), "1.0.0".into()),
                ("skill-name".into(), "zddi-hooks".into()),
            ],
        },
    )
    .await
    .unwrap_err();

    let duplicate = error.downcast_ref::<DuplicateNoteError>().unwrap();
    assert_eq!(
        duplicate.labels,
        vec![
            ("skill-name".to_string(), "zddi-hooks".to_string()),
            ("version".to_string(), "1.0.0".to_string()),
        ]
    );
    let conn = ctx.storage.connect().unwrap();
    assert_eq!(total_rows(&conn, "notes").await, 1);
    assert_eq!(total_rows(&conn, "embedding_jobs").await, 1);
    assert_eq!(
        std::fs::read_dir(dir.path().join("attachments"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn duplicate_rule_allows_missing_or_different_terms_and_scopes_fixed_values() {
    let (ctx, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[
            ("kind", Some("skill")),
            ("skill-name", None),
            ("version", None),
        ]),
    )
    .await
    .unwrap();

    for (title, labels) in [
        (
            "Existing",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        ),
        (
            "Different version",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "2.0.0".into()),
            ],
        ),
        (
            "Different scope",
            vec![
                ("kind".into(), "note".into()),
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        ),
        (
            "Missing version",
            vec![
                ("kind".into(), "skill".into()),
                ("skill-name".into(), "zddi-hooks".into()),
            ],
        ),
    ] {
        save_note(
            &ctx,
            SaveNoteInput {
                title: title.into(),
                content: "Content".into(),
                attachments: vec![],
                labels,
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_duplicate_creates_allow_only_one_note() {
    let (ctx, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    let input = || SaveNoteInput {
        title: "Concurrent".into(),
        content: "Content".into(),
        attachments: vec![],
        labels: vec![
            ("skill-name".into(), "zddi-hooks".into()),
            ("version".into(), "1.0.0".into()),
        ],
    };

    let (first, second) = tokio::join!(save_note(&ctx, input()), save_note(&ctx, input()));
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let error = first.err().or_else(|| second.err()).unwrap();
    assert!(error.downcast_ref::<DuplicateNoteError>().is_some());
    assert_eq!(
        total_rows(&ctx.storage.connect().unwrap(), "notes").await,
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_different_values_can_share_a_new_label_key() {
    let (ctx, _dir) = test_context().await;
    update_system_config(&ctx, &duplicate_config(&[("race-key", None)]))
        .await
        .unwrap();
    let input = |value: &str| SaveNoteInput {
        title: format!("Value {value}"),
        content: "Content".into(),
        attachments: vec![],
        labels: vec![("race-key".into(), value.into())],
    };

    let (first, second) = tokio::join!(save_note(&ctx, input("a")), save_note(&ctx, input("b")));
    first.unwrap();
    second.unwrap();
    assert_eq!(
        total_rows(&ctx.storage.connect().unwrap(), "notes").await,
        2
    );
}

#[tokio::test]
async fn duplicate_check_does_not_apply_to_updates() {
    let (ctx, _dir) = test_context().await;
    update_system_config(
        &ctx,
        &duplicate_config(&[("skill-name", None), ("version", None)]),
    )
    .await
    .unwrap();
    let first = save_note(
        &ctx,
        SaveNoteInput {
            title: "First".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        },
    )
    .await
    .unwrap();
    let second = save_note(
        &ctx,
        SaveNoteInput {
            title: "Second".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "other".into()),
                ("version".into(), "2.0.0".into()),
            ],
        },
    )
    .await
    .unwrap();

    let updated = update_note(
        &ctx,
        &second.id,
        SaveNoteInput {
            title: "Second".into(),
            content: "Content".into(),
            attachments: vec![],
            labels: vec![
                ("skill-name".into(), "zddi-hooks".into()),
                ("version".into(), "1.0.0".into()),
            ],
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.id, second.id);
    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn successful_save_persists_note_chunks_and_embedding_jobs() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    let conn = ctx.storage.connect().unwrap();

    // notes row + hydrated labels
    let stored = note_storage::get_note(&conn, &note.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.title, "My note");
    assert_eq!(stored.labels.len(), 1);
    assert_eq!(stored.labels[0].key, "status");

    assert!(count_rows(&conn, "note_chunks", &note.id).await >= 1);
    assert!(count_rows(&conn, "embedding_jobs", &note.id).await >= 1);
    assert_eq!(
        count_rows(&conn, "note_chunk_embeddings", &note.id).await,
        0
    );
    assert_eq!(count_rows(&conn, "note_chunk_sparse", &note.id).await, 0);
}

#[tokio::test]
async fn embedding_worker_populates_recall_tables_from_queue() {
    let (ctx, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    assert_eq!(drain_embedding_jobs(&ctx, 10).await.unwrap(), 1);

    let conn = ctx.storage.connect().unwrap();
    assert!(count_rows(&conn, "note_chunk_embeddings", &note.id).await >= 1);
    assert!(count_rows(&conn, "note_chunk_sparse", &note.id).await >= 1);
    assert_eq!(count_rows(&conn, "embedding_jobs", &note.id).await, 0);
}

#[tokio::test]
async fn update_only_queues_embedding_when_content_hash_changes() {
    let (ctx, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Stable content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    update_note(
        &ctx,
        &note.id,
        SaveNoteInput {
            title: "Renamed".into(),
            content: "Stable content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    let conn = ctx.storage.connect().unwrap();
    assert_eq!(count_rows(&conn, "embedding_jobs", &note.id).await, 0);
    assert!(count_rows(&conn, "note_chunk_embeddings", &note.id).await >= 1);

    update_note(
        &ctx,
        &note.id,
        SaveNoteInput {
            title: "Renamed".into(),
            content: "Changed content".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(count_rows(&conn, "embedding_jobs", &note.id).await, 1);
    assert_eq!(
        count_rows(&conn, "note_chunk_embeddings", &note.id).await,
        0
    );
}

// Atomicity (docs/design.md §3): a partial write is a correctness bug, not a soft failure.
// Injection point: two labels sharing the same key both pass validation (the key is known),
// but note_labels has PRIMARY KEY (note_id, label_key_id), so the second attach_label INSERT
// fails *after* the note/embedding/sparse rows have already been written inside the same
// transaction. If the write is truly atomic, that failure rolls everything back and leaves
// zero rows across all three core tables — no orphaned embedding or sparse weights.
#[tokio::test]
async fn failed_write_rolls_back_all_tables() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();

    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            attachments: vec![],
            labels: vec![
                ("status".into(), "done".into()),
                ("status".into(), "wip".into()), // duplicate key -> PK violation mid-transaction
            ],
        },
    )
    .await;
    assert!(result.is_err(), "duplicate label key must fail the save");

    let conn = ctx.storage.connect().unwrap();
    // Nothing partial persisted: no note row, and no orphan embedding/sparse rows.
    assert_eq!(total_rows(&conn, "notes").await, 0);
    assert_eq!(total_rows(&conn, "note_chunks").await, 0);
    assert_eq!(total_rows(&conn, "embedding_jobs").await, 0);
    assert_eq!(total_rows(&conn, "note_chunk_embeddings").await, 0);
    assert_eq!(total_rows(&conn, "note_chunk_sparse").await, 0);
    assert_eq!(total_rows(&conn, "note_labels").await, 0);
}

async fn count_rows(conn: &libsql::Connection, table: &str, note_id: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE note_id = ?1");
    let mut rows = conn.query(&sql, libsql::params![note_id]).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn total_rows(conn: &libsql::Connection, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut rows = conn.query(&sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}
