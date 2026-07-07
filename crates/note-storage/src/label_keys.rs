use libsql::Connection;
use note_core::LabelKey;

pub async fn insert_label_key(
    conn: &Connection,
    key: &str,
    description: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO label_keys (key, description) VALUES (?1, ?2)",
        libsql::params![key, description],
    )
    .await?;
    Ok(())
}

pub async fn list_label_keys(conn: &Connection) -> anyhow::Result<Vec<LabelKey>> {
    let mut rows = conn
        .query("SELECT key, description FROM label_keys ORDER BY key", ())
        .await?;
    let mut keys = vec![];
    while let Some(row) = rows.next().await? {
        keys.push(LabelKey {
            key: row.get::<String>(0)?,
            description: row.get::<String>(1)?,
        });
    }
    Ok(keys)
}

pub async fn update_label_key(
    conn: &Connection,
    key: &str,
    description: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE label_keys SET description = ?2 WHERE key = ?1",
        libsql::params![key, description],
    )
    .await?;
    Ok(())
}

pub async fn delete_label_key(conn: &Connection, key: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM label_keys WHERE key = ?1",
        libsql::params![key],
    )
    .await?;
    Ok(())
}
