use crate::connection::map_turso_error;
use crate::TursoSession;
use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    order_org_event_segments_by_lineage, AppliedOrgDocumentOwnershipMove, CompareAndSwap,
    ConditionalUpdate, ExpiredOrgLeaseClosure, NewOrgAttempt, NewOrgAttemptAllocation,
    NewOrgDocument, NewOrgEvent, NewOrgLease, NewOrgWorkspace, OrgArtifactReference, OrgAttempt,
    OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate, OrgDocument,
    OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate, OrgEvent,
    OrgEventType, OrgLease, OrgLeaseClosure, OrgLeaseEndReason, OrgLeaseHeartbeat, OrgLeaseKind,
    OrgLeaseOwnershipMove, OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate,
    SanitizedOrgLease, StorageError, StorageErrorKind, StorageResult, StoredOrgOperation,
    StoredOrgTimestamp,
};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::str::FromStr as _;
use tokio::sync::OwnedMutexGuard;

const WORKSPACE_COLUMNS: &str =
    "id, slug, display_name, description, timezone, policy_schema_version, policy, revision, created_at, updated_at, archived_at";
const DOCUMENT_COLUMNS: &str =
    "id, workspace_id, path, source, content_hash, revision, created_at, updated_at";
const WORK_ITEM_COLUMNS: &str =
    "id, workspace_id, document_id, parent_id, source_order, item_type, title, state, priority,
     scheduled_raw, scheduled_local, scheduled_timezone, scheduled_utc,
     deadline_raw, deadline_local, deadline_timezone, deadline_utc,
     assignee, requires_review, created_at";
// The widest projection statement binds 20 values per row, so 64 rows remain
// comfortably below both embedded and PostgreSQL driver parameter limits.
const PROJECTION_BATCH_SIZE: usize = 64;
const EVENT_COLUMNS: &str =
    "id, workspace_id, sequence, subject_kind, subject_id, actor_id, attempt_id,
     event_type, occurred_at, summary, metadata, previous_state, resulting_state";
const ATTEMPT_COLUMNS: &str =
    "id, workspace_id, work_item_id, attempt_number, actor_id, status, started_at, ended_at,
     error, result_summary, review_outcome, note_refs, artifacts, metadata";
const LEASE_COLUMNS: &str =
    "id, workspace_id, work_item_id, attempt_id, kind, actor_id, fencing_token_hash,
     acquired_at, last_heartbeat_at, expires_at, ended_at, end_reason, expiry_event_id";
const OWNERSHIP_MOVE_SAVEPOINT: &str = "org_document_ownership_move";

