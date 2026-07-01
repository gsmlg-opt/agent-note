use libsql::Connection;
use note_core::Label;

pub async fn attach_label(conn: &Connection, note_id: &str, key: &str, value: &str) -> anyhow::Result<()> {
    let label_key_id: i64 = {
        let mut rows = conn.query("SELECT id FROM label_keys WHERE key = ?1", libsql::params![key]).await?;
        let row = rows.next().await?.ok_or_else(|| anyhow::anyhow!("unknown label key: {key}"))?;
        row.get::<i64>(0)?
    };
    conn.execute(
        "INSERT INTO note_labels (note_id, label_key_id, value) VALUES (?1, ?2, ?3)",
        libsql::params![note_id, label_key_id, value],
    )
    .await?;
    Ok(())
}

pub async fn labels_for_note(conn: &Connection, note_id: &str) -> anyhow::Result<Vec<Label>> {
    let mut rows = conn
        .query(
            "SELECT lk.key, nl.value, lk.description
             FROM note_labels nl JOIN label_keys lk ON lk.id = nl.label_key_id
             WHERE nl.note_id = ?1",
            libsql::params![note_id],
        )
        .await?;
    let mut labels = vec![];
    while let Some(row) = rows.next().await? {
        labels.push(Label {
            key: row.get::<String>(0)?,
            value: row.get::<String>(1)?,
            description: row.get::<String>(2)?,
        });
    }
    Ok(labels)
}
