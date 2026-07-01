use libsql::{Builder, Connection, Database};

const SCHEMA: &str = include_str!("../schema.sql");

pub struct Storage {
    db: Database,
}

impl Storage {
    pub async fn open_local(path: &str) -> anyhow::Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        Self::apply_schema(&conn).await?;
        Ok(Self { db })
    }

    // schema.sql must stay free of embedded semicolons (no comments, no string/default literals
    // containing `;`, no multi-statement trigger bodies) since statements are split on `;` here.
    // All CREATE TABLE/INDEX statements must also stay IF NOT EXISTS, since open_local is called
    // against the same persistent DB file on every note-server process start.
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
