use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_core::{LabelSelector, Note, NoteAttachment, NoteListItem};
use note_storage::{
    resolve_label_selectors, ActiveNoteSource, AttachmentMetadataUpdate, NewNote, NoteFieldsUpdate,
    NoteUpdate, NotesRepository, ResolvedLabelSelector, StorageError, StorageErrorKind,
    StorageResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, QueryBuilder};

impl PgSession {
    async fn resolved_label_selectors(
        &self,
        selectors: &[LabelSelector],
    ) -> StorageResult<Option<Vec<ResolvedLabelSelector>>> {
        let mut keys = selectors
            .iter()
            .map(|selector| selector.key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return Ok(resolve_label_selectors(selectors, &[]));
        }

        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT DISTINCT lk.key, nl.value, lk.description, lk.value_type
             FROM label_keys lk
             JOIN note_labels nl ON nl.label_key_id = lk.id
             JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
             WHERE lk.key = ANY($1)",
        )
        .bind(keys)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("resolve label selector values", error))?;

        let labels = rows
            .into_iter()
            .map(|(key, value, description, value_type)| {
                Ok(note_core::Label {
                    key,
                    value,
                    description,
                    value_type: value_type
                        .parse()
                        .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))?,
                })
            })
            .collect::<StorageResult<Vec<_>>>()?;
        Ok(resolve_label_selectors(selectors, &labels))
    }
}

fn push_label_predicates(
    builder: &mut QueryBuilder<Postgres>,
    note_alias: &str,
    selectors: &[ResolvedLabelSelector],
) {
    if selectors.is_empty() {
        return;
    }

    builder.push(" AND ");
    push_balanced_conjunction(builder, note_alias, selectors);
}

