use crate::connection::map_turso_error as map_raw_turso_error;
use crate::TursoSession;
use note_storage::{
    AttachmentOperation, AttachmentOperationKind, AttachmentOperationRepository,
    AttachmentOperationStatus, NewAttachmentOperation, StorageError, StorageErrorKind,
    StorageResult,
};
use std::str::FromStr as _;

const OPERATION_COLUMNS: &str = "id, kind, note_id, attachment_id, storage_generation, object_key, \
    status, attempts, next_attempt_at, lease_owner, lease_expires_at, last_error, created_at, updated_at";

fn map_turso_error(context: &'static str, error: turso::Error) -> StorageError {
    let error = map_raw_turso_error(context, error);
    StorageError::new(error.kind(), context)
}

#[async_trait::async_trait]
impl AttachmentOperationRepository for TursoSession {
    async fn insert_attachment_operation(
        &self,
        operation: NewAttachmentOperation,
    ) -> StorageResult<AttachmentOperation> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "INSERT INTO attachment_operations (
                 id, kind, note_id, attachment_id, storage_generation, object_key,
                 status, attempts, next_attempt_at, lease_owner, lease_expires_at,
                 last_error, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(object_key) DO UPDATE SET object_key = excluded.object_key
             RETURNING {OPERATION_COLUMNS}"
        );
        let mut rows = self
            .connection
            .query(
                &sql,
                turso::params![
                    operation.id,
                    operation.kind.as_str(),
                    operation.note_id,
                    operation.attachment_id,
                    operation.storage_generation,
                    operation.object_key,
                    operation.status.as_str(),
                    operation.attempts,
                    operation.next_attempt_at,
                    operation.lease_owner,
                    operation.lease_expires_at,
                    operation.last_error,
                    operation.created_at,
                    operation.updated_at,
                ],
            )
            .await
            .map_err(|error| map_turso_error("insert attachment operation", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read inserted attachment operation", error))?
            .ok_or_else(|| {
                StorageError::new(
                    StorageErrorKind::Operation,
                    "attachment operation insert returned no row",
                )
            })?;
        decode_attachment_operation(&row)
    }

    async fn get_attachment_operation(
        &self,
        id: &str,
    ) -> StorageResult<Option<AttachmentOperation>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM attachment_operations WHERE id = ?1");
        let mut rows = self
            .connection
            .query(&sql, turso::params![id])
            .await
            .map_err(|error| map_turso_error("query attachment operation", error))?;
        rows.next()
            .await
            .map_err(|error| map_turso_error("read attachment operation", error))?
            .map(|row| decode_attachment_operation(&row))
            .transpose()
    }

    async fn list_attachment_operations_for_note(
        &self,
        note_id: &str,
    ) -> StorageResult<Vec<AttachmentOperation>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {OPERATION_COLUMNS}
             FROM attachment_operations
             WHERE note_id = ?1
             ORDER BY created_at, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![note_id])
            .await
            .map_err(|error| map_turso_error("query note attachment operations", error))?;
        let mut operations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read note attachment operations", error))?
        {
            operations.push(decode_attachment_operation(&row)?);
        }
        Ok(operations)
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

        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "UPDATE attachment_operations
             SET status = 'running',
                 attempts = attempts + 1,
                 lease_owner = ?1,
                 lease_expires_at = ?3,
                 updated_at = ?2
             WHERE id IN (
                 SELECT id
                 FROM attachment_operations
                 WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?2))
                    OR (status = 'running' AND lease_expires_at <= ?2)
                 ORDER BY created_at, id
                 LIMIT ?4
             )
             RETURNING {OPERATION_COLUMNS}"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![owner, now, lease_expires_at, limit])
            .await
            .map_err(|error| map_turso_error("claim attachment operations", error))?;
        let mut operations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read claimed attachment operations", error))?
        {
            operations.push(decode_attachment_operation(&row)?);
        }
        operations.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(operations)
    }

    async fn complete_attachment_operation(
        &self,
        id: &str,
        owner: &str,
        updated_at: i64,
    ) -> StorageResult<bool> {
        let _operation_guard = self.operation_guard().await;
        let changed = self
            .connection
            .execute(
                "UPDATE attachment_operations
                 SET status = 'completed',
                     next_attempt_at = NULL,
                     lease_owner = NULL,
                     lease_expires_at = NULL,
                     last_error = CASE WHEN status = 'completed' THEN last_error ELSE NULL END,
                     updated_at = CASE WHEN status = 'completed' THEN updated_at ELSE ?3 END
                 WHERE id = ?1
                   AND ((status = 'running' AND lease_owner = ?2) OR status = 'completed')",
                turso::params![id, owner, updated_at],
            )
            .await
            .map_err(|error| map_turso_error("complete attachment operation", error))?;
        Ok(changed != 0)
    }

    async fn fail_attachment_operation(
        &self,
        id: &str,
        owner: &str,
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

        let _operation_guard = self.operation_guard().await;
        let changed = self
            .connection
            .execute(
                "UPDATE attachment_operations
                 SET status = ?3,
                     next_attempt_at = ?4,
                     lease_owner = NULL,
                     lease_expires_at = NULL,
                     last_error = ?5,
                     updated_at = ?6
                 WHERE id = ?1 AND status = 'running' AND lease_owner = ?2",
                turso::params![
                    id,
                    owner,
                    status.as_str(),
                    next_attempt_at,
                    last_error,
                    updated_at,
                ],
            )
            .await
            .map_err(|error| map_turso_error("fail attachment operation", error))?;
        Ok(changed != 0)
    }
}

fn decode_attachment_operation(row: &turso::Row) -> StorageResult<AttachmentOperation> {
    Ok(AttachmentOperation {
        id: row
            .get(0)
            .map_err(|error| map_turso_error("decode attachment operation id", error))?,
        kind: AttachmentOperationKind::from_str(
            &row.get::<String>(1)
                .map_err(|error| map_turso_error("decode attachment operation kind", error))?,
        )?,
        note_id: row
            .get(2)
            .map_err(|error| map_turso_error("decode attachment operation note id", error))?,
        attachment_id: row
            .get(3)
            .map_err(|error| map_turso_error("decode attachment operation attachment id", error))?,
        storage_generation: row.get(4).map_err(|error| {
            map_turso_error("decode attachment operation storage generation", error)
        })?,
        object_key: row
            .get(5)
            .map_err(|error| map_turso_error("decode attachment operation object key", error))?,
        status: AttachmentOperationStatus::from_str(
            &row.get::<String>(6)
                .map_err(|error| map_turso_error("decode attachment operation status", error))?,
        )?,
        attempts: row
            .get(7)
            .map_err(|error| map_turso_error("decode attachment operation attempts", error))?,
        next_attempt_at: row.get(8).map_err(|error| {
            map_turso_error("decode attachment operation next attempt time", error)
        })?,
        lease_owner: row
            .get(9)
            .map_err(|error| map_turso_error("decode attachment operation lease owner", error))?,
        lease_expires_at: row
            .get(10)
            .map_err(|error| map_turso_error("decode attachment operation lease expiry", error))?,
        last_error: row
            .get(11)
            .map_err(|error| map_turso_error("decode attachment operation last error", error))?,
        created_at: row
            .get(12)
            .map_err(|error| map_turso_error("decode attachment operation creation time", error))?,
        updated_at: row
            .get(13)
            .map_err(|error| map_turso_error("decode attachment operation update time", error))?,
    })
}
