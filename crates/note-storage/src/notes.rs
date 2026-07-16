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
    insert_note_with_attachments_and_deleted_at(
        conn,
        id,
        title,
        content,
        attachments,
        created_at,
        updated_at,
        note_revision,
        None,
    )
    .await
}

pub async fn insert_note_with_attachments_and_deleted_at(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    attachments: &[NoteAttachment],
    created_at: i64,
    updated_at: i64,
    note_revision: i64,
    deleted_at: Option<i64>,
) -> anyhow::Result<()> {
    let attachments = serialize_attachments(attachments)?;
    conn.execute(
        "INSERT INTO notes (
             id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        libsql::params![
            id,
            title,
            content,
            attachments,
            created_at,
            updated_at,
            note_revision,
            deleted_at
        ],
    )
    .await?;
    Ok(())
}

pub async fn get_note_revision(conn: &Connection, id: &str) -> anyhow::Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT note_revision FROM notes WHERE id = ?1 AND deleted_at IS NULL",
            libsql::params![id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<i64>(0)?),
        None => None,
    })
}

pub async fn note_exists(conn: &Connection, id: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM notes WHERE id = ?1 LIMIT 1",
            libsql::params![id],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

pub async fn get_note(conn: &Connection, id: &str) -> anyhow::Result<Option<Note>> {
    let mut rows = conn
        .query(
            "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
             FROM notes
             WHERE id = ?1 AND deleted_at IS NULL",
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
    let deleted_at = row.get::<Option<i64>>(6)?;
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
        deleted_at,
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
             WHERE id = ?1 AND deleted_at IS NULL",
            libsql::params![id, title, content, attachments, updated_at, note_revision],
        )
        .await?;
    Ok(affected)
}

pub async fn delete_note(conn: &Connection, id: &str, deleted_at: i64) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "UPDATE notes SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            libsql::params![id, deleted_at],
        )
        .await?;
    Ok(affected)
}

pub async fn get_deleted_note_content_and_revision(
    conn: &Connection,
    id: &str,
) -> anyhow::Result<Option<(String, i64)>> {
    let mut rows = conn
        .query(
            "SELECT content, note_revision
             FROM notes
             WHERE id = ?1 AND deleted_at IS NOT NULL",
            libsql::params![id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some((row.get::<String>(0)?, row.get::<i64>(1)?)))
}

pub async fn restore_note(conn: &Connection, id: &str, note_revision: i64) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "UPDATE notes
             SET deleted_at = NULL, note_revision = ?2
             WHERE id = ?1 AND deleted_at IS NOT NULL",
            libsql::params![id, note_revision],
        )
        .await?;
    Ok(affected)
}

pub async fn permanently_delete_note(conn: &Connection, id: &str) -> anyhow::Result<u64> {
    let affected = conn
        .execute(
            "DELETE FROM notes WHERE id = ?1 AND deleted_at IS NOT NULL",
            libsql::params![id],
        )
        .await?;
    Ok(affected)
}

pub async fn list_expired_deleted_note_ids(
    conn: &Connection,
    deleted_at_cutoff: i64,
) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT id FROM notes
             WHERE deleted_at IS NOT NULL AND deleted_at <= ?1
             ORDER BY deleted_at, id",
            libsql::params![deleted_at_cutoff],
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}

pub async fn clear_note_search_data(conn: &Connection, id: &str) -> anyhow::Result<()> {
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
        "DELETE FROM embedding_jobs WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    conn.execute(
        "DELETE FROM note_chunks WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    Ok(())
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
    let mut sql = "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
                   FROM notes WHERE deleted_at IS NULL"
        .to_string();
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
        let deleted_at = row.get::<Option<i64>>(6)?;
        let labels = crate::note_labels::labels_for_note(conn, &id).await?;

        notes.push(Note {
            id,
            title,
            content,
            attachments,
            labels,
            created_at,
            updated_at,
            deleted_at,
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

pub async fn list_all_notes(conn: &Connection) -> anyhow::Result<Vec<Note>> {
    let mut rows = conn
        .query(
            "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
             FROM notes
             ORDER BY created_at DESC",
            (),
        )
        .await?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next().await? {
        let id = row.get::<String>(0)?;
        notes.push(Note {
            title: row.get::<String>(1)?,
            content: row.get::<String>(2)?,
            attachments: deserialize_attachments(&row.get::<String>(3)?)?,
            created_at: row.get::<i64>(4)?,
            updated_at: row.get::<i64>(5)?,
            deleted_at: row.get::<Option<i64>>(6)?,
            labels: crate::note_labels::labels_for_note(conn, &id).await?,
            id,
        });
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
    let mut sql = "SELECT id, title, created_at, updated_at, deleted_at
                   FROM notes WHERE deleted_at IS NULL"
        .to_string();
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
        let deleted_at = row.get::<Option<i64>>(4)?;
        let labels = crate::note_labels::labels_for_note(conn, &id).await?;

        notes.push(NoteListItem {
            id,
            title,
            labels,
            created_at,
            updated_at,
            deleted_at,
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

pub async fn list_deleted_note_summaries(conn: &Connection) -> anyhow::Result<Vec<NoteListItem>> {
    let mut rows = conn
        .query(
            "SELECT id, title, created_at, updated_at, deleted_at
             FROM notes
             WHERE deleted_at IS NOT NULL
             ORDER BY deleted_at DESC, id",
            (),
        )
        .await?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next().await? {
        let id = row.get::<String>(0)?;
        notes.push(NoteListItem {
            title: row.get::<String>(1)?,
            created_at: row.get::<i64>(2)?,
            updated_at: row.get::<i64>(3)?,
            deleted_at: row.get::<Option<i64>>(4)?,
            labels: crate::note_labels::labels_for_note(conn, &id).await?,
            id,
        });
    }
    Ok(notes)
}

pub async fn count_notes(conn: &Connection, selectors: &[LabelSelector]) -> anyhow::Result<usize> {
    if selectors.is_empty() {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM notes WHERE deleted_at IS NULL", ())
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(0);
        };
        return Ok(row.get::<i64>(0)?.max(0) as usize);
    }

    Ok(list_note_summaries(conn, selectors, None, None)
        .await?
        .len())
}
