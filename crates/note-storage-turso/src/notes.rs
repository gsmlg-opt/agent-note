use crate::connection::map_turso_error;
use crate::labels::labels_for_note_unlocked;
use crate::TursoSession;
use note_core::{LabelSelector, Note, NoteAttachment, NoteListItem};
use note_storage::{
    resolve_label_selectors, ActiveNoteSource, AttachmentMetadataUpdate, NewNote, NoteFieldsUpdate,
    NoteMutationResult, NoteUpdate, NotesRepository, PersistedAttachment, ResolvedLabelSelector,
    StorageError, StorageErrorKind, StorageResult,
};
use tokio::sync::OwnedMutexGuard;

const NOTE_MUTATION_SAVEPOINT: &str = "note_mutation_cas";

struct NoteMutationSavepoint {
    connection: turso::Connection,
    operation_guard: Option<OwnedMutexGuard<()>>,
    active: bool,
}

impl NoteMutationSavepoint {
    async fn begin(
        session: &TursoSession,
        operation_guard: OwnedMutexGuard<()>,
    ) -> StorageResult<Self> {
        let mut savepoint = Self {
            connection: session.connection.clone(),
            operation_guard: Some(operation_guard),
            active: true,
        };
        let begin = savepoint
            .connection
            .execute(&format!("SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"), ())
            .await;
        match begin {
            Ok(_) => Ok(savepoint),
            Err(error) => {
                let primary = map_turso_error("begin atomic note mutation", error);
                Err(savepoint.rollback_with_cleanup(primary).await)
            }
        }
    }

    async fn finish<T>(mut self, result: StorageResult<T>) -> StorageResult<T> {
        match result {
            Ok(value) => match self
                .connection
                .execute(&format!("RELEASE SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"), ())
                .await
            {
                Ok(_) => {
                    self.active = false;
                    Ok(value)
                }
                Err(error) => {
                    let primary = map_turso_error("finish atomic note mutation", error);
                    Err(self.rollback_with_cleanup(primary).await)
                }
            },
            Err(primary) => Err(self.rollback_with_cleanup(primary).await),
        }
    }

    async fn rollback_with_cleanup(&mut self, primary: StorageError) -> StorageError {
        let rollback = self
            .connection
            .execute(
                &format!("ROLLBACK TO SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"),
                (),
            )
            .await
            .err();
        let release = self
            .connection
            .execute(&format!("RELEASE SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"), ())
            .await
            .err();
        self.active = false;
        note_savepoint_cleanup_error(primary, rollback, release)
    }
}

impl Drop for NoteMutationSavepoint {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let connection = self.connection.clone();
        let Some(operation_guard) = self.operation_guard.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _operation_guard = operation_guard;
                let _ = connection
                    .execute(
                        &format!("ROLLBACK TO SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"),
                        (),
                    )
                    .await;
                let _ = connection
                    .execute(&format!("RELEASE SAVEPOINT {NOTE_MUTATION_SAVEPOINT}"), ())
                    .await;
            });
        } else {
            std::mem::forget(operation_guard);
        }
    }
}

fn note_savepoint_cleanup_error(
    primary: StorageError,
    rollback: Option<turso::Error>,
    release: Option<turso::Error>,
) -> StorageError {
    if rollback.is_none() && release.is_none() {
        return primary;
    }
    let mut message = primary.to_string();
    if let Some(rollback) = rollback {
        message.push_str(&format!("; savepoint rollback also failed: {rollback}"));
    }
    if let Some(release) = release {
        message.push_str(&format!("; savepoint release also failed: {release}"));
    }
    StorageError::with_source(primary.kind(), message, primary)
}

