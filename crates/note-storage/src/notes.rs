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
    if let Some(row) = rows.next().await? {
        Ok(Some(Note {
            id: row.get::<String>(0)?,
            title: row.get::<String>(1)?,
            content: row.get::<String>(2)?,
            labels: vec![],
            created_at: row.get::<i64>(3)?,
            updated_at: row.get::<i64>(4)?,
        }))
    } else {
        Ok(None)
    }
}
