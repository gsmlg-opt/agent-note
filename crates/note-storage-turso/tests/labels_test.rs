mod support;

use note_core::LabelValueType;
use note_storage::{LabelRepository, NotesRepository, StorageErrorKind};
use support::{fixture, insert_test_note};

#[tokio::test]
async fn insert_then_list_returns_the_key() {
    let fixture = fixture().await;

    fixture
        .session
        .insert_label_key("status", "Workflow status of the note")
        .await
        .unwrap();
    let keys = fixture.session.list_label_keys().await.unwrap();

    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].description, "Workflow status of the note");
    assert_eq!(keys[0].value_type.as_str(), "text");
}

#[tokio::test]
async fn duplicate_key_is_rejected_as_a_constraint() {
    let fixture = fixture().await;

    fixture
        .session
        .insert_label_key("status", "first")
        .await
        .unwrap();
    let error = fixture
        .session
        .insert_label_key("status", "second")
        .await
        .unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::Constraint);
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn insert_if_missing_preserves_an_existing_label_type() {
    let fixture = fixture().await;

    fixture
        .session
        .insert_label_key_with_type("priority", "Priority", LabelValueType::Number)
        .await
        .unwrap();
    fixture
        .session
        .insert_label_key_if_missing("priority", "")
        .await
        .unwrap();

    let keys = fixture.session.list_label_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].description, "Priority");
    assert_eq!(keys[0].value_type, LabelValueType::Number);
}

#[tokio::test]
async fn label_key_updates_preserve_or_replace_value_type_as_requested() {
    let fixture = fixture().await;
    fixture
        .session
        .insert_label_key_with_type("priority", "Priority", LabelValueType::Number)
        .await
        .unwrap();

    fixture
        .session
        .update_label_key("priority", "Updated priority")
        .await
        .unwrap();
    let keys = fixture.session.list_label_keys().await.unwrap();
    assert_eq!(keys[0].description, "Updated priority");
    assert_eq!(keys[0].value_type, LabelValueType::Number);

    fixture
        .session
        .update_label_key_with_type("priority", "Due date", LabelValueType::Date)
        .await
        .unwrap();
    let keys = fixture.session.list_label_keys().await.unwrap();
    assert_eq!(keys[0].description, "Due date");
    assert_eq!(keys[0].value_type, LabelValueType::Date);
}

#[tokio::test]
async fn attach_and_fetch_labels_for_a_note() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture
        .session
        .insert_label_key("status", "Workflow status")
        .await
        .unwrap();
    fixture
        .session
        .attach_label("note-1", "status", "done")
        .await
        .unwrap();

    let labels = fixture.session.labels_for_note("note-1").await.unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].key, "status");
    assert_eq!(labels[0].value, "done");
    assert_eq!(labels[0].description, "Workflow status");
    assert_eq!(labels[0].value_type.as_str(), "text");
}

#[tokio::test]
async fn attaching_unknown_key_fails() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;

    let result = fixture
        .session
        .attach_label("note-1", "does-not-exist", "value")
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn finds_a_note_matching_all_requested_labels_in_any_order() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    for key in ["skill-name", "version", "channel"] {
        fixture.session.insert_label_key(key, "").await.unwrap();
    }
    fixture
        .session
        .attach_label("note-1", "skill-name", "zddi-hooks")
        .await
        .unwrap();
    fixture
        .session
        .attach_label("note-1", "version", "1.0.0")
        .await
        .unwrap();
    fixture
        .session
        .attach_label("note-1", "channel", "stable")
        .await
        .unwrap();

    let matched = fixture
        .session
        .find_note_with_labels(&[
            ("version".to_string(), "1.0.0".to_string()),
            ("skill-name".to_string(), "zddi-hooks".to_string()),
        ])
        .await
        .unwrap();
    assert_eq!(matched.as_deref(), Some("note-1"));

    let different = fixture
        .session
        .find_note_with_labels(&[("version".to_string(), "2.0.0".to_string())])
        .await
        .unwrap();
    assert_eq!(different, None);

    fixture
        .session
        .soft_delete_note("note-1", 1, 2000)
        .await
        .unwrap();
    let matched = fixture
        .session
        .find_note_with_labels(&[("skill-name".to_string(), "zddi-hooks".to_string())])
        .await
        .unwrap();
    assert_eq!(matched, None);
    assert_eq!(
        fixture
            .session
            .labels_for_note("note-1")
            .await
            .unwrap()
            .len(),
        3
    );
    let counts = fixture.session.label_note_counts().await.unwrap();
    assert!(counts.iter().all(|(_, count)| *count == 0));
}

#[tokio::test]
async fn deleting_a_label_key_cascades_note_labels() {
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

    fixture.session.delete_label_key("status").await.unwrap();

    assert!(fixture
        .session
        .labels_for_note("note-1")
        .await
        .unwrap()
        .is_empty());
}
