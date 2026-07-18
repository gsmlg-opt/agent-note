use note_core::{parse_label_selectors, LabelValueType, NoteAttachment};
use note_storage::{ActiveNoteSource, NewNote, NoteUpdate, StorageBackend, StorageErrorKind};
use std::sync::Arc;

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    let attachment = NoteAttachment {
        id: "meta".into(),
        path: "meta.json".into(),
        mime: "application/json".into(),
        description: "metadata".into(),
        content: b"not stored".to_vec(),
    };
    session
        .insert_note(NewNote {
            id: "contract-notes-active",
            title: "Contract title",
            content: "Contract body",
            attachments: std::slice::from_ref(&attachment),
            created_at: 100,
            updated_at: 100,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();

    let note = session
        .get_note("contract-notes-active")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(note.title, "Contract title");
    assert_eq!(note.content, "Contract body");
    assert_eq!(note.created_at, 100);
    assert_eq!(note.updated_at, 100);
    assert_eq!(note.deleted_at, None);
    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].id, "meta");
    assert_eq!(note.attachments[0].path, "meta.json");
    assert_eq!(note.attachments[0].mime, "application/json");
    assert_eq!(note.attachments[0].description, "metadata");
    assert!(note.attachments[0].content.is_empty());
    assert_eq!(
        session
            .get_note_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(1)
    );
    assert!(session.note_exists("contract-notes-active").await.unwrap());

    let updated_attachment = NoteAttachment {
        id: "updated".into(),
        path: "updated.txt".into(),
        mime: "text/plain".into(),
        description: "updated metadata".into(),
        content: b"not stored either".to_vec(),
    };
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "contract-notes-active",
                title: "Updated contract",
                content: "Updated body",
                attachments: std::slice::from_ref(&updated_attachment),
                updated_at: 110,
                note_revision: 2,
            })
            .await
            .unwrap(),
        1
    );
    let note = session
        .get_note("contract-notes-active")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(note.title, "Updated contract");
    assert_eq!(note.content, "Updated body");
    assert_eq!(note.created_at, 100);
    assert_eq!(note.updated_at, 110);
    assert_eq!(note.attachments.len(), 1);
    assert_eq!(note.attachments[0].id, "updated");
    assert_eq!(note.attachments[0].path, "updated.txt");
    assert_eq!(note.attachments[0].mime, "text/plain");
    assert_eq!(note.attachments[0].description, "updated metadata");
    assert!(note.attachments[0].content.is_empty());
    assert_eq!(
        session
            .get_note_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(2)
    );

    session
        .insert_label_key_with_type(
            "contract-notes-priority",
            "Priority",
            LabelValueType::Number,
        )
        .await
        .unwrap();
    session
        .insert_label_key_with_type("contract-notes-status", "Status", LabelValueType::Text)
        .await
        .unwrap();
    session
        .insert_label_key("contract-notes-active-only", "Active-only marker")
        .await
        .unwrap();
    session
        .update_label_key("contract-notes-priority", "Updated priority")
        .await
        .unwrap();
    session
        .insert_label_key_if_missing("contract-notes-priority", "Must not replace")
        .await
        .unwrap();
    let keys = session.list_label_keys().await.unwrap();
    let priority = keys
        .iter()
        .find(|key| key.key == "contract-notes-priority")
        .unwrap();
    assert_eq!(priority.description, "Updated priority");
    assert_eq!(priority.value_type, LabelValueType::Number);
    let status = keys
        .iter()
        .find(|key| key.key == "contract-notes-status")
        .unwrap();
    assert_eq!(status.value_type, LabelValueType::Text);
    session
        .update_label_key_with_type(
            "contract-notes-status",
            "Versioned status",
            LabelValueType::Version,
        )
        .await
        .unwrap();
    assert_eq!(
        session
            .list_label_keys()
            .await
            .unwrap()
            .into_iter()
            .find(|key| key.key == "contract-notes-status")
            .unwrap()
            .value_type,
        LabelValueType::Version
    );
    session
        .update_label_key_with_type("contract-notes-status", "Status", LabelValueType::Text)
        .await
        .unwrap();

    let duplicate_key = session
        .insert_label_key_with_type("contract-notes-priority", "Duplicate", LabelValueType::Text)
        .await
        .unwrap_err();
    assert_eq!(duplicate_key.kind(), StorageErrorKind::Constraint);

    session
        .attach_label("contract-notes-active", "contract-notes-priority", "2")
        .await
        .unwrap();
    session
        .attach_label("contract-notes-active", "contract-notes-status", "ready")
        .await
        .unwrap();
    session
        .attach_label("contract-notes-active", "contract-notes-active-only", "yes")
        .await
        .unwrap();
    let labels = session
        .labels_for_note("contract-notes-active")
        .await
        .unwrap();
    assert_eq!(labels.len(), 3);
    assert!(labels.iter().any(|label| {
        label.key == "contract-notes-priority"
            && label.value == "2"
            && label.value_type == LabelValueType::Number
    }));
    let duplicate_label = session
        .attach_label("contract-notes-active", "contract-notes-status", "other")
        .await
        .unwrap_err();
    assert_eq!(duplicate_label.kind(), StorageErrorKind::Constraint);

    let expected = Some("contract-notes-active".to_string());
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("contract-notes-status".into(), "ready".into()),
                ("contract-notes-priority".into(), "2".into()),
            ])
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("contract-notes-priority".into(), "2".into()),
                ("contract-notes-status".into(), "ready".into()),
            ])
            .await
            .unwrap(),
        Some("contract-notes-active".to_string())
    );
    assert_eq!(
        session
            .find_note_with_labels(&[
                ("contract-notes-priority".into(), "2".into()),
                ("contract-notes-status".into(), "missing".into()),
            ])
            .await
            .unwrap(),
        None
    );

    for (id, created_at, priority, status) in [
        ("contract-notes-numeric-low", 400, "1", "ready"),
        ("contract-notes-numeric-ten", 350, "10", "ready"),
        ("contract-notes-newer", 300, "5", "ready"),
        ("contract-notes-middle", 200, "3", "ready"),
        ("contract-notes-other", 150, "9", "blocked"),
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
            .attach_label(id, "contract-notes-priority", priority)
            .await
            .unwrap();
        session
            .attach_label(id, "contract-notes-status", status)
            .await
            .unwrap();
    }

    let selectors = parse_label_selectors("contract-notes-priority>=2&contract-notes-status=ready");
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 4);
    assert!(session
        .list_notes(&selectors, Some(0), Some(0))
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .list_note_summaries(&selectors, Some(0), Some(0))
        .await
        .unwrap()
        .is_empty());
    let selected = session
        .list_notes(&selectors, Some(2), Some(1))
        .await
        .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec!["contract-notes-newer", "contract-notes-middle"]
    );
    let summaries = session
        .list_note_summaries(&selectors, Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "contract-notes-newer");
    let unbounded = session
        .list_notes(&selectors, Some(-1), Some(-10))
        .await
        .unwrap();
    assert_eq!(
        unbounded
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "contract-notes-numeric-ten",
            "contract-notes-newer",
            "contract-notes-middle",
            "contract-notes-active"
        ]
    );
    let unbounded_summaries = session
        .list_note_summaries(&selectors, Some(-1), Some(-10))
        .await
        .unwrap();
    assert_eq!(
        unbounded_summaries
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "contract-notes-numeric-ten",
            "contract-notes-newer",
            "contract-notes-middle",
            "contract-notes-active"
        ]
    );

    for id in [
        "contract-notes-tie-c",
        "contract-notes-tie-a",
        "contract-notes-tie-b",
    ] {
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
    let first_page = session.list_notes(&[], Some(2), Some(0)).await.unwrap();
    let second_page = session.list_notes(&[], Some(1), Some(2)).await.unwrap();
    assert_eq!(
        first_page
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec!["contract-notes-tie-a", "contract-notes-tie-b"]
    );
    assert_eq!(second_page[0].id, "contract-notes-tie-c");
    assert!(first_page.iter().all(|first| first.id != second_page[0].id));
    assert_eq!(
        session
            .list_note_summaries(&[], Some(3), Some(0))
            .await
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec![
            "contract-notes-tie-a",
            "contract-notes-tie-b",
            "contract-notes-tie-c",
        ]
    );
    assert_eq!(
        session
            .list_all_notes()
            .await
            .unwrap()
            .into_iter()
            .take(3)
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        vec![
            "contract-notes-tie-a",
            "contract-notes-tie-b",
            "contract-notes-tie-c",
        ]
    );
    for id in [
        "contract-notes-tie-a",
        "contract-notes-tie-b",
        "contract-notes-tie-c",
    ] {
        session.soft_delete_note(id, 450).await.unwrap();
        session.permanently_delete_note(id).await.unwrap();
    }

    assert_eq!(
        session
            .soft_delete_note("contract-notes-active", 500)
            .await
            .unwrap(),
        1
    );
    assert!(session
        .get_note("contract-notes-active")
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 3);
    assert!(session
        .list_notes(&selectors, None, None)
        .await
        .unwrap()
        .iter()
        .all(|note| note.id != "contract-notes-active"));
    assert_eq!(
        session
            .find_note_with_labels(&[("contract-notes-active-only".into(), "yes".into())])
            .await
            .unwrap(),
        None
    );
    let counts = session.label_note_counts().await.unwrap();
    assert!(counts
        .iter()
        .any(|(key, count)| { key == "contract-notes-priority" && *count == 5 }));
    assert!(counts
        .iter()
        .any(|(key, count)| { key == "contract-notes-active-only" && *count == 0 }));
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(("Updated body".to_string(), 2))
    );
    let deleted = session.list_deleted_note_summaries().await.unwrap();
    let deleted = deleted
        .iter()
        .find(|note| note.id == "contract-notes-active")
        .unwrap();
    assert_eq!(deleted.deleted_at, Some(500));
    assert_eq!(
        session
            .update_note(NoteUpdate {
                id: "contract-notes-active",
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
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(("Updated body".to_string(), 2))
    );
    assert_eq!(
        session
            .restore_note("contract-notes-active", 3)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .get_note_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(3)
    );
    assert!(session
        .get_note("contract-notes-active")
        .await
        .unwrap()
        .is_some());

    for (id, deleted_at) in [
        ("contract-notes-expired-b", 700),
        ("contract-notes-expired-a", 700),
        ("contract-notes-expired-old", 699),
        ("contract-notes-expired-new", 701),
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
        vec![
            "contract-notes-expired-old",
            "contract-notes-expired-a",
            "contract-notes-expired-b",
        ]
    );

    assert_eq!(
        session
            .permanently_delete_note("contract-notes-newer")
            .await
            .unwrap(),
        0
    );
    session
        .soft_delete_note("contract-notes-other", 800)
        .await
        .unwrap();
    assert_eq!(
        session
            .permanently_delete_note("contract-notes-other")
            .await
            .unwrap(),
        1
    );
    assert!(session
        .labels_for_note("contract-notes-other")
        .await
        .unwrap()
        .is_empty());

    let module_sources = session
        .list_active_note_sources()
        .await
        .unwrap()
        .into_iter()
        .filter(|source| source.id.starts_with("contract-notes-"))
        .collect::<Vec<_>>();
    assert_eq!(
        module_sources,
        vec![
            ActiveNoteSource {
                id: "contract-notes-active".into(),
                content: "Updated body".into(),
                note_revision: 3,
            },
            ActiveNoteSource {
                id: "contract-notes-middle".into(),
                content: "contract-notes-middle".into(),
                note_revision: 1,
            },
            ActiveNoteSource {
                id: "contract-notes-newer".into(),
                content: "contract-notes-newer".into(),
                note_revision: 1,
            },
            ActiveNoteSource {
                id: "contract-notes-numeric-low".into(),
                content: "contract-notes-numeric-low".into(),
                note_revision: 1,
            },
            ActiveNoteSource {
                id: "contract-notes-numeric-ten".into(),
                content: "contract-notes-numeric-ten".into(),
                note_revision: 1,
            },
        ]
    );
}
