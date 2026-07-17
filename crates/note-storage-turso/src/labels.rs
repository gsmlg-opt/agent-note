use crate::connection::map_turso_error;
use crate::TursoSession;
use note_core::{Label, LabelKey, LabelValueType};
use note_storage::{LabelRepository, StorageError, StorageErrorKind, StorageResult};
use std::str::FromStr;

#[async_trait::async_trait]
impl LabelRepository for TursoSession {
    async fn insert_label_key(&self, key: &str, description: &str) -> StorageResult<()> {
        self.insert_label_key_with_type(key, description, LabelValueType::Text)
            .await
    }

    async fn insert_label_key_if_missing(&self, key: &str, description: &str) -> StorageResult<()> {
        self.connection
            .execute(
                "INSERT INTO label_keys (key, description, value_type) VALUES (?1, ?2, 'text')
                 ON CONFLICT(key) DO NOTHING",
                turso::params![key, description],
            )
            .await
            .map_err(|error| map_turso_error("insert missing label key", error))?;
        Ok(())
    }

    async fn insert_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: LabelValueType,
    ) -> StorageResult<()> {
        self.connection
            .execute(
                "INSERT INTO label_keys (key, description, value_type) VALUES (?1, ?2, ?3)",
                turso::params![key, description, value_type.as_str()],
            )
            .await
            .map_err(|error| map_turso_error("insert label key", error))?;
        Ok(())
    }

    async fn list_label_keys(&self) -> StorageResult<Vec<LabelKey>> {
        let mut rows = self
            .connection
            .query(
                "SELECT key, description, value_type FROM label_keys ORDER BY key",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query label keys", error))?;
        let mut keys = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read label keys", error))?
        {
            let value_type = row
                .get::<String>(2)
                .map_err(|error| map_turso_error("decode label key value type", error))?;
            keys.push(LabelKey {
                key: row
                    .get::<String>(0)
                    .map_err(|error| map_turso_error("decode label key", error))?,
                description: row
                    .get::<String>(1)
                    .map_err(|error| map_turso_error("decode label key description", error))?,
                value_type: parse_value_type(&value_type)?,
            });
        }
        Ok(keys)
    }

    async fn update_label_key(&self, key: &str, description: &str) -> StorageResult<()> {
        self.connection
            .execute(
                "UPDATE label_keys SET description = ?2 WHERE key = ?1",
                turso::params![key, description],
            )
            .await
            .map_err(|error| map_turso_error("update label key", error))?;
        Ok(())
    }

    async fn update_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: LabelValueType,
    ) -> StorageResult<()> {
        self.connection
            .execute(
                "UPDATE label_keys SET description = ?2, value_type = ?3 WHERE key = ?1",
                turso::params![key, description, value_type.as_str()],
            )
            .await
            .map_err(|error| map_turso_error("update label key and type", error))?;
        Ok(())
    }

    async fn delete_label_key(&self, key: &str) -> StorageResult<()> {
        self.connection
            .execute("DELETE FROM label_keys WHERE key = ?1", turso::params![key])
            .await
            .map_err(|error| map_turso_error("delete label key", error))?;
        Ok(())
    }

    async fn attach_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<()> {
        let label_key_id = {
            let mut rows = self
                .connection
                .query(
                    "SELECT id FROM label_keys WHERE key = ?1",
                    turso::params![key],
                )
                .await
                .map_err(|error| map_turso_error("query label key for attachment", error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read label key for attachment", error))?
                .ok_or_else(|| {
                    StorageError::new(
                        StorageErrorKind::Operation,
                        format!("unknown label key: {key}"),
                    )
                })?;
            row.get::<i64>(0)
                .map_err(|error| map_turso_error("decode label key id", error))?
        };

        self.connection
            .execute(
                "INSERT INTO note_labels (note_id, label_key_id, value) VALUES (?1, ?2, ?3)",
                turso::params![note_id, label_key_id, value],
            )
            .await
            .map_err(|error| map_turso_error("attach label", error))?;
        Ok(())
    }

    async fn labels_for_note(&self, note_id: &str) -> StorageResult<Vec<Label>> {
        let mut rows = self
            .connection
            .query(
                "SELECT lk.key, nl.value, lk.description, lk.value_type
                 FROM note_labels nl JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = ?1",
                turso::params![note_id],
            )
            .await
            .map_err(|error| map_turso_error("query note labels", error))?;
        let mut labels = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note labels", error))?
        {
            let value_type = row
                .get::<String>(3)
                .map_err(|error| map_turso_error("decode note label value type", error))?;
            labels.push(Label {
                key: row
                    .get::<String>(0)
                    .map_err(|error| map_turso_error("decode note label key", error))?,
                value: row
                    .get::<String>(1)
                    .map_err(|error| map_turso_error("decode note label value", error))?,
                description: row
                    .get::<String>(2)
                    .map_err(|error| map_turso_error("decode note label description", error))?,
                value_type: parse_value_type(&value_type)?,
            });
        }
        Ok(labels)
    }

    async fn label_note_counts(&self) -> StorageResult<Vec<(String, usize)>> {
        let mut rows = self
            .connection
            .query(
                "SELECT lk.key, COUNT(n.id)
                 FROM label_keys lk
                 LEFT JOIN note_labels nl ON nl.label_key_id = lk.id
                 LEFT JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
                 GROUP BY lk.id, lk.key
                 ORDER BY lk.key",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query label note counts", error))?;
        let mut counts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read label note counts", error))?
        {
            counts.push((
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode counted label key", error))?,
                row.get::<i64>(1)
                    .map_err(|error| map_turso_error("decode label note count", error))?
                    .max(0) as usize,
            ));
        }
        Ok(counts)
    }

    async fn find_note_with_labels(
        &self,
        labels: &[(String, String)],
    ) -> StorageResult<Option<String>> {
        if labels.is_empty() {
            return Ok(None);
        }

        let predicates = (0..labels.len())
            .map(|index| {
                let key_param = index * 2 + 1;
                let value_param = key_param + 1;
                format!("(lk.key = ?{key_param} AND nl.value = ?{value_param})")
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let count_param = labels.len() * 2 + 1;
        let sql = format!(
            "SELECT nl.note_id
             FROM note_labels nl
             JOIN label_keys lk ON lk.id = nl.label_key_id
             JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
             WHERE {predicates}
             GROUP BY nl.note_id
             HAVING COUNT(*) = ?{count_param}
             LIMIT 1"
        );
        let mut params = Vec::<turso::Value>::with_capacity(labels.len() * 2 + 1);
        for (key, value) in labels {
            params.push(key.clone().into());
            params.push(value.clone().into());
        }
        params.push((labels.len() as i64).into());

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query note by exact labels", error))?;
        rows.next()
            .await
            .map_err(|error| map_turso_error("read note by exact labels", error))?
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode note id by exact labels", error))
            })
            .transpose()
    }
}

fn parse_value_type(value: &str) -> StorageResult<LabelValueType> {
    LabelValueType::from_str(value)
        .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))
}
