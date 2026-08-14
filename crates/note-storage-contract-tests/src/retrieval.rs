use crate::unit;
use note_core::LabelValueType;
use note_storage::{
    NewNote, NoteMutationResult, NoteUpdate, StorageBackend, StorageErrorKind, UpsertNoteChunk,
    EMBEDDING_DIMENSION,
};
use std::sync::Arc;

async fn insert_note(
    session: &dyn note_storage::StorageSession,
    id: &str,
    title: &str,
    content: &str,
) {
    session
        .insert_note(NewNote {
            id,
            title,
            content,
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    insert_note(
        session.as_ref(),
        "contract-retrieval-validation",
        "Validation",
        "body",
    )
    .await;
    for length in [EMBEDDING_DIMENSION - 1, EMBEDDING_DIMENSION + 1] {
        let error = session
            .insert_chunk_embedding("contract-retrieval-validation", 0, &vec![0.0; length])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
        let error = session
            .dense_search(&vec![0.0; length], 10, None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
    }
    for invalid in [f32::NAN, f32::INFINITY] {
        let mut vector = unit(0);
        vector[1] = invalid;
        let error = session
            .insert_chunk_embedding("contract-retrieval-validation", 0, &vector)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
        let error = session.dense_search(&vector, 10, None).await.unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Operation);
    }

    for id in [
        "contract-retrieval-a",
        "contract-retrieval-b",
        "contract-retrieval-c",
    ] {
        insert_note(session.as_ref(), id, id, "body").await;
    }
    let mut diagonal = vec![0.0; EMBEDDING_DIMENSION];
    diagonal[0] = 0.8;
    diagonal[1] = 0.6;
    session
        .insert_chunk_embedding("contract-retrieval-a", 0, &unit(0))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("contract-retrieval-a", 1, &unit(1))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("contract-retrieval-b", 0, &diagonal)
        .await
        .unwrap();
    session
        .insert_chunk_embedding("contract-retrieval-c", 0, &unit(0))
        .await
        .unwrap();
    assert_eq!(
        session.dense_search(&unit(0), 3, None).await.unwrap(),
        vec![
            "contract-retrieval-a",
            "contract-retrieval-c",
            "contract-retrieval-b"
        ]
    );
    assert!(session
        .dense_search(&unit(0), 0, None)
        .await
        .unwrap()
        .is_empty());
    session
        .soft_delete_note("contract-retrieval-a", 1, 2)
        .await
        .unwrap();
    assert!(session
        .dense_search(&unit(0), 10, None)
        .await
        .unwrap()
        .iter()
        .all(|id| id != "contract-retrieval-a"));
    session
        .restore_note("contract-retrieval-a", 2)
        .await
        .unwrap();
    assert_eq!(
        session.dense_search(&unit(0), 1, None).await.unwrap(),
        vec!["contract-retrieval-a"]
    );
    for id in [
        "contract-retrieval-a",
        "contract-retrieval-b",
        "contract-retrieval-c",
    ] {
        session.clear_note_search_data(id).await.unwrap();
    }

    insert_note(
        session.as_ref(),
        "contract-retrieval-allowed",
        "Needle",
        "allowed body",
    )
    .await;
    insert_note(
        session.as_ref(),
        "contract-retrieval-excluded",
        "Needle Needle Needle",
        "excluded body",
    )
    .await;
    session
        .insert_chunk_embedding("contract-retrieval-allowed", 0, &unit(1))
        .await
        .unwrap();
    session
        .insert_chunk_embedding("contract-retrieval-excluded", 0, &unit(0))
        .await
        .unwrap();
    assert_eq!(
        session.title_search("Needle", 1, None).await.unwrap(),
        vec!["contract-retrieval-excluded"]
    );
    assert_eq!(
        session.dense_search(&unit(0), 1, None).await.unwrap(),
        vec!["contract-retrieval-excluded"]
    );
    let allowed = vec!["contract-retrieval-allowed".to_string()];
    assert_eq!(
        session
            .title_search("Needle", 1, Some(&allowed))
            .await
            .unwrap(),
        allowed
    );
    assert_eq!(
        session
            .dense_search(&unit(0), 1, Some(&allowed))
            .await
            .unwrap(),
        allowed
    );
    assert!(session
        .title_search("Needle", 1, Some(&[]))
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .dense_search(&unit(0), 1, Some(&[]))
        .await
        .unwrap()
        .is_empty());

    insert_note(
        session.as_ref(),
        "contract-retrieval-title",
        "Rust ownership guide",
        "unrelated body",
    )
    .await;
    insert_note(
        session.as_ref(),
        "contract-retrieval-body",
        "Other guide",
        "Rust ownership only in body",
    )
    .await;
    assert_eq!(
        session.title_search("Rust", 10, None).await.unwrap(),
        vec!["contract-retrieval-title"]
    );
    assert!(session
        .title_search("unrelated", 10, None)
        .await
        .unwrap()
        .is_empty());
    session
        .update_note(NoteUpdate {
            id: "contract-retrieval-title",
            title: "New heading",
            content: "unrelated body",
            attachments: &[],
            updated_at: 2,
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert!(session
        .title_search("ownership", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session.title_search("New", 10, None).await.unwrap(),
        vec!["contract-retrieval-title"]
    );
    session
        .soft_delete_note("contract-retrieval-title", 2, 3)
        .await
        .unwrap();
    assert!(session
        .title_search("New", 10, None)
        .await
        .unwrap()
        .is_empty());
    session
        .restore_note("contract-retrieval-title", 3)
        .await
        .unwrap();
    assert_eq!(
        session.title_search("New", 10, None).await.unwrap(),
        vec!["contract-retrieval-title"]
    );

    insert_note(
        session.as_ref(),
        "contract-retrieval-punctuation-a",
        "Alpha reference",
        "body",
    )
    .await;
    insert_note(
        session.as_ref(),
        "contract-retrieval-punctuation-b",
        "Beta reference",
        "body",
    )
    .await;
    let punctuation = session
        .title_search("(alpha / beta):", 10, None)
        .await
        .unwrap();
    assert_eq!(
        punctuation,
        vec![
            "contract-retrieval-punctuation-a",
            "contract-retrieval-punctuation-b"
        ]
    );
    assert!(session
        .title_search("   ", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .title_search(r#""():-"#, 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .title_search("Alpha", 0, None)
        .await
        .unwrap()
        .is_empty());

    for id in [
        "contract-retrieval-title-tie-b",
        "contract-retrieval-title-tie-a",
    ] {
        insert_note(session.as_ref(), id, "Shared ranking heading", "body").await;
    }
    assert_eq!(
        session.title_search("Shared", 2, None).await.unwrap(),
        vec![
            "contract-retrieval-title-tie-a",
            "contract-retrieval-title-tie-b"
        ]
    );

    insert_note(
        session.as_ref(),
        "contract-retrieval-cascade",
        "Cascade target",
        "body",
    )
    .await;
    session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "contract-retrieval-cascade",
            chunk_idx: 0,
            content_hash: "cascade-hash",
            content: "body",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    session
        .insert_chunk_embedding("contract-retrieval-cascade", 0, &unit(0))
        .await
        .unwrap();
    session
        .enqueue_embedding_job(
            "contract-retrieval-cascade",
            0,
            "cascade-hash",
            "body",
            1,
            1,
        )
        .await
        .unwrap();
    session
        .insert_label_key_with_type("contract-retrieval-label", "Cascade", LabelValueType::Text)
        .await
        .unwrap();
    session
        .attach_label(
            "contract-retrieval-cascade",
            "contract-retrieval-label",
            "yes",
        )
        .await
        .unwrap();
    session
        .soft_delete_note("contract-retrieval-cascade", 1, 4)
        .await
        .unwrap();
    assert_eq!(
        session
            .permanently_delete_note("contract-retrieval-cascade", 2)
            .await
            .unwrap(),
        NoteMutationResult::Applied {
            value: (),
            revision: 2
        }
    );
    assert!(session
        .list_note_chunks("contract-retrieval-cascade")
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .labels_for_note("contract-retrieval-cascade")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        session
            .get_deleted_note_content_and_revision("contract-retrieval-cascade")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        session
            .permanently_delete_note("contract-retrieval-cascade", 2)
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    assert_eq!(
        session
            .restore_note("contract-retrieval-cascade", 2)
            .await
            .unwrap(),
        NoteMutationResult::NotFound
    );
    assert!(!session
        .chunk_embedding_exists("contract-retrieval-cascade", 0)
        .await
        .unwrap());
    assert!(session
        .title_search("Cascade", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .dense_search(&unit(0), 20, None)
        .await
        .unwrap()
        .iter()
        .all(|id| id != "contract-retrieval-cascade"));
    assert!(session
        .claim_pending_embedding_jobs(1, 10)
        .await
        .unwrap()
        .is_empty());

    insert_note(
        session.as_ref(),
        "contract-retrieval-cascade",
        "Reused clean note",
        "fresh body",
    )
    .await;
    assert_eq!(
        session.title_search("Reused", 10, None).await.unwrap(),
        vec!["contract-retrieval-cascade"]
    );
    assert!(session
        .title_search("Cascade", 10, None)
        .await
        .unwrap()
        .is_empty());
    assert!(!session
        .chunk_embedding_exists("contract-retrieval-cascade", 0)
        .await
        .unwrap());
    assert!(session
        .dense_search(&unit(0), 20, None)
        .await
        .unwrap()
        .iter()
        .all(|id| id != "contract-retrieval-cascade"));
    assert!(session
        .claim_pending_embedding_jobs(1, 11)
        .await
        .unwrap()
        .is_empty());
}
