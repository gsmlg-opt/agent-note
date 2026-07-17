use crate::connection::map_turso_error;
use crate::TursoSession;
use note_core::SystemConfig;
use note_storage::{SettingsRepository, StorageError, StorageErrorKind, StorageResult};

const SYSTEM_CONFIG_KEY: &str = "system_config";

#[async_trait::async_trait]
impl SettingsRepository for TursoSession {
    async fn get_system_config(&self) -> StorageResult<SystemConfig> {
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
}
