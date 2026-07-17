use crate::connection::map_turso_error;
use crate::TursoSession;
use note_core::{label_matches_selector, LabelSelector, Note, NoteAttachment, NoteListItem};
use note_storage::{
    ActiveNoteSource, LabelRepository, NewNote, NoteUpdate, NotesRepository, StorageError,
    StorageErrorKind, StorageResult,
};

#[async_trait::async_trait]
impl NotesRepository for TursoSession {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()> {
        let attachments = serialize_attachments(note.attachments)?;
        self.connection
            .execute(
                "INSERT INTO notes (
                     id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                turso::params![
                    note.id,
                    note.title,
                    note.content,
                    attachments,
                    note.created_at,
                    note.updated_at,
                    note.note_revision,
                    note.deleted_at
                ],
            )
            .await
            .map_err(|error| map_turso_error("insert note", error))?;
        Ok(())
    }

    async fn get_note_revision(&self, id: &str) -> StorageResult<Option<i64>> {
        let mut rows = self
            .connection
            .query(
                "SELECT note_revision FROM notes WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("query note revision", error))?;
        match rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note revision", error))?
        {
            Some(row) => row
                .get::<i64>(0)
                .map(Some)
                .map_err(|error| map_turso_error("decode note revision", error)),
            None => Ok(None),
        }
    }

    async fn note_exists(&self, id: &str) -> StorageResult<bool> {
        let mut rows = self
            .connection
            .query(
                "SELECT 1 FROM notes WHERE id = ?1 LIMIT 1",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("query note existence", error))?;
        rows.next()
            .await
            .map(|row| row.is_some())
            .map_err(|error| map_turso_error("read note existence", error))
    }

    async fn get_note(&self, id: &str) -> StorageResult<Option<Note>> {
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
                 FROM notes
                 WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("query note", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note", error))?
        else {
            return Ok(None);
        };
        let row = decode_note_row(&row)?;
        drop(rows);

        Ok(Some(Note {
            labels: self.labels_for_note(&row.id).await?,
            id: row.id,
            title: row.title,
            content: row.content,
            attachments: row.attachments,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        }))
    }

    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>> {
        let mut rows = self
            .connection
            .query(
                "SELECT content FROM notes WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("query note content", error))?;
        match rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note content", error))?
        {
            Some(row) => row
                .get::<String>(0)
                .map(Some)
                .map_err(|error| map_turso_error("decode note content", error)),
            None => Ok(None),
        }
    }

    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<u64> {
        let attachments = serialize_attachments(note.attachments)?;
        self.connection
            .execute(
                "UPDATE notes
                 SET title = ?2, content = ?3, attachments = ?4, updated_at = ?5, note_revision = ?6
                 WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![
                    note.id,
                    note.title,
                    note.content,
                    attachments,
                    note.updated_at,
                    note.note_revision
                ],
            )
            .await
            .map_err(|error| map_turso_error("update note", error))
    }

    async fn soft_delete_note(&self, id: &str, deleted_at: i64) -> StorageResult<u64> {
        self.connection
            .execute(
                "UPDATE notes SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![id, deleted_at],
            )
            .await
            .map_err(|error| map_turso_error("soft-delete note", error))
    }

    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>> {
        let mut rows = self
            .connection
            .query(
                "SELECT content, note_revision
                 FROM notes
                 WHERE id = ?1 AND deleted_at IS NOT NULL",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("query deleted note", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read deleted note", error))?
        else {
            return Ok(None);
        };
        Ok(Some((
            row.get::<String>(0)
                .map_err(|error| map_turso_error("decode deleted note content", error))?,
            row.get::<i64>(1)
                .map_err(|error| map_turso_error("decode deleted note revision", error))?,
        )))
    }

    async fn restore_note(&self, id: &str, note_revision: i64) -> StorageResult<u64> {
        self.connection
            .execute(
                "UPDATE notes
                 SET deleted_at = NULL, note_revision = ?2
                 WHERE id = ?1 AND deleted_at IS NOT NULL",
                turso::params![id, note_revision],
            )
            .await
            .map_err(|error| map_turso_error("restore note", error))
    }

    async fn permanently_delete_note(&self, id: &str) -> StorageResult<u64> {
        self.connection
            .execute(
                "DELETE FROM notes WHERE id = ?1 AND deleted_at IS NOT NULL",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("permanently delete note", error))
    }

    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>> {
        let mut rows = self
            .connection
            .query(
                "SELECT id FROM notes
                 WHERE deleted_at IS NOT NULL AND deleted_at <= ?1
                 ORDER BY deleted_at, id",
                turso::params![cutoff],
            )
            .await
            .map_err(|error| map_turso_error("query expired deleted notes", error))?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read expired deleted notes", error))?
        {
            ids.push(
                row.get::<String>(0)
                    .map_err(|error| map_turso_error("decode expired deleted note id", error))?,
            );
        }
        Ok(ids)
    }

    async fn clear_note_search_data(&self, id: &str) -> StorageResult<()> {
        for (sql, context) in [
            (
                "DELETE FROM note_chunk_embeddings WHERE note_id = ?1",
                "clear note embeddings",
            ),
            (
                "DELETE FROM embedding_jobs WHERE note_id = ?1",
                "clear note embedding jobs",
            ),
            (
                "DELETE FROM note_chunks WHERE note_id = ?1",
                "clear note chunks",
            ),
        ] {
            self.connection
                .execute(sql, turso::params![id])
                .await
                .map_err(|error| map_turso_error(context, error))?;
        }
        Ok(())
    }

    async fn clear_note_labels(&self, id: &str) -> StorageResult<()> {
        self.connection
            .execute(
                "DELETE FROM note_labels WHERE note_id = ?1",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("clear note labels", error))?;
        Ok(())
    }

    async fn clear_note_chunk_derived(&self, id: &str, chunk_idx: i64) -> StorageResult<()> {
        self.connection
            .execute(
                "DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx = ?2",
                turso::params![id, chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("clear note chunk embedding", error))?;
        Ok(())
    }

    async fn clear_note_chunks_from_derived(
        &self,
        id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<()> {
        self.connection
            .execute(
                "DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx >= ?2",
                turso::params![id, min_chunk_idx],
            )
            .await
            .map_err(|error| map_turso_error("clear trailing note chunk embeddings", error))?;
        Ok(())
    }

    async fn list_notes(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<Note>> {
        let mut sql = "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
                       FROM notes WHERE deleted_at IS NULL"
            .to_string();
        let mut params = Vec::<turso::Value>::new();

        sql.push_str(" ORDER BY created_at DESC");
        if selectors.is_empty() && (limit.is_some() || offset.is_some()) {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(limit.unwrap_or(-1).into());
            params.push(offset.unwrap_or(0).into());
        }

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query notes", error))?;
        let mut decoded = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read notes", error))?
        {
            decoded.push(decode_note_row(&row)?);
        }
        drop(rows);

        let mut notes = Vec::with_capacity(decoded.len());
        for row in decoded {
            notes.push(Note {
                labels: self.labels_for_note(&row.id).await?,
                id: row.id,
                title: row.title,
                content: row.content,
                attachments: row.attachments,
                created_at: row.created_at,
                updated_at: row.updated_at,
                deleted_at: row.deleted_at,
            });
        }
        if !selectors.is_empty() {
            notes.retain(|note| note_matches_selectors(note, selectors));
            notes = paginate(notes, limit, offset);
        }
        Ok(notes)
    }

    async fn list_all_notes(&self) -> StorageResult<Vec<Note>> {
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, content, attachments, created_at, updated_at, deleted_at
                 FROM notes
                 ORDER BY created_at DESC",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query all notes", error))?;
        let mut decoded = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read all notes", error))?
        {
            decoded.push(decode_note_row(&row)?);
        }
        drop(rows);

        let mut notes = Vec::with_capacity(decoded.len());
        for row in decoded {
            notes.push(Note {
                labels: self.labels_for_note(&row.id).await?,
                id: row.id,
                title: row.title,
                content: row.content,
                attachments: row.attachments,
                created_at: row.created_at,
                updated_at: row.updated_at,
                deleted_at: row.deleted_at,
            });
        }
        Ok(notes)
    }

    async fn list_note_summaries(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<NoteListItem>> {
        let mut sql = "SELECT id, title, created_at, updated_at, deleted_at
                       FROM notes WHERE deleted_at IS NULL"
            .to_string();
        let mut params = Vec::<turso::Value>::new();

        sql.push_str(" ORDER BY created_at DESC");
        if selectors.is_empty() && (limit.is_some() || offset.is_some()) {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(limit.unwrap_or(-1).into());
            params.push(offset.unwrap_or(0).into());
        }

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query note summaries", error))?;
        let mut decoded = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note summaries", error))?
        {
            decoded.push(decode_summary_row(&row)?);
        }
        drop(rows);

        let mut notes = Vec::with_capacity(decoded.len());
        for row in decoded {
            notes.push(NoteListItem {
                labels: self.labels_for_note(&row.id).await?,
                id: row.id,
                title: row.title,
                created_at: row.created_at,
                updated_at: row.updated_at,
                deleted_at: row.deleted_at,
            });
        }
        if !selectors.is_empty() {
            notes.retain(|note| summary_matches_selectors(note, selectors));
            notes = paginate(notes, limit, offset);
        }
        Ok(notes)
    }

    async fn list_deleted_note_summaries(&self) -> StorageResult<Vec<NoteListItem>> {
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, created_at, updated_at, deleted_at
                 FROM notes
                 WHERE deleted_at IS NOT NULL
                 ORDER BY deleted_at DESC, id",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query deleted note summaries", error))?;
        let mut decoded = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read deleted note summaries", error))?
        {
            decoded.push(decode_summary_row(&row)?);
        }
        drop(rows);

        let mut notes = Vec::with_capacity(decoded.len());
        for row in decoded {
            notes.push(NoteListItem {
                labels: self.labels_for_note(&row.id).await?,
                id: row.id,
                title: row.title,
                created_at: row.created_at,
                updated_at: row.updated_at,
                deleted_at: row.deleted_at,
            });
        }
        Ok(notes)
    }

    async fn count_notes(&self, selectors: &[LabelSelector]) -> StorageResult<usize> {
        if !selectors.is_empty() {
            return Ok(self.list_note_summaries(selectors, None, None).await?.len());
        }

        let mut rows = self
            .connection
            .query("SELECT COUNT(*) FROM notes WHERE deleted_at IS NULL", ())
            .await
            .map_err(|error| map_turso_error("count notes", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note count", error))?
        else {
            return Ok(0);
        };
        Ok(row
            .get::<i64>(0)
            .map_err(|error| map_turso_error("decode note count", error))?
            .max(0) as usize)
    }

    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>> {
        let mut rows = self
            .connection
            .query(
                "SELECT id, content, note_revision
                 FROM notes
                 WHERE deleted_at IS NULL
                 ORDER BY id",
                (),
            )
            .await
            .map_err(|error| map_turso_error("query active note sources", error))?;
        let mut sources = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read active note sources", error))?
        {
            sources.push(ActiveNoteSource {
                id: row
                    .get::<String>(0)
                    .map_err(|error| map_turso_error("decode active note source id", error))?,
                content: row
                    .get::<String>(1)
                    .map_err(|error| map_turso_error("decode active note source content", error))?,
                note_revision: row.get::<i64>(2).map_err(|error| {
                    map_turso_error("decode active note source revision", error)
                })?,
            });
        }
        Ok(sources)
    }
}

struct NoteRow {
    id: String,
    title: String,
    content: String,
    attachments: Vec<NoteAttachment>,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
}

fn decode_note_row(row: &turso::Row) -> StorageResult<NoteRow> {
    Ok(NoteRow {
        id: row
            .get::<String>(0)
            .map_err(|error| map_turso_error("decode note id", error))?,
        title: row
            .get::<String>(1)
            .map_err(|error| map_turso_error("decode note title", error))?,
        content: row
            .get::<String>(2)
            .map_err(|error| map_turso_error("decode note content", error))?,
        attachments: deserialize_attachments(
            &row.get::<String>(3)
                .map_err(|error| map_turso_error("decode note attachments", error))?,
        )?,
        created_at: row
            .get::<i64>(4)
            .map_err(|error| map_turso_error("decode note creation time", error))?,
        updated_at: row
            .get::<i64>(5)
            .map_err(|error| map_turso_error("decode note update time", error))?,
        deleted_at: row
            .get::<Option<i64>>(6)
            .map_err(|error| map_turso_error("decode note deletion time", error))?,
    })
}

struct SummaryRow {
    id: String,
    title: String,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
}

fn decode_summary_row(row: &turso::Row) -> StorageResult<SummaryRow> {
    Ok(SummaryRow {
        id: row
            .get::<String>(0)
            .map_err(|error| map_turso_error("decode note summary id", error))?,
        title: row
            .get::<String>(1)
            .map_err(|error| map_turso_error("decode note summary title", error))?,
        created_at: row
            .get::<i64>(2)
            .map_err(|error| map_turso_error("decode note summary creation time", error))?,
        updated_at: row
            .get::<i64>(3)
            .map_err(|error| map_turso_error("decode note summary update time", error))?,
        deleted_at: row
            .get::<Option<i64>>(4)
            .map_err(|error| map_turso_error("decode note summary deletion time", error))?,
    })
}

fn serialize_attachments(attachments: &[NoteAttachment]) -> StorageResult<String> {
    let metadata = attachments
        .iter()
        .map(|attachment| StoredAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&metadata).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "serialize note attachment metadata",
            error,
        )
    })
}

fn deserialize_attachments(value: &str) -> StorageResult<Vec<NoteAttachment>> {
    let metadata: Vec<StoredAttachment> = serde_json::from_str(value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "deserialize note attachment metadata",
            error,
        )
    })?;
    Ok(metadata
        .into_iter()
        .map(|attachment| NoteAttachment {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
            content: Vec::new(),
        })
        .collect())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredAttachment {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
}

fn note_matches_selectors(note: &Note, selectors: &[LabelSelector]) -> bool {
    selectors.iter().all(|selector| {
        note.labels
            .iter()
            .any(|label| label_matches_selector(label, selector))
    })
}

fn summary_matches_selectors(note: &NoteListItem, selectors: &[LabelSelector]) -> bool {
    selectors.iter().all(|selector| {
        note.labels
            .iter()
            .any(|label| label_matches_selector(label, selector))
    })
}

fn paginate<T>(items: Vec<T>, limit: Option<i64>, offset: Option<i64>) -> Vec<T> {
    let start = offset.unwrap_or(0).max(0) as usize;
    if start >= items.len() {
        return Vec::new();
    }
    let end = limit
        .and_then(|limit| {
            if limit < 0 {
                None
            } else {
                Some(start.saturating_add(limit as usize))
            }
        })
        .unwrap_or(items.len())
        .min(items.len());
    items.into_iter().skip(start).take(end - start).collect()
}