#[async_trait::async_trait]
impl OrgRepository for TursoSession {
    async fn insert_org_workspace(&self, value: NewOrgWorkspace<'_>) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        let policy = serialize_policy(value.policy)?;
        self.connection
            .execute(
                "INSERT INTO org_workspaces (
                     id, slug, display_name, description, timezone,
                     policy_schema_version, policy, revision,
                     created_at, updated_at, archived_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8, NULL)",
                turso::params![
                    value.id.to_string(),
                    value.slug,
                    value.display_name,
                    value.description,
                    value.timezone,
                    value.policy_schema_version,
                    policy,
                    value.now
                ],
            )
            .await
            .map_err(|error| map_turso_error("insert Org workspace", error))?;
        Ok(())
    }

    async fn get_org_workspace(&self, id: WorkspaceId) -> StorageResult<Option<OrgWorkspace>> {
        let _operation_guard = self.operation_guard().await;
        get_org_workspace_unlocked(self, id).await
    }

    async fn get_org_workspace_by_slug(&self, slug: &str) -> StorageResult<Option<OrgWorkspace>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!("SELECT {WORKSPACE_COLUMNS} FROM org_workspaces WHERE slug = ?1");
        query_optional_workspace(self, &sql, turso::params![slug]).await
    }

    async fn list_org_workspaces(
        &self,
        include_archived: bool,
    ) -> StorageResult<Vec<OrgWorkspace>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {WORKSPACE_COLUMNS}
             FROM org_workspaces
             WHERE (?1 = 1 OR archived_at IS NULL)
             ORDER BY slug, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![i64::from(include_archived)])
            .await
            .map_err(|error| map_turso_error("list Org workspaces", error))?;
        let mut workspaces = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org workspace list", error))?
        {
            workspaces.push(decode_workspace(&row)?);
        }
        Ok(workspaces)
    }

    async fn compare_and_swap_org_workspace(
        &self,
        update: OrgWorkspaceUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgWorkspace>> {
        let _operation_guard = self.operation_guard().await;
        let policy = serialize_policy(update.policy)?;
        let sql = format!(
            "UPDATE org_workspaces
             SET slug=?2, display_name=?3, description=?4, timezone=?5,
                 policy_schema_version=?6, policy=?7, archived_at=?8, updated_at=?9,
                 revision=revision+1
             WHERE id=?1 AND revision=?10
             RETURNING {WORKSPACE_COLUMNS}"
        );
        let mut rows = self
            .connection
            .query(
                &sql,
                turso::params![
                    update.id.to_string(),
                    update.slug,
                    update.display_name,
                    update.description,
                    update.timezone,
                    update.policy_schema_version,
                    policy,
                    update.archived_at,
                    update.updated_at,
                    update.expected_revision
                ],
            )
            .await
            .map_err(|error| map_turso_error("compare-and-swap Org workspace", error))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read updated Org workspace", error))?
        {
            return decode_workspace(&row).map(CompareAndSwap::Applied);
        }
        drop(rows);

        current_workspace_revision(self, update.id)
            .await
            .map(|revision| match revision {
                Some(current_revision) => CompareAndSwap::Conflict { current_revision },
                None => CompareAndSwap::NotFound,
            })
    }

    async fn insert_org_document(&self, value: NewOrgDocument<'_>) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        self.connection
            .execute(
                "INSERT INTO org_documents (
                     id, workspace_id, path, source, content_hash, revision, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
                turso::params![
                    value.id.to_string(),
                    value.workspace_id.to_string(),
                    value.path,
                    value.source,
                    value.content_hash,
                    value.now
                ],
            )
            .await
            .map_err(|error| map_turso_error("insert Org document", error))?;
        Ok(())
    }

    async fn get_org_document(&self, id: DocumentId) -> StorageResult<Option<OrgDocument>> {
        let _operation_guard = self.operation_guard().await;
        get_org_document_unlocked(self, id).await
    }

    async fn list_org_documents(
        &self,
        workspace_id: WorkspaceId,
    ) -> StorageResult<Vec<OrgDocument>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM org_documents
             WHERE workspace_id = ?1
             ORDER BY path, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![workspace_id.to_string()])
            .await
            .map_err(|error| map_turso_error("list Org documents", error))?;
        let mut documents = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org document list", error))?
        {
            documents.push(decode_document(&row)?);
        }
        Ok(documents)
    }

    async fn compare_and_swap_org_document(
        &self,
        update: OrgDocumentUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgDocument>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "UPDATE org_documents
             SET path=?2, source=?3, content_hash=?4, updated_at=?5, revision=revision+1
             WHERE id=?1 AND revision=?6
             RETURNING {DOCUMENT_COLUMNS}"
        );
        let mut rows = self
            .connection
            .query(
                &sql,
                turso::params![
                    update.id.to_string(),
                    update.path,
                    update.source,
                    update.content_hash,
                    update.updated_at,
                    update.expected_revision
                ],
            )
            .await
            .map_err(|error| map_turso_error("compare-and-swap Org document", error))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read updated Org document", error))?
        {
            return decode_document(&row).map(CompareAndSwap::Applied);
        }
        drop(rows);

        current_document_revision(self, update.id)
            .await
            .map(|revision| match revision {
                Some(current_revision) => CompareAndSwap::Conflict { current_revision },
                None => CompareAndSwap::NotFound,
            })
    }

    async fn compare_and_swap_org_document_ownership(
        &self,
        update: OrgDocumentOwnershipMove,
    ) -> StorageResult<OrgDocumentOwnershipMoveResult> {
        let operation_guard = self.operation_guard().await;
        if update.source_workspace_id == update.target_workspace_id {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "Org document ownership move requires distinct workspaces",
            ));
        }
        let savepoint = OwnershipMoveSavepoint::begin(self, operation_guard).await?;
        let result = move_document_ownership(self, &update).await;
        savepoint.finish(result).await
    }

    async fn replace_org_document_projection(
        &self,
        document_id: DocumentId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        let plan = validate_document_projection(self, document_id, items).await?;
        stage_source_orders(self, &plan.staged_existing).await?;
        upsert_projection(self, items, &plan.order).await
    }

    async fn list_org_document_projection(
        &self,
        document_id: DocumentId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {WORK_ITEM_COLUMNS}
             FROM org_work_items
             WHERE document_id = ?1
             ORDER BY source_order, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![document_id.to_string()])
            .await
            .map_err(|error| map_turso_error("list Org document projection", error))?;
        let mut items = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org document projection", error))?
        {
            items.push(decode_projected_item(&row)?);
        }
        drop(rows);
        load_projection_relations(self, document_id, &mut items).await?;
        Ok(items)
    }

    async fn get_org_work_item(
        &self,
        id: WorkItemId,
    ) -> StorageResult<Option<OrgProjectedWorkItem>> {
        let _operation_guard = self.operation_guard().await;
        get_org_work_item_unlocked(self, id).await
    }

    async fn list_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _operation_guard = self.operation_guard().await;
        list_org_workspace_projection_unlocked(self, workspace_id).await
    }

    async fn list_org_work_item_children(
        &self,
        parent_id: WorkItemId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _operation_guard = self.operation_guard().await;
        let Some(parent) = get_org_work_item_unlocked(self, parent_id).await? else {
            return Ok(Vec::new());
        };
        Ok(
            list_org_workspace_projection_unlocked(self, parent.workspace_id)
                .await?
                .into_iter()
                .filter(|item| item.parent_id == Some(parent_id))
                .collect(),
        )
    }

    async fn list_org_work_items_linking_note(
        &self,
        note_id: &str,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT DISTINCT link.work_item_id
                 FROM org_note_links link
                 JOIN org_work_items item ON item.id = link.work_item_id
                 JOIN org_documents document ON document.id = item.document_id
                 WHERE link.note_id = ?1
                 ORDER BY item.workspace_id, document.path, item.source_order, item.id",
                turso::params![note_id],
            )
            .await
            .map_err(|error| map_turso_error("list Org reverse note links", error))?;
        let mut ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org reverse note links", error))?
        {
            ids.push(decode_work_item_id(row.get(0).map_err(|error| {
                map_turso_error("decode Org reverse note link owner", error)
            })?)?);
        }
        drop(rows);
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(item) = get_org_work_item_unlocked(self, id).await? {
                items.push(item);
            }
        }
        Ok(items)
    }

    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        let plan = validate_workspace_rebuild(self, workspace_id, items).await?;
        let workspace = workspace_id.to_string();
        for (sql, context) in [
            (
                "DELETE FROM org_dependencies
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1)
                    OR depends_on_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1)",
                "delete Org workspace dependencies for rebuild",
            ),
            (
                "DELETE FROM org_work_item_tags
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1)",
                "delete Org workspace tags for rebuild",
            ),
            (
                "DELETE FROM org_note_links
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1)",
                "delete Org workspace note links for rebuild",
            ),
            (
                "UPDATE org_work_items SET parent_id = NULL WHERE workspace_id = ?1",
                "clear Org workspace parents for rebuild",
            ),
            (
                "DELETE FROM org_work_items WHERE workspace_id = ?1",
                "delete Org workspace work items for rebuild",
            ),
        ] {
            self.connection
                .execute(sql, turso::params![workspace.clone()])
                .await
                .map_err(|error| map_turso_error(context, error))?;
        }
        upsert_projection(self, items, &plan.order).await
    }

    async fn append_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let _operation_guard = self.operation_guard().await;
        append_event(self, event, false).await
    }

    async fn append_internal_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let _operation_guard = self.operation_guard().await;
        append_event(self, event, true).await
    }

    async fn list_org_events(
        &self,
        workspace_id: WorkspaceId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _operation_guard = self.operation_guard().await;
        validate_event_limit(limit)?;
        let sql = format!(
            "SELECT {EVENT_COLUMNS}
             FROM org_events
             WHERE workspace_id=?1 AND sequence>?2
             ORDER BY sequence
             LIMIT ?3"
        );
        let mut rows = self
            .connection
            .query(
                &sql,
                turso::params![
                    workspace_id.to_string(),
                    after_sequence.unwrap_or(0),
                    limit as i64
                ],
            )
            .await
            .map_err(|error| map_turso_error("list Org events", error))?;
        let mut events = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org event list", error))?
        {
            events.push(decode_event(&row)?);
        }
        Ok(events)
    }

    async fn list_org_subject_events(
        &self,
        workspace_id: WorkspaceId,
        subject_kind: &str,
        subject_id: &str,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _operation_guard = self.operation_guard().await;
        validate_event_limit(limit)?;
        let sql = format!(
            "SELECT {EVENT_COLUMNS}
             FROM org_events
             WHERE workspace_id=?1 AND subject_kind=?2 AND subject_id=?3 AND sequence>?4
             ORDER BY sequence
             LIMIT ?5"
        );
        query_events(
            self,
            &sql,
            turso::params![
                workspace_id.to_string(),
                subject_kind,
                subject_id,
                after_sequence.unwrap_or(0),
                limit as i64
            ],
            "list Org subject events",
        )
        .await
    }

    async fn list_org_global_subject_events(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> StorageResult<Vec<OrgEvent>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {EVENT_COLUMNS}
             FROM org_events
             WHERE subject_kind=?1 AND subject_id=?2
             ORDER BY workspace_id, sequence"
        );
        let events = query_events(
            self,
            &sql,
            turso::params![subject_kind, subject_id],
            "list global Org subject events",
        )
        .await?;
        order_org_event_segments_by_lineage(events)
    }

    async fn insert_org_attempt(&self, attempt: NewOrgAttempt<'_>) -> StorageResult<OrgAttempt> {
        let _operation_guard = self.operation_guard().await;
        validate_new_attempt(&attempt)?;
        let note_refs = serialize_json(attempt.note_refs, "serialize Org attempt note refs")?;
        let artifacts = serialize_json(attempt.artifacts, "serialize Org attempt artifacts")?;
        let metadata = serialize_json(attempt.metadata, "serialize Org attempt metadata")?;
        let sql = format!(
            "INSERT INTO org_attempts (
                 id, workspace_id, work_item_id, attempt_number, actor_id, status,
                 started_at, ended_at, error, result_summary, review_outcome,
                 note_refs, artifacts, metadata
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, NULL, ?8, ?9, ?10)
             RETURNING {ATTEMPT_COLUMNS}"
        );
        query_one_attempt(
            self,
            &sql,
            turso::params![
                attempt.id,
                attempt.workspace_id.to_string(),
                attempt.work_item_id.to_string(),
                attempt.attempt_number,
                attempt.actor_id,
                attempt_status_name(attempt.status),
                attempt.started_at,
                note_refs,
                artifacts,
                metadata
            ],
            "insert Org attempt",
        )
        .await
    }

    async fn allocate_next_org_attempt(
        &self,
        attempt: NewOrgAttemptAllocation<'_>,
    ) -> StorageResult<OrgAttempt> {
        let _operation_guard = self.operation_guard().await;
        validate_attempt_allocation(&attempt)?;
        let note_refs = serialize_json(attempt.note_refs, "serialize Org attempt note refs")?;
        let artifacts = serialize_json(attempt.artifacts, "serialize Org attempt artifacts")?;
        let metadata = serialize_json(attempt.metadata, "serialize Org attempt metadata")?;
        let sql = format!(
            "INSERT INTO org_attempts (
                 id, workspace_id, work_item_id, attempt_number, actor_id, status,
                 started_at, ended_at, error, result_summary, review_outcome,
                 note_refs, artifacts, metadata
             ) SELECT ?1, ?2, ?3,
                      COALESCE(MAX(attempt_number), 0) + 1,
                      ?4, 'running', ?5, NULL, NULL, NULL, NULL, ?6, ?7, ?8
               FROM org_attempts
               WHERE work_item_id=?3
             RETURNING {ATTEMPT_COLUMNS}"
        );
        query_one_attempt(
            self,
            &sql,
            turso::params![
                attempt.id,
                attempt.workspace_id.to_string(),
                attempt.work_item_id.to_string(),
                attempt.actor_id,
                attempt.started_at,
                note_refs,
                artifacts,
                metadata
            ],
            "allocate next Org attempt",
        )
        .await
    }

    async fn get_org_attempt(&self, id: &str) -> StorageResult<Option<OrgAttempt>> {
        let _operation_guard = self.operation_guard().await;
        get_org_attempt_unlocked(self, id).await
    }

    async fn list_org_attempts(&self, work_item_id: WorkItemId) -> StorageResult<Vec<OrgAttempt>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {ATTEMPT_COLUMNS}
             FROM org_attempts
             WHERE work_item_id=?1
             ORDER BY attempt_number, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![work_item_id.to_string()])
            .await
            .map_err(|error| map_turso_error("list Org attempts", error))?;
        let mut attempts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org attempt list", error))?
        {
            attempts.push(decode_attempt(&row)?);
        }
        Ok(attempts)
    }

    async fn count_org_attempts(&self, work_item_id: WorkItemId) -> StorageResult<i64> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*) FROM org_attempts WHERE work_item_id=?1",
                turso::params![work_item_id.to_string()],
            )
            .await
            .map_err(|error| map_turso_error("count Org attempts", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org attempt count", error))?
            .ok_or_else(|| StorageError::new(StorageErrorKind::Operation, "missing count row"))?;
        row.get(0)
            .map_err(|error| map_turso_error("decode Org attempt count", error))
    }

    async fn update_org_attempt(
        &self,
        update: OrgAttemptUpdate<'_>,
    ) -> StorageResult<ConditionalUpdate<OrgAttempt>> {
        let _operation_guard = self.operation_guard().await;
        validate_attempt_update(&update)?;
        let note_refs = serialize_json(update.note_refs, "serialize Org attempt note refs")?;
        let artifacts = serialize_json(update.artifacts, "serialize Org attempt artifacts")?;
        let metadata = serialize_json(update.metadata, "serialize Org attempt metadata")?;
        let sql = format!(
            "UPDATE org_attempts
             SET status=?2, ended_at=?3, error=?4, result_summary=?5, review_outcome=?6,
                 note_refs=?7, artifacts=?8, metadata=?9
             WHERE id=?1 AND status=?10
             RETURNING {ATTEMPT_COLUMNS}"
        );
        let updated = query_optional_attempt(
            self,
            &sql,
            turso::params![
                update.id,
                attempt_status_name(update.status),
                update.ended_at,
                update.error,
                update.result_summary,
                update.review_outcome,
                note_refs,
                artifacts,
                metadata,
                attempt_status_name(update.expected_status)
            ],
            "conditionally update Org attempt",
        )
        .await?;
        if let Some(attempt) = updated {
            return Ok(ConditionalUpdate::Applied(attempt));
        }
        Ok(
            if get_org_attempt_unlocked(self, update.id).await?.is_some() {
                ConditionalUpdate::Conflict
            } else {
                ConditionalUpdate::NotFound
            },
        )
    }

    async fn insert_org_lease_if_capacity(
        &self,
        lease: NewOrgLease<'_>,
        capacity: i64,
        now: i64,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        validate_new_lease(&lease, capacity, now)?;
        let sql = format!(
            "INSERT INTO org_leases (
                 id, workspace_id, work_item_id, attempt_id, kind, actor_id,
                 fencing_token_hash, acquired_at, last_heartbeat_at, expires_at,
                 ended_at, end_reason, expiry_event_id
             )
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, NULL
             WHERE NOT EXISTS (SELECT 1 FROM org_leases WHERE id=?1)
               AND NOT EXISTS (
                   SELECT 1 FROM org_leases WHERE work_item_id=?3 AND ended_at IS NULL
               )
               AND (SELECT COUNT(*) FROM org_leases
                    WHERE workspace_id=?2 AND ended_at IS NULL AND expires_at>?11) < ?12
               AND EXISTS (
                   SELECT 1 FROM org_attempts
                   WHERE id=?4 AND work_item_id=?3
                     AND ((?5='execution' AND status='running')
                       OR (?5='review' AND status='submitted'))
               )
             RETURNING {LEASE_COLUMNS}"
        );
        let lease = query_optional_lease(
            self,
            &sql,
            turso::params![
                lease.id,
                lease.workspace_id.to_string(),
                lease.work_item_id.to_string(),
                lease.attempt_id,
                lease_kind_name(lease.kind),
                lease.actor_id,
                lease.fencing_token_hash,
                lease.acquired_at,
                lease.last_heartbeat_at,
                lease.expires_at,
                now,
                capacity
            ],
            "insert Org lease within capacity",
        )
        .await?;
        Ok(match lease {
            Some(lease) => ConditionalUpdate::Applied(lease.into()),
            None => ConditionalUpdate::Conflict,
        })
    }

    async fn get_open_org_lease_internal(
        &self,
        work_item_id: WorkItemId,
    ) -> StorageResult<Option<OrgLease>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {LEASE_COLUMNS} FROM org_leases
             WHERE work_item_id=?1 AND ended_at IS NULL"
        );
        query_optional_lease(
            self,
            &sql,
            turso::params![work_item_id.to_string()],
            "query open Org lease",
        )
        .await
    }

    async fn get_active_org_lease(
        &self,
        work_item_id: WorkItemId,
        now: i64,
    ) -> StorageResult<Option<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {LEASE_COLUMNS} FROM org_leases
             WHERE work_item_id=?1 AND ended_at IS NULL AND expires_at>?2"
        );
        query_optional_lease(
            self,
            &sql,
            turso::params![work_item_id.to_string(), now],
            "query active Org lease",
        )
        .await
        .map(|lease| lease.map(Into::into))
    }

    async fn list_org_lease_history(
        &self,
        work_item_id: WorkItemId,
    ) -> StorageResult<Vec<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        let sql = format!(
            "SELECT {LEASE_COLUMNS} FROM org_leases
             WHERE work_item_id=?1 ORDER BY acquired_at, id"
        );
        let mut rows = self
            .connection
            .query(&sql, turso::params![work_item_id.to_string()])
            .await
            .map_err(|error| map_turso_error("list Org lease history", error))?;
        let mut leases = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read Org lease history", error))?
        {
            leases.push(decode_lease(&row)?.into());
        }
        Ok(leases)
    }

    async fn count_active_org_leases(
        &self,
        workspace_id: WorkspaceId,
        now: i64,
    ) -> StorageResult<i64> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*) FROM org_leases
                 WHERE workspace_id=?1 AND ended_at IS NULL AND expires_at>?2",
                turso::params![workspace_id.to_string(), now],
            )
            .await
            .map_err(|error| map_turso_error("count active Org leases", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read active Org lease count", error))?
            .ok_or_else(|| StorageError::new(StorageErrorKind::Operation, "missing count row"))?;
        row.get(0)
            .map_err(|error| map_turso_error("decode active Org lease count", error))
    }

    async fn heartbeat_org_lease(
        &self,
        update: OrgLeaseHeartbeat<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        validate_lease_heartbeat(&update)?;
        let sql = format!(
            "UPDATE org_leases
             SET last_heartbeat_at=?8, expires_at=?9
             WHERE id=?1 AND workspace_id=?2 AND work_item_id=?3
               AND fencing_token_hash=?4 AND kind=?5 AND actor_id=?6
               AND ended_at IS NULL AND expires_at>?7
               AND last_heartbeat_at<=?8 AND expires_at<=?9
             RETURNING {LEASE_COLUMNS}"
        );
        conditional_lease_write(
            self,
            &sql,
            turso::params![
                update.proof.lease_id,
                update.proof.workspace_id.to_string(),
                update.proof.work_item_id.to_string(),
                update.proof.fencing_token_hash,
                lease_kind_name(update.proof.kind),
                update.proof.actor_id,
                update.proof.now,
                update.last_heartbeat_at,
                update.expires_at
            ],
            update.proof.lease_id,
            "heartbeat Org lease",
        )
        .await
    }

    async fn close_org_lease(
        &self,
        update: OrgLeaseClosure<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        validate_lease_closure(&update)?;
        let sql = format!(
            "UPDATE org_leases
             SET ended_at=?8, end_reason=?9, expiry_event_id=NULL
             WHERE id=?1 AND workspace_id=?2 AND work_item_id=?3
               AND fencing_token_hash=?4 AND kind=?5 AND actor_id=?6
               AND ended_at IS NULL AND expires_at>?7
             RETURNING {LEASE_COLUMNS}"
        );
        conditional_lease_write(
            self,
            &sql,
            turso::params![
                update.proof.lease_id,
                update.proof.workspace_id.to_string(),
                update.proof.work_item_id.to_string(),
                update.proof.fencing_token_hash,
                lease_kind_name(update.proof.kind),
                update.proof.actor_id,
                update.proof.now,
                update.ended_at,
                lease_end_reason_name(update.end_reason)
            ],
            update.proof.lease_id,
            "close Org lease",
        )
        .await
    }

    async fn close_expired_org_lease(
        &self,
        update: ExpiredOrgLeaseClosure<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        if update.lease_id.trim().is_empty()
            || update.expiry_event_id.trim().is_empty()
            || update.ended_at < update.now
        {
            return Err(StorageError::new(
                StorageErrorKind::Operation,
                "invalid expired Org lease closure",
            ));
        }
        let sql = format!(
            "UPDATE org_leases
             SET ended_at=?4, end_reason='lease_expiry', expiry_event_id=?5
             WHERE id=?1 AND work_item_id=?2 AND ended_at IS NULL AND expires_at<=?3
             RETURNING {LEASE_COLUMNS}"
        );
        conditional_lease_write(
            self,
            &sql,
            turso::params![
                update.lease_id,
                update.work_item_id.to_string(),
                update.now,
                update.ended_at,
                update.expiry_event_id
            ],
            update.lease_id,
            "close expired Org lease",
        )
        .await
    }

    async fn move_org_lease_ownership(
        &self,
        update: OrgLeaseOwnershipMove<'_>,
    ) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
        let _operation_guard = self.operation_guard().await;
        validate_lease_move(&update)?;
        let sql = format!(
            "UPDATE org_leases
             SET workspace_id=?8
             WHERE id=?1 AND workspace_id=?2 AND work_item_id=?3
               AND fencing_token_hash=?4 AND kind=?5 AND actor_id=?6
               AND ended_at IS NULL AND expires_at>?7
               AND (SELECT COUNT(*) FROM org_leases
                    WHERE workspace_id=?8 AND ended_at IS NULL AND expires_at>?7) < ?9
             RETURNING {LEASE_COLUMNS}"
        );
        conditional_lease_write(
            self,
            &sql,
            turso::params![
                update.proof.lease_id,
                update.proof.workspace_id.to_string(),
                update.proof.work_item_id.to_string(),
                update.proof.fencing_token_hash,
                lease_kind_name(update.proof.kind),
                update.proof.actor_id,
                update.proof.now,
                update.target_workspace_id.to_string(),
                update.target_capacity
            ],
            update.proof.lease_id,
            "move Org lease ownership",
        )
        .await
    }

    async fn insert_org_operation(&self, operation: &StoredOrgOperation) -> StorageResult<()> {
        let _operation_guard = self.operation_guard().await;
        let result = serde_json::to_string(&operation.result).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org operation result",
                error,
            )
        })?;
        self.connection
            .execute(
                "INSERT INTO org_operations (
                     workspace_id, operation_id, request_fingerprint, result, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                turso::params![
                    operation.workspace_id.to_string(),
                    operation.operation_id.as_str(),
                    operation.request_fingerprint.as_str(),
                    result,
                    operation.created_at
                ],
            )
            .await
            .map_err(|error| map_turso_error("insert Org operation", error))?;
        Ok(())
    }

    async fn get_org_operation(
        &self,
        workspace_id: WorkspaceId,
        operation_id: &str,
    ) -> StorageResult<Option<StoredOrgOperation>> {
        let _operation_guard = self.operation_guard().await;
        let mut rows = self
            .connection
            .query(
                "SELECT workspace_id, operation_id, request_fingerprint, result, created_at
                 FROM org_operations
                 WHERE workspace_id=?1 AND operation_id=?2",
                turso::params![workspace_id.to_string(), operation_id],
            )
            .await
            .map_err(|error| map_turso_error("query Org operation", error))?;
        rows.next()
            .await
            .map_err(|error| map_turso_error("read Org operation", error))?
            .as_ref()
            .map(decode_operation)
            .transpose()
    }
}