fn push_balanced_conjunction(
    builder: &mut QueryBuilder<Postgres>,
    note_alias: &str,
    selectors: &[ResolvedLabelSelector],
) {
    if selectors.len() == 1 {
        let selector = &selectors[0];
        builder
            .push(
                "EXISTS (
                     SELECT 1 FROM note_labels filtered_nl
                     JOIN label_keys filtered_lk ON filtered_lk.id = filtered_nl.label_key_id
                     WHERE filtered_nl.note_id = ",
            )
            .push(note_alias)
            .push(".id AND filtered_lk.key = ")
            .push_bind(selector.key.clone());
        if let Some(values) = &selector.values {
            builder
                .push(" AND filtered_nl.value = ANY(")
                .push_bind(values.clone())
                .push(")");
        }
        builder.push(")");
        return;
    }

    let middle = selectors.len() / 2;
    builder.push("(");
    push_balanced_conjunction(builder, note_alias, &selectors[..middle]);
    builder.push(" AND ");
    push_balanced_conjunction(builder, note_alias, &selectors[middle..]);
    builder.push(")");
}

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

    async fn update_note_fields(&self, note: NoteFieldsUpdate<'_>) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes
             SET title = $2, content = $3, updated_at = $4, note_revision = $5
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(note.id)
        .bind(note.title)
        .bind(note.content)
        .bind(note.updated_at)
        .bind(note.note_revision)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note fields", error))?;
        Ok(result.rows_affected())
    }

    async fn update_note_attachments(
        &self,
        note: AttachmentMetadataUpdate<'_>,
    ) -> StorageResult<u64> {
        let attachments = serialize_attachments(note.attachments)?;
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes
             SET attachments = $2, updated_at = $3
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(note.id)
        .bind(attachments)
        .bind(note.updated_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note attachments", error))?;
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
        let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
            return Ok(Vec::new());
        };
        let rows = self.filtered_note_page(&resolved, limit, offset).await?;
        rows.into_iter().map(NoteRow::into_note).collect()
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
        let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
            return Ok(Vec::new());
        };
        let rows = self.filtered_summary_page(&resolved, limit, offset).await?;
        rows.into_iter().map(SummaryRow::into_summary).collect()
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
            return usize::try_from(count.max(0)).map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Operation,
                    "convert PostgreSQL note count",
                    error,
                )
            });
        }
        let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
            return Ok(0);
        };
        let mut builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT COUNT(*)::bigint FROM notes n WHERE n.deleted_at IS NULL");
        push_label_predicates(&mut builder, "n", &resolved);
        let mut connection = self.connection().await?;
        let (count,) = builder
            .build_query_as::<(i64,)>()
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("count notes", error))?;
        usize::try_from(count.max(0)).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "convert PostgreSQL note count",
                error,
            )
        })
    }

    async fn matching_note_ids(&self, selectors: &[LabelSelector]) -> StorageResult<Vec<String>> {
        if selectors.is_empty() {
            let mut connection = self.connection().await?;
            return sqlx::query_scalar("SELECT id FROM notes WHERE deleted_at IS NULL ORDER BY id")
                .fetch_all(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query matching note ids", error));
        }
        let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
            return Ok(Vec::new());
        };
        let mut builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT n.id FROM notes n WHERE n.deleted_at IS NULL");
        push_label_predicates(&mut builder, "n", &resolved);
        builder.push(" ORDER BY n.id");
        let mut connection = self.connection().await?;
        builder
            .build_query_scalar::<String>()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query matching note ids", error))
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

    async fn filtered_note_page(
        &self,
        selectors: &[ResolvedLabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<NoteRow>> {
        let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
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
             WHERE n.deleted_at IS NULL",
        );
        push_label_predicates(&mut builder, "n", selectors);
        builder
            .push(" ORDER BY n.created_at DESC, n.id ASC LIMIT ")
            .push_bind(normalize_limit(limit))
            .push(" OFFSET ")
            .push_bind(offset.unwrap_or(0).max(0));
        let mut connection = self.connection().await?;
        builder
            .build_query_as::<NoteRow>()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query notes", error))
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

    async fn filtered_summary_page(
        &self,
        selectors: &[ResolvedLabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<SummaryRow>> {
        let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
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
        );
        push_label_predicates(&mut builder, "n", selectors);
        builder
            .push(" ORDER BY n.created_at DESC, n.id ASC LIMIT ")
            .push_bind(normalize_limit(limit))
            .push(" OFFSET ")
            .push_bind(offset.unwrap_or(0).max(0));
        let mut connection = self.connection().await?;
        builder
            .build_query_as::<SummaryRow>()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query note summaries", error))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_predicates_precede_order_and_limit() {
        let resolved = vec![
            ResolvedLabelSelector {
                key: "status".into(),
                values: Some(vec!["ready".into()]),
            },
            ResolvedLabelSelector {
                key: "archived".into(),
                values: None,
            },
        ];
        let mut builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT n.id FROM notes n WHERE n.deleted_at IS NULL");
        push_label_predicates(&mut builder, "n", &resolved);
        builder
            .push(" ORDER BY n.created_at DESC, n.id ASC LIMIT ")
            .push_bind(10_i64);
        let sql = builder.sql();
        let sql = sql.as_str();

        assert_eq!(sql.matches("EXISTS").count(), 2);
        assert!(sql.contains("= ANY("));
        assert!(sql.find("EXISTS").unwrap() < sql.find("ORDER BY").unwrap());
        assert!(sql.find("ORDER BY").unwrap() < sql.find("LIMIT").unwrap());
    }

    #[test]
    fn balanced_predicates_preserve_placeholder_order_for_many_selectors() {
        let resolved = (0..128)
            .map(|index| ResolvedLabelSelector {
                key: format!("key-{index}"),
                values: None,
            })
            .collect::<Vec<_>>();
        let mut builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT n.id FROM notes n WHERE n.deleted_at IS NULL");

        push_label_predicates(&mut builder, "n", &resolved);
        let sql = builder.sql();
        let sql = sql.as_str();

        assert_eq!(sql.matches("EXISTS").count(), 128);
        assert!(sql.contains("filtered_lk.key = $1)"));
        assert!(sql.contains("filtered_lk.key = $128)"));
        assert!(sql.find("filtered_lk.key = $1)") < sql.find("filtered_lk.key = $128)"));

        let mut depth = 0usize;
        let mut max_depth = 0usize;
        for character in sql.chars() {
            match character {
                '(' => {
                    depth += 1;
                    max_depth = max_depth.max(depth);
                }
                ')' => {
                    depth = depth
                        .checked_sub(1)
                        .expect("generated SQL has an unmatched closing parenthesis");
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "generated SQL has unclosed parentheses");
        assert!(
            max_depth <= 10,
            "balanced conjunction nesting depth was {max_depth}"
        );
    }
}

fn normalize_limit(limit: Option<i64>) -> Option<i64> {
    limit.and_then(|limit| (limit >= 0).then_some(limit))
}
