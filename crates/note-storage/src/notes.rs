use libsql::Connection;
use note_core::Note;

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
