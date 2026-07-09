use libsql::Connection;
use note_core::{label_matches_selector, LabelSelector, Note};

pub async fn insert_note(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    created_at: i64,
    updated_at: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO notes (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id, title, content, created_at, updated_at],
    )
    .await?;
    Ok(())
}

pub async fn get_note(conn: &Connection, id: &str) -> anyhow::Result<Option<Note>> {
    let mut rows = conn
        .query(
            "SELECT id, title, content, created_at, updated_at FROM notes WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    let row = match rows.next().await? {
        Some(row) => row,
        None => return Ok(None),
    };
    let id = row.get::<String>(0)?;
    let title = row.get::<String>(1)?;
    let content = row.get::<String>(2)?;
    let created_at = row.get::<i64>(3)?;
    let updated_at = row.get::<i64>(4)?;
    drop(rows);

    let labels = crate::note_labels::labels_for_note(conn, &id).await?;

    Ok(Some(Note {
        id,
        title,
        content,
        labels,
        created_at,
        updated_at,
    }))
}

pub async fn update_note(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    updated_at: i64,
) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "UPDATE notes SET title = ?2, content = ?3, updated_at = ?4 WHERE id = ?1",
            libsql::params![id, title, content, updated_at],
        )
        .await?;
    Ok(affected)
}

pub async fn delete_note(conn: &Connection, id: &str) -> anyhow::Result<u64> {
    let affected = conn
        .execute("DELETE FROM notes WHERE id = ?1", libsql::params![id])
        .await?;
    Ok(affected)
}

pub async fn clear_note_derived(conn: &Connection, id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM note_chunk_embeddings WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    conn.execute(
        "DELETE FROM note_chunk_sparse WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    conn.execute(
        "DELETE FROM note_labels WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

pub async fn clear_note_labels(conn: &Connection, id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM note_labels WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

pub async fn clear_note_chunk_derived(
    conn: &Connection,
    id: &str,
    chunk_idx: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx = ?2",
        libsql::params![id, chunk_idx],
    )
    .await?;
    conn.execute(
        "DELETE FROM note_chunk_sparse WHERE note_id = ?1 AND chunk_idx = ?2",
        libsql::params![id, chunk_idx],
    )
    .await?;
    Ok(())
}

pub async fn clear_note_chunks_from_derived(
    conn: &Connection,
    id: &str,
    min_chunk_idx: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx >= ?2",
        libsql::params![id, min_chunk_idx],
    )
    .await?;
    conn.execute(
        "DELETE FROM note_chunk_sparse WHERE note_id = ?1 AND chunk_idx >= ?2",
        libsql::params![id, min_chunk_idx],
    )
    .await?;
    Ok(())
}

pub async fn list_notes(
    conn: &Connection,
    selectors: &[LabelSelector],
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Vec<Note>> {
    let mut sql = "SELECT id, title, content, created_at, updated_at FROM notes".to_string();
    let mut params = Vec::<libsql::Value>::new();

    sql.push_str(" ORDER BY created_at DESC");

    if selectors.is_empty() && (limit.is_some() || offset.is_some()) {
        sql.push_str(" LIMIT ? OFFSET ?");
        params.push(limit.unwrap_or(-1).into());
        params.push(offset.unwrap_or(0).into());
    }

    let mut rows = conn.query(&sql, libsql::params_from_iter(params)).await?;
    let mut notes = vec![];
    while let Some(row) = rows.next().await? {
        let id = row.get::<String>(0)?;
        let title = row.get::<String>(1)?;
        let content = row.get::<String>(2)?;
        let created_at = row.get::<i64>(3)?;
        let updated_at = row.get::<i64>(4)?;
        let labels = crate::note_labels::labels_for_note(conn, &id).await?;

        notes.push(Note {
            id,
            title,
            content,
            labels,
            created_at,
            updated_at,
        });
    }
    if !selectors.is_empty() {
        notes.retain(|note| {
            selectors.iter().all(|selector| {
                note.labels
                    .iter()
                    .any(|label| label_matches_selector(label, selector))
            })
        });
        let start = offset.unwrap_or(0).max(0) as usize;
        let end = limit
            .and_then(|limit| {
                if limit < 0 {
                    None
                } else {
                    Some(start.saturating_add(limit as usize))
                }
            })
            .unwrap_or(notes.len())
            .min(notes.len());
        notes = if start >= notes.len() {
            Vec::new()
        } else {
            notes[start..end].to_vec()
        };
    }
    Ok(notes)
}