async fn get_org_workspace_unlocked(
    session: &TursoSession,
    id: WorkspaceId,
) -> StorageResult<Option<OrgWorkspace>> {
    let sql = format!("SELECT {WORKSPACE_COLUMNS} FROM org_workspaces WHERE id = ?1");
    query_optional_workspace(session, &sql, turso::params![id.to_string()]).await
}

async fn get_org_document_unlocked(
    session: &TursoSession,
    id: DocumentId,
) -> StorageResult<Option<OrgDocument>> {
    let sql = format!("SELECT {DOCUMENT_COLUMNS} FROM org_documents WHERE id = ?1");
    query_optional_document(session, &sql, turso::params![id.to_string()]).await
}

async fn get_org_work_item_unlocked(
    session: &TursoSession,
    id: WorkItemId,
) -> StorageResult<Option<OrgProjectedWorkItem>> {
    let sql = format!("SELECT {WORK_ITEM_COLUMNS} FROM org_work_items WHERE id = ?1");
    let mut rows = session
        .connection
        .query(&sql, turso::params![id.to_string()])
        .await
        .map_err(|error| map_turso_error("query Org work item", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read Org work item", error))?
    else {
        return Ok(None);
    };
    let item = decode_projected_item(&row)?;
    drop(rows);
    let document_id = item.document_id;
    let mut items = vec![item];
    load_projection_relations(session, document_id, &mut items).await?;
    Ok(items.pop())
}

async fn list_org_workspace_projection_unlocked(
    session: &TursoSession,
    workspace_id: WorkspaceId,
) -> StorageResult<Vec<OrgProjectedWorkItem>> {
    let sql = "SELECT item.id, item.workspace_id, item.document_id, item.parent_id,
                item.source_order, item.item_type, item.title, item.state, item.priority,
                item.scheduled_raw, item.scheduled_local, item.scheduled_timezone,
                item.scheduled_utc, item.deadline_raw, item.deadline_local,
                item.deadline_timezone, item.deadline_utc, item.assignee,
                item.requires_review, item.created_at
         FROM org_work_items item
         JOIN org_documents document ON document.id=item.document_id
         WHERE item.workspace_id = ?1
         ORDER BY document.path, document.id, item.source_order, item.id";
    let mut rows = session
        .connection
        .query(sql, turso::params![workspace_id.to_string()])
        .await
        .map_err(|error| map_turso_error("list Org workspace projection", error))?;
    let mut items = Vec::new();
    let mut document_ids = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read Org workspace projection", error))?
    {
        let item = decode_projected_item(&row)?;
        document_ids.insert(item.document_id);
        items.push(item);
    }
    drop(rows);
    for document_id in document_ids {
        load_projection_relations(session, document_id, &mut items).await?;
    }
    Ok(items)
}

async fn get_org_attempt_unlocked(
    session: &TursoSession,
    id: &str,
) -> StorageResult<Option<OrgAttempt>> {
    let sql = format!("SELECT {ATTEMPT_COLUMNS} FROM org_attempts WHERE id=?1");
    query_optional_attempt(session, &sql, turso::params![id], "query Org attempt").await
}

async fn move_document_ownership(
    session: &TursoSession,
    update: &OrgDocumentOwnershipMove,
) -> StorageResult<OrgDocumentOwnershipMoveResult> {
    let Some(document) = get_org_document_unlocked(session, update.document_id).await? else {
        return Ok(OrgDocumentOwnershipMoveResult::NotFound);
    };
    let Some(source_workspace) =
        get_org_workspace_unlocked(session, update.source_workspace_id).await?
    else {
        return Ok(OrgDocumentOwnershipMoveResult::NotFound);
    };
    let Some(target_workspace) =
        get_org_workspace_unlocked(session, update.target_workspace_id).await?
    else {
        return Ok(OrgDocumentOwnershipMoveResult::NotFound);
    };
    if document.workspace_id != update.source_workspace_id
        || document.revision != update.expected_document_revision
        || source_workspace.revision != update.expected_source_workspace_revision
        || target_workspace.revision != update.expected_target_workspace_revision
    {
        return Ok(OrgDocumentOwnershipMoveResult::Conflict {
            current_document_workspace_id: document.workspace_id,
            current_document_revision: document.revision,
            current_source_workspace_revision: source_workspace.revision,
            current_target_workspace_revision: target_workspace.revision,
        });
    }

    let moved = self_execute(
        session,
        "UPDATE org_documents
         SET workspace_id=?2, revision=revision+1, updated_at=?3
         WHERE id=?1 AND workspace_id=?4 AND revision=?5",
        turso::params![
            update.document_id.to_string(),
            update.target_workspace_id.to_string(),
            update.updated_at,
            update.source_workspace_id.to_string(),
            update.expected_document_revision
        ],
        "move Org document ownership",
    )
    .await?;
    if moved != 1 {
        return Err(StorageError::new(
            StorageErrorKind::Conflict,
            "Org document ownership changed during compare-and-swap",
        ));
    }
    for (workspace_id, expected_revision, context) in [
        (
            update.source_workspace_id,
            update.expected_source_workspace_revision,
            "advance source Org workspace revision",
        ),
        (
            update.target_workspace_id,
            update.expected_target_workspace_revision,
            "advance target Org workspace revision",
        ),
    ] {
        let changed = self_execute(
            session,
            "UPDATE org_workspaces
             SET revision=revision+1, updated_at=?2
             WHERE id=?1 AND revision=?3",
            turso::params![
                workspace_id.to_string(),
                update.updated_at,
                expected_revision
            ],
            context,
        )
        .await?;
        if changed != 1 {
            return Err(StorageError::new(
                StorageErrorKind::Conflict,
                "Org workspace changed during document ownership compare-and-swap",
            ));
        }
    }

    let document = get_org_document_unlocked(session, update.document_id)
        .await?
        .ok_or_else(|| {
            StorageError::new(StorageErrorKind::Corrupt, "moved Org document vanished")
        })?;
    Ok(OrgDocumentOwnershipMoveResult::Applied(
        AppliedOrgDocumentOwnershipMove {
            document,
            source_workspace_revision: update.expected_source_workspace_revision + 1,
            target_workspace_revision: update.expected_target_workspace_revision + 1,
        },
    ))
}

struct OwnershipMoveSavepoint {
    connection: turso::Connection,
    operation_guard: Option<OwnedMutexGuard<()>>,
    active: bool,
}

impl OwnershipMoveSavepoint {
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
            .execute(&format!("SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"), ())
            .await;
        match begin {
            Ok(_) => Ok(savepoint),
            Err(error) => {
                let primary = map_turso_error("begin Org document ownership move", error);
                Err(savepoint.rollback_with_cleanup(primary).await)
            }
        }
    }

    async fn finish<T>(mut self, result: StorageResult<T>) -> StorageResult<T> {
        match result {
            Ok(value) => match self
                .connection
                .execute(&format!("RELEASE SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"), ())
                .await
            {
                Ok(_) => {
                    self.active = false;
                    Ok(value)
                }
                Err(error) => {
                    let primary = map_turso_error("finish Org document ownership move", error);
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
                &format!("ROLLBACK TO SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"),
                (),
            )
            .await
            .err();
        let release = self
            .connection
            .execute(&format!("RELEASE SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"), ())
            .await
            .err();
        self.active = false;
        savepoint_cleanup_error(primary, rollback, release)
    }
}

impl Drop for OwnershipMoveSavepoint {
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
                        &format!("ROLLBACK TO SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"),
                        (),
                    )
                    .await;
                let _ = connection
                    .execute(&format!("RELEASE SAVEPOINT {OWNERSHIP_MOVE_SAVEPOINT}"), ())
                    .await;
            });
        } else {
            // Never allow another operation to enter an abandoned savepoint.
            std::mem::forget(operation_guard);
        }
    }
}

