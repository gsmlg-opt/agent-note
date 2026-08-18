mod support;

use note_core::NoteAttachment;
use note_embedding::StubEmbedder;
use note_pipelines::{
    define_label_key, delete_note, export_data, get_note, import_data, import_json, list_all_notes,
    list_deleted_note_summaries, list_label_keys, save_note, Context, SaveNoteInput,
};
use note_storage::{NewNote, StorageBackend};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use support::{event_log, test_context, ControlledAttachmentStore, EventStorageBackend};

async fn controlled_import_context() -> (
    Context,
    Arc<EventStorageBackend>,
    Arc<ControlledAttachmentStore>,
    support::EventLog,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let raw_backend: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let events = event_log();
    let event_backend = Arc::new(EventStorageBackend::new(raw_backend, events.clone()));
    let backend: Arc<dyn StorageBackend> = event_backend.clone();
    let attachments = Arc::new(ControlledAttachmentStore::new(
        backend.clone(),
        events.clone(),
    ));
    let ctx = Context::new(backend, Arc::new(StubEmbedder), attachments.clone());
    (ctx, event_backend, attachments, events, dir)
}

#[tokio::test]
async fn import_publishes_generated_objects_before_begin_without_user_key_components() {
    let (ctx, _backend, attachments, events, _dir) = controlled_import_context().await;
    let note_id = "../private/user-note";
    let attachment_id = "../../user-attachment";
    let path = "secret/user-path.bin";
    let bytes = vec![0, 159, 146, 150, 255];
    let input = format!(
        r#"{{
            "version": 2,
            "label_keys": [],
            "notes": [{{
                "id": {note_id:?},
                "title": "Imported",
                "content": "Object first",
                "attachments": [{{
                    "id": {attachment_id:?},
                    "path": {path:?},
                    "mime": "application/octet-stream",
                    "storage": {{
                        "object_key": "attacker-controlled/source-key",
                        "storage_generation": "attacker-generation",
                        "size_bytes": 999,
                        "checksum_sha256": "attacker-checksum"
                    }},
                    "content_base64": "AJ+Slv8="
                }}],
                "created_at": 1,
                "updated_at": 1,
                "labels": []
            }}]
        }}"#
    );

    let stats = import_json(&ctx, &input).await.unwrap();

    assert_eq!(stats.notes_added, 1);
    let note = get_note(&ctx, note_id).await.unwrap().unwrap();
    assert_eq!(note.attachments[0].content, bytes);
    let storage = note.attachments[0].storage.as_ref().unwrap();
    let namespace = format!("{:x}", Sha256::digest(note_id.as_bytes()));
    assert!(storage
        .object_key
        .starts_with(&format!("notes/{namespace}/objects/")));
    for user_value in [note_id, attachment_id, path, "private", "secret"] {
        assert!(!storage.object_key.contains(user_value));
    }
    assert!(!storage.object_key.contains("attacker-controlled"));
    assert_ne!(storage.storage_generation, "attacker-generation");
    assert!(attachments.has_object(&storage.object_key));

    let events = events.lock().unwrap().clone();
    let put = events
        .iter()
        .position(|event| event.starts_with("put_object:"))
        .unwrap();
    let begin = events.iter().position(|event| event == "begin").unwrap();
    assert!(put < begin);
    assert!(!events.iter().any(|event| event.starts_with("prepare:")));
    assert!(!events.iter().any(|event| event.starts_with("publish:")));
}

#[tokio::test]
async fn partial_import_object_failure_cleans_all_attempted_keys_without_beginning() {
    let (ctx, _backend, attachments, events, _dir) = controlled_import_context().await;
    attachments.fail_put_on_call(2);
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{
            "id":"note-a","title":"A","content":"first",
            "attachments":[
                {"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="},
                {"id":"b","path":"b.bin","mime":"application/octet-stream","content_base64":"gP4="}
            ],"created_at":1,"updated_at":1,"labels":[]
        }]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("attachment object write failed"));
    assert!(list_all_notes(&ctx).await.unwrap().is_empty());
    let events = events.lock().unwrap().clone();
    let keys: Vec<&str> = events
        .iter()
        .filter_map(|event| event.strip_prefix("put_object:"))
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|key| !attachments.has_object(key)));
    assert!(!events.iter().any(|event| event == "begin"));
}

