use libsql::Connection;
use note_core::{Label, LabelValueType};
use std::str::FromStr;

pub async fn attach_label(
    conn: &Connection,
    note_id: &str,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    let label_key_id: i64 = {
        let mut rows = conn
            .query(
                "SELECT id FROM label_keys WHERE key = ?1",
                libsql::params![key],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown label key: {key}"))?;
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
            "SELECT lk.key, nl.value, lk.description, lk.value_type
             FROM note_labels nl JOIN label_keys lk ON lk.id = nl.label_key_id
             WHERE nl.note_id = ?1",
            libsql::params![note_id],
        )
        .await?;
    let mut labels = vec![];
    while let Some(row) = rows.next().await? {
        let value_type = row.get::<String>(3)?;
        labels.push(Label {
            key: row.get::<String>(0)?,
            value: row.get::<String>(1)?,
            description: row.get::<String>(2)?,
            value_type: LabelValueType::from_str(&value_type)
                .map_err(|err| anyhow::anyhow!(err))?,
        });
    }
    Ok(labels)
}

pub async fn label_note_counts(conn: &Connection) -> anyhow::Result<Vec<(String, usize)>> {
    let mut rows = conn
        .query(
            "SELECT lk.key, COUNT(nl.note_id)
             FROM label_keys lk
             LEFT JOIN note_labels nl ON nl.label_key_id = lk.id
             GROUP BY lk.id, lk.key
             ORDER BY lk.key",
            (),
        )
        .await?;
    let mut counts = Vec::new();
    while let Some(row) = rows.next().await? {
        counts.push((row.get::<String>(0)?, row.get::<i64>(1)?.max(0) as usize));
    }
    Ok(counts)
}