fn savepoint_cleanup_error(
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

async fn self_execute(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    context: &str,
) -> StorageResult<u64> {
    session
        .connection
        .execute(sql, params)
        .await
        .map_err(|error| map_turso_error(context, error))
}

async fn append_event(
    session: &TursoSession,
    event: NewOrgEvent<'_>,
    internal: bool,
) -> StorageResult<OrgEvent> {
    validate_event(session, &event, internal).await?;
    let metadata = serialize_json(event.metadata, "serialize Org event metadata")?;
    let sql = format!(
        "INSERT INTO org_events (
             id, workspace_id, sequence, subject_kind, subject_id,
             actor_id, attempt_id, event_type, occurred_at, summary, metadata,
             previous_state, resulting_state
         )
         SELECT ?1, workspace.id, workspace.last_event_sequence+1,
                ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
         FROM org_workspaces workspace
         WHERE workspace.id=?2
         RETURNING {EVENT_COLUMNS}"
    );
    let mut rows = session
        .connection
        .query(
            &sql,
            turso::params![
                event.id,
                event.workspace_id.to_string(),
                event.subject_kind,
                event.subject_id,
                event.actor_id,
                event.attempt_id,
                event.event_type.as_str(),
                event.occurred_at,
                event.summary,
                metadata,
                event.previous_state,
                event.resulting_state
            ],
        )
        .await
        .map_err(|error| map_turso_error("append Org event", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read appended Org event", error))?
    else {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "Org event workspace does not exist",
        ));
    };
    let stored = decode_event(&row)?;
    if rows
        .next()
        .await
        .map_err(|error| map_turso_error("finish atomic Org event append", error))?
        .is_some()
    {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "append Org event returned more than one row",
        ));
    }
    Ok(stored)
}

async fn validate_event(
    session: &TursoSession,
    event: &NewOrgEvent<'_>,
    internal: bool,
) -> StorageResult<()> {
    for (label, value) in [
        ("id", event.id),
        ("subject kind", event.subject_kind),
        ("subject id", event.subject_id),
        ("actor id", event.actor_id),
        ("summary", event.summary),
    ] {
        if value.trim().is_empty() {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                format!("Org event {label} must not be empty"),
            ));
        }
    }
    if !event.event_type.is_known() {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "client and internal event writes require a known Org event type",
        ));
    }
    if internal != (event.actor_id == "system") {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            if internal {
                "internal Org events must use the system actor"
            } else {
                "client Org events cannot use the reserved system actor"
            },
        ));
    }
    if event.previous_state.is_some() != event.resulting_state.is_some()
        || event
            .previous_state
            .is_some_and(|value| value.trim().is_empty())
        || event
            .resulting_state
            .is_some_and(|value| value.trim().is_empty())
    {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "Org event state snapshots must be paired and non-empty",
        ));
    }
    if let Some(attempt_id) = event.attempt_id {
        let Some(attempt) = get_org_attempt_unlocked(session, attempt_id).await? else {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "Org event attempt does not exist",
            ));
        };
        if attempt.workspace_id != event.workspace_id
            || (event.subject_kind == "work_item"
                && event.subject_id != attempt.work_item_id.to_string())
        {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "Org event attempt does not match its workspace and work item subject",
            ));
        }
    }
    Ok(())
}

async fn query_events(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    context: &str,
) -> StorageResult<Vec<OrgEvent>> {
    let mut rows = session
        .connection
        .query(sql, params)
        .await
        .map_err(|error| map_turso_error(context, error))?;
    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error(context, error))?
    {
        events.push(decode_event(&row)?);
    }
    Ok(events)
}

fn validate_new_attempt(attempt: &NewOrgAttempt<'_>) -> StorageResult<()> {
    if attempt.id.trim().is_empty() || attempt.actor_id.trim().is_empty() {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "Org attempt id and actor id must not be empty",
        ));
    }
    if attempt.attempt_number < 1 {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "Org attempt number must be positive",
        ));
    }
    Ok(())
}

fn validate_attempt_allocation(attempt: &NewOrgAttemptAllocation<'_>) -> StorageResult<()> {
    if attempt.id.trim().is_empty() || attempt.actor_id.trim().is_empty() {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "Org attempt id and actor id must not be empty",
        ));
    }
    if attempt.started_at <= 0 {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "Org attempt start must be positive",
        ));
    }
    Ok(())
}

fn validate_attempt_update(update: &OrgAttemptUpdate<'_>) -> StorageResult<()> {
    if update.id.trim().is_empty() {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "Org attempt id must not be empty",
        ));
    }
    let allowed = matches!(
        (update.expected_status, update.status),
        (
            OrgAttemptStatus::Running,
            OrgAttemptStatus::Submitted
                | OrgAttemptStatus::Completed
                | OrgAttemptStatus::Failed
                | OrgAttemptStatus::Cancelled
                | OrgAttemptStatus::Expired
        ) | (
            OrgAttemptStatus::Submitted,
            OrgAttemptStatus::Completed | OrgAttemptStatus::Failed | OrgAttemptStatus::Cancelled
        )
    );
    if !allowed {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "invalid Org attempt status transition",
        ));
    }
    Ok(())
}

fn validate_new_lease(lease: &NewOrgLease<'_>, capacity: i64, now: i64) -> StorageResult<()> {
    if lease.id.trim().is_empty()
        || lease.actor_id.trim().is_empty()
        || lease.attempt_id.trim().is_empty()
        || lease.fencing_token_hash.len() != 64
        || !lease
            .fencing_token_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "invalid Org lease identity or fencing hash",
        ));
    }
    if capacity < 1
        || now <= 0
        || lease.acquired_at <= 0
        || lease.last_heartbeat_at < lease.acquired_at
        || lease.expires_at < lease.last_heartbeat_at
    {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "invalid Org lease capacity or timestamps",
        ));
    }
    Ok(())
}

fn validate_lease_heartbeat(update: &OrgLeaseHeartbeat<'_>) -> StorageResult<()> {
    validate_lease_proof(&update.proof)?;
    if update.last_heartbeat_at < update.proof.now || update.expires_at < update.last_heartbeat_at {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "invalid Org lease heartbeat timestamps",
        ));
    }
    Ok(())
}

fn validate_lease_closure(update: &OrgLeaseClosure<'_>) -> StorageResult<()> {
    validate_lease_proof(&update.proof)?;
    if update.end_reason == OrgLeaseEndReason::LeaseExpiry || update.ended_at < update.proof.now {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "invalid active Org lease closure",
        ));
    }
    Ok(())
}

fn validate_lease_move(update: &OrgLeaseOwnershipMove<'_>) -> StorageResult<()> {
    validate_lease_proof(&update.proof)?;
    if update.target_capacity < 1 || update.target_workspace_id == update.proof.workspace_id {
        return Err(StorageError::new(
            StorageErrorKind::Constraint,
            "invalid Org lease ownership move",
        ));
    }
    Ok(())
}