impl TursoSession {
    async fn classify_note_mutation(
        &self,
        id: &str,
        expected_revision: i64,
        eligible_when_deleted: bool,
    ) -> StorageResult<NoteMutationResult<()>> {
        let mut rows = self
            .connection
            .query(
                "SELECT note_revision, deleted_at FROM notes WHERE id = ?1",
                turso::params![id],
            )
            .await
            .map_err(|error| map_turso_error("classify note mutation", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note mutation state", error))?
        else {
            return Ok(NoteMutationResult::NotFound);
        };
        let current_revision = row
            .get::<i64>(0)
            .map_err(|error| map_turso_error("decode note mutation revision", error))?;
        let deleted_at = row
            .get::<Option<i64>>(1)
            .map_err(|error| map_turso_error("decode note mutation state", error))?;
        if deleted_at.is_some() != eligible_when_deleted {
            Ok(NoteMutationResult::NotFound)
        } else {
            Ok(NoteMutationResult::Conflict {
                expected_revision,
                current_revision,
            })
        }
    }

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

        let keys_json = serde_json::to_string(&keys).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize label selector keys",
                error,
            )
        })?;
        let mut rows = self
            .connection
            .query(
                "SELECT DISTINCT lk.key, nl.value, lk.description, lk.value_type
                 FROM label_keys lk
                 JOIN note_labels nl ON nl.label_key_id = lk.id
                 JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
                 WHERE lk.key IN (SELECT value FROM json_each(?1))",
                turso::params![keys_json],
            )
            .await
            .map_err(|error| map_turso_error("resolve label selector values", error))?;
        let mut labels = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read label selector values", error))?
        {
            let value_type = row
                .get::<String>(3)
                .map_err(|error| map_turso_error("decode label selector value type", error))?
                .parse::<note_core::LabelValueType>()
                .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))?;
            labels.push(note_core::Label {
                key: row
                    .get(0)
                    .map_err(|error| map_turso_error("decode label key", error))?,
                value: row
                    .get(1)
                    .map_err(|error| map_turso_error("decode label value", error))?,
                description: row
                    .get(2)
                    .map_err(|error| map_turso_error("decode label description", error))?,
                value_type,
            });
        }
        Ok(resolve_label_selectors(selectors, &labels))
    }

    async fn matching_note_ids_unlocked(
        &self,
        selectors: &[LabelSelector],
    ) -> StorageResult<Vec<String>> {
        let resolved = if selectors.is_empty() {
            None
        } else {
            let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
                return Ok(Vec::new());
            };
            Some(resolved)
        };
        let mut sql = "SELECT n.id FROM notes n WHERE n.deleted_at IS NULL".to_string();
        let mut params = Vec::<turso::Value>::new();
        if let Some(resolved) = &resolved {
            push_label_predicates(&mut sql, &mut params, "n", resolved)?;
        }
        sql.push_str(" ORDER BY n.id");

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error("query matching note ids", error))?;
        let mut note_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read matching note ids", error))?
        {
            note_ids.push(
                row.get(0)
                    .map_err(|error| map_turso_error("decode matching note id", error))?,
            );
        }
        Ok(note_ids)
    }
}

fn push_label_predicates(
    sql: &mut String,
    params: &mut Vec<turso::Value>,
    note_alias: &str,
    selectors: &[ResolvedLabelSelector],
) -> StorageResult<()> {
    let mut predicates = Vec::with_capacity(selectors.len());
    for selector in selectors {
        params.push(selector.key.clone().into());
        let key_param = params.len();
        let mut predicate = format!(
            "EXISTS (
                 SELECT 1
                 FROM note_labels filtered_nl
                 JOIN label_keys filtered_lk ON filtered_lk.id = filtered_nl.label_key_id
                 WHERE filtered_nl.note_id = {note_alias}.id
                   AND filtered_lk.key = ?{key_param}"
        );
        if let Some(values) = &selector.values {
            let values_json = serde_json::to_string(values).map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Operation,
                    "serialize resolved label values",
                    error,
                )
            })?;
            params.push(values_json.into());
            let values_param = params.len();
            predicate.push_str(&format!(
                " AND filtered_nl.value IN (
                     SELECT value FROM json_each(?{values_param})
                 )"
            ));
        }
        predicate.push(')');
        predicates.push(predicate);
    }
    if !predicates.is_empty() {
        sql.push_str(" AND ");
        push_balanced_conjunction(sql, &predicates);
    }
    Ok(())
}

fn push_balanced_conjunction(sql: &mut String, predicates: &[String]) {
    if predicates.len() == 1 {
        sql.push_str(&predicates[0]);
        return;
    }

    let middle = predicates.len() / 2;
    sql.push('(');
    push_balanced_conjunction(sql, &predicates[..middle]);
    sql.push_str(" AND ");
    push_balanced_conjunction(sql, &predicates[middle..]);
    sql.push(')');
}

