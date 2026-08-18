use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_core::{Label, LabelKey, LabelValueType};
use note_storage::{
    LabelRepository, LabelValueCount, StorageError, StorageErrorKind, StorageResult,
};
use serde_json::json;
use std::collections::HashSet;
use std::str::FromStr;

#[async_trait::async_trait]
impl LabelRepository for PgSession {
    async fn insert_label_key(&self, key: &str, description: &str) -> StorageResult<()> {
        self.insert_label_key_with_type(key, description, LabelValueType::Text)
            .await
    }

    async fn insert_label_key_if_missing(&self, key: &str, description: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO label_keys (key, description, value_type)
             VALUES ($1, $2, 'text')
             ON CONFLICT (key) DO NOTHING",
        )
        .bind(key)
        .bind(description)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert missing label key", error))?;
        Ok(())
    }

    async fn insert_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: LabelValueType,
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO label_keys (key, description, value_type)
             VALUES ($1, $2, $3)",
        )
        .bind(key)
        .bind(description)
        .bind(value_type.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert label key", error))?;
        Ok(())
    }

    async fn list_label_keys(&self) -> StorageResult<Vec<LabelKey>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT key, description, value_type
             FROM label_keys
             ORDER BY key",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query label keys", error))?;

        rows.into_iter()
            .map(|(key, description, value_type)| {
                Ok(LabelKey {
                    key,
                    description,
                    value_type: parse_value_type(&value_type)?,
                })
            })
            .collect()
    }

    async fn update_label_key(&self, key: &str, description: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query("UPDATE label_keys SET description = $2 WHERE key = $1")
            .bind(key)
            .bind(description)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("update label key", error))?;
        Ok(())
    }

    async fn update_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: LabelValueType,
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "UPDATE label_keys
             SET description = $2, value_type = $3
             WHERE key = $1",
        )
        .bind(key)
        .bind(description)
        .bind(value_type.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update label key and type", error))?;
        Ok(())
    }

    async fn delete_label_key(&self, key: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query("DELETE FROM label_keys WHERE key = $1")
            .bind(key)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("delete label key", error))?;
        Ok(())
    }

    async fn attach_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "INSERT INTO note_labels (note_id, label_key_id, value)
             SELECT $1, id, $3 FROM label_keys WHERE key = $2",
        )
        .bind(note_id)
        .bind(key)
        .bind(value)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("attach label", error))?;
        if result.rows_affected() == 0 {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                format!("unknown label key: {key}"),
            ));
        }
        Ok(())
    }

    async fn set_note_label(&self, note_id: &str, key: &str, value: &str) -> StorageResult<bool> {
        let mut connection = self.connection().await?;
        let label_key_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM label_keys WHERE key = $1")
                .bind(key)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query label key for note label", error))?;
        let label_key_id = label_key_id.ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                format!("unknown label key: {key}"),
            )
        })?;
        let result = sqlx::query(
            "INSERT INTO note_labels (note_id, label_key_id, value)
             VALUES ($1, $2, $3)
             ON CONFLICT(note_id, label_key_id) DO UPDATE
             SET value = EXCLUDED.value
             WHERE note_labels.value IS DISTINCT FROM EXCLUDED.value",
        )
        .bind(note_id)
        .bind(label_key_id)
        .bind(value)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("set note label", error))?;
        Ok(result.rows_affected() > 0)
    }

    async fn labels_for_note(&self, note_id: &str) -> StorageResult<Vec<Label>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT lk.key, nl.value, lk.description, lk.value_type
             FROM note_labels nl
             JOIN label_keys lk ON lk.id = nl.label_key_id
             WHERE nl.note_id = $1
             ORDER BY lk.key",
        )
        .bind(note_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note labels", error))?;

        rows.into_iter()
            .map(|(key, value, description, value_type)| {
                Ok(Label {
                    key,
                    value,
                    description,
                    value_type: parse_value_type(&value_type)?,
                })
            })
            .collect()
    }

    async fn label_note_counts(&self) -> StorageResult<Vec<(String, usize)>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT lk.key, COUNT(n.id)::bigint
             FROM label_keys lk
             LEFT JOIN note_labels nl ON nl.label_key_id = lk.id
             LEFT JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
             GROUP BY lk.id, lk.key
             ORDER BY lk.key",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query label note counts", error))?;
        rows.into_iter()
            .map(|(key, count)| Ok((key, checked_label_count(count)?)))
            .collect()
    }

    async fn label_value_counts(&self, keys: &[String]) -> StorageResult<Vec<LabelValueCount>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT lk.key, nl.value, COUNT(n.id)::bigint
             FROM label_keys lk
             JOIN note_labels nl ON nl.label_key_id = lk.id
             JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
             WHERE lk.key = ANY($1)
             GROUP BY lk.key, nl.value
             ORDER BY lk.key COLLATE \"C\", COUNT(n.id) DESC, nl.value COLLATE \"C\"",
        )
        .bind(keys.to_vec())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query label value counts", error))?;
        rows.into_iter()
            .map(|(key, value, count)| {
                Ok(LabelValueCount {
                    key,
                    value,
                    count: checked_label_count(count)?,
                })
            })
            .collect()
    }

    async fn find_note_with_labels(
        &self,
        labels: &[(String, String)],
    ) -> StorageResult<Option<String>> {
        if labels.is_empty() {
            return Ok(None);
        }

        let mut unique = HashSet::with_capacity(labels.len());
        if labels
            .iter()
            .any(|(key, value)| !unique.insert((key.as_str(), value.as_str())))
        {
            return Ok(None);
        }

        let requested = labels
            .iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect::<Vec<_>>();
        let mut connection = self.connection().await?;
        sqlx::query_scalar(
            "WITH requested AS (
                 SELECT key, value
                 FROM jsonb_to_recordset($1::jsonb) AS item(key text, value text)
             )
             SELECT nl.note_id
             FROM requested r
             JOIN label_keys lk ON lk.key = r.key
             JOIN note_labels nl ON nl.label_key_id = lk.id AND nl.value = r.value
             JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
             GROUP BY nl.note_id
             HAVING COUNT(*) = (SELECT COUNT(*) FROM requested)
             ORDER BY nl.note_id
             LIMIT 1",
        )
        .bind(serde_json::Value::Array(requested))
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note by exact labels", error))
    }
}

fn checked_label_count(count: i64) -> StorageResult<usize> {
    usize::try_from(count).map_err(|_| {
        StorageError::new(
            StorageErrorKind::Operation,
            "label value count is outside the supported range",
        )
    })
}

fn parse_value_type(value: &str) -> StorageResult<LabelValueType> {
    LabelValueType::from_str(value)
        .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))
}