fn validate_lease_proof(proof: &note_storage::OrgLeaseProof<'_>) -> StorageResult<()> {
    if proof.lease_id.trim().is_empty()
        || proof.actor_id.trim().is_empty()
        || proof.fencing_token_hash.len() != 64
        || !proof
            .fencing_token_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || proof.now <= 0
    {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "invalid Org lease proof",
        ));
    }
    Ok(())
}

fn lease_kind_name(kind: OrgLeaseKind) -> &'static str {
    match kind {
        OrgLeaseKind::Execution => "execution",
        OrgLeaseKind::Review => "review",
    }
}

fn decode_lease_kind(value: &str) -> StorageResult<OrgLeaseKind> {
    match value {
        "execution" => Ok(OrgLeaseKind::Execution),
        "review" => Ok(OrgLeaseKind::Review),
        _ => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("stored Org lease has unknown kind {value:?}"),
        )),
    }
}

fn lease_end_reason_name(reason: OrgLeaseEndReason) -> &'static str {
    match reason {
        OrgLeaseEndReason::Release => "release",
        OrgLeaseEndReason::Completion => "completion",
        OrgLeaseEndReason::Failure => "failure",
        OrgLeaseEndReason::Block => "block",
        OrgLeaseEndReason::Cancellation => "cancellation",
        OrgLeaseEndReason::LeaseExpiry => "lease_expiry",
        OrgLeaseEndReason::ReviewRequest => "review_request",
        OrgLeaseEndReason::Approval => "approval",
        OrgLeaseEndReason::Rejection => "rejection",
        OrgLeaseEndReason::Reassignment => "reassignment",
    }
}

fn decode_lease_end_reason(value: &str) -> StorageResult<OrgLeaseEndReason> {
    match value {
        "release" => Ok(OrgLeaseEndReason::Release),
        "completion" => Ok(OrgLeaseEndReason::Completion),
        "failure" => Ok(OrgLeaseEndReason::Failure),
        "block" => Ok(OrgLeaseEndReason::Block),
        "cancellation" => Ok(OrgLeaseEndReason::Cancellation),
        "lease_expiry" => Ok(OrgLeaseEndReason::LeaseExpiry),
        "review_request" => Ok(OrgLeaseEndReason::ReviewRequest),
        "approval" => Ok(OrgLeaseEndReason::Approval),
        "rejection" => Ok(OrgLeaseEndReason::Rejection),
        "reassignment" => Ok(OrgLeaseEndReason::Reassignment),
        _ => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("stored Org lease has unknown end reason {value:?}"),
        )),
    }
}

fn attempt_status_name(status: OrgAttemptStatus) -> &'static str {
    match status {
        OrgAttemptStatus::Running => "running",
        OrgAttemptStatus::Submitted => "submitted",
        OrgAttemptStatus::Completed => "completed",
        OrgAttemptStatus::Failed => "failed",
        OrgAttemptStatus::Cancelled => "cancelled",
        OrgAttemptStatus::Expired => "expired",
    }
}

fn decode_attempt_status(value: &str) -> StorageResult<OrgAttemptStatus> {
    match value {
        "running" => Ok(OrgAttemptStatus::Running),
        "submitted" => Ok(OrgAttemptStatus::Submitted),
        "completed" => Ok(OrgAttemptStatus::Completed),
        "failed" => Ok(OrgAttemptStatus::Failed),
        "cancelled" => Ok(OrgAttemptStatus::Cancelled),
        "expired" => Ok(OrgAttemptStatus::Expired),
        _ => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("stored Org attempt has unknown status {value:?}"),
        )),
    }
}

fn serialize_json<T: serde::Serialize + ?Sized>(value: &T, context: &str) -> StorageResult<String> {
    serde_json::to_string(value)
        .map_err(|error| StorageError::with_source(StorageErrorKind::Operation, context, error))
}

async fn query_one_attempt(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    context: &str,
) -> StorageResult<OrgAttempt> {
    query_optional_attempt(session, sql, params, context)
        .await?
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Operation,
                "Org attempt write returned no row",
            )
        })
}

async fn query_optional_attempt(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    context: &str,
) -> StorageResult<Option<OrgAttempt>> {
    let mut rows = session
        .connection
        .query(sql, params)
        .await
        .map_err(|error| map_turso_error(context, error))?;
    rows.next()
        .await
        .map_err(|error| map_turso_error(context, error))?
        .as_ref()
        .map(decode_attempt)
        .transpose()
}

fn decode_attempt(row: &turso::Row) -> StorageResult<OrgAttempt> {
    let note_refs_text: String = row
        .get(11)
        .map_err(|error| map_turso_error("decode Org attempt note refs", error))?;
    let artifacts_text: String = row
        .get(12)
        .map_err(|error| map_turso_error("decode Org attempt artifacts", error))?;
    let metadata_text: String = row
        .get(13)
        .map_err(|error| map_turso_error("decode Org attempt metadata", error))?;
    Ok(OrgAttempt {
        id: row
            .get(0)
            .map_err(|error| map_turso_error("decode Org attempt id", error))?,
        workspace_id: decode_workspace_id(
            row.get(1)
                .map_err(|error| map_turso_error("decode Org attempt workspace", error))?,
        )?,
        work_item_id: decode_work_item_id(
            row.get(2)
                .map_err(|error| map_turso_error("decode Org attempt work item", error))?,
        )?,
        attempt_number: row
            .get(3)
            .map_err(|error| map_turso_error("decode Org attempt number", error))?,
        actor_id: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org attempt actor", error))?,
        status: decode_attempt_status(
            &row.get::<String>(5)
                .map_err(|error| map_turso_error("decode Org attempt status", error))?,
        )?,
        started_at: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org attempt start", error))?,
        ended_at: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org attempt end", error))?,
        error: row
            .get(8)
            .map_err(|error| map_turso_error("decode Org attempt error", error))?,
        result_summary: row
            .get(9)
            .map_err(|error| map_turso_error("decode Org attempt result", error))?,
        review_outcome: row
            .get(10)
            .map_err(|error| map_turso_error("decode Org attempt review", error))?,
        note_refs: serde_json::from_str::<Vec<OrgAttemptNoteReference>>(&note_refs_text).map_err(
            |error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    "decode stored Org attempt note refs",
                    error,
                )
            },
        )?,
        artifacts: serde_json::from_str::<Vec<OrgArtifactReference>>(&artifacts_text).map_err(
            |error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    "decode stored Org attempt artifacts",
                    error,
                )
            },
        )?,
        metadata: serde_json::from_str(&metadata_text).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Corrupt,
                "decode stored Org attempt metadata",
                error,
            )
        })?,
    })
}

async fn conditional_lease_write(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    lease_id: &str,
    context: &str,
) -> StorageResult<ConditionalUpdate<SanitizedOrgLease>> {
    if let Some(lease) = query_optional_lease(session, sql, params, context).await? {
        return Ok(ConditionalUpdate::Applied(lease.into()));
    }
    let mut rows = session
        .connection
        .query(
            "SELECT EXISTS(SELECT 1 FROM org_leases WHERE id=?1)",
            turso::params![lease_id],
        )
        .await
        .map_err(|error| map_turso_error("check Org lease conditional conflict", error))?;
    let exists: i64 = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read Org lease conditional conflict", error))?
        .ok_or_else(|| StorageError::new(StorageErrorKind::Operation, "missing exists row"))?
        .get(0)
        .map_err(|error| map_turso_error("decode Org lease conditional conflict", error))?;
    Ok(if exists == 0 {
        ConditionalUpdate::NotFound
    } else {
        ConditionalUpdate::Conflict
    })
}

async fn query_optional_lease(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
    context: &str,
) -> StorageResult<Option<OrgLease>> {
    let mut rows = session
        .connection
        .query(sql, params)
        .await
        .map_err(|error| map_turso_error(context, error))?;
    rows.next()
        .await
        .map_err(|error| map_turso_error(context, error))?
        .as_ref()
        .map(decode_lease)
        .transpose()
}

fn decode_lease(row: &turso::Row) -> StorageResult<OrgLease> {
    let end_reason = row
        .get::<Option<String>>(11)
        .map_err(|error| map_turso_error("decode Org lease end reason", error))?
        .as_deref()
        .map(decode_lease_end_reason)
        .transpose()?;
    Ok(OrgLease {
        id: row
            .get(0)
            .map_err(|error| map_turso_error("decode Org lease id", error))?,
        workspace_id: decode_workspace_id(
            row.get(1)
                .map_err(|error| map_turso_error("decode Org lease workspace", error))?,
        )?,
        work_item_id: decode_work_item_id(
            row.get(2)
                .map_err(|error| map_turso_error("decode Org lease work item", error))?,
        )?,
        attempt_id: row
            .get(3)
            .map_err(|error| map_turso_error("decode Org lease attempt", error))?,
        kind: decode_lease_kind(
            &row.get::<String>(4)
                .map_err(|error| map_turso_error("decode Org lease kind", error))?,
        )?,
        actor_id: row
            .get(5)
            .map_err(|error| map_turso_error("decode Org lease actor", error))?,
        fencing_token_hash: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org lease fencing hash", error))?,
        acquired_at: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org lease acquired time", error))?,
        last_heartbeat_at: row
            .get(8)
            .map_err(|error| map_turso_error("decode Org lease heartbeat", error))?,
        expires_at: row
            .get(9)
            .map_err(|error| map_turso_error("decode Org lease expiry", error))?,
        ended_at: row
            .get(10)
            .map_err(|error| map_turso_error("decode Org lease end time", error))?,
        end_reason,
        expiry_event_id: row
            .get(12)
            .map_err(|error| map_turso_error("decode Org lease expiry event", error))?,
    })
}

fn validate_event_limit(limit: usize) -> StorageResult<()> {
    if !(1..=200).contains(&limit) {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "Org event list limit must be between 1 and 200",
        ));
    }
    Ok(())
}

