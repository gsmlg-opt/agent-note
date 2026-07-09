use libsql::Connection;
use note_core::{LabelKey, LabelValueType};
use std::str::FromStr;

pub async fn insert_label_key(
    conn: &Connection,
    key: &str,
    description: &str,
) -> anyhow::Result<()> {
    insert_label_key_with_type(conn, key, description, LabelValueType::Text).await
}

pub async fn insert_label_key_with_type(
    conn: &Connection,
    key: &str,
    description: &str,
    value_type: LabelValueType,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO label_keys (key, description, value_type) VALUES (?1, ?2, ?3)",
        libsql::params![key, description, value_type.as_str()],
    )
    .await?;
    Ok(())
}

pub async fn list_label_keys(conn: &Connection) -> anyhow::Result<Vec<LabelKey>> {
    let mut rows = conn
        .query(
            "SELECT key, description, value_type FROM label_keys ORDER BY key",
            (),
        )
        .await?;
    let mut keys = vec![];
    while let Some(row) = rows.next().await? {
        let value_type = row.get::<String>(2)?;
        keys.push(LabelKey {
            key: row.get::<String>(0)?,
            description: row.get::<String>(1)?,
            value_type: LabelValueType::from_str(&value_type)
                .map_err(|err| anyhow::anyhow!(err))?,
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

pub async fn update_label_key_with_type(
    conn: &Connection,
    key: &str,
    description: &str,
    value_type: LabelValueType,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE label_keys SET description = ?2, value_type = ?3 WHERE key = ?1",
        libsql::params![key, description, value_type.as_str()],
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
