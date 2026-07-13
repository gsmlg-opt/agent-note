use libsql::Connection;
use note_core::{label_matches_selector, LabelSelector, Note, NoteAttachment, NoteListItem};

pub async fn insert_note(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    created_at: i64,
    updated_at: i64,
    note_revision: i64,
) -> anyhow::Result<()> {
    insert_note_with_attachments(
        conn,
        id,
        title,
        content,
        &[],
        created_at,
        updated_at,
        note_revision,
    )
    .await
}

pub async fn insert_note_with_attachments(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    created_at: i64,
    updated_at: i64,
    note_revision: i64,
) -> anyhow::Result<()> {
    let attachments = serialize_attachments(attachments)?;
    conn.execute(
        "INSERT INTO notes (id, title, content, attachments, created_at, updated_at, note_revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            id,
            title,
            content,
            attachments,
            created_at,
            updated_at,
            note_revision
        ],
    )
    .await?;
    Ok(())
}

pub async fn get_note_revision(conn: &Connection, id: &str) -> anyhow::Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT note_revision FROM notes WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<i64>(0)?),
        None => None,
    })
}

pub async fn get_note(conn: &Connection, id: &str) -> anyhow::Result<Option<Note>> {
    let mut rows = conn
        .query(
            "SELECT id, title, content, attachments, created_at, updated_at FROM notes WHERE id = ?1",
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
    let attachments = deserialize_attachments(&row.get::<String>(3)?)?;
    let created_at = row.get::<i64>(4)?;
    let updated_at = row.get::<i64>(5)?;
    drop(rows);

    let labels = crate::note_labels::labels_for_note(conn, &id).await?;

    Ok(Some(Note {
        id,
        title,
        content,
        attachments,
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
    note_revision: i64,
) -> anyhow::Result<u64> {
    let attachments = get_note(conn, id)
        .await?
        .map(|note| note.attachments)
        .unwrap_or_default();
    update_note_with_attachments(
        conn,
        id,
        title,
        content,
        &attachments,
        updated_at,
        note_revision,
    )
    .await
}

pub async fn update_note_with_attachments(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    updated_at: i64,
    note_revision: i64,
) -> anyhow::Result<u64> {
    let attachments = serialize_attachments(attachments)?;
    let affected = conn
        .execute(
            "UPDATE notes
             SET title = ?2, content = ?3, attachments = ?4, updated_at = ?5, note_revision = ?6
             WHERE id = ?1",
            libsql::params![id, title, content, attachments, updated_at, note_revision],
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
    let mut sql =
        "SELECT id, title, content, attachments, created_at, updated_at FROM notes".to_string();
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
        let attachments = deserialize_attachments(&row.get::<String>(3)?)?;
        let created_at = row.get::<i64>(4)?;
        let updated_at = row.get::<i64>(5)?;
        let labels = crate::note_labels::labels_for_note(conn, &id).await?;

        notes.push(Note {
            id,
            title,
            content,
            attachments,
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

fn serialize_attachments(attachments: &[NoteAttachment]) -> anyhow::Result<String> {
    let metadata = attachments
        .iter()
        .map(|attachment| NoteAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
            content: String::new(),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&metadata).map_err(anyhow::Error::new)
}

fn deserialize_attachments(value: &str) -> anyhow::Result<Vec<NoteAttachment>> {
    serde_json::from_str(value).map_err(anyhow::Error::new)
}

pub async fn list_note_summaries(
    conn: &Connection,
    selectors: &[LabelSelector],
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Vec<NoteListItem>> {
    let mut sql = "SELECT id, title, created_at, updated_at FROM notes".to_string();
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
        let created_at = row.get::<i64>(2)?;
        let updated_at = row.get::<i64>(3)?;
        let labels = crate::note_labels::labels_for_note(conn, &id).await?;

        notes.push(NoteListItem {
            id,
            title,
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

pub async fn count_notes(conn: &Connection, selectors: &[LabelSelector]) -> anyhow::Result<usize> {
    if selectors.is_empty() {
        let mut rows = conn.query("SELECT COUNT(*) FROM notes", ()).await?;
        let Some(row) = rows.next().await? else {
            return Ok(0);
        };
        return Ok(row.get::<i64>(0)?.max(0) as usize);
    }

    Ok(list_note_summaries(conn, selectors, None, None)
        .await?
        .len())
}