fn decode_event(row: &turso::Row) -> StorageResult<OrgEvent> {
    let metadata_text: String = row
        .get(10)
        .map_err(|error| map_turso_error("decode Org event metadata", error))?;
    let metadata = serde_json::from_str(&metadata_text).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            "decode stored Org event metadata",
            error,
        )
    })?;
    Ok(OrgEvent {
        id: row
            .get(0)
            .map_err(|error| map_turso_error("decode Org event id", error))?,
        workspace_id: decode_workspace_id(
            row.get(1)
                .map_err(|error| map_turso_error("decode Org event workspace id", error))?,
        )?,
        sequence: row
            .get(2)
            .map_err(|error| map_turso_error("decode Org event sequence", error))?,
        subject_kind: row
            .get(3)
            .map_err(|error| map_turso_error("decode Org event subject kind", error))?,
        subject_id: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org event subject id", error))?,
        actor_id: row
            .get(5)
            .map_err(|error| map_turso_error("decode Org event actor id", error))?,
        attempt_id: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org event attempt id", error))?,
        event_type: row
            .get::<String>(7)
            .map_err(|error| map_turso_error("decode Org event type", error))?
            .parse::<OrgEventType>()
            .map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    "decode stored Org event type",
                    error,
                )
            })?,
        occurred_at: row
            .get(8)
            .map_err(|error| map_turso_error("decode Org event occurred at", error))?,
        summary: row
            .get(9)
            .map_err(|error| map_turso_error("decode Org event summary", error))?,
        metadata,
        previous_state: row
            .get(11)
            .map_err(|error| map_turso_error("decode Org event previous state", error))?,
        resulting_state: row
            .get(12)
            .map_err(|error| map_turso_error("decode Org event resulting state", error))?,
    })
}

fn decode_operation(row: &turso::Row) -> StorageResult<StoredOrgOperation> {
    let result_text: String = row
        .get(3)
        .map_err(|error| map_turso_error("decode Org operation result", error))?;
    let result = serde_json::from_str(&result_text).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            "decode stored Org operation result",
            error,
        )
    })?;
    Ok(StoredOrgOperation {
        workspace_id: decode_workspace_id(
            row.get(0)
                .map_err(|error| map_turso_error("decode Org operation workspace id", error))?,
        )?,
        operation_id: row
            .get(1)
            .map_err(|error| map_turso_error("decode Org operation id", error))?,
        request_fingerprint: row
            .get(2)
            .map_err(|error| map_turso_error("decode Org operation fingerprint", error))?,
        result,
        created_at: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org operation created at", error))?,
    })
}

struct ProjectionPlan {
    order: Vec<usize>,
    staged_existing: Vec<(WorkItemId, i64)>,
}

struct ExistingProjection {
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    parent_id: Option<WorkItemId>,
    source_order: i64,
}

