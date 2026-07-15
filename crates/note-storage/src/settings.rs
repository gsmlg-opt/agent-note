use libsql::Connection;
use note_core::SystemConfig;

const SYSTEM_CONFIG_KEY: &str = "system_config";

pub async fn get_system_config(conn: &Connection) -> anyhow::Result<SystemConfig> {
    let mut rows = conn
        .query(
            "SELECT value FROM app_settings WHERE key = ?1",
            libsql::params![SYSTEM_CONFIG_KEY],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(SystemConfig::default());
    };
    let json = row.get::<String>(0)?;
    Ok(serde_json::from_str(&json)?)
}

pub async fn set_system_config(conn: &Connection, config: &SystemConfig) -> anyhow::Result<()> {
    let json = serde_json::to_string(config)?;
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        libsql::params![SYSTEM_CONFIG_KEY, json],
    )
    .await?;
    Ok(())
}
