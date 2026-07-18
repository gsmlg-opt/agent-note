use crate::connection::map_sqlx_error;
use crate::PgSession;
use futures_util::TryStreamExt;
use note_core::{label_matches_selector, LabelSelector, Note, NoteAttachment, NoteListItem};
use note_storage::{
    ActiveNoteSource, NewNote, NoteUpdate, NotesRepository, StorageError, StorageErrorKind,
    StorageResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[async_trait::async_trait]
impl NotesRepository for PgSession {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()> {
        let attachments = serialize_attachments(note.attachments)?;
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO notes (
                 id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(note.id)
        .bind(note.title)
        .bind(note.content)
        .bind(attachments)
        .bind(note.created_at)
        .bind(note.updated_at)
        .bind(note.note_revision)
        .bind(note.deleted_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert note", error))?;
        Ok(())
    }

    async fn get_note_revision(&self, id: &str) -> StorageResult<Option<i64>> {
        let mut connection = self.connection().await?;
        sqlx::query_scalar("SELECT note_revision FROM notes WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query note revision", error))
    }

    async fn note_exists(&self, id: &str) -> StorageResult<bool> {
        let mut connection = self.connection().await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notes WHERE id = $1)")
            .bind(id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query note existence", error))?;
        Ok(exists)
    }

    async fn get_note(&self, id: &str) -> StorageResult<Option<Note>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, NoteRow>(
            "SELECT n.id, n.title, n.content, n.attachments, n.created_at, n.updated_at,
                    n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.id = $1 AND n.deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note", error))?
        .map(NoteRow::into_note)
        .transpose()
    }

    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>> {
        let mut connection = self.connection().await?;
        sqlx::query_scalar("SELECT content FROM notes WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query note content", error))
    }

    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<u64> {
        let attachments = serialize_attachments(note.attachments)?;
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes
             SET title = $2, content = $3, attachments = $4,
                 updated_at = $5, note_revision = $6
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(note.id)
        .bind(note.title)
        .bind(note.content)
        .bind(attachments)
        .bind(note.updated_at)
        .bind(note.note_revision)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note", error))?;
        Ok(result.rows_affected())
    }

    async fn soft_delete_note(&self, id: &str, deleted_at: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes SET deleted_at = $2
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(deleted_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("soft-delete note", error))?;
        Ok(result.rows_affected())
    }

    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>> {
        let mut connection = self.connection().await?;
        sqlx::query_as(
            "SELECT content, note_revision
             FROM notes
             WHERE id = $1 AND deleted_at IS NOT NULL",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query deleted note", error))
    }

    async fn restore_note(&self, id: &str, note_revision: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes
             SET deleted_at = NULL, note_revision = $2
             WHERE id = $1 AND deleted_at IS NOT NULL",
        )
        .bind(id)
        .bind(note_revision)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("restore note", error))?;
        Ok(result.rows_affected())
    }

    async fn permanently_delete_note(&self, id: &str) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query("DELETE FROM notes WHERE id = $1 AND deleted_at IS NOT NULL")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("permanently delete note", error))?;
        Ok(result.rows_affected())
    }

    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>> {
        let mut connection = self.connection().await?;
        sqlx::query_scalar(
            "SELECT id FROM notes
             WHERE deleted_at IS NOT NULL AND deleted_at <= $1
             ORDER BY deleted_at, id",
        )
        .bind(cutoff)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query expired deleted notes", error))
    }

    async fn clear_note_search_data(&self, id: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        for (sql, context) in [
            (
                "DELETE FROM note_chunk_embeddings WHERE note_id = $1",
                "clear note embeddings",
            ),
            (
                "DELETE FROM embedding_jobs WHERE note_id = $1",
                "clear note embedding jobs",
            ),
            (
                "DELETE FROM note_chunks WHERE note_id = $1",
                "clear note chunks",
            ),
        ] {
            sqlx::query(sql)
                .bind(id)
                .execute(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error(context, error))?;
        }
        Ok(())
    }

    async fn clear_note_labels(&self, id: &str) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query("DELETE FROM note_labels WHERE note_id = $1")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("clear note labels", error))?;
        Ok(())
    }

    async fn clear_note_chunk_derived(&self, id: &str, chunk_idx: i64) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "DELETE FROM note_chunk_embeddings
             WHERE note_id = $1 AND chunk_idx = $2",
        )
        .bind(id)
        .bind(chunk_idx)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("clear note chunk embedding", error))?;
        Ok(())
    }

    async fn clear_note_chunks_from_derived(
        &self,
        id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "DELETE FROM note_chunk_embeddings
             WHERE note_id = $1 AND chunk_idx >= $2",
        )
        .bind(id)
        .bind(min_chunk_idx)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("clear trailing note chunk embeddings", error))?;
        Ok(())
    }

    async fn list_notes(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<Note>> {
        if normalize_limit(limit) == Some(0) {
            return Ok(Vec::new());
        }
        if selectors.is_empty() {
            let rows = self.active_note_page(limit, offset).await?;
            return rows.into_iter().map(NoteRow::into_note).collect();
        }
        self.stream_matching_notes(selectors, limit, offset).await
    }

    async fn list_all_notes(&self) -> StorageResult<Vec<Note>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, NoteRow>(
            "SELECT n.id, n.title, n.content, n.attachments, n.created_at, n.updated_at,
                    n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             ORDER BY n.created_at DESC, n.id ASC",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query all notes", error))?;
        rows.into_iter().map(NoteRow::into_note).collect()
    }

    async fn list_note_summaries(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<NoteListItem>> {
        if normalize_limit(limit) == Some(0) {
            return Ok(Vec::new());
        }
        if selectors.is_empty() {
            let rows = self.active_summary_page(limit, offset).await?;
            return rows.into_iter().map(SummaryRow::into_summary).collect();
        }
        self.stream_matching_summaries(selectors, limit, offset)
            .await
    }

    async fn list_deleted_note_summaries(&self) -> StorageResult<Vec<NoteListItem>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, SummaryRow>(
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NOT NULL
             ORDER BY n.deleted_at DESC, n.id ASC",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query deleted note summaries", error))?;
        rows.into_iter().map(SummaryRow::into_summary).collect()
    }

    async fn count_notes(&self, selectors: &[LabelSelector]) -> StorageResult<usize> {
        if selectors.is_empty() {
            let mut connection = self.connection().await?;
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*)::bigint FROM notes WHERE deleted_at IS NULL")
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| map_sqlx_error("count notes", error))?;
            return Ok(count.max(0) as usize);
        }
        self.count_matching_summaries(selectors).await
    }

    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT id, content, note_revision
             FROM notes
             WHERE deleted_at IS NULL
             ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query active note sources", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, content, note_revision)| ActiveNoteSource {
                id,
                content,
                note_revision,
            })
            .collect())
    }
}