#[async_trait::async_trait]
impl NotesRepository for TursoSession {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
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
            labels: labels_for_note_unlocked(self, &row.id).await?,
            id: row.id,
            title: row.title,
            content: row.content,
            attachments: row.attachments,
            created_at: row.created_at,
            updated_at: row.updated_at,
            revision: row.revision,
            deleted_at: row.deleted_at,
        }))
    }

    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>> {
        let _operation_guard = self.operation_guard().await;
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

    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let attachments = serialize_attachments(note.attachments)?;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let mut rows = self
                .connection
                .query(
                    "UPDATE notes
                 SET title = ?2, content = ?3, attachments = ?4, updated_at = ?5,
                     note_revision = note_revision + 1
                 WHERE id = ?1 AND deleted_at IS NULL AND note_revision = ?6
                 RETURNING note_revision",
                    turso::params![
                        note.id,
                        note.title,
                        note.content,
                        attachments,
                        note.updated_at,
                        note.expected_revision
                    ],
                )
                .await
                .map_err(|error| map_turso_error("update note", error))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read updated note revision", error))?
            {
                let revision = row
                    .get(0)
                    .map_err(|error| map_turso_error("decode updated note revision", error))?;
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision,
                })
            } else {
                drop(rows);
                self.classify_note_mutation(note.id, note.expected_revision, false)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn update_note_fields(
        &self,
        note: NoteFieldsUpdate<'_>,
    ) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let mut rows = self
                .connection
                .query(
                    "UPDATE notes
                 SET title = ?2, content = ?3, updated_at = ?4, note_revision = note_revision + 1
                 WHERE id = ?1 AND deleted_at IS NULL AND note_revision = ?5
                 RETURNING note_revision",
                    turso::params![
                        note.id,
                        note.title,
                        note.content,
                        note.updated_at,
                        note.expected_revision
                    ],
                )
                .await
                .map_err(|error| map_turso_error("update note fields", error))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read updated note fields revision", error))?
            {
                let revision = row.get(0).map_err(|error| {
                    map_turso_error("decode updated note fields revision", error)
                })?;
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision,
                })
            } else {
                drop(rows);
                self.classify_note_mutation(note.id, note.expected_revision, false)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn update_note_attachments(
        &self,
        note: AttachmentMetadataUpdate<'_>,
    ) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let attachments = serialize_attachments(note.attachments)?;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let mut rows = self
                .connection
                .query(
                    "UPDATE notes
                 SET attachments = ?2, updated_at = ?3, note_revision = note_revision + 1
                 WHERE id = ?1 AND deleted_at IS NULL AND note_revision = ?4
                 RETURNING note_revision",
                    turso::params![
                        note.id,
                        attachments,
                        note.updated_at,
                        note.expected_revision
                    ],
                )
                .await
                .map_err(|error| map_turso_error("update note attachments", error))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read updated attachment revision", error))?
            {
                let revision = row.get(0).map_err(|error| {
                    map_turso_error("decode updated attachment revision", error)
                })?;
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision,
                })
            } else {
                drop(rows);
                self.classify_note_mutation(note.id, note.expected_revision, false)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn advance_note_updated_at(&self, id: &str, now: i64) -> StorageResult<u64> {
        let _operation_guard = self.operation_guard().await;
        self.connection
            .execute(
                "UPDATE notes
                 SET updated_at = CASE WHEN updated_at >= ?2 THEN updated_at + 1 ELSE ?2 END
                 WHERE id = ?1 AND deleted_at IS NULL",
                turso::params![id, now],
            )
            .await
            .map_err(|error| map_turso_error("advance note updated timestamp", error))
    }

    async fn soft_delete_note(
        &self,
        id: &str,
        expected_revision: i64,
        deleted_at: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let mut rows = self
                .connection
                .query(
                    "UPDATE notes SET deleted_at = ?2, note_revision = note_revision + 1
                 WHERE id = ?1 AND deleted_at IS NULL AND note_revision = ?3
                 RETURNING note_revision",
                    turso::params![id, deleted_at, expected_revision],
                )
                .await
                .map_err(|error| map_turso_error("soft-delete note", error))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read soft-delete revision", error))?
            {
                let revision = row
                    .get(0)
                    .map_err(|error| map_turso_error("decode soft-delete revision", error))?;
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision,
                })
            } else {
                drop(rows);
                self.classify_note_mutation(id, expected_revision, false)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>> {
        let _operation_guard = self.operation_guard().await;
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

    async fn restore_note(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let mut rows = self
                .connection
                .query(
                    "UPDATE notes
                 SET deleted_at = NULL, note_revision = note_revision + 1
                 WHERE id = ?1 AND deleted_at IS NOT NULL AND note_revision = ?2
                 RETURNING note_revision",
                    turso::params![id, expected_revision],
                )
                .await
                .map_err(|error| map_turso_error("restore note", error))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| map_turso_error("read restored note revision", error))?
            {
                let revision = row
                    .get(0)
                    .map_err(|error| map_turso_error("decode restored note revision", error))?;
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision,
                })
            } else {
                drop(rows);
                self.classify_note_mutation(id, expected_revision, true)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn permanently_delete_note(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> StorageResult<NoteMutationResult<()>> {
        let operation_guard = self.operation_guard().await;
        let savepoint = NoteMutationSavepoint::begin(self, operation_guard).await?;
        let result = async {
            let affected = self
            .connection
            .execute(
                "DELETE FROM notes WHERE id = ?1 AND deleted_at IS NOT NULL AND note_revision = ?2",
                turso::params![id, expected_revision],
            )
            .await
            .map_err(|error| map_turso_error("permanently delete note", error))?;
            if affected == 1 {
                Ok(NoteMutationResult::Applied {
                    value: (),
                    revision: expected_revision,
                })
            } else {
                self.classify_note_mutation(id, expected_revision, true)
                    .await
            }
        }
        .await;
        savepoint.finish(result).await
    }

    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>> {
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
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
        let _operation_guard = self.operation_guard().await;
        let resolved = if selectors.is_empty() {
            None
        } else {
            let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
                return Ok(Vec::new());
            };
            Some(resolved)
        };
        let mut sql =
            "SELECT n.id, n.title, n.content, n.attachments, n.created_at, n.updated_at, n.note_revision, n.deleted_at
                       FROM notes n WHERE n.deleted_at IS NULL"
            .to_string();
        let mut params = Vec::<turso::Value>::new();
        if let Some(resolved) = &resolved {
            push_label_predicates(&mut sql, &mut params, "n", resolved)?;
        }

        sql.push_str(" ORDER BY n.created_at DESC, n.id ASC");
        if limit.is_some() || offset.is_some() {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(limit.unwrap_or(-1).into());
            params.push(offset.unwrap_or(0).max(0).into());
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
                labels: labels_for_note_unlocked(self, &row.id).await?,
                id: row.id,
                title: row.title,
                content: row.content,
                attachments: row.attachments,
                created_at: row.created_at,
                updated_at: row.updated_at,
                revision: row.revision,
                deleted_at: row.deleted_at,
            });
        }
        Ok(notes)
    }

    async fn list_all_notes(&self) -> StorageResult<Vec<Note>> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, content, attachments, created_at, updated_at, note_revision, deleted_at
                 FROM notes
                 ORDER BY created_at DESC, id ASC",
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
                labels: labels_for_note_unlocked(self, &row.id).await?,
                id: row.id,
                title: row.title,
                content: row.content,
                attachments: row.attachments,
                created_at: row.created_at,
                updated_at: row.updated_at,
                revision: row.revision,
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
        let _operation_guard = self.operation_guard().await;
        let resolved = if selectors.is_empty() {
            None
        } else {
            let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
                return Ok(Vec::new());
            };
            Some(resolved)
        };
        let mut sql =
            "SELECT n.id, n.title, n.created_at, n.updated_at, n.note_revision, n.deleted_at
                       FROM notes n WHERE n.deleted_at IS NULL"
                .to_string();
        let mut params = Vec::<turso::Value>::new();
        if let Some(resolved) = &resolved {
            push_label_predicates(&mut sql, &mut params, "n", resolved)?;
        }

        sql.push_str(" ORDER BY n.created_at DESC, n.id ASC");
        if limit.is_some() || offset.is_some() {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(limit.unwrap_or(-1).into());
            params.push(offset.unwrap_or(0).max(0).into());
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
                labels: labels_for_note_unlocked(self, &row.id).await?,
                id: row.id,
                title: row.title,
                created_at: row.created_at,
                updated_at: row.updated_at,
                revision: row.revision,
                deleted_at: row.deleted_at,
            });
        }
        Ok(notes)
    }

    async fn list_deleted_note_summaries(&self) -> StorageResult<Vec<NoteListItem>> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT id, title, created_at, updated_at, note_revision, deleted_at
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
                labels: labels_for_note_unlocked(self, &row.id).await?,
                id: row.id,
                title: row.title,
                created_at: row.created_at,
                updated_at: row.updated_at,
                revision: row.revision,
                deleted_at: row.deleted_at,
            });
        }
        Ok(notes)
    }

    async fn count_notes(&self, selectors: &[LabelSelector]) -> StorageResult<usize> {
        let _operation_guard = self.operation_guard().await;
        let resolved = if selectors.is_empty() {
            None
        } else {
            let Some(resolved) = self.resolved_label_selectors(selectors).await? else {
                return Ok(0);
            };
            Some(resolved)
        };
        let mut sql = "SELECT COUNT(*) FROM notes n WHERE n.deleted_at IS NULL".to_string();
        let mut params = Vec::<turso::Value>::new();
        if let Some(resolved) = &resolved {
            push_label_predicates(&mut sql, &mut params, "n", resolved)?;
        }

        let mut rows = self
            .connection
            .query(&sql, turso::params_from_iter(params))
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

    async fn matching_note_ids(&self, selectors: &[LabelSelector]) -> StorageResult<Vec<String>> {
        let _operation_guard = self.operation_guard().await;
        self.matching_note_ids_unlocked(selectors).await
    }

    async fn matching_note_ids_for_update(
        &self,
        selectors: &[LabelSelector],
    ) -> StorageResult<Vec<String>> {
        if self.transaction_mode != Some(note_storage::TransactionMode::Immediate) {
            return Err(StorageError::new(
                StorageErrorKind::Transaction,
                "matching note IDs for update requires an immediate transaction",
            ));
        }
        let _operation_guard = self.operation_guard().await;
        self.matching_note_ids_unlocked(selectors).await
    }

    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>> {
        let _operation_guard = self.operation_guard().await;
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
    revision: i64,
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
        revision: row
            .get::<i64>(6)
            .map_err(|error| map_turso_error("decode note revision", error))?,
        deleted_at: row
            .get::<Option<i64>>(7)
            .map_err(|error| map_turso_error("decode note deletion time", error))?,
    })
}

