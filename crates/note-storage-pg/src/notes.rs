use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_core::{LabelSelector, Note, NoteAttachment, NoteListItem};
use note_storage::{
    resolve_label_selectors, ActiveNoteSource, AttachmentMetadataUpdate, DeletedNoteSnapshot,
    NewNote, NoteFieldsUpdate, NoteMutationResult, NoteUpdate, NotesRepository,
    PersistedAttachment, ResolvedLabelSelector, StorageError, StorageErrorKind, StorageResult,
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{Postgres, QueryBuilder};

fn classify_note_mutation(
    row: (Option<i64>, Option<i64>, Option<bool>),
    expected_revision: i64,
    eligible_when_deleted: bool,
) -> NoteMutationResult<()> {
    // Each caller obtains this state from the same statement that locks and conditionally mutates
    // the row, so these values cannot be changed by an interleaving autocommit session.
    let (applied_revision, current_revision, is_deleted) = row;
    if let Some(revision) = applied_revision {
        return NoteMutationResult::Applied {
            value: (),
            revision,
        };
    }
    match (current_revision, is_deleted) {
        (Some(_), Some(is_deleted)) if is_deleted != eligible_when_deleted => {
            NoteMutationResult::NotFound
        }
        (Some(current_revision), _) => NoteMutationResult::Conflict {
            expected_revision,
            current_revision,
        },
        _ => NoteMutationResult::NotFound,
    }
}

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

    async fn matching_note_ids_impl(
        &self,
        selectors: &[LabelSelector],
        for_update: bool,
    ) -> StorageResult<Vec<String>> {
        let resolved = if selectors.is_empty() {
            None
        } else {
            let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
                return Ok(Vec::new());
            };
            Some(resolved)
        };
        let mut builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT n.id FROM notes n WHERE n.deleted_at IS NULL");
        if let Some(resolved) = &resolved {
            push_label_predicates(&mut builder, "n", resolved);
        }
        builder.push(" ORDER BY n.id");
        if for_update {
            builder.push(" FOR UPDATE OF n");
        }
        let mut connection = self.connection().await?;
        builder
            .build_query_scalar::<String>()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query matching note ids", error))
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
                    n.note_revision, n.deleted_at, labels.labels
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

    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<NoteMutationResult<()>> {
        let attachments = serialize_attachments(note.attachments)?;
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 UPDATE notes AS n
                 SET title = $2, content = $3, attachments = $4,
                     updated_at = $5, note_revision = n.note_revision + 1
                 FROM current
                 WHERE n.id = $1 AND current.deleted_at IS NULL
                   AND current.note_revision = $6 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(note.id)
        .bind(note.title)
        .bind(note.content)
        .bind(attachments)
        .bind(note.updated_at)
        .bind(note.expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note", error))?;
        Ok(classify_note_mutation(row, note.expected_revision, false))
    }

    async fn update_note_fields(
        &self,
        note: NoteFieldsUpdate<'_>,
    ) -> StorageResult<NoteMutationResult<()>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 UPDATE notes AS n
                 SET title = $2, content = $3, updated_at = $4,
                     note_revision = n.note_revision + 1
                 FROM current
                 WHERE n.id = $1 AND current.deleted_at IS NULL
                   AND current.note_revision = $5 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(note.id)
        .bind(note.title)
        .bind(note.content)
        .bind(note.updated_at)
        .bind(note.expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note fields", error))?;
        Ok(classify_note_mutation(row, note.expected_revision, false))
    }

    async fn update_note_attachments(
        &self,
        note: AttachmentMetadataUpdate<'_>,
    ) -> StorageResult<NoteMutationResult<()>> {
        let attachments = serialize_attachments(note.attachments)?;
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 UPDATE notes AS n
                 SET attachments = $2, updated_at = $3, note_revision = n.note_revision + 1
                 FROM current
                 WHERE n.id = $1 AND current.deleted_at IS NULL
                   AND current.note_revision = $4 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(note.id)
        .bind(attachments)
        .bind(note.updated_at)
        .bind(note.expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("update note attachments", error))?;
        Ok(classify_note_mutation(row, note.expected_revision, false))
    }

    async fn advance_note_updated_at(&self, id: &str, now: i64) -> StorageResult<u64> {
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE notes
             SET updated_at = GREATEST(updated_at + 1, $2)
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("advance note updated timestamp", error))?;
        Ok(result.rows_affected())
    }

    async fn soft_delete_note(
        &self,
        id: &str,
        expected_revision: i64,
        deleted_at: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 UPDATE notes AS n
                 SET deleted_at = $2, note_revision = n.note_revision + 1
                 FROM current
                 WHERE n.id = $1 AND current.deleted_at IS NULL
                   AND current.note_revision = $3 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(id)
        .bind(deleted_at)
        .bind(expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("soft-delete note", error))?;
        Ok(classify_note_mutation(row, expected_revision, false))
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

    async fn get_deleted_note_snapshot(
        &self,
        id: &str,
    ) -> StorageResult<Option<DeletedNoteSnapshot>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (String, i64, Value)>(
            "SELECT content, note_revision, attachments
             FROM notes
             WHERE id = $1 AND deleted_at IS NOT NULL",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query deleted note snapshot", error))?;
        row.map(|(content, revision, attachments)| {
            Ok(DeletedNoteSnapshot {
                content,
                revision,
                attachments: deserialize_attachments(attachments)?,
            })
        })
        .transpose()
    }

    async fn restore_note(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 UPDATE notes AS n
                 SET deleted_at = NULL, note_revision = n.note_revision + 1
                 FROM current
                 WHERE n.id = $1 AND current.deleted_at IS NOT NULL
                   AND current.note_revision = $2 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(id)
        .bind(expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("restore note", error))?;
        Ok(classify_note_mutation(row, expected_revision, true))
    }

    async fn permanently_delete_note(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<bool>)>(
            "WITH current AS MATERIALIZED (
                 SELECT note_revision, deleted_at FROM notes WHERE id = $1 FOR UPDATE
             ), applied AS (
                 DELETE FROM notes AS n
                 USING current
                 WHERE n.id = $1 AND current.deleted_at IS NOT NULL
                   AND current.note_revision = $2 AND n.note_revision = current.note_revision
                 RETURNING n.note_revision
             )
             SELECT (SELECT note_revision FROM applied),
                    (SELECT note_revision FROM current),
                    (SELECT deleted_at IS NOT NULL FROM current)",
        )
        .bind(id)
        .bind(expected_revision)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("permanently delete note", error))?;
        Ok(classify_note_mutation(row, expected_revision, true))
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
                    n.note_revision, n.deleted_at, labels.labels
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
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.note_revision, n.deleted_at, labels.labels
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
        self.matching_note_ids_impl(selectors, false).await
    }

    async fn matching_note_ids_for_update(
        &self,
        selectors: &[LabelSelector],
    ) -> StorageResult<Vec<String>> {
        self.matching_note_ids_impl(selectors, true).await
    }

    async fn active_note_revisions_for_update(
        &self,
        ids: &[String],
    ) -> StorageResult<Vec<(String, i64)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, (String, i64)>(
            "SELECT id, note_revision
             FROM notes
             WHERE id = ANY($1) AND deleted_at IS NULL
             ORDER BY id
             FOR UPDATE",
        )
        .bind(ids)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("lock active note revisions", error))
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
                    n.note_revision, n.deleted_at, labels.labels
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
                    n.note_revision, n.deleted_at, labels.labels
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
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.note_revision, n.deleted_at, labels.labels
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
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.note_revision, n.deleted_at, labels.labels
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
    note_revision: i64,
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
            revision: self.note_revision,
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
    note_revision: i64,
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
            revision: self.note_revision,
            deleted_at: self.deleted_at,
        })
    }
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
        .map(PersistedAttachment::from)
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
    let metadata: Vec<PersistedAttachment> = serde_json::from_value(value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "deserialize note attachment metadata",
            error,
        )
    })?;
    metadata.into_iter().map(NoteAttachment::try_from).collect()
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