impl PgSession {
    async fn active_note_page(
        &self,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<NoteRow>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, NoteRow>(
            "SELECT n.id, n.title, n.content, n.attachments, n.created_at, n.updated_at,
                    n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NULL
             ORDER BY n.created_at DESC, n.id ASC
             LIMIT $1 OFFSET $2",
        )
        .bind(normalize_limit(limit))
        .bind(offset.unwrap_or(0).max(0))
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query notes", error))
    }

    async fn stream_matching_notes(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<Note>> {
        let mut connection = self.connection().await?;
        let mut rows = sqlx::query_as::<_, NoteRow>(
            "SELECT n.id, n.title, n.content, n.attachments, n.created_at, n.updated_at,
                    n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NULL
             ORDER BY n.created_at DESC, n.id ASC",
        )
        .fetch(&mut *connection);
        let skip = normalize_offset_usize(offset);
        let take = normalize_limit_usize(limit);
        let mut matched = 0usize;
        let mut notes = Vec::new();
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|error| map_sqlx_error("read notes", error))?
        {
            let note = row.into_note()?;
            if !note_matches_selectors(&note, selectors) {
                continue;
            }
            if matched < skip {
                matched = matched.saturating_add(1);
                continue;
            }
            notes.push(note);
            if take == Some(notes.len()) {
                break;
            }
        }
        Ok(notes)
    }

    async fn active_summary_page(
        &self,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<SummaryRow>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, SummaryRow>(
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NULL
             ORDER BY n.created_at DESC, n.id ASC
             LIMIT $1 OFFSET $2",
        )
        .bind(normalize_limit(limit))
        .bind(offset.unwrap_or(0).max(0))
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query note summaries", error))
    }