async fn validate_document_projection(
    session: &TursoSession,
    document_id: DocumentId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<ProjectionPlan> {
    let workspace_id = document_workspace(session, document_id).await?;
    for item in items {
        if item.document_id != document_id || item.workspace_id != workspace_id {
            return Err(projection_constraint(
                "projected item does not belong to the target Org document and workspace",
            ));
        }
    }
    validate_projection(session, workspace_id, items, false).await
}

async fn validate_workspace_rebuild(
    session: &TursoSession,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<ProjectionPlan> {
    let mut documents = HashSet::new();
    for item in items {
        if item.workspace_id != workspace_id {
            return Err(projection_constraint(
                "projected item does not belong to the rebuilt Org workspace",
            ));
        }
        if documents.insert(item.document_id) {
            let stored_workspace = document_workspace(session, item.document_id).await?;
            if stored_workspace != workspace_id {
                return Err(projection_constraint(
                    "projected item document does not belong to the rebuilt Org workspace",
                ));
            }
        }
    }
    validate_projection(session, workspace_id, items, true).await
}

async fn document_workspace(
    session: &TursoSession,
    document_id: DocumentId,
) -> StorageResult<WorkspaceId> {
    let mut rows = session
        .connection
        .query(
            "SELECT workspace_id FROM org_documents WHERE id = ?1",
            turso::params![document_id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("query Org projection document workspace", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read Org projection document workspace", error))?
    else {
        return Err(projection_constraint(
            "Org projection document does not exist",
        ));
    };
    decode_workspace_id(
        row.get(0)
            .map_err(|error| map_turso_error("decode Org projection document workspace", error))?,
    )
}

fn projection_batches<T>(values: &[T]) -> impl Iterator<Item = &[T]> {
    values.chunks(PROJECTION_BATCH_SIZE)
}

async fn load_existing_projections(
    session: &TursoSession,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<HashMap<WorkItemId, ExistingProjection>> {
    let mut existing = HashMap::new();
    load_existing_projection_query(
        session,
        "SELECT id, workspace_id, document_id, parent_id, source_order
         FROM org_work_items WHERE workspace_id = ?1",
        vec![turso::Value::from(workspace_id.to_string())],
        &mut existing,
    )
    .await?;
    for batch in projection_batches(items) {
        let mut sql = String::from(
            "SELECT id, workspace_id, document_id, parent_id, source_order
             FROM org_work_items WHERE id IN (",
        );
        let mut params = Vec::with_capacity(batch.len());
        for (index, item) in batch.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            sql.push_str(&(index + 1).to_string());
            params.push(turso::Value::from(item.id.to_string()));
        }
        sql.push(')');
        load_existing_projection_query(session, &sql, params, &mut existing).await?;
    }
    Ok(existing)
}

async fn load_existing_projection_query(
    session: &TursoSession,
    sql: &str,
    params: Vec<turso::Value>,
    existing: &mut HashMap<WorkItemId, ExistingProjection>,
) -> StorageResult<()> {
    let mut rows = session
        .connection
        .query(sql, turso::params_from_iter(params))
        .await
        .map_err(|error| map_turso_error("query existing Org projection identities", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read existing Org projection identities", error))?
    {
        let id =
            decode_work_item_id(row.get(0).map_err(|error| {
                map_turso_error("decode existing Org projection identity", error)
            })?)?;
        let stored_workspace = decode_workspace_id(row.get(1).map_err(|error| {
            map_turso_error("decode existing Org projection workspace", error)
        })?)?;
        let document =
            decode_document_id(row.get(2).map_err(|error| {
                map_turso_error("decode existing Org projection document", error)
            })?)?;
        let parent = row
            .get::<Option<String>>(3)
            .map_err(|error| map_turso_error("decode existing Org projection parent", error))?
            .map(decode_work_item_id)
            .transpose()?;
        let source_order = row
            .get(4)
            .map_err(|error| map_turso_error("decode existing Org source order", error))?;
        existing.insert(
            id,
            ExistingProjection {
                workspace_id: stored_workspace,
                document_id: document,
                parent_id: parent,
                source_order,
            },
        );
    }
    Ok(())
}

async fn validate_projection(
    session: &TursoSession,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
    rebuilding: bool,
) -> StorageResult<ProjectionPlan> {
    let existing = load_existing_projections(session, workspace_id, items).await?;

    let mut ids = HashSet::new();
    let mut source_orders = HashSet::new();
    for item in items {
        if !ids.insert(item.id) {
            return Err(projection_constraint(
                "duplicate Org projected work item id",
            ));
        }
        if item.source_order < 0 || !source_orders.insert((item.document_id, item.source_order)) {
            return Err(projection_constraint(
                "invalid or duplicate source order in Org document projection",
            ));
        }
        if let Some(stored) = existing.get(&item.id) {
            if stored.workspace_id != workspace_id
                || (!rebuilding && stored.document_id != item.document_id)
            {
                return Err(projection_constraint(
                    "Org projected work item id is owned by another workspace or document",
                ));
            }
        }
    }

    if !rebuilding {
        let omitted_orders = existing
            .iter()
            .filter(|(id, stored)| {
                stored.workspace_id == workspace_id
                    && items
                        .first()
                        .is_some_and(|item| stored.document_id == item.document_id)
                    && !ids.contains(id)
            })
            .map(|(_, stored)| stored.source_order)
            .collect::<HashSet<_>>();
        if items
            .iter()
            .any(|item| omitted_orders.contains(&item.source_order))
        {
            return Err(projection_constraint(
                "Org projection source order conflicts with an omitted work item",
            ));
        }
    }

    let mut parents = existing
        .iter()
        .filter(|(_, stored)| stored.workspace_id == workspace_id)
        .map(|(id, stored)| (*id, stored.parent_id))
        .collect::<HashMap<_, _>>();
    if rebuilding {
        parents.clear();
    }
    for item in items {
        parents.insert(item.id, item.parent_id);
    }

    for item in items {
        for target in item.parent_id.iter().chain(item.dependencies.iter()) {
            let supplied = ids.contains(target);
            let stored_here = existing
                .get(target)
                .is_some_and(|stored| stored.workspace_id == workspace_id);
            if !supplied && (rebuilding || !stored_here) {
                return Err(projection_constraint(
                    "Org projected parent or dependency target is missing or cross-workspace",
                ));
            }
        }
    }

    let depths = compute_candidate_depths(&parents, items)?;

    let mut dependencies = if rebuilding {
        HashMap::new()
    } else {
        load_workspace_dependencies(session, workspace_id).await?
    };
    for item in items {
        dependencies.insert(item.id, item.dependencies.clone());
    }
    let nodes = parents.keys().copied().collect::<HashSet<_>>();
    if dependency_graph_has_cycle(&nodes, &dependencies)? {
        return Err(projection_constraint(
            "cyclic Org projected dependency structure",
        ));
    }

    let mut order = (0..items.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let item = &items[*index];
        (depths[&item.id], item.source_order, item.id)
    });
    let staged_existing = if rebuilding || items.is_empty() {
        Vec::new()
    } else {
        let document_id = items[0].document_id;
        let maximum = existing
            .values()
            .filter(|stored| stored.document_id == document_id)
            .map(|stored| stored.source_order)
            .chain(items.iter().map(|item| item.source_order))
            .max()
            .unwrap_or(0);
        let mut supplied_existing = items
            .iter()
            .filter(|item| existing.contains_key(&item.id))
            .map(|item| item.id)
            .collect::<Vec<_>>();
        supplied_existing.sort();
        supplied_existing
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                maximum
                    .checked_add(index as i64 + 1)
                    .map(|source_order| (id, source_order))
                    .ok_or_else(|| projection_constraint("Org projection source order overflow"))
            })
            .collect::<StorageResult<Vec<_>>>()?
    };
    Ok(ProjectionPlan {
        order,
        staged_existing,
    })
}

fn compute_candidate_depths(
    parents: &HashMap<WorkItemId, Option<WorkItemId>>,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<HashMap<WorkItemId, usize>> {
    let mut depths = HashMap::<WorkItemId, usize>::new();
    for item in items {
        if depths.contains_key(&item.id) {
            continue;
        }
        let mut path = Vec::new();
        let mut visiting = HashSet::new();
        let mut current = item.id;
        let mut next_depth = loop {
            if let Some(depth) = depths.get(&current) {
                break depth
                    .checked_add(1)
                    .ok_or_else(|| projection_constraint("Org projected parent depth overflow"))?;
            }
            if !visiting.insert(current) {
                return Err(projection_constraint(
                    "cyclic Org projected parent structure",
                ));
            }
            path.push(current);
            match parents.get(&current) {
                Some(Some(parent)) => current = *parent,
                Some(None) => break 0,
                None => {
                    return Err(projection_constraint(
                        "Org projected parent target is missing or cross-workspace",
                    ));
                }
            }
        };
        while let Some(id) = path.pop() {
            depths.insert(id, next_depth);
            next_depth = next_depth
                .checked_add(1)
                .ok_or_else(|| projection_constraint("Org projected parent depth overflow"))?;
        }
    }
    Ok(depths)
}

async fn load_workspace_dependencies(
    session: &TursoSession,
    workspace_id: WorkspaceId,
) -> StorageResult<HashMap<WorkItemId, Vec<WorkItemId>>> {
    let mut rows = session
        .connection
        .query(
            "SELECT dependency.work_item_id, dependency.depends_on_id
             FROM org_dependencies dependency
             JOIN org_work_items item ON item.id = dependency.work_item_id
             WHERE item.workspace_id = ?1
             ORDER BY dependency.work_item_id, dependency.depends_on_id",
            turso::params![workspace_id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("query Org workspace dependencies", error))?;
    let mut dependencies = HashMap::<WorkItemId, Vec<WorkItemId>>::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read Org workspace dependencies", error))?
    {
        let source = decode_work_item_id(
            row.get(0)
                .map_err(|error| map_turso_error("decode Org dependency source", error))?,
        )?;
        let target = decode_work_item_id(
            row.get(1)
                .map_err(|error| map_turso_error("decode Org dependency target", error))?,
        )?;
        dependencies.entry(source).or_default().push(target);
    }
    Ok(dependencies)
}

fn dependency_graph_has_cycle(
    nodes: &HashSet<WorkItemId>,
    dependencies: &HashMap<WorkItemId, Vec<WorkItemId>>,
) -> StorageResult<bool> {
    let mut incoming = nodes
        .iter()
        .map(|id| (*id, 0usize))
        .collect::<HashMap<_, _>>();
    for targets in dependencies.values() {
        for target in targets {
            let Some(count) = incoming.get_mut(target) else {
                return Err(projection_constraint(
                    "Org projected dependency target is missing or cross-workspace",
                ));
            };
            *count += 1;
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;
    while let Some(source) = ready.pop_front() {
        visited += 1;
        for target in dependencies.get(&source).into_iter().flatten() {
            let count = incoming
                .get_mut(target)
                .expect("dependency targets were validated above");
            *count -= 1;
            if *count == 0 {
                ready.push_back(*target);
            }
        }
    }
    Ok(visited != nodes.len())
}

async fn stage_source_orders(
    session: &TursoSession,
    staged_existing: &[(WorkItemId, i64)],
) -> StorageResult<()> {
    for (id, source_order) in staged_existing {
        session
            .connection
            .execute(
                "UPDATE org_work_items SET source_order = ?2 WHERE id = ?1",
                turso::params![id.to_string(), *source_order],
            )
            .await
            .map_err(|error| map_turso_error("stage Org projection source order", error))?;
    }
    Ok(())
}

async fn upsert_projection(
    session: &TursoSession,
    items: &[OrgProjectedWorkItem],
    order: &[usize],
) -> StorageResult<()> {
    for index in order {
        let item = &items[*index];
        let item_type = serde_json::to_value(item.item_type)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| {
                StorageError::new(StorageErrorKind::Operation, "serialize Org work item type")
            })?;
        let priority = item.priority.map(|value| value.to_string());
        let (scheduled_raw, scheduled_local, scheduled_timezone, scheduled_utc) =
            timestamp_columns(item.scheduled.as_ref());
        let (deadline_raw, deadline_local, deadline_timezone, deadline_utc) =
            timestamp_columns(item.deadline.as_ref());
        session
            .connection
            .execute(
                "INSERT INTO org_work_items (
                     id, workspace_id, document_id, parent_id, source_order, item_type,
                     title, state, priority, scheduled_raw, scheduled_local,
                     scheduled_timezone, scheduled_utc, deadline_raw, deadline_local,
                     deadline_timezone, deadline_utc, assignee, requires_review, created_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
                 ) ON CONFLICT(id) DO UPDATE SET
                     workspace_id=excluded.workspace_id, document_id=excluded.document_id,
                     parent_id=excluded.parent_id, source_order=excluded.source_order,
                     item_type=excluded.item_type, title=excluded.title, state=excluded.state,
                     priority=excluded.priority, scheduled_raw=excluded.scheduled_raw,
                     scheduled_local=excluded.scheduled_local,
                     scheduled_timezone=excluded.scheduled_timezone,
                     scheduled_utc=excluded.scheduled_utc, deadline_raw=excluded.deadline_raw,
                     deadline_local=excluded.deadline_local,
                     deadline_timezone=excluded.deadline_timezone,
                     deadline_utc=excluded.deadline_utc, assignee=excluded.assignee,
                     requires_review=excluded.requires_review, created_at=excluded.created_at",
                turso::params![
                    item.id.to_string(),
                    item.workspace_id.to_string(),
                    item.document_id.to_string(),
                    item.parent_id.map(|value| value.to_string()),
                    item.source_order,
                    item_type,
                    item.title.clone(),
                    item.state.clone(),
                    priority,
                    scheduled_raw,
                    scheduled_local,
                    scheduled_timezone,
                    scheduled_utc,
                    deadline_raw,
                    deadline_local,
                    deadline_timezone,
                    deadline_utc,
                    item.assignee.clone(),
                    i64::from(item.requires_review),
                    item.created_at
                ],
            )
            .await
            .map_err(|error| map_turso_error("upsert Org projected work item", error))?;
    }

    for item in items {
        let id = item.id.to_string();
        for (sql, context) in [
            (
                "DELETE FROM org_work_item_tags WHERE work_item_id = ?1",
                "replace Org projected work item tags",
            ),
            (
                "DELETE FROM org_dependencies WHERE work_item_id = ?1",
                "replace Org projected work item dependencies",
            ),
            (
                "DELETE FROM org_note_links WHERE work_item_id = ?1",
                "replace Org projected work item note links",
            ),
        ] {
            session
                .connection
                .execute(sql, turso::params![id.clone()])
                .await
                .map_err(|error| map_turso_error(context, error))?;
        }
    }
    insert_projection_relations(session, items).await?;
    Ok(())
}

async fn insert_projection_relations(
    session: &TursoSession,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<()> {
    let tags = items
        .iter()
        .flat_map(|item| {
            item.tags
                .iter()
                .map(move |tag| vec![item.id.to_string(), tag.clone()])
        })
        .collect::<Vec<_>>();
    execute_turso_bulk_insert(
        session,
        "INSERT INTO org_work_item_tags (work_item_id, tag) VALUES ",
        &tags,
        "insert Org projected work item tags",
    )
    .await?;

    let dependencies = items
        .iter()
        .flat_map(|item| {
            item.dependencies
                .iter()
                .map(move |target| vec![item.id.to_string(), target.to_string()])
        })
        .collect::<Vec<_>>();
    execute_turso_bulk_insert(
        session,
        "INSERT INTO org_dependencies (work_item_id, depends_on_id) VALUES ",
        &dependencies,
        "insert Org projected dependencies",
    )
    .await?;

    let links = items
        .iter()
        .flat_map(|item| {
            item.note_links
                .iter()
                .enumerate()
                .map(move |(ordinal, link)| {
                    vec![
                        turso::Value::from(item.id.to_string()),
                        turso::Value::from(ordinal as i64),
                        turso::Value::from(link.purpose.clone()),
                        turso::Value::from(link.note_id.to_string()),
                        turso::Value::from(link.description.clone()),
                    ]
                })
        })
        .collect::<Vec<_>>();
    execute_turso_value_bulk_insert(
        session,
        "INSERT INTO org_note_links (
             work_item_id, ordinal, purpose, note_id, description
         ) VALUES ",
        &links,
        "insert Org projected note links",
    )
    .await
}

async fn execute_turso_bulk_insert(
    session: &TursoSession,
    prefix: &str,
    rows: &[Vec<String>],
    context: &str,
) -> StorageResult<()> {
    let values = rows
        .iter()
        .map(|row| row.iter().cloned().map(turso::Value::from).collect())
        .collect::<Vec<Vec<turso::Value>>>();
    execute_turso_value_bulk_insert(session, prefix, &values, context).await
}

async fn execute_turso_value_bulk_insert(
    session: &TursoSession,
    prefix: &str,
    rows: &[Vec<turso::Value>],
    context: &str,
) -> StorageResult<()> {
    for chunk in projection_batches(rows) {
        let mut sql = String::from(prefix);
        let mut params = Vec::new();
        for (row_index, row) in chunk.iter().enumerate() {
            if row_index > 0 {
                sql.push_str(", ");
            }
            sql.push('(');
            for (column_index, value) in row.iter().enumerate() {
                if column_index > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
                sql.push_str(&(params.len() + 1).to_string());
                params.push(value.clone());
            }
            sql.push(')');
        }
        session
            .connection
            .execute(&sql, turso::params_from_iter(params))
            .await
            .map_err(|error| map_turso_error(context, error))?;
    }
    Ok(())
}

fn timestamp_columns(
    timestamp: Option<&StoredOrgTimestamp>,
) -> (Option<String>, Option<String>, Option<String>, Option<i64>) {
    timestamp.map_or((None, None, None, None), |value| {
        (
            Some(value.raw.clone()),
            Some(value.local.clone()),
            Some(value.timezone.clone()),
            Some(value.utc_timestamp),
        )
    })
}

fn decode_projected_item(row: &turso::Row) -> StorageResult<OrgProjectedWorkItem> {
    let id = decode_work_item_id(
        row.get(0)
            .map_err(|error| map_turso_error("decode Org projected work item id", error))?,
    )?;
    let priority = row
        .get::<Option<String>>(8)
        .map_err(|error| map_turso_error("decode Org projected priority", error))?
        .map(|value| decode_priority(&value))
        .transpose()?;
    Ok(OrgProjectedWorkItem {
        id,
        workspace_id: decode_workspace_id(
            row.get(1)
                .map_err(|error| map_turso_error("decode Org projected workspace", error))?,
        )?,
        document_id: decode_document_id(
            row.get(2)
                .map_err(|error| map_turso_error("decode Org projected document", error))?,
        )?,
        parent_id: row
            .get::<Option<String>>(3)
            .map_err(|error| map_turso_error("decode Org projected parent", error))?
            .map(decode_work_item_id)
            .transpose()?,
        source_order: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org projected source order", error))?,
        item_type: decode_work_item_type(
            row.get(5)
                .map_err(|error| map_turso_error("decode Org projected item type", error))?,
        )?,
        title: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org projected title", error))?,
        state: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org projected state", error))?,
        priority,
        scheduled: decode_timestamp(row, 9, "scheduled")?,
        deadline: decode_timestamp(row, 13, "deadline")?,
        assignee: row
            .get(17)
            .map_err(|error| map_turso_error("decode Org projected assignee", error))?,
        requires_review: decode_bool(
            row.get(18)
                .map_err(|error| map_turso_error("decode Org projected review flag", error))?,
        )?,
        created_at: row
            .get(19)
            .map_err(|error| map_turso_error("decode Org projected created at", error))?,
        tags: Vec::new(),
        dependencies: Vec::new(),
        note_links: Vec::new(),
    })
}

async fn load_projection_relations(
    session: &TursoSession,
    document_id: DocumentId,
    items: &mut [OrgProjectedWorkItem],
) -> StorageResult<()> {
    let index = items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, index))
        .collect::<HashMap<_, _>>();
    let mut tags = session
        .connection
        .query(
            "SELECT tag.work_item_id, tag.tag
             FROM org_work_item_tags tag
             JOIN org_work_items item ON item.id = tag.work_item_id
             WHERE item.document_id = ?1
             ORDER BY tag.work_item_id, tag.tag",
            turso::params![document_id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("list Org projected tags", error))?;
    while let Some(row) = tags
        .next()
        .await
        .map_err(|error| map_turso_error("read Org projected tags", error))?
    {
        let id = decode_work_item_id(
            row.get(0)
                .map_err(|error| map_turso_error("decode Org projected tag owner", error))?,
        )?;
        let tag = row
            .get(1)
            .map_err(|error| map_turso_error("decode Org projected tag", error))?;
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.tags.push(tag);
        }
    }
    drop(tags);

    let mut dependencies = session
        .connection
        .query(
            "SELECT dependency.work_item_id, dependency.depends_on_id
             FROM org_dependencies dependency
             JOIN org_work_items item ON item.id = dependency.work_item_id
             WHERE item.document_id = ?1
             ORDER BY dependency.work_item_id, dependency.depends_on_id",
            turso::params![document_id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("list Org projected dependencies", error))?;
    while let Some(row) = dependencies
        .next()
        .await
        .map_err(|error| map_turso_error("read Org projected dependencies", error))?
    {
        let id =
            decode_work_item_id(row.get(0).map_err(|error| {
                map_turso_error("decode Org projected dependency owner", error)
            })?)?;
        let target =
            decode_work_item_id(row.get(1).map_err(|error| {
                map_turso_error("decode Org projected dependency target", error)
            })?)?;
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.dependencies.push(target);
        }
    }
    drop(dependencies);

    let mut links = session
        .connection
        .query(
            "SELECT link.work_item_id, link.purpose, link.note_id, link.description
             FROM org_note_links link
             JOIN org_work_items item ON item.id = link.work_item_id
             WHERE item.document_id = ?1
             ORDER BY link.work_item_id, link.ordinal",
            turso::params![document_id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("list Org projected note links", error))?;
    while let Some(row) = links
        .next()
        .await
        .map_err(|error| map_turso_error("read Org projected note links", error))?
    {
        let id =
            decode_work_item_id(row.get(0).map_err(|error| {
                map_turso_error("decode Org projected note link owner", error)
            })?)?;
        let note_id_text: String = row
            .get(2)
            .map_err(|error| map_turso_error("decode Org projected note id", error))?;
        let link = NoteLink {
            purpose: row
                .get(1)
                .map_err(|error| map_turso_error("decode Org projected note purpose", error))?,
            note_id: note_id_text.parse().map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    format!("decode stored Org note id {note_id_text:?}"),
                    error,
                )
            })?,
            description: row
                .get(3)
                .map_err(|error| map_turso_error("decode Org projected note description", error))?,
        };
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.note_links.push(link);
        }
    }
    Ok(())
}

fn decode_timestamp(
    row: &turso::Row,
    start: usize,
    label: &str,
) -> StorageResult<Option<StoredOrgTimestamp>> {
    let raw: Option<String> = row
        .get(start)
        .map_err(|error| map_turso_error(&format!("decode Org projected {label} raw"), error))?;
    let local: Option<String> = row
        .get(start + 1)
        .map_err(|error| map_turso_error(&format!("decode Org projected {label} local"), error))?;
    let timezone: Option<String> = row.get(start + 2).map_err(|error| {
        map_turso_error(&format!("decode Org projected {label} timezone"), error)
    })?;
    let utc_timestamp: Option<i64> = row
        .get(start + 3)
        .map_err(|error| map_turso_error(&format!("decode Org projected {label} utc"), error))?;
    match (raw, local, timezone, utc_timestamp) {
        (None, None, None, None) => Ok(None),
        (Some(raw), Some(local), Some(timezone), Some(utc_timestamp)) => {
            Ok(Some(StoredOrgTimestamp {
                raw,
                local,
                timezone,
                utc_timestamp,
            }))
        }
        _ => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("stored Org projected {label} timestamp is incomplete"),
        )),
    }
}

