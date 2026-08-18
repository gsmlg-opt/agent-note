use note_core::parse_label_selectors;
use note_storage::{NewNote, StorageBackend, TransactionMode};
use std::sync::Arc;

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    for (id, content, updated_at, note_revision, deleted_at) in [
        ("contract-bulk-active-a", "bulk content a", 100, 7, None),
        ("contract-bulk-active-b", "bulk content b", 200, 8, None),
        (
            "contract-bulk-deleted",
            "deleted bulk content",
            300,
            9,
            Some(400),
        ),
    ] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content,
                attachments: &[],
                created_at: updated_at,
                updated_at,
                note_revision,
                deleted_at,
            })
            .await
            .unwrap();
    }

    for (key, description) in [
        ("contract-bulk-selector", "Bulk selector"),
        ("contract-bulk-target", "Bulk target"),
        ("contract-bulk-unrelated", "Unrelated label"),
    ] {
        session.insert_label_key(key, description).await.unwrap();
    }
    for id in [
        "contract-bulk-active-a",
        "contract-bulk-active-b",
        "contract-bulk-deleted",
    ] {
        session
            .attach_label(id, "contract-bulk-selector", "selected")
            .await
            .unwrap();
    }
    session
        .attach_label(
            "contract-bulk-active-a",
            "contract-bulk-unrelated",
            "preserved",
        )
        .await
        .unwrap();

    drop(session);

    let selectors = parse_label_selectors("contract-bulk-selector=selected");
    let selection = storage.begin(TransactionMode::Immediate).await.unwrap();
    assert_eq!(
        selection
            .matching_note_ids_for_update(&selectors)
            .await
            .unwrap(),
        vec!["contract-bulk-active-a", "contract-bulk-active-b"]
    );
    selection.commit().await.unwrap();

    let session = storage.session().await.unwrap();

    assert!(session
        .set_note_label("contract-bulk-active-a", "contract-bulk-target", "first")
        .await
        .unwrap());
    assert!(!session
        .set_note_label("contract-bulk-active-a", "contract-bulk-target", "first")
        .await
        .unwrap());
    assert!(session
        .set_note_label(
            "contract-bulk-active-a",
            "contract-bulk-target",
            "replacement",
        )
        .await
        .unwrap());

    let labels = session
        .labels_for_note("contract-bulk-active-a")
        .await
        .unwrap();
    assert!(labels
        .iter()
        .any(|label| { label.key == "contract-bulk-target" && label.value == "replacement" }));
    assert!(labels
        .iter()
        .any(|label| { label.key == "contract-bulk-unrelated" && label.value == "preserved" }));

    assert!(session
        .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
        .await
        .unwrap());
    assert!(!session
        .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
        .await
        .unwrap());
    assert!(!session
        .remove_note_label("contract-bulk-active-a", "contract-bulk-unknown")
        .await
        .unwrap());
    let labels = session
        .labels_for_note("contract-bulk-active-a")
        .await
        .unwrap();
    assert!(!labels
        .iter()
        .any(|label| label.key == "contract-bulk-target"));
    assert!(labels
        .iter()
        .any(|label| { label.key == "contract-bulk-unrelated" && label.value == "preserved" }));
    assert!(session
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "contract-bulk-target"));
    assert!(session
        .set_note_label(
            "contract-bulk-active-a",
            "contract-bulk-target",
            "before-rollback",
        )
        .await
        .unwrap());

    assert_eq!(
        session
            .advance_note_updated_at("contract-bulk-active-a", 90)
            .await
            .unwrap(),
        1
    );
    let advanced = session
        .get_note("contract-bulk-active-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(advanced.updated_at, 101);
    assert_eq!(advanced.content, "bulk content a");
    assert_eq!(
        session
            .get_note_revision("contract-bulk-active-a")
            .await
            .unwrap(),
        Some(7)
    );

    assert_eq!(
        session
            .advance_note_updated_at("contract-bulk-active-a", 500)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .get_note("contract-bulk-active-a")
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        500
    );
    assert_eq!(
        session
            .advance_note_updated_at("contract-bulk-deleted", 600)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .list_deleted_note_summaries()
            .await
            .unwrap()
            .into_iter()
            .find(|note| note.id == "contract-bulk-deleted")
            .unwrap()
            .updated_at,
        300
    );
    drop(session);

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    assert!(rollback
        .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
        .await
        .unwrap());
    rollback
        .insert_label_key_if_missing("contract-bulk-rollback", "Rolled back label")
        .await
        .unwrap();
    assert!(rollback
        .set_note_label(
            "contract-bulk-active-a",
            "contract-bulk-rollback",
            "rolled-back",
        )
        .await
        .unwrap());
    assert_eq!(
        rollback
            .advance_note_updated_at("contract-bulk-active-a", 800)
            .await
            .unwrap(),
        1
    );
    rollback.rollback().await.unwrap();

    let observer = storage.session().await.unwrap();
    assert!(!observer
        .list_label_keys()
        .await
        .unwrap()
        .iter()
        .any(|key| key.key == "contract-bulk-rollback"));
    assert!(!observer
        .labels_for_note("contract-bulk-active-a")
        .await
        .unwrap()
        .iter()
        .any(|label| label.key == "contract-bulk-rollback"));
    assert!(observer
        .labels_for_note("contract-bulk-active-a")
        .await
        .unwrap()
        .iter()
        .any(|label| { label.key == "contract-bulk-target" && label.value == "before-rollback" }));
    let after_rollback = observer
        .get_note("contract-bulk-active-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_rollback.updated_at, 500);
    assert_eq!(after_rollback.content, "bulk content a");
    assert_eq!(
        observer
            .get_note_revision("contract-bulk-active-a")
            .await
            .unwrap(),
        Some(7)
    );
}
