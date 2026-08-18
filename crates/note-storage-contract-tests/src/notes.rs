use note_core::{parse_label_selectors, LabelValueType, NoteAttachment};
use note_storage::{
    ActiveNoteSource, AttachmentMetadataUpdate, LabelValueCount, NewNote, NoteFieldsUpdate,
    NoteMutationResult, NoteUpdate, StorageBackend, StorageErrorKind,
};
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
    assert_eq!(note.revision, 1);
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
                expected_revision: 1,
            })
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
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
        .insert_note(NewNote {
            id: "contract-notes-partial",
            title: "Partial title",
            content: "Partial body",
            attachments: std::slice::from_ref(&attachment),
            created_at: 100,
            updated_at: 100,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-partial",
                title: "Partial title updated",
                content: "Partial body updated",
                updated_at: 120,
                expected_revision: 1,
            })
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
    let partial = session
        .get_note("contract-notes-partial")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(partial.title, "Partial title updated");
    assert_eq!(partial.content, "Partial body updated");
    assert_eq!(partial.updated_at, 120);
    assert_eq!(partial.attachments.len(), 1);
    assert_eq!(partial.attachments[0].id, "meta");
    assert_eq!(partial.attachments[0].path, "meta.json");
    assert_eq!(partial.attachments[0].mime, "application/json");
    assert_eq!(partial.attachments[0].description, "metadata");
    assert!(partial.attachments[0].content.is_empty());
    assert_eq!(
        session
            .get_note_revision("contract-notes-partial")
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-missing",
                title: "Missing",
                content: "Missing",
                updated_at: 120,
                expected_revision: 1,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    session
        .insert_note(NewNote {
            id: "contract-notes-partial-deleted",
            title: "Deleted title",
            content: "Deleted body",
            attachments: std::slice::from_ref(&attachment),
            created_at: 100,
            updated_at: 100,
            note_revision: 1,
            deleted_at: Some(10_000),
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-partial-deleted",
                title: "Must not update",
                content: "Must not update",
                updated_at: 120,
                expected_revision: 1,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );

    let partial_attachment = NoteAttachment {
        id: "partial-updated".into(),
        path: "partial-updated.txt".into(),
        mime: "text/plain".into(),
        description: "partial updated metadata".into(),
        content: b"must not be stored".to_vec(),
    };
    assert_eq!(
        session
            .update_note_attachments(AttachmentMetadataUpdate {
                id: "contract-notes-partial",
                expected_revision: 2,
                attachments: std::slice::from_ref(&partial_attachment),
                updated_at: 130,
            })
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 3
        }
    );
    let partial = session
        .get_note("contract-notes-partial")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(partial.title, "Partial title updated");
    assert_eq!(partial.content, "Partial body updated");
    assert_eq!(partial.created_at, 100);
    assert_eq!(partial.updated_at, 130);
    assert_eq!(partial.attachments.len(), 1);
    assert_eq!(partial.attachments[0].id, "partial-updated");
    assert_eq!(partial.attachments[0].path, "partial-updated.txt");
    assert_eq!(partial.attachments[0].mime, "text/plain");
    assert_eq!(
        partial.attachments[0].description,
        "partial updated metadata"
    );
    assert!(partial.attachments[0].content.is_empty());
    assert_eq!(
        session
            .get_note_revision("contract-notes-partial")
            .await
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        session
            .update_note_attachments(AttachmentMetadataUpdate {
                id: "contract-notes-missing",
                expected_revision: 1,
                attachments: &[],
                updated_at: 130,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    assert_eq!(
        session
            .update_note_attachments(AttachmentMetadataUpdate {
                id: "contract-notes-partial-deleted",
                expected_revision: 1,
                attachments: &[],
                updated_at: 130,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
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

    let selectors = parse_label_selectors(
        "contract-notes-priority>=2\
         &contract-notes-priority<=10\
         &contract-notes-status=ready",
    );
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
    assert_eq!(
        session
            .count_notes(&parse_label_selectors("contract-notes-status"))
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        session
            .count_notes(&parse_label_selectors("contract-notes-status!=blocked"))
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        session.matching_note_ids(&selectors).await.unwrap(),
        vec![
            "contract-notes-active",
            "contract-notes-middle",
            "contract-notes-newer",
            "contract-notes-numeric-ten",
        ]
    );
    let missing = parse_label_selectors("contract-notes-missing=value");
    assert!(session
        .matching_note_ids(&missing)
        .await
        .unwrap()
        .is_empty());
    let invalid = parse_label_selectors("contract-notes-status~=[");
    assert!(session
        .matching_note_ids(&invalid)
        .await
        .unwrap()
        .is_empty());

    for selector in [
        "contract-notes-status^=RE",
        "contract-notes-status$=DY",
        "contract-notes-status~=^R.*Y$",
    ] {
        let string_match_selectors = parse_label_selectors(selector);
        assert_eq!(
            session.count_notes(&string_match_selectors).await.unwrap(),
            5
        );
        assert_eq!(
            session
                .list_notes(&string_match_selectors, Some(-1), Some(0))
                .await
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            session
                .list_note_summaries(&string_match_selectors, Some(-1), Some(0))
                .await
                .unwrap()
                .len(),
            5
        );
    }
    assert_eq!(
        session
            .count_notes(&parse_label_selectors("contract-notes-status^="))
            .await
            .unwrap(),
        6
    );

    let numeric_prefix_selectors = parse_label_selectors("contract-notes-priority^=1");
    assert_eq!(
        session
            .count_notes(&numeric_prefix_selectors)
            .await
            .unwrap(),
        2
    );

    let invalid_regex_selectors = parse_label_selectors("contract-notes-status~=[");
    assert_eq!(
        session.count_notes(&invalid_regex_selectors).await.unwrap(),
        0
    );

    for (key, value_type) in [
        ("contract-filter-text", LabelValueType::Text),
        ("contract-filter-number", LabelValueType::Number),
        ("contract-filter-version", LabelValueType::Version),
        ("contract-filter-date", LabelValueType::Date),
        ("contract-filter-datetime", LabelValueType::DateTime),
        ("contract-filter-time", LabelValueType::Time),
    ] {
        session
            .insert_label_key_with_type(key, key, value_type)
            .await
            .unwrap();
    }
    session
        .insert_note(NewNote {
            id: "contract-filter-all-types",
            title: "All typed filters",
            content: "All typed filters",
            attachments: &[],
            created_at: 50,
            updated_at: 50,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    for (key, value) in [
        ("contract-filter-text", "alpha"),
        ("contract-filter-number", "2.5"),
        ("contract-filter-version", "1.10.0"),
        ("contract-filter-date", "2026-07-27"),
        ("contract-filter-datetime", "2026-07-27T12:30:00"),
        ("contract-filter-time", "12:30:00"),
    ] {
        session
            .attach_label("contract-filter-all-types", key, value)
            .await
            .unwrap();
    }
    session
        .insert_note(NewNote {
            id: "contract-filter-nonmatching",
            title: "Nonmatching typed filters",
            content: "Nonmatching typed filters",
            attachments: &[],
            created_at: 40,
            updated_at: 40,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    for (key, value) in [
        ("contract-filter-text", "beta"),
        ("contract-filter-number", "2.0"),
        ("contract-filter-version", "1.2.0"),
        ("contract-filter-date", "2026-07-26"),
        ("contract-filter-datetime", "2026-07-27T14:00:00"),
        ("contract-filter-time", "12:00:00"),
    ] {
        session
            .attach_label("contract-filter-nonmatching", key, value)
            .await
            .unwrap();
    }
    for selector in [
        "contract-filter-text=alpha",
        "contract-filter-number>=2.5",
        "contract-filter-version>=1.10.0",
        "contract-filter-date=2026-07-27",
        "contract-filter-datetime<2026-07-27T13:00:00",
        "contract-filter-time>=12:30",
    ] {
        assert_eq!(
            session
                .matching_note_ids(&parse_label_selectors(selector))
                .await
                .unwrap(),
            vec!["contract-filter-all-types"],
            "selector: {selector}"
        );
    }
    let all_types = parse_label_selectors(
        "contract-filter-text=alpha\
         &contract-filter-number>=2.5\
         &contract-filter-version>=1.10.0\
         &contract-filter-date=2026-07-27\
         &contract-filter-datetime<2026-07-27T13:00:00\
         &contract-filter-time>=12:30",
    );
    assert_eq!(
        session.matching_note_ids(&all_types).await.unwrap(),
        vec!["contract-filter-all-types"]
    );
    assert_eq!(session.count_notes(&all_types).await.unwrap(), 1);
    assert_eq!(
        session
            .list_note_summaries(&all_types, Some(1), Some(0))
            .await
            .unwrap()[0]
            .id,
        "contract-filter-all-types"
    );
    assert_eq!(
        session.matching_note_ids(&[]).await.unwrap(),
        vec![
            "contract-filter-all-types",
            "contract-filter-nonmatching",
            "contract-notes-active",
            "contract-notes-middle",
            "contract-notes-newer",
            "contract-notes-numeric-low",
            "contract-notes-numeric-ten",
            "contract-notes-other",
            "contract-notes-partial",
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
        session.soft_delete_note(id, 1, 450).await.unwrap();
        session.permanently_delete_note(id, 2).await.unwrap();
    }

    assert_eq!(
        session
            .soft_delete_note("contract-notes-active", 2, 500)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 3
        }
    );
    assert!(session
        .get_note("contract-notes-active")
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 3);
    assert_eq!(
        session.matching_note_ids(&selectors).await.unwrap(),
        vec![
            "contract-notes-middle",
            "contract-notes-newer",
            "contract-notes-numeric-ten",
        ]
    );
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
            .label_value_counts(&["contract-notes-status".to_string()])
            .await
            .unwrap(),
        vec![
            LabelValueCount {
                key: "contract-notes-status".into(),
                value: "ready".into(),
                count: 4,
            },
            LabelValueCount {
                key: "contract-notes-status".into(),
                value: "blocked".into(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        session
            .label_value_counts(&["contract-notes-priority".to_string()])
            .await
            .unwrap(),
        ["1", "10", "3", "5", "9"]
            .into_iter()
            .map(|value| LabelValueCount {
                key: "contract-notes-priority".into(),
                value: value.into(),
                count: 1,
            })
            .collect::<Vec<_>>()
    );
    assert!(session.label_value_counts(&[]).await.unwrap().is_empty());
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(("Updated body".to_string(), 3))
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
                expected_revision: 3,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(("Updated body".to_string(), 3))
    );
    assert_eq!(
        session
            .restore_note("contract-notes-active", 3)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 4
        }
    );
    assert_eq!(
        session
            .get_note_revision("contract-notes-active")
            .await
            .unwrap(),
        Some(4)
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
            .permanently_delete_note("contract-notes-newer", 1)
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    session
        .soft_delete_note("contract-notes-other", 1, 800)
        .await
        .unwrap();
    assert_eq!(
        session
            .permanently_delete_note("contract-notes-other", 2)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
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
                note_revision: 4,
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
            ActiveNoteSource {
                id: "contract-notes-partial".into(),
                content: "Partial body updated".into(),
                note_revision: 3,
            },
        ]
    );

    session
        .insert_note(NewNote {
            id: "contract-notes-cas-update",
            title: "Original",
            content: "Original content",
            attachments: &[],
            created_at: 2_000,
            updated_at: 2_000,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-cas-update",
                expected_revision: 1,
                title: "Winner",
                content: "Winner content",
                updated_at: 2_001,
            })
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2,
        }
    );
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-cas-update",
                expected_revision: 1,
                title: "Stale loser",
                content: "Stale content",
                updated_at: 2_002,
            })
            .await
            .unwrap(),
        NoteMutationResult::Conflict {
            expected_revision: 1,
            current_revision: 2,
        }
    );
    let cas_note = session
        .get_note("contract-notes-cas-update")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cas_note.title, "Winner");
    assert_eq!(cas_note.content, "Winner content");
    assert_eq!(cas_note.revision, 2);

    session
        .insert_note(NewNote {
            id: "contract-notes-cas-attachment",
            title: "Attachment CAS",
            content: "Body",
            attachments: std::slice::from_ref(&attachment),
            created_at: 2_010,
            updated_at: 2_010,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .update_note_attachments(AttachmentMetadataUpdate {
                id: "contract-notes-cas-attachment",
                expected_revision: 0,
                attachments: std::slice::from_ref(&partial_attachment),
                updated_at: 2_011,
            })
            .await
            .unwrap(),
        NoteMutationResult::Conflict {
            expected_revision: 0,
            current_revision: 1,
        }
    );
    let attachment_note = session
        .get_note("contract-notes-cas-attachment")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attachment_note.attachments[0].id, "meta");
    assert_eq!(attachment_note.updated_at, 2_010);
    assert_eq!(attachment_note.revision, 1);

    session
        .insert_note(NewNote {
            id: "contract-notes-cas-lifecycle",
            title: "Lifecycle CAS",
            content: "Body",
            attachments: &[],
            created_at: 2_020,
            updated_at: 2_020,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        session
            .soft_delete_note("contract-notes-cas-lifecycle", 1, 2_021)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2,
        }
    );
    let deleted_summary = session
        .list_deleted_note_summaries()
        .await
        .unwrap()
        .into_iter()
        .find(|note| note.id == "contract-notes-cas-lifecycle")
        .unwrap();
    assert_eq!(deleted_summary.revision, 2);
    assert_eq!(
        session
            .restore_note("contract-notes-cas-lifecycle", 1)
            .await
            .unwrap(),
        NoteMutationResult::Conflict {
            expected_revision: 1,
            current_revision: 2,
        }
    );
    assert!(session
        .get_note("contract-notes-cas-lifecycle")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session
            .permanently_delete_note("contract-notes-cas-lifecycle", 1)
            .await
            .unwrap(),
        NoteMutationResult::Conflict {
            expected_revision: 1,
            current_revision: 2,
        }
    );
    assert!(session
        .note_exists("contract-notes-cas-lifecycle")
        .await
        .unwrap());
    assert_eq!(
        session
            .update_note_fields(NoteFieldsUpdate {
                id: "contract-notes-cas-missing",
                expected_revision: 1,
                title: "Missing",
                content: "Missing",
                updated_at: 2_030,
            })
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );

    session
        .insert_note(NewNote {
            id: "contract-notes-cas-race",
            title: "Before race",
            content: "Before race",
            attachments: &[],
            created_at: 2_040,
            updated_at: 2_040,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    let first = storage.session().await.unwrap();
    let second = storage.session().await.unwrap();
    let (first_result, second_result) = tokio::join!(
        first.update_note_fields(NoteFieldsUpdate {
            id: "contract-notes-cas-race",
            expected_revision: 1,
            title: "First winner candidate",
            content: "First content",
            updated_at: 2_041,
        }),
        second.update_note_fields(NoteFieldsUpdate {
            id: "contract-notes-cas-race",
            expected_revision: 1,
            title: "Second winner candidate",
            content: "Second content",
            updated_at: 2_042,
        })
    );
    let results = [first_result.unwrap(), second_result.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, NoteMutationResult::Applied { revision: 2, .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                NoteMutationResult::Conflict {
                    expected_revision: 1,
                    current_revision: 2,
                }
            ))
            .count(),
        1
    );
    let raced = session
        .get_note("contract-notes-cas-race")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raced.revision, 2);
    assert!(matches!(
        raced.title.as_str(),
        "First winner candidate" | "Second winner candidate"
    ));
}
