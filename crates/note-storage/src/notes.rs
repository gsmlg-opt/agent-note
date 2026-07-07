use libsql::Connection;
use note_core::{LabelSelector, Note};

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
        "DELETE FROM notes_embeddings WHERE note_id = ?1",
        libsql::params![id],
    )
    .await?;
    conn.execute(
        "DELETE FROM notes_sparse_weights WHERE note_id = ?1",
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

pub async fn list_notes(
    conn: &Connection,
    selectors: &[LabelSelector],
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Vec<Note>> {
    let mut sql = "SELECT id, title, content, created_at, updated_at FROM notes".to_string();
    let mut params = Vec::<libsql::Value>::new();

    if !selectors.is_empty() {
        sql.push_str(" WHERE ");
        for (idx, selector) in selectors.iter().enumerate() {
            if idx > 0 {
                sql.push_str(" AND ");
            }
            sql.push_str(
                "EXISTS (
                    SELECT 1
                    FROM note_labels nl
                    JOIN label_keys lk ON lk.id = nl.label_key_id
                    WHERE nl.note_id = notes.id AND lk.key = ?",
            );
            params.push(selector.key.clone().into());
            if let Some(value) = &selector.value {
                sql.push_str(" AND nl.value = ?");
                params.push(value.clone().into());
            }
            sql.push(')');
        }
    }

    sql.push_str(" ORDER BY created_at DESC");

    if limit.is_some() || offset.is_some() {
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
    Ok(notes)
}
