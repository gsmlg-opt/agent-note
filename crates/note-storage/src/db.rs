use libsql::{Builder, Connection, Database};
use std::time::Duration;

const SCHEMA: &str = include_str!("../schema.sql");
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Storage {
    db: Database,
}

impl Storage {
    pub async fn open_local(path: &str) -> anyhow::Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
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
        Self::apply_migrations(conn).await?;
        Ok(())
    }

    async fn apply_migrations(conn: &Connection) -> anyhow::Result<()> {
        if !Self::column_exists(conn, "label_keys", "value_type").await? {
            conn.execute(
                "ALTER TABLE label_keys ADD COLUMN value_type TEXT NOT NULL DEFAULT 'text'",
                (),
            )
            .await?;
        }
        if !Self::column_exists(conn, "notes", "note_revision").await? {
            conn.execute(
                "ALTER TABLE notes ADD COLUMN note_revision INTEGER NOT NULL DEFAULT 1",
                (),
            )
            .await?;
        }
        if !Self::column_exists(conn, "notes", "attachments").await? {
            conn.execute(
                "ALTER TABLE notes ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
                (),
            )
            .await?;
        }
        if !Self::column_exists(conn, "note_chunks", "note_revision").await? {
            conn.execute(
                "ALTER TABLE note_chunks ADD COLUMN note_revision INTEGER NOT NULL DEFAULT 1",
                (),
            )
            .await?;
        }
        if !Self::column_exists(conn, "embedding_jobs", "note_revision").await? {
            conn.execute(
                "ALTER TABLE embedding_jobs ADD COLUMN note_revision INTEGER NOT NULL DEFAULT 1",
                (),
            )
            .await?;
        }
        Ok(())
    }

    async fn column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
        let mut rows = conn
            .query(&format!("PRAGMA table_info({table})"), ())
            .await?;
        while let Some(row) = rows.next().await? {
            let name = row.get::<String>(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn connect(&self) -> anyhow::Result<Connection> {
        let conn = self.db.connect()?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Ok(conn)
    }
}
