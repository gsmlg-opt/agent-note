mod support;

use note_core::{parse_label_selectors, LabelValueType, NoteAttachment};
use note_storage::{ActiveNoteSource, LabelRepository, NewNote, NoteUpdate, NotesRepository};
use note_storage_turso::TursoSession;
use support::{fixture, insert_test_note};

#[allow(clippy::too_many_arguments)]
async fn insert_note(
    session: &TursoSession,
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    created_at: i64,
    updated_at: i64,
    note_revision: i64,
    deleted_at: Option<i64>,
) {
    session
        .insert_note(NewNote {
            id,
            title,
            content,
            attachments,
            created_at,
            updated_at,
            note_revision,
            deleted_at,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_then_get_returns_the_same_note() {
    let fixture = fixture().await;
    insert_note(
        &fixture.session,
        "note-1",
        "Title",
        "Content",
        &[],
        1000,
        1000,
        1,
        None,
    )
    .await;

    let fetched = fixture
        .session
        .get_note("note-1")
        .await
        .unwrap()
        .expect("note should exist");

    assert_eq!(fetched.title, "Title");
    assert_eq!(fetched.content, "Content");
    assert!(fetched.attachments.is_empty());
}

#[tokio::test]
async fn insert_with_attachments_roundtrips_metadata_without_content() {
    let fixture = fixture().await;
    insert_note(
        &fixture.session,
        "note-1",
        "Title",
        "Content",
        &[NoteAttachment {
            id: "meta".into(),
            path: "./meta.json".into(),
            mime: "application/json".into(),
            description: "metadata".into(),
            content: b"{}".to_vec(),
        }],
        1000,
        1000,
        1,
        None,
    )
    .await;

    let database = turso::Builder::new_local(fixture._dir.path().join("test.db").to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query(
            "SELECT attachments FROM notes WHERE id = ?1",
            turso::params!["note-1"],
        )
        .await
        .unwrap();
    let raw = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(metadata[0].get("content").is_none());

    let fetched = fixture
        .session
        .get_note("note-1")
        .await
        .unwrap()
        .expect("note should exist");

    assert_eq!(fetched.attachments.len(), 1);
    assert_eq!(fetched.attachments[0].id, "meta");
    assert_eq!(fetched.attachments[0].path, "./meta.json");
    assert_eq!(fetched.attachments[0].description, "metadata");
    assert!(fetched.attachments[0].content.is_empty());
}

#[tokio::test]
async fn reads_legacy_attachment_metadata_with_empty_content() {
    let fixture = fixture().await;
    insert_note(
        &fixture.session,
        "note-1",
        "Title",
        "Content",
        &[],
        1000,
        1000,
        1,
        None,
    )
    .await;

    let database = turso::Builder::new_local(fixture._dir.path().join("test.db").to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute(
            "UPDATE notes SET attachments = ?2 WHERE id = ?1",
            turso::params![
                "note-1",
                r#"[{"id":"meta","path":"./meta.json","mime":"application/json","description":"metadata","content":""}]"#
            ],
        )
        .await
        .unwrap();

    let fetched = fixture
        .session
        .get_note("note-1")
        .await
        .unwrap()
        .expect("note should exist");
    assert_eq!(fetched.attachments.len(), 1);
    assert_eq!(fetched.attachments[0].id, "meta");
    assert_eq!(fetched.attachments[0].description, "metadata");
    assert!(fetched.attachments[0].content.is_empty());
}

#[tokio::test]
async fn invalid_attachment_description_is_rejected() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;

    let database = turso::Builder::new_local(fixture._dir.path().join("test.db").to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute(
            "UPDATE notes SET attachments = ?2 WHERE id = ?1",
            turso::params![
                "note-1",
                r#"[{"id":"meta","path":"./meta.json","mime":"application/json","description":42}]"#
            ],
        )
        .await
        .unwrap();

    assert!(fixture.session.get_note("note-1").await.is_err());
}

#[tokio::test]
async fn duplicate_attachment_metadata_fields_are_rejected() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;

    let database = turso::Builder::new_local(fixture._dir.path().join("test.db").to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute(
            "UPDATE notes SET attachments = ?2 WHERE id = ?1",
            turso::params![
                "note-1",
                r#"[{"id":"meta","path":"./meta.json","mime":"application/json","description":"first","description":"second"}]"#
            ],
        )
        .await
        .unwrap();

    assert!(fixture.session.get_note("note-1").await.is_err());
}

#[tokio::test]
async fn get_returns_none_for_missing_note() {
    let fixture = fixture().await;

    assert!(fixture
        .session
        .get_note("does-not-exist")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn deleted_notes_are_only_visible_in_trash_until_permanently_deleted() {
    let fixture = fixture().await;
    insert_note(
        &fixture.session,
        "active",
        "Active",
        "Content",
        &[],
        1000,
        1000,
        1,
        None,
    )
    .await;
    insert_note(
        &fixture.session,
        "deleted",
        "Deleted",
        "Content",
        &[],
        1001,
        1001,
        1,
        None,
    )
    .await;

    assert_eq!(
        fixture
            .session
            .soft_delete_note("deleted", 2000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .session
            .soft_delete_note("deleted", 2001)
            .await
            .unwrap(),
        0
    );
    assert!(fixture.session.get_note("deleted").await.unwrap().is_none());
    assert!(fixture.session.note_exists("deleted").await.unwrap());
    assert_eq!(
        fixture
            .session
            .get_deleted_note_content_and_revision("deleted")
            .await
            .unwrap(),
        Some(("Content".to_string(), 1))
    );

    let active = fixture.session.list_notes(&[], None, None).await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "active");
    let summaries = fixture
        .session
        .list_note_summaries(&[], None, None)
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(fixture.session.count_notes(&[]).await.unwrap(), 1);
    assert_eq!(fixture.session.list_all_notes().await.unwrap().len(), 2);

    let deleted = fixture.session.list_deleted_note_summaries().await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, "deleted");
    assert_eq!(deleted[0].deleted_at, Some(2000));

    assert_eq!(fixture.session.restore_note("deleted", 2).await.unwrap(), 1);
    assert_eq!(fixture.session.restore_note("deleted", 3).await.unwrap(), 0);
    assert!(fixture.session.get_note("deleted").await.unwrap().is_some());
    assert_eq!(
        fixture.session.get_note_revision("deleted").await.unwrap(),
        Some(2)
    );
    assert_eq!(fixture.session.count_notes(&[]).await.unwrap(), 2);
    assert!(fixture
        .session
        .list_deleted_note_summaries()
        .await
        .unwrap()
        .is_empty());

    assert_eq!(
        fixture
            .session
            .soft_delete_note("deleted", 3000)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .session
            .permanently_delete_note("deleted")
            .await
            .unwrap(),
        1
    );
    assert!(!fixture.session.note_exists("deleted").await.unwrap());
    assert!(fixture
        .session
        .list_deleted_note_summaries()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .session
            .permanently_delete_note("active")
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn expired_deleted_note_ids_include_the_ninety_day_boundary() {
    let fixture = fixture().await;
    for id in ["active", "older", "boundary", "recent"] {
        insert_test_note(&fixture.session, id).await;
    }
    fixture
        .session
        .soft_delete_note("older", 999)
        .await
        .unwrap();
    fixture
        .session
        .soft_delete_note("boundary", 1000)
        .await
        .unwrap();
    fixture
        .session
        .soft_delete_note("recent", 1001)
        .await
        .unwrap();

    assert_eq!(
        fixture
            .session
            .list_expired_deleted_note_ids(1000)
            .await
            .unwrap(),
        vec!["older", "boundary"]
    );
}

#[tokio::test]
async fn typed_selectors_preserve_ordering_pagination_summaries_and_counts() {
    let fixture = fixture().await;
    fixture
        .session
        .insert_label_key_with_type("priority", "Priority", LabelValueType::Number)
        .await
        .unwrap();
    for (id, created_at, priority) in [("older", 1, "2"), ("middle", 2, "5"), ("newer", 3, "10")] {
        insert_note(
            &fixture.session,
            id,
            id,
            "Content",
            &[],
            created_at,
            created_at,
            1,
            None,
        )
        .await;
        fixture
            .session
            .attach_label(id, "priority", priority)
            .await
            .unwrap();
    }

    let selectors = parse_label_selectors("priority>=5");
    let notes = fixture
        .session
        .list_notes(&selectors, Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(
        notes
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec!["middle"]
    );

    let summaries = fixture
        .session
        .list_note_summaries(&selectors, None, None)
        .await
        .unwrap();
    assert_eq!(
        summaries
            .iter()
            .map(|note| note.id.as_str())
            .collect::<Vec<_>>(),
        vec!["newer", "middle"]
    );
    assert_eq!(fixture.session.count_notes(&selectors).await.unwrap(), 2);

    let paged = fixture
        .session
        .list_notes(&[], Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(paged[0].id, "middle");
}

#[tokio::test]
async fn update_roundtrips_content_attachments_and_revision() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    let attachment = NoteAttachment {
        id: "updated".into(),
        path: "./updated.txt".into(),
        mime: "text/plain".into(),
        description: "updated attachment".into(),
        content: b"body".to_vec(),
    };

    assert_eq!(
        fixture
            .session
            .update_note(NoteUpdate {
                id: "note-1",
                title: "Updated",
                content: "Updated content",
                attachments: std::slice::from_ref(&attachment),
                updated_at: 2,
                note_revision: 2,
            })
            .await
            .unwrap(),
        1
    );

    assert_eq!(
        fixture
            .session
            .get_note_content("note-1")
            .await
            .unwrap()
            .as_deref(),
        Some("Updated content")
    );
    assert_eq!(
        fixture.session.get_note_revision("note-1").await.unwrap(),
        Some(2)
    );
    let note = fixture.session.get_note("note-1").await.unwrap().unwrap();
    assert_eq!(note.title, "Updated");
    assert_eq!(note.attachments[0].id, "updated");
    assert!(note.attachments[0].content.is_empty());
}

#[tokio::test]
async fn active_note_sources_are_filtered_and_ordered_by_id() {
    let fixture = fixture().await;
    for (id, content, revision) in [("b", "second", 2), ("a", "first", 3), ("c", "deleted", 4)] {
        insert_note(&fixture.session, id, id, content, &[], 1, 1, revision, None).await;
    }
    fixture.session.soft_delete_note("c", 2).await.unwrap();

    assert_eq!(
        fixture.session.list_active_note_sources().await.unwrap(),
        vec![
            ActiveNoteSource {
                id: "a".into(),
                content: "first".into(),
                note_revision: 3,
            },
            ActiveNoteSource {
                id: "b".into(),
                content: "second".into(),
                note_revision: 2,
            },
        ]
    );
}

#[tokio::test]
async fn deleting_a_note_cascades_note_labels() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .insert_label_key("status", "")
        .await
        .unwrap();
    fixture
        .session
        .attach_label("note-1", "status", "ready")
        .await
        .unwrap();
    fixture.session.soft_delete_note("note-1", 2).await.unwrap();
    fixture
        .session
        .permanently_delete_note("note-1")
        .await
        .unwrap();

    assert!(!fixture.session.note_exists("note-1").await.unwrap());
    assert!(fixture
        .session
        .labels_for_note("note-1")
        .await
        .unwrap()
        .is_empty());
}