struct SummaryRow {
    id: String,
    title: String,
    created_at: i64,
    updated_at: i64,
    revision: i64,
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
        revision: row
            .get::<i64>(4)
            .map_err(|error| map_turso_error("decode note summary revision", error))?,
        deleted_at: row
            .get::<Option<i64>>(5)
            .map_err(|error| map_turso_error("decode note summary deletion time", error))?,
    })
}

fn serialize_attachments(attachments: &[NoteAttachment]) -> StorageResult<String> {
    let metadata = attachments
        .iter()
        .map(PersistedAttachment::from)
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
    let metadata: Vec<PersistedAttachment> = serde_json::from_str(value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "deserialize note attachment metadata",
            error,
        )
    })?;
    metadata.into_iter().map(NoteAttachment::try_from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TursoStorage;
    use note_core::parse_label_selectors;
    use note_storage::LabelRepository;

    #[test]
    fn resolved_predicates_precede_order_and_limit() {
        let mut sql = "SELECT n.id FROM notes n WHERE n.deleted_at IS NULL".to_string();
        let mut params = Vec::new();
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
        push_label_predicates(&mut sql, &mut params, "n", &resolved).unwrap();
        sql.push_str(" ORDER BY n.created_at DESC, n.id ASC LIMIT ? OFFSET ?");

        assert_eq!(sql.matches("EXISTS").count(), 2);
        assert!(sql.contains("json_each"));
        assert_eq!(params.len(), 3);
        assert!(sql.contains("filtered_lk.key = ?1"));
        assert!(sql.contains("json_each(?2)"));
        assert!(sql.contains("filtered_lk.key = ?3"));
        assert!(sql.find("EXISTS").unwrap() < sql.find("ORDER BY").unwrap());
        assert!(sql.find("ORDER BY").unwrap() < sql.find("LIMIT").unwrap());
    }

    #[tokio::test]
    async fn handles_128_value_label_selectors_without_expression_depth_error() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        let session = storage.connect().await.unwrap();
        session.insert_label_key("status", "Status").await.unwrap();
        session
            .insert_note(NewNote {
                id: "ready",
                title: "Ready",
                content: "Content",
                attachments: &[],
                created_at: 1,
                updated_at: 1,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        session
            .attach_label("ready", "status", "ready")
            .await
            .unwrap();
        let selectors = parse_label_selectors(
            &std::iter::repeat_n("status=ready", 128)
                .collect::<Vec<_>>()
                .join("&"),
        );

        let summaries = session
            .list_note_summaries(&selectors, Some(1), Some(0))
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "ready");
    }
}
