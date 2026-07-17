mod support;

use note_core::{LabelValueType, NoteAttachment};
use note_pipelines::{
    define_label_key, define_label_key_with_type, delete_note, drain_embedding_jobs, get_note,
    list_deleted_note_summaries, list_label_keys, purge_expired_deleted_notes, restore_notes,
    save_note, update_note, update_system_config, SaveNoteInput, TRASH_RETENTION_SECONDS,
};
use note_storage::TransactionMode;
use support::test_context;

use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, DuplicateNoteError, SystemConfig,
};

#[tokio::test]
async fn saves_and_returns_a_persisted_note() {
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, backend, _dir) = test_context().await;
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

    let session = backend.session().await.unwrap();
    let stored = session.get_note(&note.id).await.unwrap().unwrap();
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
    let (ctx, _backend, dir) = test_context().await;
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
    let (ctx, backend, dir) = test_context().await;
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

    let session = backend.session().await.unwrap();
    assert_eq!(session.labels_for_note(&note.id).await.unwrap().len(), 1);
    assert!(session.list_note_chunks(&note.id).await.unwrap().is_empty());
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());

    assert!(restore_notes(&ctx, &[note.id.clone(), note.id.clone()])
        .await
        .unwrap());
    let restored = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(restored.labels.len(), 1);
    assert_eq!(restored.attachments[0].content, b"{}");
    let observer = backend.session().await.unwrap();
    assert_eq!(observer.get_note_revision(&note.id).await.unwrap(), Some(2));
    assert!(list_deleted_note_summaries(&ctx).await.unwrap().is_empty());
    assert!(!observer
        .list_note_chunks(&note.id)
        .await
        .unwrap()
        .is_empty());
    assert!(!observer
        .claim_pending_embedding_jobs(10, 2)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn restore_batch_rolls_back_when_any_note_is_not_in_trash() {
    let (ctx, backend, _dir) = test_context().await;
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
    let session = backend.session().await.unwrap();
    assert!(session
        .list_note_chunks(&deleted.id)
        .await
        .unwrap()
        .is_empty());
    let pending = session.claim_pending_embedding_jobs(10, 1).await.unwrap();
    assert!(pending.iter().all(|job| job.note_id != deleted.id));
}

#[tokio::test]
async fn purge_removes_notes_at_the_ninety_day_boundary_and_their_attachments() {
    let (ctx, backend, dir) = test_context().await;
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
    let now = 1_800_000_000;
    let cutoff = now - TRASH_RETENTION_SECONDS;
    let unit = backend.begin(TransactionMode::Immediate).await.unwrap();
    assert_eq!(unit.soft_delete_note(&expired.id, cutoff).await.unwrap(), 1);
    assert_eq!(
        unit.soft_delete_note(&retained.id, cutoff + 1)
            .await
            .unwrap(),
        1
    );
    unit.commit().await.unwrap();

    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 1);
    let observer = backend.session().await.unwrap();
    assert!(!observer.note_exists(&expired.id).await.unwrap());
    assert!(observer.note_exists(&retained.id).await.unwrap());
    assert!(!dir.path().join("attachments").join(&expired.id).exists());
    assert!(dir.path().join("attachments").join(&retained.id).exists());

    let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, retained.id);
    assert_eq!(purge_expired_deleted_notes(&ctx, now).await.unwrap(), 0);
}

#[tokio::test]
async fn duplicate_attachment_id_is_rejected() {
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, backend, dir) = test_context().await;
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
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 1);
    assert_eq!(
        session
            .claim_pending_embedding_jobs(10, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("attachments"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn duplicate_rule_allows_missing_or_different_terms_and_scopes_fixed_values() {
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, backend, _dir) = test_context().await;
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
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_different_values_can_share_a_new_label_key() {
    let (ctx, backend, _dir) = test_context().await;
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
    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 2);
}

#[tokio::test]
async fn duplicate_check_does_not_apply_to_updates() {
    let (ctx, _backend, _dir) = test_context().await;
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
    let (ctx, backend, _dir) = test_context().await;
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

    let session = backend.session().await.unwrap();
    let stored = session.get_note(&note.id).await.unwrap().unwrap();
    assert_eq!(stored.title, "My note");
    assert_eq!(stored.labels.len(), 1);
    assert_eq!(stored.labels[0].key, "status");

    assert!(!session.list_note_chunks(&note.id).await.unwrap().is_empty());
    assert!(!session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());
}

#[tokio::test]
async fn embedding_worker_populates_recall_tables_from_queue() {
    let (ctx, backend, _dir) = test_context().await;
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

    let (dense, sparse) = ctx.embedder.embed("Some content").await.unwrap();
    let token_ids = sparse.keys().copied().collect::<Vec<_>>();
    let session = backend.session().await.unwrap();
    assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());
    assert!(session
        .dense_search(&dense, 10)
        .await
        .unwrap()
        .contains(&note.id));
    assert!(session
        .sparse_postings_query(&token_ids, 10)
        .await
        .unwrap()
        .contains(&note.id));
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn update_only_queues_embedding_when_content_hash_changes() {
    let (ctx, backend, _dir) = test_context().await;
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

    let session = backend.session().await.unwrap();
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
    assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());

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

    assert_eq!(
        session
            .claim_pending_embedding_jobs(10, 2)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!session.chunk_embedding_exists(&note.id, 0).await.unwrap());
}

#[tokio::test]
async fn failed_write_rolls_back_the_note_and_embedding_job() {
    let (ctx, backend, _dir) = test_context().await;
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

    let session = backend.session().await.unwrap();
    assert_eq!(session.count_notes(&[]).await.unwrap(), 0);
    assert!(session
        .claim_pending_embedding_jobs(10, 1)
        .await
        .unwrap()
        .is_empty());
}
