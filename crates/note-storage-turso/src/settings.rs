use crate::connection::map_turso_error;
use crate::TursoSession;
use note_core::SystemConfig;
use note_storage::{SettingsRepository, StorageError, StorageErrorKind, StorageResult};

const SYSTEM_CONFIG_KEY: &str = "system_config";
const EMBEDDING_FINGERPRINT_KEY: &str = "embedding_fingerprint";

#[async_trait::async_trait]
impl SettingsRepository for TursoSession {
    async fn get_sync_record(
        &self,
        namespace: &str,
        key: &str,
    ) -> StorageResult<Option<serde_json::Value>> {
        let _guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT value FROM sync_records WHERE namespace=?1 AND record_key=?2",
                turso::params![namespace, key],
            )
            .await
            .map_err(|e| map_turso_error("read sync record", e))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| map_turso_error("read sync row", e))?
        else {
            return Ok(None);
        };
        let value = row
            .get::<String>(0)
            .map_err(|e| map_turso_error("decode sync row", e))?;
        serde_json::from_str(&value).map(Some).map_err(|e| {
            StorageError::with_source(StorageErrorKind::Corrupt, "decode sync JSON", e)
        })
    }
    async fn put_sync_record(
        &self,
        namespace: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> StorageResult<()> {
        let _guard = self.operation_guard().await;
        self.connection.execute("INSERT INTO sync_records(namespace,record_key,value) VALUES(?1,?2,?3) ON CONFLICT(namespace,record_key) DO UPDATE SET value=excluded.value",turso::params![namespace,key,value.to_string()]).await.map_err(|e|map_turso_error("store sync record",e))?;
        Ok(())
    }
    async fn delete_sync_record(&self, namespace: &str, key: &str) -> StorageResult<()> {
        let _guard = self.operation_guard().await;
        self.connection
            .execute(
                "DELETE FROM sync_records WHERE namespace=?1 AND record_key=?2",
                turso::params![namespace, key],
            )
            .await
            .map_err(|e| map_turso_error("delete sync record", e))?;
        Ok(())
    }
    async fn list_sync_records(
        &self,
        namespace: &str,
        after_key: Option<&str>,
        limit: usize,
    ) -> StorageResult<Vec<(String, serde_json::Value)>> {
        let _guard = self.operation_guard().await;
        let mut rows=self.connection.query("SELECT record_key,value FROM sync_records WHERE namespace=?1 AND record_key>?2 ORDER BY record_key LIMIT ?3",turso::params![namespace,after_key.unwrap_or(""),limit.min(1000) as i64]).await.map_err(|e|map_turso_error("scan sync records",e))?;
        let mut records = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| map_turso_error("read sync records", e))?
        {
            let key = row
                .get::<String>(0)
                .map_err(|e| map_turso_error("decode sync key", e))?;
            let value = row
                .get::<String>(1)
                .map_err(|e| map_turso_error("decode sync value", e))?;
            records.push((
                key,
                serde_json::from_str(&value).map_err(|e| {
                    StorageError::with_source(StorageErrorKind::Corrupt, "decode sync JSON", e)
                })?,
            ));
        }
        Ok(records)
    }

    async fn list_sync_audit_records(
        &self,
        after_key: Option<&str>,
        limit: usize,
    ) -> StorageResult<Vec<(String, serde_json::Value)>> {
        let _guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT m.record_key,m.value FROM sync_records m
             WHERE m.namespace='member' AND m.record_key>?1
             AND NOT EXISTS (SELECT 1 FROM sync_records p
               WHERE p.namespace='pending' AND p.record_key=m.record_key
               AND json_extract(p.value,'$.event')=json_extract(m.value,'$.event'))
             ORDER BY m.record_key LIMIT ?2",
                turso::params![after_key.unwrap_or(""), limit.min(1000) as i64],
            )
            .await
            .map_err(|e| map_turso_error("scan sync audit records", e))?;
        let mut records = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| map_turso_error("read sync audit records", e))?
        {
            let key = row
                .get::<String>(0)
                .map_err(|e| map_turso_error("decode sync key", e))?;
            let value = row
                .get::<String>(1)
                .map_err(|e| map_turso_error("decode sync value", e))?;
            records.push((
                key,
                serde_json::from_str(&value).map_err(|e| {
                    StorageError::with_source(StorageErrorKind::Corrupt, "decode sync JSON", e)
                })?,
            ));
        }
        Ok(records)
    }

    async fn get_system_config(&self) -> StorageResult<SystemConfig> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT value FROM app_settings WHERE key = ?1",
                turso::params![SYSTEM_CONFIG_KEY],
            )
            .await
            .map_err(|error| map_turso_error("query system config", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read system config", error))?
        else {
            return Ok(SystemConfig::default());
        };
        let json = row
            .get::<String>(0)
            .map_err(|error| map_turso_error("decode stored system config", error))?;
        serde_json::from_str(&json).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "deserialize system config",
                error,
            )
        })
    }

    async fn set_system_config(&self, config: &SystemConfig) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        let json = serde_json::to_string(config).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize system config",
                error,
            )
        })?;
        self.connection
            .execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                turso::params![SYSTEM_CONFIG_KEY, json],
            )
            .await
            .map_err(|error| map_turso_error("store system config", error))?;
        Ok(())
    }

    async fn get_embedding_fingerprint(&self) -> StorageResult<Option<String>> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT value FROM app_settings WHERE key = ?1",
                turso::params![EMBEDDING_FINGERPRINT_KEY],
            )
            .await
            .map_err(|error| map_turso_error("query embedding fingerprint", error))?;
        rows.next()
            .await
            .map_err(|error| map_turso_error("read embedding fingerprint", error))?
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode embedding fingerprint", error))
            })
            .transpose()
    }

    async fn set_embedding_fingerprint(&self, fingerprint: &str) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        if fingerprint.trim().is_empty() {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                "embedding fingerprint must not be blank",
            ));
        }
        self.connection
            .execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                turso::params![EMBEDDING_FINGERPRINT_KEY, fingerprint],
            )
            .await
            .map_err(|error| map_turso_error("store embedding fingerprint", error))?;
        Ok(())
    }
}
