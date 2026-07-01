use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, save_note, Context, SaveNoteInput};
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
    let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));
    (ctx, dir)
}

#[tokio::test]
async fn saves_and_returns_a_persisted_note() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
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

#[tokio::test]
async fn unknown_label_key_rejects_the_whole_request() {
    let (ctx, _dir) = test_context().await;
    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            labels: vec![("nonexistent".into(), "value".into())],
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
            labels: vec![],
        },
    )
    .await;
    assert!(result.is_err());
}

// A successful save must populate all three recall tables consistently (docs/design.md §3):
// notes, notes_embeddings, notes_sparse_weights. A note present in `notes` but missing from
// the embedding or sparse table silently degrades recall, so we assert all three hold rows.
#[tokio::test]
async fn successful_save_populates_all_three_tables() {
    let (ctx, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();

    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
            labels: vec![("status".into(), "done".into())],
        },
    )
    .await
    .unwrap();

    let conn = ctx.storage.connect().unwrap();

    // notes row + hydrated labels
    let stored = note_storage::get_note(&conn, &note.id).await.unwrap().unwrap();
    assert_eq!(stored.title, "My note");
    assert_eq!(stored.labels.len(), 1);
    assert_eq!(stored.labels[0].key, "status");

    // notes_embeddings row
    assert_eq!(count_rows(&conn, "notes_embeddings", &note.id).await, 1);
    // notes_sparse_weights rows (StubEmbedder emits at least one sparse term for this content)
    assert!(count_rows(&conn, "notes_sparse_weights", &note.id).await >= 1);
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
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();

    let result = save_note(
        &ctx,
        SaveNoteInput {
            title: "My note".into(),
            content: "Some content".into(),
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
    assert_eq!(total_rows(&conn, "notes_embeddings").await, 0);
    assert_eq!(total_rows(&conn, "notes_sparse_weights").await, 0);
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
