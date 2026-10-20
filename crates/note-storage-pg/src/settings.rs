use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_core::SystemConfig;
use note_storage::{SettingsRepository, StorageError, StorageErrorKind, StorageResult};
use serde_json::Value;

const EMBEDDING_FINGERPRINT_KEY: &str = "embedding_fingerprint";

#[async_trait::async_trait]
impl SettingsRepository for PgSession {
    async fn get_sync_record(
        &self,
        namespace: &str,
        key: &str,
    ) -> StorageResult<Option<serde_json::Value>> {
        let mut conn = self.connection().await?;
        sqlx::query_scalar("SELECT value FROM sync_records WHERE namespace=$1 AND record_key=$2")
            .bind(namespace)
            .bind(key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| map_sqlx_error("read sync record", e))
    }
    async fn put_sync_record(
        &self,
        namespace: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> StorageResult<()> {
        let mut conn = self.connection().await?;
        sqlx::query("INSERT INTO sync_records(namespace,record_key,value) VALUES($1,$2,$3) ON CONFLICT(namespace,record_key) DO UPDATE SET value=EXCLUDED.value").bind(namespace).bind(key).bind(value).execute(&mut *conn).await.map_err(|e|map_sqlx_error("store sync record",e))?;
        Ok(())
    }
    async fn delete_sync_record(&self, namespace: &str, key: &str) -> StorageResult<()> {
        let mut conn = self.connection().await?;
        sqlx::query("DELETE FROM sync_records WHERE namespace=$1 AND record_key=$2")
            .bind(namespace)
            .bind(key)
            .execute(&mut *conn)
            .await
            .map_err(|e| map_sqlx_error("delete sync record", e))?;
        Ok(())
    }
    async fn list_sync_records(
        &self,
        namespace: &str,
        after_key: Option<&str>,
        limit: usize,
    ) -> StorageResult<Vec<(String, serde_json::Value)>> {
        let mut conn = self.connection().await?;
        sqlx::query_as("SELECT record_key,value FROM sync_records WHERE namespace=$1 AND record_key>$2 ORDER BY record_key LIMIT $3").bind(namespace).bind(after_key.unwrap_or("")).bind(limit.min(1000) as i64).fetch_all(&mut *conn).await.map_err(|e|map_sqlx_error("scan sync records",e))
    }

    async fn list_sync_audit_records(
        &self,
        after_key: Option<&str>,
        limit: usize,
    ) -> StorageResult<Vec<(String, serde_json::Value)>> {
        let mut conn = self.connection().await?;
        sqlx::query_as(
            "SELECT m.record_key,m.value FROM sync_records m
             WHERE m.namespace='member' AND m.record_key>$1
             AND NOT EXISTS (SELECT 1 FROM sync_records p
               WHERE p.namespace='pending' AND p.record_key=m.record_key
               AND p.value->'event'=m.value->'event')
             ORDER BY m.record_key LIMIT $2",
        )
        .bind(after_key.unwrap_or(""))
        .bind(limit.min(1000) as i64)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| map_sqlx_error("scan sync audit records", e))
    }

    async fn get_system_config(&self) -> StorageResult<SystemConfig> {
        let mut connection = self.connection().await?;
        let value: Option<Value> =
            sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'system_config'")
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query system config", error))?;
        let Some(value) = value else {
            return Ok(SystemConfig::default());
        };
        serde_json::from_value(value).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "deserialize system config",
                error,
            )
        })
    }

    async fn set_system_config(&self, config: &SystemConfig) -> StorageResult<()> {
        let value = serde_json::to_value(config).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize system config",
                error,
            )
        })?;
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO app_settings (key, value)
             VALUES ('system_config', $1)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(value)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("store system config", error))?;
        Ok(())
    }

    async fn get_embedding_fingerprint(&self) -> StorageResult<Option<String>> {
        let mut connection = self.connection().await?;
        let value: Option<Value> =
            sqlx::query_scalar("SELECT value FROM app_settings WHERE key = $1")
                .bind(EMBEDDING_FINGERPRINT_KEY)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query embedding fingerprint", error))?;
        value
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    StorageError::new(
                        StorageErrorKind::Operation,
                        "decode stored embedding fingerprint",
                    )
                })
            })
            .transpose()
    }

    async fn set_embedding_fingerprint(&self, fingerprint: &str) -> StorageResult<()> {
        if fingerprint.trim().is_empty() {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                "embedding fingerprint must not be blank",
            ));
        }
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO app_settings (key, value) VALUES ($1, $2)
             ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(EMBEDDING_FINGERPRINT_KEY)
        .bind(Value::String(fingerprint.to_owned()))
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("store embedding fingerprint", error))?;
        Ok(())
    }
}