#[tokio::test]
async fn import_begin_failure_cleans_published_objects_without_database_writes() {
    let (ctx, backend, attachments, events, _dir) = controlled_import_context().await;
    backend.fail_next_begin();
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{"id":"note-a","title":"A","content":"first","attachments":[{"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]}]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("controlled begin failure"));
    assert!(list_all_notes(&ctx).await.unwrap().is_empty());
    let events = events.lock().unwrap().clone();
    let object_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(!attachments.has_object(object_key));
    assert!(events.iter().any(|event| event == "begin_failed"));
}

#[tokio::test]
async fn import_body_error_rolls_back_then_deletes_every_published_object() {
    let (ctx, backend, attachments, events, _dir) = controlled_import_context().await;
    backend.fail_next_repository_call("insert_note");
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [
            {"id":"note-a","title":"A","content":"first","attachments":[{"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]},
            {"id":"note-b","title":"B","content":"second","attachments":[{"id":"b","path":"b.bin","mime":"application/octet-stream","content_base64":"gP4="}],"created_at":1,"updated_at":1,"labels":[]}
        ]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("controlled repository failure"));
    assert!(list_all_notes(&ctx).await.unwrap().is_empty());
    let events = events.lock().unwrap().clone();
    let begin = events.iter().position(|event| event == "begin").unwrap();
    assert!(events[..begin]
        .iter()
        .all(|event| event.starts_with("put_object:")));
    assert_eq!(
        events.iter().filter(|event| *event == "rollback").count(),
        1
    );
    let keys: Vec<&str> = events
        .iter()
        .filter_map(|event| event.strip_prefix("put_object:"))
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|key| !attachments.has_object(key)));
}

#[tokio::test]
async fn import_race_deletes_only_the_skipped_notes_objects_after_commit() {
    let (ctx, _backend, attachments, events, _dir) = controlled_import_context().await;
    attachments.race_note_on_next_put("race-note");
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [
            {"id":"race-note","title":"Imported","content":"skip after recheck","attachments":[{"id":"race","path":"race.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]},
            {"id":"inserted-note","title":"Inserted","content":"keep objects","attachments":[{"id":"keep","path":"keep.bin","mime":"application/octet-stream","content_base64":"gP4="}],"created_at":1,"updated_at":1,"labels":[]}
        ]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();

    assert_eq!(stats.notes_added, 1);
    assert_eq!(stats.notes_skipped, 1);
    let race_namespace = format!("{:x}", Sha256::digest(b"race-note"));
    let inserted_namespace = format!("{:x}", Sha256::digest(b"inserted-note"));
    let events = events.lock().unwrap().clone();
    let race_key = events
        .iter()
        .find_map(|event| event.strip_prefix(&format!("put_object:notes/{race_namespace}/")))
        .map(|suffix| format!("notes/{race_namespace}/{suffix}"))
        .unwrap();
    let inserted_key = events
        .iter()
        .find_map(|event| event.strip_prefix(&format!("put_object:notes/{inserted_namespace}/")))
        .map(|suffix| format!("notes/{inserted_namespace}/{suffix}"))
        .unwrap();
    assert!(!attachments.has_object(&race_key));
    assert!(attachments.has_object(&inserted_key));
    let commit = events.iter().position(|event| event == "commit").unwrap();
    let delete = events
        .iter()
        .position(|event| event == &format!("delete_object:{race_key}"))
        .unwrap();
    assert!(commit < delete);
}

#[tokio::test]
async fn raced_skip_cleanup_failure_is_a_safe_orphan_not_an_import_failure() {
    let (ctx, _backend, attachments, events, _dir) = controlled_import_context().await;
    attachments.race_note_on_next_put("race-note");
    attachments.fail_delete_object();
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [
            {"id":"race-note","title":"Imported","content":"skip","attachments":[{"id":"race","path":"race.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]},
            {"id":"inserted-note","title":"Inserted","content":"keep","attachments":[],"created_at":1,"updated_at":1,"labels":[]}
        ]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();

    assert_eq!(stats.notes_added, 1);
    assert_eq!(stats.notes_skipped, 1);
    let events = events.lock().unwrap().clone();
    let object_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(attachments.has_object(object_key));
    assert!(events
        .iter()
        .any(|event| event == &format!("delete_object:{object_key}")));
}

#[tokio::test]
async fn ambiguous_import_commit_retains_objects_and_committed_rows() {
    let (ctx, backend, attachments, events, _dir) = controlled_import_context().await;
    backend.fail_next_commit_acknowledgement();
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{"id":"note-a","title":"A","content":"first","attachments":[{"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]}]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("commit outcome is unknown"));
    assert!(get_note(&ctx, "note-a").await.unwrap().is_some());
    let events = events.lock().unwrap().clone();
    let object_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(attachments.has_object(object_key));
    assert!(!events
        .iter()
        .any(|event| event.starts_with("delete_object:")));
}

#[tokio::test]
async fn any_import_commit_error_retains_published_objects() {
    let (ctx, backend, attachments, events, _dir) = controlled_import_context().await;
    backend.fail_next_commit();
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{"id":"note-a","title":"A","content":"first","attachments":[{"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]}]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("commit outcome is unknown"));
    assert!(get_note(&ctx, "note-a").await.unwrap().is_none());
    let events = events.lock().unwrap().clone();
    let object_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(attachments.has_object(object_key));
    assert!(!events
        .iter()
        .any(|event| event.starts_with("delete_object:")));
}

#[tokio::test]
async fn import_rollback_uncertainty_retains_published_objects() {
    let (ctx, backend, attachments, events, _dir) = controlled_import_context().await;
    backend.fail_next_repository_call("insert_note");
    backend.fail_next_rollback();
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{"id":"note-a","title":"A","content":"first","attachments":[{"id":"a","path":"a.bin","mime":"application/octet-stream","content_base64":"AP8="}],"created_at":1,"updated_at":1,"labels":[]}]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();

    assert!(error.to_string().contains("rollback outcome is unknown"));
    let events = events.lock().unwrap().clone();
    let object_key = events
        .iter()
        .find_map(|event| event.strip_prefix("put_object:"))
        .unwrap();
    assert!(attachments.has_object(object_key));
    assert!(events.iter().any(|event| event == "rollback_failed"));
    assert!(!events
        .iter()
        .any(|event| event.starts_with("delete_object:")));
}

#[tokio::test]
async fn export_import_roundtrips_notes_and_label_keys() {
    let (source, _source_backend, _source_dir) = test_context().await;
    define_label_key(&source, "status", "Workflow status")
        .await
        .unwrap();
    let labeled = save_note(
        &source,
        SaveNoteInput {
            title: "Plan".into(),
            content: "Draft the release plan".into(),
            attachments: vec![
                NoteAttachment {
                    id: "meta".into(),
                    path: "./meta.json".into(),
                    mime: "application/json".into(),
                    description: "metadata".into(),
                    content: b"{}".to_vec(),
                    storage: None,
                },
                NoteAttachment {
                    id: "binary".into(),
                    path: "./blob.bin".into(),
                    mime: "application/octet-stream".into(),
                    description: "binary data".into(),
                    content: vec![0, 159, 146, 150],
                    storage: None,
                },
            ],
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();
    let plain = save_note(
        &source,
        SaveNoteInput {
            title: "Scratch".into(),
            content: "Unlabeled thought".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    delete_note(&source, &labeled.id, labeled.revision)
        .await
        .unwrap();

    let data = export_data(&source).await.unwrap();
    assert_eq!(data.version, 2);
    assert_eq!(data.notes.len(), 2);
    let deleted_export = data
        .notes
        .iter()
        .find(|note| note.id == labeled.id)
        .unwrap();
    assert!(deleted_export.deleted_at.is_some());
    assert_eq!(deleted_export.attachments[0].content.as_deref(), Some("{}"));
    assert_eq!(
        deleted_export.attachments[0].content_base64.as_deref(),
        Some("e30=")
    );
    assert!(deleted_export.attachments[1].content.is_none());
    assert_eq!(
        deleted_export.attachments[1].content_base64.as_deref(),
        Some("AJ+Slg==")
    );

    let (target, _target_backend, target_dir) = test_context().await;
    let stats = import_data(&target, data).await.unwrap();
    assert_eq!(stats.notes_added, 2);
    assert_eq!(stats.notes_skipped, 0);
    assert_eq!(stats.label_keys_added, 1);
    assert!(stats.embedding_jobs_queued > 0);

    let keys = list_label_keys(&target).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].description, "Workflow status");
    assert_eq!(keys[0].value_type.as_str(), "text");

    assert!(get_note(&target, &labeled.id).await.unwrap().is_none());
    let deleted = list_deleted_note_summaries(&target).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].id, labeled.id);
    let restored_notes = list_all_notes(&target).await.unwrap();
    let restored_labeled = restored_notes
        .iter()
        .find(|note| note.id == labeled.id)
        .unwrap();
    assert_eq!(restored_labeled.title, labeled.title);
    assert_eq!(restored_labeled.content, labeled.content);
    assert_eq!(restored_labeled.created_at, labeled.created_at);
    assert_eq!(restored_labeled.updated_at, labeled.updated_at);
    assert_eq!(
        restored_labeled.attachments.len(),
        labeled.attachments.len()
    );
    for (restored, source) in restored_labeled
        .attachments
        .iter()
        .zip(&labeled.attachments)
    {
        assert_eq!(restored.id, source.id);
        assert_eq!(restored.path, source.path);
        assert_eq!(restored.mime, source.mime);
        assert_eq!(restored.description, source.description);
        assert_eq!(restored.content, source.content);
        assert!(restored.storage.is_some());
        assert_ne!(restored.storage, source.storage);
    }
    assert_eq!(restored_labeled.labels.len(), 1);
    assert_eq!(restored_labeled.labels[0].key, "status");
    assert_eq!(restored_labeled.labels[0].value, "done");
    assert_eq!(restored_labeled.labels[0].description, "Workflow status");
    assert_eq!(restored_labeled.labels[0].value_type.as_str(), "text");
    assert!(restored_labeled.deleted_at.is_some());
    let target_attachment_root = target_dir.path().join("attachments");
    assert!(target_attachment_root.exists());

    let restored_plain = get_note(&target, &plain.id).await.unwrap().unwrap();
    assert_eq!(restored_plain.title, plain.title);
    assert_eq!(restored_plain.content, plain.content);
    assert_eq!(restored_plain.created_at, plain.created_at);
    assert_eq!(restored_plain.updated_at, plain.updated_at);
    assert!(restored_plain.labels.is_empty());
}

#[tokio::test]
async fn export_hydrates_legacy_bytes_without_emitting_storage_metadata() {
    let (ctx, backend, attachments, _events, _dir) = controlled_import_context().await;
    let legacy_attachment = NoteAttachment {
        id: "legacy-id".into(),
        path: "legacy/file.bin".into(),
        mime: "application/octet-stream".into(),
        description: "legacy bytes".into(),
        content: Vec::new(),
        storage: None,
    };
    backend
        .session()
        .await
        .unwrap()
        .insert_note(NewNote {
            id: "legacy-note",
            title: "Legacy",
            content: "Hydrate during export",
            attachments: &[legacy_attachment],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
    attachments.set_read_content("legacy-note", "legacy/file.bin", &[0, 255, 1]);

    let data = export_data(&ctx).await.unwrap();
    let attachment = &data.notes[0].attachments[0];
    assert_eq!(attachment.content_base64.as_deref(), Some("AP8B"));
    let json = serde_json::to_value(&data).unwrap();
    assert!(json["notes"][0]["attachments"][0].get("storage").is_none());
}

#[tokio::test]
async fn blank_imported_content_creates_no_chunks_or_embedding_jobs() {
    let (ctx, backend, _dir) = test_context().await;
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{
            "id": "blank-legacy",
            "title": "Title only",
            "content": " \n\t",
            "attachments": [],
            "created_at": 1000,
            "updated_at": 1000,
            "labels": []
        }]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();
    assert_eq!(stats.embedding_jobs_queued, 0);
    let session = backend.session().await.unwrap();
    assert!(session
        .list_note_chunks("blank-legacy")
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .claim_pending_embedding_jobs(10, chrono::Utc::now().timestamp())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn imports_version_one_text_attachments() {
    let (ctx, _backend, _dir) = test_context().await;
    let input = r#"{
        "version": 1,
        "label_keys": [],
        "notes": [{
            "id": "legacy-note",
            "title": "Legacy",
            "content": "Imported note",
            "attachments": [{
                "id": "text",
                "path": "legacy.txt",
                "mime": "text/plain",
                "description": "legacy attachment",
                "content": "legacy text"
            }],
            "created_at": 1000,
            "updated_at": 1000,
            "labels": []
        }]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();
    assert_eq!(stats.notes_added, 1);
    let note = get_note(&ctx, "legacy-note").await.unwrap().unwrap();
    assert_eq!(note.attachments[0].content, b"legacy text");
}

#[tokio::test]
async fn rejects_unsupported_export_versions() {
    let (ctx, _backend, _dir) = test_context().await;
    let error = import_json(&ctx, r#"{"version":3,"label_keys":[],"notes":[]}"#)
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "unsupported export version: 3");
}

#[tokio::test]
async fn invalid_version_two_attachment_rolls_back_the_import() {
    let (ctx, _backend, dir) = test_context().await;
    let input = r#"{
        "version": 2,
        "label_keys": [{"key":"status","description":"Workflow status","value_type":"text"}],
        "notes": [
            {
                "id": "valid-note",
                "title": "Valid",
                "content": "Would otherwise import",
                "attachments": [{
                    "id": "blob",
                    "path": "blob.bin",
                    "mime": "application/octet-stream",
                    "content_base64": "AP8="
                }],
                "created_at": 1000,
                "updated_at": 1000,
                "labels": [["status", "done"]]
            },
            {
                "id": "invalid-note",
                "title": "Invalid",
                "content": "Reject the whole import",
                "attachments": [{
                    "id": "text",
                    "path": "text.txt",
                    "mime": "text/plain",
                    "content": "text",
                    "content_base64": "b3RoZXI="
                }],
                "created_at": 1000,
                "updated_at": 1000,
                "labels": []
            }
        ]
    }"#;

    let error = import_json(&ctx, input).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("attachment content and content_base64 do not match"));
    assert!(list_all_notes(&ctx).await.unwrap().is_empty());
    assert!(list_label_keys(&ctx).await.unwrap().is_empty());

    let attachments_dir = dir.path().join("attachments");
    let object_files = if attachments_dir.exists() {
        walk_files(&attachments_dir)
    } else {
        Vec::new()
    };
    assert!(object_files.is_empty(), "safe empty directories may remain");
}

fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
