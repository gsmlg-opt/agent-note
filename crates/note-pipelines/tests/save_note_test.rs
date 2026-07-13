use note_core::{LabelValueType, NoteAttachment};
use note_embedding::StubEmbedder;
use note_pipelines::{
    define_label_key, define_label_key_with_type, drain_embedding_jobs, get_note, list_label_keys,
    save_note, update_note, Context, SaveNoteInput,
};
use note_storage::Storage;
use std::sync::Arc;
use tempfile::TempDir;

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
                content: "{}".into(),
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
    assert_eq!(stored.attachments[0].content, "");

    let attachment_path = _dir
        .path()
        .join("attachments")
        .join(&note.id)
        .join("meta.json");
    assert_eq!(std::fs::read_to_string(attachment_path).unwrap(), "{}");

    let hydrated = get_note(&ctx, &note.id).await.unwrap().unwrap();
    assert_eq!(hydrated.attachments[0].content, "{}");
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
                    content: "{}".into(),
                },
                NoteAttachment {
                    id: "meta".into(),
                    path: "./other.json".into(),
                    mime: "application/json".into(),
                    description: String::new(),
                    content: "{}".into(),
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
