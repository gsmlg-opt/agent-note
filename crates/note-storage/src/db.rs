use libsql::{Builder, Connection, Database};

const SCHEMA: &str = include_str!("../schema.sql");

pub struct Storage {
    pub db: Database,
}

impl Storage {
    pub async fn open_local(path: &str) -> anyhow::Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        Self::apply_schema(&conn).await?;
        Ok(Self { db })
    }

    async fn apply_schema(conn: &Connection) -> anyhow::Result<()> {
        for statement in SCHEMA.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            conn.execute(statement, ()).await?;
        }
        Ok(())
    }

    pub fn connect(&self) -> anyhow::Result<Connection> {
        Ok(self.db.connect()?)
    }
}