fn decode_priority(value: &str) -> StorageResult<char> {
    let mut characters = value.chars();
    let Some(priority) = characters.next() else {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "stored Org projected priority is empty",
        ));
    };
    if characters.next().is_some() {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "stored Org projected priority has more than one character",
        ));
    }
    Ok(priority)
}

fn decode_bool(value: i64) -> StorageResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StorageError::new(
            StorageErrorKind::Corrupt,
            "stored Org projected boolean is not zero or one",
        )),
    }
}

fn decode_work_item_type(value: String) -> StorageResult<WorkItemType> {
    WorkItemType::from_str(&value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            format!("decode stored Org work item type {value:?}"),
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })
}

fn projection_constraint(message: &str) -> StorageError {
    StorageError::new(StorageErrorKind::Constraint, message)
}

fn serialize_policy(policy: &WorkspacePolicy) -> StorageResult<String> {
    serde_json::to_string(policy).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "serialize Org workspace policy",
            error,
        )
    })
}

async fn query_optional_workspace(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
) -> StorageResult<Option<OrgWorkspace>> {
    let mut rows = session
        .connection
        .query(sql, params)
        .await
        .map_err(|error| map_turso_error("query Org workspace", error))?;
    rows.next()
        .await
        .map_err(|error| map_turso_error("read Org workspace", error))?
        .as_ref()
        .map(decode_workspace)
        .transpose()
}

async fn query_optional_document(
    session: &TursoSession,
    sql: &str,
    params: impl turso::IntoParams,
) -> StorageResult<Option<OrgDocument>> {
    let mut rows = session
        .connection
        .query(sql, params)
        .await
        .map_err(|error| map_turso_error("query Org document", error))?;
    rows.next()
        .await
        .map_err(|error| map_turso_error("read Org document", error))?
        .as_ref()
        .map(decode_document)
        .transpose()
}

async fn current_workspace_revision(
    session: &TursoSession,
    id: WorkspaceId,
) -> StorageResult<Option<i64>> {
    current_revision(
        session,
        "SELECT revision FROM org_workspaces WHERE id = ?1",
        id.to_string(),
        "Org workspace",
    )
    .await
}

async fn current_document_revision(
    session: &TursoSession,
    id: DocumentId,
) -> StorageResult<Option<i64>> {
    current_revision(
        session,
        "SELECT revision FROM org_documents WHERE id = ?1",
        id.to_string(),
        "Org document",
    )
    .await
}

async fn current_revision(
    session: &TursoSession,
    sql: &str,
    id: String,
    subject: &str,
) -> StorageResult<Option<i64>> {
    let mut rows = session
        .connection
        .query(sql, turso::params![id])
        .await
        .map_err(|error| map_turso_error(&format!("query current {subject} revision"), error))?;
    match rows
        .next()
        .await
        .map_err(|error| map_turso_error(&format!("read current {subject} revision"), error))?
    {
        Some(row) => row
            .get::<i64>(0)
            .map(Some)
            .map_err(|error| map_turso_error(&format!("decode current {subject} revision"), error)),
        None => Ok(None),
    }
}

fn decode_workspace(row: &turso::Row) -> StorageResult<OrgWorkspace> {
    let id = decode_workspace_id(
        row.get::<String>(0)
            .map_err(|error| map_turso_error("decode Org workspace id column", error))?,
    )?;
    let policy_text = row
        .get::<String>(6)
        .map_err(|error| map_turso_error("decode Org workspace policy column", error))?;
    let policy = serde_json::from_str(&policy_text).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            "decode stored Org workspace policy",
            error,
        )
    })?;
    Ok(OrgWorkspace {
        id,
        slug: row
            .get(1)
            .map_err(|error| map_turso_error("decode Org workspace slug", error))?,
        display_name: row
            .get(2)
            .map_err(|error| map_turso_error("decode Org workspace display name", error))?,
        description: row
            .get(3)
            .map_err(|error| map_turso_error("decode Org workspace description", error))?,
        timezone: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org workspace timezone", error))?,
        policy_schema_version: row.get(5).map_err(|error| {
            map_turso_error("decode Org workspace policy schema version", error)
        })?,
        policy,
        revision: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org workspace revision", error))?,
        created_at: row
            .get(8)
            .map_err(|error| map_turso_error("decode Org workspace created at", error))?,
        updated_at: row
            .get(9)
            .map_err(|error| map_turso_error("decode Org workspace updated at", error))?,
        archived_at: row
            .get(10)
            .map_err(|error| map_turso_error("decode Org workspace archived at", error))?,
    })
}

fn decode_document(row: &turso::Row) -> StorageResult<OrgDocument> {
    Ok(OrgDocument {
        id: decode_document_id(
            row.get::<String>(0)
                .map_err(|error| map_turso_error("decode Org document id column", error))?,
        )?,
        workspace_id: decode_workspace_id(
            row.get::<String>(1).map_err(|error| {
                map_turso_error("decode Org document workspace id column", error)
            })?,
        )?,
        path: row
            .get(2)
            .map_err(|error| map_turso_error("decode Org document path", error))?,
        source: row
            .get(3)
            .map_err(|error| map_turso_error("decode Org document source", error))?,
        content_hash: row
            .get(4)
            .map_err(|error| map_turso_error("decode Org document content hash", error))?,
        revision: row
            .get(5)
            .map_err(|error| map_turso_error("decode Org document revision", error))?,
        created_at: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org document created at", error))?,
        updated_at: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org document updated at", error))?,
    })
}

fn decode_workspace_id(value: String) -> StorageResult<WorkspaceId> {
    WorkspaceId::from_str(&value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            format!("decode stored Org workspace id {value:?}"),
            error,
        )
    })
}

fn decode_document_id(value: String) -> StorageResult<DocumentId> {
    DocumentId::from_str(&value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            format!("decode stored Org document id {value:?}"),
            error,
        )
    })
}

fn decode_work_item_id(value: String) -> StorageResult<WorkItemId> {
    WorkItemId::from_str(&value).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Corrupt,
            format!("decode stored Org work item id {value:?}"),
            error,
        )
    })
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn projection_batches_bound_every_driver_statement() {
        let values = (0..257).collect::<Vec<_>>();
        let sizes = projection_batches(&values)
            .map(<[_]>::len)
            .collect::<Vec<_>>();

        assert_eq!(sizes, vec![64, 64, 64, 64, 1]);
        assert!(sizes.iter().all(|size| *size <= PROJECTION_BATCH_SIZE));
    }
}
