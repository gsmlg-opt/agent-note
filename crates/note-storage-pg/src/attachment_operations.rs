use crate::connection::map_sqlx_error as map_raw_sqlx_error;
use crate::PgSession;
use note_storage::{
    AttachmentOperation, AttachmentOperationKind, AttachmentOperationRepository,
    AttachmentOperationStatus, NewAttachmentOperation, StorageError, StorageErrorKind,
    StorageResult,
};
use std::str::FromStr as _;

fn map_sqlx_error(context: &'static str, error: sqlx::Error) -> StorageError {
    let error = map_raw_sqlx_error(context, error);
    StorageError::new(error.kind(), context)
}

#[async_trait::async_trait]
impl AttachmentOperationRepository for PgSession {
    async fn insert_attachment_operation(
        &self,
        operation: NewAttachmentOperation,
    ) -> StorageResult<AttachmentOperation> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, AttachmentOperationRow>(
            "INSERT INTO attachment_operations (
                 id, kind, note_id, attachment_id, storage_generation, object_key,
                 status, attempts, next_attempt_at, lease_owner, lease_expires_at,
                 last_error, created_at, updated_at
             ) VALUES (
                 $1, $2, $3, $4, $5, $6, 'pending', 0, $7, NULL, NULL, NULL, $8, $8
             )
             ON CONFLICT (kind, object_key) DO UPDATE
             SET object_key = attachment_operations.object_key
             RETURNING id, kind, note_id, attachment_id, storage_generation,
                       object_key, status, attempts, next_attempt_at, lease_owner,
                       lease_expires_at, last_error, created_at, updated_at",
        )
        .bind(operation.id)
        .bind(operation.kind.as_str())
        .bind(operation.note_id)
        .bind(operation.attachment_id)
        .bind(operation.storage_generation)
        .bind(operation.object_key)
        .bind(operation.next_attempt_at)
        .bind(operation.created_at)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert attachment operation", error))?;
        row.into_operation()
    }

    async fn get_attachment_operation(
        &self,
        id: &str,
    ) -> StorageResult<Option<AttachmentOperation>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttachmentOperationRow>(
            "SELECT id, kind, note_id, attachment_id, storage_generation,
                    object_key, status, attempts, next_attempt_at, lease_owner,
                    lease_expires_at, last_error, created_at, updated_at
             FROM attachment_operations WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query attachment operation", error))?
        .map(AttachmentOperationRow::into_operation)
        .transpose()
    }

    async fn list_attachment_operations_for_note(
        &self,
        note_id: &str,
    ) -> StorageResult<Vec<AttachmentOperation>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttachmentOperationRow>(
            "SELECT id, kind, note_id, attachment_id, storage_generation,
                    object_key, status, attempts, next_attempt_at, lease_owner,
                    lease_expires_at, last_error, created_at, updated_at
             FROM attachment_operations
             WHERE note_id = $1
             ORDER BY created_at, id",
        )
        .bind(note_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list attachment operations for note", error))?
        .into_iter()
        .map(AttachmentOperationRow::into_operation)
        .collect()
    }

    async fn claim_attachment_operations(
        &self,
        owner: &str,
        now: i64,
        lease_expires_at: i64,
        limit: i64,
    ) -> StorageResult<Vec<AttachmentOperation>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        if owner.trim().is_empty() || lease_expires_at <= now {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "attachment operation claim requires a nonblank owner and a future lease expiry",
            ));
        }
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttachmentOperationRow>(
            "WITH candidates AS (
                 SELECT id
                 FROM attachment_operations
                 WHERE (status = 'pending'
                        AND (next_attempt_at IS NULL OR next_attempt_at <= $2))
                    OR (status = 'running' AND lease_expires_at <= $2)
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT $4
             ),
             claimed AS (
                 UPDATE attachment_operations AS operation
                 SET status = 'running',
                     attempts = operation.attempts + 1,
                     lease_owner = $1,
                     lease_expires_at = $3,
                     updated_at = $2
                 FROM candidates
                 WHERE operation.id = candidates.id
                 RETURNING operation.*
             )
             SELECT id, kind, note_id, attachment_id, storage_generation,
                    object_key, status, attempts, next_attempt_at, lease_owner,
                    lease_expires_at, last_error, created_at, updated_at
             FROM claimed
             ORDER BY created_at, id",
        )
        .bind(owner)
        .bind(now)
        .bind(lease_expires_at)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("claim attachment operations", error))?
        .into_iter()
        .map(AttachmentOperationRow::into_operation)
        .collect()
    }

    async fn complete_attachment_operation(
        &self,
        id: &str,
        owner: &str,
        expected_attempt: i64,
        updated_at: i64,
    ) -> StorageResult<bool> {
        let mut connection = self.connection().await?;
        let completed: bool = sqlx::query_scalar(
            "WITH updated AS (
                 UPDATE attachment_operations
                 SET status = 'completed', next_attempt_at = NULL,
                     lease_owner = NULL, lease_expires_at = NULL,
                     last_error = NULL, updated_at = $4
                 WHERE id = $1 AND status = 'running' AND lease_owner = $2
                   AND attempts = $3
                 RETURNING id
             )
             SELECT EXISTS(SELECT 1 FROM updated)
                 OR EXISTS(
                     SELECT 1 FROM attachment_operations
                     WHERE id = $1 AND status = 'completed' AND attempts = $3
                 )",
        )
        .bind(id)
        .bind(owner)
        .bind(expected_attempt)
        .bind(updated_at)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("complete attachment operation", error))?;
        Ok(completed)
    }

    async fn fail_attachment_operation(
        &self,
        id: &str,
        owner: &str,
        expected_attempt: i64,
        status: AttachmentOperationStatus,
        next_attempt_at: Option<i64>,
        last_error: &str,
        updated_at: i64,
    ) -> StorageResult<bool> {
        let next_attempt_at = match (status, next_attempt_at) {
            (AttachmentOperationStatus::Pending, Some(next_attempt_at)) => Some(next_attempt_at),
            (AttachmentOperationStatus::Dead, _) => None,
            (AttachmentOperationStatus::Pending, None) => {
                return Err(StorageError::new(
                    StorageErrorKind::Constraint,
                    "pending attachment operation failure requires a next attempt time",
                ));
            }
            (AttachmentOperationStatus::Running | AttachmentOperationStatus::Completed, _) => {
                return Err(StorageError::new(
                    StorageErrorKind::Constraint,
                    "failed attachment operation status must be pending or dead",
                ));
            }
        };
        let mut connection = self.connection().await?;
        let result = sqlx::query(
            "UPDATE attachment_operations
             SET status = $4, next_attempt_at = $5,
                 lease_owner = NULL, lease_expires_at = NULL,
                 last_error = $6, updated_at = $7
             WHERE id = $1 AND status = 'running' AND lease_owner = $2
               AND attempts = $3",
        )
        .bind(id)
        .bind(owner)
        .bind(expected_attempt)
        .bind(status.as_str())
        .bind(next_attempt_at)
        .bind(last_error)
        .bind(updated_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("fail attachment operation", error))?;
        Ok(result.rows_affected() == 1)
    }
}

#[derive(sqlx::FromRow)]
struct AttachmentOperationRow {
    id: String,
    kind: String,
    note_id: String,
    attachment_id: String,
    storage_generation: String,
    object_key: String,
    status: String,
    attempts: i64,
    next_attempt_at: Option<i64>,
    lease_owner: Option<String>,
    lease_expires_at: Option<i64>,
    last_error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl AttachmentOperationRow {
    fn into_operation(self) -> StorageResult<AttachmentOperation> {
        Ok(AttachmentOperation {
            id: self.id,
            kind: AttachmentOperationKind::from_str(&self.kind)?,
            note_id: self.note_id,
            attachment_id: self.attachment_id,
            storage_generation: self.storage_generation,
            object_key: self.object_key,
            status: AttachmentOperationStatus::from_str(&self.status)?,
            attempts: self.attempts,
            next_attempt_at: self.next_attempt_at,
            lease_owner: self.lease_owner,
            lease_expires_at: self.lease_expires_at,
            last_error: self.last_error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