    async fn stream_matching_summaries(
        &self,
        selectors: &[LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<NoteListItem>> {
        let mut connection = self.connection().await?;
        let mut rows = sqlx::query_as::<_, SummaryRow>(
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NULL
             ORDER BY n.created_at DESC, n.id ASC",
        )
        .fetch(&mut *connection);
        let skip = normalize_offset_usize(offset);
        let take = normalize_limit_usize(limit);
        let mut matched = 0usize;
        let mut notes = Vec::new();
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|error| map_sqlx_error("read note summaries", error))?
        {
            let note = row.into_summary()?;
            if !summary_matches_selectors(&note, selectors) {
                continue;
            }
            if matched < skip {
                matched = matched.saturating_add(1);
                continue;
            }
            notes.push(note);
            if take == Some(notes.len()) {
                break;
            }
        }
        Ok(notes)
    }

    async fn count_matching_summaries(&self, selectors: &[LabelSelector]) -> StorageResult<usize> {
        let mut connection = self.connection().await?;
        let mut rows = sqlx::query_as::<_, SummaryRow>(
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.deleted_at, labels.labels
             FROM notes n
             CROSS JOIN LATERAL (
                 SELECT COALESCE(
                     jsonb_agg(
                         jsonb_build_object(
                             'key', lk.key,
                             'value', nl.value,
                             'description', lk.description,
                             'value_type', lk.value_type
                         )
                         ORDER BY lk.key
                     ),
                     '[]'::jsonb
                 ) AS labels
                 FROM note_labels nl
                 JOIN label_keys lk ON lk.id = nl.label_key_id
                 WHERE nl.note_id = n.id
             ) labels
             WHERE n.deleted_at IS NULL",
        )
        .fetch(&mut *connection);
        let mut count = 0usize;
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|error| map_sqlx_error("read note summaries for count", error))?
        {
            let note = row.into_summary()?;
            if summary_matches_selectors(&note, selectors) {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }
}

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: String,
    title: String,
    content: String,
    attachments: Value,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
    labels: Value,
}

impl NoteRow {
    fn into_note(self) -> StorageResult<Note> {
        Ok(Note {
            id: self.id,
            title: self.title,
            content: self.content,
            attachments: deserialize_attachments(self.attachments)?,
            labels: deserialize_labels(self.labels)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
            deleted_at: self.deleted_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: String,
    title: String,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
    labels: Value,
}

impl SummaryRow {
    fn into_summary(self) -> StorageResult<NoteListItem> {
        Ok(NoteListItem {
            id: self.id,
            title: self.title,
            labels: deserialize_labels(self.labels)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
            deleted_at: self.deleted_at,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredAttachment {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct StoredLabel {
    key: String,
    value: String,
    description: String,
    value_type: String,
}

fn serialize_attachments(attachments: &[NoteAttachment]) -> StorageResult<Value> {
    let metadata = attachments
        .iter()
        .map(|attachment| StoredAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
        })
        .collect::<Vec<_>>();
    serde_json::to_value(metadata).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "serialize note attachment metadata",
            error,
        )
    })
}

fn deserialize_attachments(value: Value) -> StorageResult<Vec<NoteAttachment>> {
    let metadata: Vec<StoredAttachment> = serde_json::from_value(value).map_err(|error| {
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

fn deserialize_labels(value: Value) -> StorageResult<Vec<note_core::Label>> {
    let labels: Vec<StoredLabel> = serde_json::from_value(value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "deserialize note labels",
            error,
        )
    })?;
    labels
        .into_iter()
        .map(|label| {
            Ok(note_core::Label {
                key: label.key,
                value: label.value,
                description: label.description,
                value_type: label
                    .value_type
                    .parse()
                    .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))?,
            })
        })
        .collect()
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

fn normalize_limit(limit: Option<i64>) -> Option<i64> {
    limit.and_then(|limit| (limit >= 0).then_some(limit))
}

fn normalize_limit_usize(limit: Option<i64>) -> Option<usize> {
    normalize_limit(limit).map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
}

fn normalize_offset_usize(offset: Option<i64>) -> usize {
    usize::try_from(offset.unwrap_or(0).max(0)).unwrap_or(usize::MAX)
}
