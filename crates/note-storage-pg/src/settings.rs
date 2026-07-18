use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_core::SystemConfig;
use note_storage::{SettingsRepository, StorageError, StorageErrorKind, StorageResult};
use serde_json::Value;

#[async_trait::async_trait]
impl SettingsRepository for PgSession {
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
}
