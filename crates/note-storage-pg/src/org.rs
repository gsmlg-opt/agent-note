use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    order_org_event_segments_by_lineage, AppliedOrgDocumentOwnershipMove, CompareAndSwap,
    ConditionalUpdate, NewOrgAttempt, NewOrgDocument, NewOrgEvent, NewOrgWorkspace,
    OrgArtifactReference, OrgAttempt, OrgAttemptNoteReference, OrgAttemptStatus, OrgAttemptUpdate,
    OrgDocument, OrgDocumentOwnershipMove, OrgDocumentOwnershipMoveResult, OrgDocumentUpdate,
    OrgEvent, OrgEventType, OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate,
    StorageError, StorageErrorKind, StorageResult, StoredOrgOperation, StoredOrgTimestamp,
};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, QueryBuilder};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::str::FromStr as _;

// The widest projection statement binds 20 values per row, so 64 rows remain
// comfortably below both embedded and PostgreSQL driver parameter limits.
const PROJECTION_BATCH_SIZE: usize = 64;

#[async_trait::async_trait]
impl OrgRepository for PgSession {
    async fn insert_org_workspace(&self, value: NewOrgWorkspace<'_>) -> StorageResult<()> {
        let policy = serialize_policy(value.policy)?;
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO org_workspaces (
                 id, slug, display_name, description, timezone,
                 policy_schema_version, policy, revision,
                 created_at, updated_at, archived_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8, NULL)",
        )
        .bind(value.id.to_string())
        .bind(value.slug)
        .bind(value.display_name)
        .bind(value.description)
        .bind(value.timezone)
        .bind(value.policy_schema_version)
        .bind(policy)
        .bind(value.now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert Org workspace", error))?;
        Ok(())
    }

    async fn get_org_workspace(&self, id: WorkspaceId) -> StorageResult<Option<OrgWorkspace>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, slug, display_name, description, timezone,
                    policy_schema_version, policy, revision, created_at, updated_at, archived_at
             FROM org_workspaces WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org workspace", error))?
        .map(WorkspaceRow::into_workspace)
        .transpose()
    }

    async fn get_org_workspace_by_slug(&self, slug: &str) -> StorageResult<Option<OrgWorkspace>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, slug, display_name, description, timezone,
                    policy_schema_version, policy, revision, created_at, updated_at, archived_at
             FROM org_workspaces WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org workspace by slug", error))?
        .map(WorkspaceRow::into_workspace)
        .transpose()
    }

    async fn list_org_workspaces(
        &self,
        include_archived: bool,
    ) -> StorageResult<Vec<OrgWorkspace>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, slug, display_name, description, timezone,
                    policy_schema_version, policy, revision, created_at, updated_at, archived_at
             FROM org_workspaces
             WHERE ($1 OR archived_at IS NULL)
             ORDER BY slug, id",
        )
        .bind(include_archived)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org workspaces", error))?
        .into_iter()
        .map(WorkspaceRow::into_workspace)
        .collect()
    }

    async fn compare_and_swap_org_workspace(
        &self,
        update: OrgWorkspaceUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgWorkspace>> {
        let policy = serialize_policy(update.policy)?;
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, WorkspaceRow>(
            "UPDATE org_workspaces
             SET slug=$2, display_name=$3, description=$4, timezone=$5,
                 policy_schema_version=$6, policy=$7, archived_at=$8, updated_at=$9,
                 revision=revision+1
             WHERE id=$1 AND revision=$10
             RETURNING id, slug, display_name, description, timezone,
                       policy_schema_version, policy, revision, created_at, updated_at, archived_at",
        )
            .bind(update.id.to_string())
            .bind(update.slug)
            .bind(update.display_name)
            .bind(update.description)
            .bind(update.timezone)
            .bind(update.policy_schema_version)
            .bind(policy)
            .bind(update.archived_at)
            .bind(update.updated_at)
            .bind(update.expected_revision)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("compare-and-swap Org workspace", error))?;
        if let Some(row) = row {
            return row.into_workspace().map(CompareAndSwap::Applied);
        }

        let current_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM org_workspaces WHERE id = $1")
                .bind(update.id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query current Org workspace revision", error))?;
        Ok(match current_revision {
            Some(current_revision) => CompareAndSwap::Conflict { current_revision },
            None => CompareAndSwap::NotFound,
        })
    }

    async fn insert_org_document(&self, value: NewOrgDocument<'_>) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO org_documents (
                 id, workspace_id, path, source, content_hash, revision, created_at, updated_at
             ) VALUES ($1, $2, $3, $4, $5, 1, $6, $6)",
        )
        .bind(value.id.to_string())
        .bind(value.workspace_id.to_string())
        .bind(value.path)
        .bind(value.source)
        .bind(value.content_hash)
        .bind(value.now)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert Org document", error))?;
        Ok(())
    }

    async fn get_org_document(&self, id: DocumentId) -> StorageResult<Option<OrgDocument>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, DocumentRow>(
            "SELECT id, workspace_id, path, source, content_hash, revision, created_at, updated_at
             FROM org_documents WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org document", error))?
        .map(DocumentRow::into_document)
        .transpose()
    }

    async fn list_org_documents(
        &self,
        workspace_id: WorkspaceId,
    ) -> StorageResult<Vec<OrgDocument>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, DocumentRow>(
            "SELECT id, workspace_id, path, source, content_hash, revision, created_at, updated_at
             FROM org_documents
             WHERE workspace_id = $1
             ORDER BY path, id",
        )
        .bind(workspace_id.to_string())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org documents", error))?
        .into_iter()
        .map(DocumentRow::into_document)
        .collect()
    }

    async fn compare_and_swap_org_document(
        &self,
        update: OrgDocumentUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgDocument>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, DocumentRow>(
            "UPDATE org_documents
             SET path=$2, source=$3, content_hash=$4, updated_at=$5, revision=revision+1
             WHERE id=$1 AND revision=$6
             RETURNING id, workspace_id, path, source, content_hash, revision, created_at, updated_at",
        )
            .bind(update.id.to_string())
            .bind(update.path)
            .bind(update.source)
            .bind(update.content_hash)
            .bind(update.updated_at)
            .bind(update.expected_revision)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("compare-and-swap Org document", error))?;
        if let Some(row) = row {
            return row.into_document().map(CompareAndSwap::Applied);
        }

        let current_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM org_documents WHERE id = $1")
                .bind(update.id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("query current Org document revision", error))?;
        Ok(match current_revision {
            Some(current_revision) => CompareAndSwap::Conflict { current_revision },
            None => CompareAndSwap::NotFound,
        })
    }

    async fn compare_and_swap_org_document_ownership(
        &self,
        update: OrgDocumentOwnershipMove,
    ) -> StorageResult<OrgDocumentOwnershipMoveResult> {
        if update.source_workspace_id == update.target_workspace_id {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "Org document ownership move requires distinct workspaces",
            ));
        }
        let mut connection = self.connection().await?;
        let applied = sqlx::query_as::<_, OwnershipMoveRow>(
            "WITH locked AS MATERIALIZED (
                 SELECT document.id
                 FROM org_documents document
                 JOIN org_workspaces source_workspace ON source_workspace.id = $2
                 JOIN org_workspaces target_workspace ON target_workspace.id = $3
                 WHERE document.id = $1
                   AND document.workspace_id = $2
                   AND document.revision = $4
                   AND source_workspace.revision = $5
                   AND target_workspace.revision = $6
                 FOR UPDATE OF document, source_workspace, target_workspace
             ), moved_document AS (
                 UPDATE org_documents document
                 SET workspace_id=$3, revision=document.revision+1, updated_at=$7
                 WHERE document.id=$1 AND EXISTS (SELECT 1 FROM locked)
                 RETURNING document.id, document.workspace_id, document.path, document.source,
                           document.content_hash, document.revision, document.created_at,
                           document.updated_at
             ), advanced_source AS (
                 UPDATE org_workspaces workspace
                 SET revision=workspace.revision+1, updated_at=$7
                 WHERE workspace.id=$2 AND EXISTS (SELECT 1 FROM moved_document)
                 RETURNING workspace.revision
             ), advanced_target AS (
                 UPDATE org_workspaces workspace
                 SET revision=workspace.revision+1, updated_at=$7
                 WHERE workspace.id=$3 AND EXISTS (SELECT 1 FROM advanced_source)
                 RETURNING workspace.revision
             )
             SELECT moved_document.id, moved_document.workspace_id, moved_document.path,
                    moved_document.source, moved_document.content_hash, moved_document.revision,
                    moved_document.created_at, moved_document.updated_at,
                    advanced_source.revision AS source_workspace_revision,
                    advanced_target.revision AS target_workspace_revision
             FROM moved_document CROSS JOIN advanced_source CROSS JOIN advanced_target",
        )
        .bind(update.document_id.to_string())
        .bind(update.source_workspace_id.to_string())
        .bind(update.target_workspace_id.to_string())
        .bind(update.expected_document_revision)
        .bind(update.expected_source_workspace_revision)
        .bind(update.expected_target_workspace_revision)
        .bind(update.updated_at)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("compare-and-swap Org document ownership", error))?;
        if let Some(applied) = applied {
            return applied.into_result();
        }

        let document = sqlx::query_as::<_, DocumentRow>(
            "SELECT id, workspace_id, path, source, content_hash, revision, created_at, updated_at
             FROM org_documents WHERE id=$1",
        )
        .bind(update.document_id.to_string())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query document after ownership conflict", error))?;
        let source_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM org_workspaces WHERE id=$1")
                .bind(update.source_workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| {
                    map_sqlx_error("query source workspace after ownership conflict", error)
                })?;
        let target_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM org_workspaces WHERE id=$1")
                .bind(update.target_workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| {
                    map_sqlx_error("query target workspace after ownership conflict", error)
                })?;
        let (Some(document), Some(source_revision), Some(target_revision)) =
            (document, source_revision, target_revision)
        else {
            return Ok(OrgDocumentOwnershipMoveResult::NotFound);
        };
        Ok(OrgDocumentOwnershipMoveResult::Conflict {
            current_document_workspace_id: decode_workspace_id(document.workspace_id)?,
            current_document_revision: document.revision,
            current_source_workspace_revision: source_revision,
            current_target_workspace_revision: target_revision,
        })
    }

    async fn replace_org_document_projection(
        &self,
        document_id: DocumentId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        let plan = validate_document_projection(&mut connection, document_id, items).await?;
        stage_source_orders(&mut connection, &plan.staged_existing).await?;
        upsert_projection(&mut connection, items, &plan.order).await
    }

    async fn list_org_document_projection(
        &self,
        document_id: DocumentId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, WorkItemRow>(
            "SELECT id, workspace_id, document_id, parent_id, source_order, item_type,
                    title, state, priority, scheduled_raw, scheduled_local,
                    scheduled_timezone, scheduled_utc, deadline_raw, deadline_local,
                    deadline_timezone, deadline_utc, assignee, requires_review, created_at
             FROM org_work_items
             WHERE document_id = $1
             ORDER BY source_order, id",
        )
        .bind(document_id.to_string())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org document projection", error))?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(decode_projected_item(row)?);
        }
        load_projection_relations(&mut connection, document_id, &mut items).await?;
        Ok(items)
    }

    async fn get_org_work_item(
        &self,
        id: WorkItemId,
    ) -> StorageResult<Option<OrgProjectedWorkItem>> {
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, WorkItemRow>(
            "SELECT id, workspace_id, document_id, parent_id, source_order, item_type,
                    title, state, priority, scheduled_raw, scheduled_local,
                    scheduled_timezone, scheduled_utc, deadline_raw, deadline_local,
                    deadline_timezone, deadline_utc, assignee, requires_review, created_at
             FROM org_work_items WHERE id=$1",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org work item", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut items = vec![decode_projected_item(row)?];
        load_relations_for_projected_items(&mut connection, &mut items).await?;
        Ok(items.pop())
    }

    async fn list_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, WorkItemRow>(
            "SELECT item.id, item.workspace_id, item.document_id, item.parent_id,
                    item.source_order, item.item_type, item.title, item.state, item.priority,
                    item.scheduled_raw, item.scheduled_local, item.scheduled_timezone,
                    item.scheduled_utc, item.deadline_raw, item.deadline_local,
                    item.deadline_timezone, item.deadline_utc, item.assignee,
                    item.requires_review, item.created_at
             FROM org_work_items item
             JOIN org_documents document ON document.id=item.document_id
             WHERE item.workspace_id=$1
             ORDER BY document.path, document.id, item.source_order, item.id",
        )
        .bind(workspace_id.to_string())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org workspace projection", error))?;
        let mut items = rows
            .into_iter()
            .map(decode_projected_item)
            .collect::<StorageResult<Vec<_>>>()?;
        load_relations_for_projected_items(&mut connection, &mut items).await?;
        Ok(items)
    }

    async fn list_org_work_item_children(
        &self,
        parent_id: WorkItemId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, WorkItemRow>(
            "SELECT item.id, item.workspace_id, item.document_id, item.parent_id,
                    item.source_order, item.item_type, item.title, item.state, item.priority,
                    item.scheduled_raw, item.scheduled_local, item.scheduled_timezone,
                    item.scheduled_utc, item.deadline_raw, item.deadline_local,
                    item.deadline_timezone, item.deadline_utc, item.assignee,
                    item.requires_review, item.created_at
             FROM org_work_items item
             JOIN org_documents document ON document.id=item.document_id
             WHERE item.parent_id=$1
             ORDER BY document.path, document.id, item.source_order, item.id",
        )
        .bind(parent_id.to_string())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org work item children", error))?;
        let mut items = rows
            .into_iter()
            .map(decode_projected_item)
            .collect::<StorageResult<Vec<_>>>()?;
        load_relations_for_projected_items(&mut connection, &mut items).await?;
        Ok(items)
    }

    async fn list_org_work_items_linking_note(
        &self,
        note_id: &str,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
        let mut connection = self.connection().await?;
        let rows = sqlx::query_as::<_, WorkItemRow>(
            "SELECT item.id, item.workspace_id, item.document_id, item.parent_id,
                    item.source_order, item.item_type, item.title, item.state, item.priority,
                    item.scheduled_raw, item.scheduled_local, item.scheduled_timezone,
                    item.scheduled_utc, item.deadline_raw, item.deadline_local,
                    item.deadline_timezone, item.deadline_utc, item.assignee,
                    item.requires_review, item.created_at
             FROM org_work_items item
             JOIN org_documents document ON document.id=item.document_id
             WHERE EXISTS (
                 SELECT 1 FROM org_note_links link
                 WHERE link.work_item_id=item.id AND link.note_id=$1
             )
             ORDER BY item.workspace_id, document.path, item.source_order, item.id",
        )
        .bind(note_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org reverse note links", error))?;
        let mut items = rows
            .into_iter()
            .map(decode_projected_item)
            .collect::<StorageResult<Vec<_>>>()?;
        load_relations_for_projected_items(&mut connection, &mut items).await?;
        Ok(items)
    }

    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        let plan = validate_workspace_rebuild(&mut connection, workspace_id, items).await?;
        let workspace = workspace_id.to_string();
        for (sql, context) in [
            (
                "DELETE FROM org_dependencies
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = $1)
                    OR depends_on_id IN (SELECT id FROM org_work_items WHERE workspace_id = $1)",
                "delete Org workspace dependencies for rebuild",
            ),
            (
                "DELETE FROM org_work_item_tags
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = $1)",
                "delete Org workspace tags for rebuild",
            ),
            (
                "DELETE FROM org_note_links
                 WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = $1)",
                "delete Org workspace note links for rebuild",
            ),
            (
                "UPDATE org_work_items SET parent_id = NULL WHERE workspace_id = $1",
                "clear Org workspace parents for rebuild",
            ),
            (
                "DELETE FROM org_work_items WHERE workspace_id = $1",
                "delete Org workspace work items for rebuild",
            ),
        ] {
            sqlx::query(sql)
                .bind(&workspace)
                .execute(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error(context, error))?;
        }
        upsert_projection(&mut connection, items, &plan.order).await
    }

    async fn append_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let mut connection = self.connection().await?;
        append_event(&mut connection, event, false).await
    }

    async fn append_internal_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent> {
        let mut connection = self.connection().await?;
        append_event(&mut connection, event, true).await
    }

    async fn list_org_events(
        &self,
        workspace_id: WorkspaceId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        validate_event_limit(limit)?;
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, EventRow>(
            "SELECT id, workspace_id, sequence, subject_kind, subject_id,
                    actor_id, attempt_id, event_type, occurred_at, summary, metadata,
                    previous_state, resulting_state
             FROM org_events
             WHERE workspace_id=$1 AND sequence>$2
             ORDER BY sequence
             LIMIT $3",
        )
        .bind(workspace_id.to_string())
        .bind(after_sequence.unwrap_or(0))
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org events", error))?
        .into_iter()
        .map(EventRow::into_event)
        .collect()
    }

    async fn list_org_subject_events(
        &self,
        workspace_id: WorkspaceId,
        subject_kind: &str,
        subject_id: &str,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
        validate_event_limit(limit)?;
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, EventRow>(
            "SELECT id, workspace_id, sequence, subject_kind, subject_id,
                    actor_id, attempt_id, event_type, occurred_at, summary, metadata,
                    previous_state, resulting_state
             FROM org_events
             WHERE workspace_id=$1 AND subject_kind=$2 AND subject_id=$3 AND sequence>$4
             ORDER BY sequence
             LIMIT $5",
        )
        .bind(workspace_id.to_string())
        .bind(subject_kind)
        .bind(subject_id)
        .bind(after_sequence.unwrap_or(0))
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org subject events", error))?
        .into_iter()
        .map(EventRow::into_event)
        .collect()
    }

    async fn list_org_global_subject_events(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> StorageResult<Vec<OrgEvent>> {
        let mut connection = self.connection().await?;
        let events = sqlx::query_as::<_, EventRow>(
            "SELECT id, workspace_id, sequence, subject_kind, subject_id,
                    actor_id, attempt_id, event_type, occurred_at, summary, metadata,
                    previous_state, resulting_state
             FROM org_events
             WHERE subject_kind=$1 AND subject_id=$2
             ORDER BY workspace_id, sequence",
        )
        .bind(subject_kind)
        .bind(subject_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list global Org subject events", error))?
        .into_iter()
        .map(EventRow::into_event)
        .collect::<StorageResult<Vec<_>>>()?;
        order_org_event_segments_by_lineage(events)
    }

    async fn insert_org_attempt(&self, attempt: NewOrgAttempt<'_>) -> StorageResult<OrgAttempt> {
        validate_new_attempt(&attempt)?;
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttemptRow>(
            "INSERT INTO org_attempts (
                 id, workspace_id, work_item_id, attempt_number, actor_id, status,
                 started_at, ended_at, error, result_summary, review_outcome,
                 note_refs, artifacts, metadata
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, NULL, NULL, NULL, $8, $9, $10)
             RETURNING id, workspace_id, work_item_id, attempt_number, actor_id, status,
                       started_at, ended_at, error, result_summary, review_outcome,
                       note_refs, artifacts, metadata",
        )
        .bind(attempt.id)
        .bind(attempt.workspace_id.to_string())
        .bind(attempt.work_item_id.to_string())
        .bind(attempt.attempt_number)
        .bind(attempt.actor_id)
        .bind(attempt_status_name(attempt.status))
        .bind(attempt.started_at)
        .bind(serde_json::to_value(attempt.note_refs).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org attempt note refs",
                error,
            )
        })?)
        .bind(serde_json::to_value(attempt.artifacts).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org attempt artifacts",
                error,
            )
        })?)
        .bind(attempt.metadata)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert Org attempt", error))?
        .into_attempt()
    }

    async fn get_org_attempt(&self, id: &str) -> StorageResult<Option<OrgAttempt>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttemptRow>(
            "SELECT id, workspace_id, work_item_id, attempt_number, actor_id, status,
                    started_at, ended_at, error, result_summary, review_outcome,
                    note_refs, artifacts, metadata
             FROM org_attempts WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org attempt", error))?
        .map(AttemptRow::into_attempt)
        .transpose()
    }

    async fn list_org_attempts(&self, work_item_id: WorkItemId) -> StorageResult<Vec<OrgAttempt>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, AttemptRow>(
            "SELECT id, workspace_id, work_item_id, attempt_number, actor_id, status,
                    started_at, ended_at, error, result_summary, review_outcome,
                    note_refs, artifacts, metadata
             FROM org_attempts
             WHERE work_item_id=$1
             ORDER BY attempt_number, id",
        )
        .bind(work_item_id.to_string())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("list Org attempts", error))?
        .into_iter()
        .map(AttemptRow::into_attempt)
        .collect()
    }

    async fn update_org_attempt(
        &self,
        update: OrgAttemptUpdate<'_>,
    ) -> StorageResult<ConditionalUpdate<OrgAttempt>> {
        validate_attempt_update(&update)?;
        let mut connection = self.connection().await?;
        let row = sqlx::query_as::<_, AttemptRow>(
            "UPDATE org_attempts
             SET status=$2, ended_at=$3, error=$4, result_summary=$5, review_outcome=$6,
                 note_refs=$7, artifacts=$8, metadata=$9
             WHERE id=$1 AND status=$10 AND ended_at IS NULL
             RETURNING id, workspace_id, work_item_id, attempt_number, actor_id, status,
                       started_at, ended_at, error, result_summary, review_outcome,
                       note_refs, artifacts, metadata",
        )
        .bind(update.id)
        .bind(attempt_status_name(update.status))
        .bind(update.ended_at)
        .bind(update.error)
        .bind(update.result_summary)
        .bind(update.review_outcome)
        .bind(serde_json::to_value(update.note_refs).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org attempt note refs",
                error,
            )
        })?)
        .bind(serde_json::to_value(update.artifacts).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org attempt artifacts",
                error,
            )
        })?)
        .bind(update.metadata)
        .bind(attempt_status_name(update.expected_status))
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("conditionally update Org attempt", error))?;
        if let Some(row) = row {
            return row.into_attempt().map(ConditionalUpdate::Applied);
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM org_attempts WHERE id=$1)")
                .bind(update.id)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error("check Org attempt update conflict", error))?;
        Ok(if exists {
            ConditionalUpdate::Conflict
        } else {
            ConditionalUpdate::NotFound
        })
    }

    async fn insert_org_operation(&self, operation: &StoredOrgOperation) -> StorageResult<()> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO org_operations (
                 workspace_id, operation_id, request_fingerprint, result, created_at
             ) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(operation.workspace_id.to_string())
        .bind(&operation.operation_id)
        .bind(&operation.request_fingerprint)
        .bind(&operation.result)
        .bind(operation.created_at)
        .execute(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("insert Org operation", error))?;
        Ok(())
    }

    async fn get_org_operation(
        &self,
        workspace_id: WorkspaceId,
        operation_id: &str,
    ) -> StorageResult<Option<StoredOrgOperation>> {
        let mut connection = self.connection().await?;
        sqlx::query_as::<_, OperationRow>(
            "SELECT workspace_id, operation_id, request_fingerprint, result, created_at
             FROM org_operations
             WHERE workspace_id=$1 AND operation_id=$2",
        )
        .bind(workspace_id.to_string())
        .bind(operation_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org operation", error))?
        .map(OperationRow::into_operation)
        .transpose()
    }
}

#[derive(sqlx::FromRow)]
struct OwnershipMoveRow {
    id: String,
    workspace_id: String,
    path: String,
    source: String,
    content_hash: String,
    revision: i64,
    created_at: i64,
    updated_at: i64,
    source_workspace_revision: i64,
    target_workspace_revision: i64,
}

impl OwnershipMoveRow {
    fn into_result(self) -> StorageResult<OrgDocumentOwnershipMoveResult> {
        Ok(OrgDocumentOwnershipMoveResult::Applied(
            AppliedOrgDocumentOwnershipMove {
                document: OrgDocument {
                    id: decode_document_id(self.id)?,
                    workspace_id: decode_workspace_id(self.workspace_id)?,
                    path: self.path,
                    source: self.source,
                    content_hash: self.content_hash,
                    revision: self.revision,
                    created_at: self.created_at,
                    updated_at: self.updated_at,
                },
                source_workspace_revision: self.source_workspace_revision,
                target_workspace_revision: self.target_workspace_revision,
            },
        ))
    }
}

async fn append_event(
    connection: &mut PgConnection,
    event: NewOrgEvent<'_>,
    internal: bool,
) -> StorageResult<OrgEvent> {
    validate_event(connection, &event, internal).await?;
    let row = sqlx::query_as::<_, EventRow>(
        "WITH next_sequence AS (
             UPDATE org_workspaces
             SET last_event_sequence=last_event_sequence+1
             WHERE id=$1
             RETURNING last_event_sequence
         )
         INSERT INTO org_events (
             id, workspace_id, sequence, subject_kind, subject_id,
             actor_id, attempt_id, event_type, occurred_at, summary, metadata,
             previous_state, resulting_state
         )
         SELECT $2, $1, last_event_sequence, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12
         FROM next_sequence
         RETURNING id, workspace_id, sequence, subject_kind, subject_id,
                   actor_id, attempt_id, event_type, occurred_at, summary, metadata,
                   previous_state, resulting_state",
    )
    .bind(event.workspace_id.to_string())
    .bind(event.id)
    .bind(event.subject_kind)
    .bind(event.subject_id)
    .bind(event.actor_id)
    .bind(event.attempt_id)
    .bind(event.event_type.as_str())
    .bind(event.occurred_at)
    .bind(event.summary)
    .bind(event.metadata)
    .bind(event.previous_state)
    .bind(event.resulting_state)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("append Org event", error))?;
    row.map(EventRow::into_event).transpose()?.ok_or_else(|| {
        StorageError::new(
            StorageErrorKind::Constraint,
            "Org event workspace does not exist",
        )
    })
}

async fn validate_event(
    connection: &mut PgConnection,
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
        let attempt = sqlx::query_as::<_, AttemptRow>(
            "SELECT id, workspace_id, work_item_id, attempt_number, actor_id, status,
                    started_at, ended_at, error, result_summary, review_outcome,
                    note_refs, artifacts, metadata
             FROM org_attempts WHERE id=$1",
        )
        .bind(attempt_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query Org event attempt", error))?
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorKind::Constraint,
                "Org event attempt does not exist",
            )
        })?;
        if attempt.workspace_id != event.workspace_id.to_string()
            || (event.subject_kind == "work_item" && event.subject_id != attempt.work_item_id)
        {
            return Err(StorageError::new(
                StorageErrorKind::Constraint,
                "Org event attempt does not match its workspace and work item subject",
            ));
        }
    }
    Ok(())
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

fn validate_attempt_update(update: &OrgAttemptUpdate<'_>) -> StorageResult<()> {
    if update.id.trim().is_empty() {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "Org attempt id must not be empty",
        ));
    }
    Ok(())
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

#[derive(sqlx::FromRow)]
struct AttemptRow {
    id: String,
    workspace_id: String,
    work_item_id: String,
    attempt_number: i64,
    actor_id: String,
    status: String,
    started_at: i64,
    ended_at: Option<i64>,
    error: Option<String>,
    result_summary: Option<String>,
    review_outcome: Option<String>,
    note_refs: Value,
    artifacts: Value,
    metadata: Value,
}

impl AttemptRow {
    fn into_attempt(self) -> StorageResult<OrgAttempt> {
        Ok(OrgAttempt {
            id: self.id,
            workspace_id: decode_workspace_id(self.workspace_id)?,
            work_item_id: decode_work_item_id(self.work_item_id)?,
            attempt_number: self.attempt_number,
            actor_id: self.actor_id,
            status: decode_attempt_status(&self.status)?,
            started_at: self.started_at,
            ended_at: self.ended_at,
            error: self.error,
            result_summary: self.result_summary,
            review_outcome: self.review_outcome,
            note_refs: serde_json::from_value::<Vec<OrgAttemptNoteReference>>(self.note_refs)
                .map_err(|error| {
                    StorageError::with_source(
                        StorageErrorKind::Corrupt,
                        "decode stored Org attempt note refs",
                        error,
                    )
                })?,
            artifacts: serde_json::from_value::<Vec<OrgArtifactReference>>(self.artifacts)
                .map_err(|error| {
                    StorageError::with_source(
                        StorageErrorKind::Corrupt,
                        "decode stored Org attempt artifacts",
                        error,
                    )
                })?,
            metadata: self.metadata,
        })
    }
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

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    workspace_id: String,
    sequence: i64,
    subject_kind: String,
    subject_id: String,
    actor_id: String,
    attempt_id: Option<String>,
    event_type: String,
    occurred_at: i64,
    summary: String,
    metadata: Value,
    previous_state: Option<String>,
    resulting_state: Option<String>,
}

impl EventRow {
    fn into_event(self) -> StorageResult<OrgEvent> {
        Ok(OrgEvent {
            id: self.id,
            workspace_id: decode_workspace_id(self.workspace_id)?,
            sequence: self.sequence,
            subject_kind: self.subject_kind,
            subject_id: self.subject_id,
            actor_id: self.actor_id,
            attempt_id: self.attempt_id,
            event_type: self.event_type.parse::<OrgEventType>().map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    "decode stored Org event type",
                    error,
                )
            })?,
            occurred_at: self.occurred_at,
            summary: self.summary,
            metadata: self.metadata,
            previous_state: self.previous_state,
            resulting_state: self.resulting_state,
        })
    }
}

#[derive(sqlx::FromRow)]
struct OperationRow {
    workspace_id: String,
    operation_id: String,
    request_fingerprint: String,
    result: Value,
    created_at: i64,
}

impl OperationRow {
    fn into_operation(self) -> StorageResult<StoredOrgOperation> {
        Ok(StoredOrgOperation {
            workspace_id: decode_workspace_id(self.workspace_id)?,
            operation_id: self.operation_id,
            request_fingerprint: self.request_fingerprint,
            result: self.result,
            created_at: self.created_at,
        })
    }
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
    connection: &mut PgConnection,
    document_id: DocumentId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<ProjectionPlan> {
    let workspace_id = document_workspace(connection, document_id).await?;
    for item in items {
        if item.document_id != document_id || item.workspace_id != workspace_id {
            return Err(projection_constraint(
                "projected item does not belong to the target Org document and workspace",
            ));
        }
    }
    validate_projection(connection, workspace_id, items, false).await
}

async fn validate_workspace_rebuild(
    connection: &mut PgConnection,
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
            let stored_workspace = document_workspace(connection, item.document_id).await?;
            if stored_workspace != workspace_id {
                return Err(projection_constraint(
                    "projected item document does not belong to the rebuilt Org workspace",
                ));
            }
        }
    }
    validate_projection(connection, workspace_id, items, true).await
}

async fn document_workspace(
    connection: &mut PgConnection,
    document_id: DocumentId,
) -> StorageResult<WorkspaceId> {
    let workspace: Option<String> =
        sqlx::query_scalar("SELECT workspace_id FROM org_documents WHERE id = $1")
            .bind(document_id.to_string())
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query Org projection document workspace", error))?;
    workspace
        .map(decode_workspace_id)
        .transpose()?
        .ok_or_else(|| projection_constraint("Org projection document does not exist"))
}

#[derive(sqlx::FromRow)]
struct ProjectionIdentityRow {
    id: String,
    workspace_id: String,
    document_id: String,
    parent_id: Option<String>,
    source_order: i64,
}

fn projection_batches<T>(values: &[T]) -> impl Iterator<Item = &[T]> {
    values.chunks(PROJECTION_BATCH_SIZE)
}

async fn load_existing_projections(
    connection: &mut PgConnection,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<HashMap<WorkItemId, ExistingProjection>> {
    let mut existing = HashMap::new();
    let workspace_rows = sqlx::query_as::<_, ProjectionIdentityRow>(
        "SELECT id, workspace_id, document_id, parent_id, source_order
         FROM org_work_items WHERE workspace_id = $1",
    )
    .bind(workspace_id.to_string())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("query existing Org projection identities", error))?;
    merge_existing_projection_rows(workspace_rows, &mut existing)?;

    for batch in projection_batches(items) {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, workspace_id, document_id, parent_id, source_order
             FROM org_work_items WHERE id IN (",
        );
        let mut ids = query.separated(", ");
        for item in batch {
            ids.push_bind(item.id.to_string());
        }
        ids.push_unseparated(")");
        let rows = query
            .build_query_as::<ProjectionIdentityRow>()
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("query candidate Org projection identities", error))?;
        merge_existing_projection_rows(rows, &mut existing)?;
    }
    Ok(existing)
}

fn merge_existing_projection_rows(
    rows: Vec<ProjectionIdentityRow>,
    existing: &mut HashMap<WorkItemId, ExistingProjection>,
) -> StorageResult<()> {
    for row in rows {
        existing.insert(
            decode_work_item_id(row.id)?,
            ExistingProjection {
                workspace_id: decode_workspace_id(row.workspace_id)?,
                document_id: decode_document_id(row.document_id)?,
                parent_id: row.parent_id.map(decode_work_item_id).transpose()?,
                source_order: row.source_order,
            },
        );
    }
    Ok(())
}

async fn validate_projection(
    connection: &mut PgConnection,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
    rebuilding: bool,
) -> StorageResult<ProjectionPlan> {
    let existing = load_existing_projections(connection, workspace_id, items).await?;

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
        load_workspace_dependencies(connection, workspace_id).await?
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
    connection: &mut PgConnection,
    workspace_id: WorkspaceId,
) -> StorageResult<HashMap<WorkItemId, Vec<WorkItemId>>> {
    let rows = sqlx::query_as::<_, DependencyRow>(
        "SELECT dependency.work_item_id, dependency.depends_on_id
         FROM org_dependencies dependency
         JOIN org_work_items item ON item.id = dependency.work_item_id
         WHERE item.workspace_id = $1
         ORDER BY dependency.work_item_id, dependency.depends_on_id",
    )
    .bind(workspace_id.to_string())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("query Org workspace dependencies", error))?;
    let mut dependencies = HashMap::<WorkItemId, Vec<WorkItemId>>::new();
    for row in rows {
        dependencies
            .entry(decode_work_item_id(row.work_item_id)?)
            .or_default()
            .push(decode_work_item_id(row.depends_on_id)?);
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
    connection: &mut PgConnection,
    staged_existing: &[(WorkItemId, i64)],
) -> StorageResult<()> {
    for (id, source_order) in staged_existing {
        sqlx::query("UPDATE org_work_items SET source_order = $2 WHERE id = $1")
            .bind(id.to_string())
            .bind(*source_order)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("stage Org projection source order", error))?;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct DependencyRow {
    work_item_id: String,
    depends_on_id: String,
}

async fn upsert_projection(
    connection: &mut PgConnection,
    items: &[OrgProjectedWorkItem],
    order: &[usize],
) -> StorageResult<()> {
    for chunk in projection_batches(order) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO org_work_items (
                 id, workspace_id, document_id, parent_id, source_order, item_type,
                 title, state, priority, scheduled_raw, scheduled_local,
                 scheduled_timezone, scheduled_utc, deadline_raw, deadline_local,
                 deadline_timezone, deadline_utc, assignee, requires_review, created_at
             ) ",
        );
        query.push_values(chunk, |mut row, index| {
            let item = &items[*index];
            row.push_bind(item.id.to_string())
                .push_bind(item.workspace_id.to_string())
                .push_bind(item.document_id.to_string())
                .push_bind(item.parent_id.map(|value| value.to_string()))
                .push_bind(item.source_order)
                .push_bind(work_item_type_name(item.item_type))
                .push_bind(&item.title)
                .push_bind(&item.state)
                .push_bind(item.priority.map(|value| value.to_string()))
                .push_bind(item.scheduled.as_ref().map(|value| value.raw.clone()))
                .push_bind(item.scheduled.as_ref().map(|value| value.local.clone()))
                .push_bind(item.scheduled.as_ref().map(|value| value.timezone.clone()))
                .push_bind(item.scheduled.as_ref().map(|value| value.utc_timestamp))
                .push_bind(item.deadline.as_ref().map(|value| value.raw.clone()))
                .push_bind(item.deadline.as_ref().map(|value| value.local.clone()))
                .push_bind(item.deadline.as_ref().map(|value| value.timezone.clone()))
                .push_bind(item.deadline.as_ref().map(|value| value.utc_timestamp))
                .push_bind(&item.assignee)
                .push_bind(item.requires_review)
                .push_bind(item.created_at);
        });
        query.push(
            " ON CONFLICT(id) DO UPDATE SET
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
        );
        query
            .build()
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("upsert Org projected work item", error))?;
    }

    for item in items {
        let id = item.id.to_string();
        for (sql, context) in [
            (
                "DELETE FROM org_work_item_tags WHERE work_item_id = $1",
                "replace Org projected work item tags",
            ),
            (
                "DELETE FROM org_dependencies WHERE work_item_id = $1",
                "replace Org projected work item dependencies",
            ),
            (
                "DELETE FROM org_note_links WHERE work_item_id = $1",
                "replace Org projected work item note links",
            ),
        ] {
            sqlx::query(sql)
                .bind(&id)
                .execute(&mut *connection)
                .await
                .map_err(|error| map_sqlx_error(context, error))?;
        }
    }
    let tags = items
        .iter()
        .flat_map(|item| {
            item.tags
                .iter()
                .map(move |tag| (item.id.to_string(), tag.as_str()))
        })
        .collect::<Vec<_>>();
    for chunk in projection_batches(&tags) {
        let mut query =
            QueryBuilder::<Postgres>::new("INSERT INTO org_work_item_tags (work_item_id, tag) ");
        query.push_values(chunk, |mut row, (id, tag)| {
            row.push_bind(id).push_bind(*tag);
        });
        query
            .build()
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("insert Org projected work item tags", error))?;
    }
    let dependencies = items
        .iter()
        .flat_map(|item| {
            item.dependencies
                .iter()
                .map(move |target| (item.id.to_string(), target.to_string()))
        })
        .collect::<Vec<_>>();
    for chunk in projection_batches(&dependencies) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO org_dependencies (work_item_id, depends_on_id) ",
        );
        query.push_values(chunk, |mut row, (id, target)| {
            row.push_bind(id).push_bind(target);
        });
        query
            .build()
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("insert Org projected dependencies", error))?;
    }
    let links = items
        .iter()
        .flat_map(|item| {
            item.note_links
                .iter()
                .enumerate()
                .map(move |(ordinal, link)| (item.id.to_string(), ordinal as i64, link))
        })
        .collect::<Vec<_>>();
    for chunk in projection_batches(&links) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO org_note_links (
                 work_item_id, ordinal, purpose, note_id, description
             ) ",
        );
        query.push_values(chunk, |mut row, (id, ordinal, link)| {
            row.push_bind(id)
                .push_bind(*ordinal)
                .push_bind(&link.purpose)
                .push_bind(link.note_id.to_string())
                .push_bind(&link.description);
        });
        query
            .build()
            .execute(&mut *connection)
            .await
            .map_err(|error| map_sqlx_error("insert Org projected note links", error))?;
    }
    Ok(())
}

fn work_item_type_name(value: WorkItemType) -> &'static str {
    match value {
        WorkItemType::Project => "project",
        WorkItemType::Epic => "epic",
        WorkItemType::Issue => "issue",
        WorkItemType::Task => "task",
        WorkItemType::Subtask => "subtask",
        WorkItemType::Review => "review",
        WorkItemType::Approval => "approval",
        WorkItemType::Incident => "incident",
        WorkItemType::Milestone => "milestone",
    }
}

fn serialize_policy(policy: &WorkspacePolicy) -> StorageResult<Value> {
    serde_json::to_value(policy).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "serialize Org workspace policy",
            error,
        )
    })
}

#[derive(sqlx::FromRow)]
struct WorkItemRow {
    id: String,
    workspace_id: String,
    document_id: String,
    parent_id: Option<String>,
    source_order: i64,
    item_type: String,
    title: String,
    state: Option<String>,
    priority: Option<String>,
    scheduled_raw: Option<String>,
    scheduled_local: Option<String>,
    scheduled_timezone: Option<String>,
    scheduled_utc: Option<i64>,
    deadline_raw: Option<String>,
    deadline_local: Option<String>,
    deadline_timezone: Option<String>,
    deadline_utc: Option<i64>,
    assignee: Option<String>,
    requires_review: bool,
    created_at: i64,
}

fn decode_projected_item(row: WorkItemRow) -> StorageResult<OrgProjectedWorkItem> {
    let id = decode_work_item_id(row.id)?;
    Ok(OrgProjectedWorkItem {
        id,
        workspace_id: decode_workspace_id(row.workspace_id)?,
        document_id: decode_document_id(row.document_id)?,
        parent_id: row.parent_id.map(decode_work_item_id).transpose()?,
        source_order: row.source_order,
        item_type: decode_work_item_type(row.item_type)?,
        title: row.title,
        state: row.state,
        priority: row
            .priority
            .map(|value| decode_priority(&value))
            .transpose()?,
        scheduled: decode_timestamp(
            row.scheduled_raw,
            row.scheduled_local,
            row.scheduled_timezone,
            row.scheduled_utc,
            "scheduled",
        )?,
        deadline: decode_timestamp(
            row.deadline_raw,
            row.deadline_local,
            row.deadline_timezone,
            row.deadline_utc,
            "deadline",
        )?,
        assignee: row.assignee,
        requires_review: row.requires_review,
        created_at: row.created_at,
        tags: Vec::new(),
        dependencies: Vec::new(),
        note_links: Vec::new(),
    })
}

#[derive(sqlx::FromRow)]
struct TagRow {
    work_item_id: String,
    tag: String,
}

#[derive(sqlx::FromRow)]
struct ListedNoteLinkRow {
    work_item_id: String,
    purpose: String,
    note_id: String,
    description: String,
}

impl ListedNoteLinkRow {
    fn into_note_link(self) -> StorageResult<NoteLink> {
        let note_id_text = self.note_id;
        Ok(NoteLink {
            purpose: self.purpose,
            note_id: note_id_text.parse().map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    format!("decode stored Org note id {note_id_text:?}"),
                    error,
                )
            })?,
            description: self.description,
        })
    }
}

async fn load_relations_for_projected_items(
    connection: &mut PgConnection,
    items: &mut [OrgProjectedWorkItem],
) -> StorageResult<()> {
    let document_ids = items
        .iter()
        .map(|item| item.document_id)
        .collect::<BTreeSet<_>>();
    for document_id in document_ids {
        load_projection_relations(connection, document_id, items).await?;
    }
    Ok(())
}

async fn load_projection_relations(
    connection: &mut PgConnection,
    document_id: DocumentId,
    items: &mut [OrgProjectedWorkItem],
) -> StorageResult<()> {
    let index = items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, index))
        .collect::<HashMap<_, _>>();
    let tags = sqlx::query_as::<_, TagRow>(
        "SELECT tag.work_item_id, tag.tag
         FROM org_work_item_tags tag
         JOIN org_work_items item ON item.id = tag.work_item_id
         WHERE item.document_id = $1
         ORDER BY tag.work_item_id, tag.tag",
    )
    .bind(document_id.to_string())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("list Org projected tags", error))?;
    for row in tags {
        let id = decode_work_item_id(row.work_item_id)?;
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.tags.push(row.tag);
        }
    }

    let dependencies = sqlx::query_as::<_, DependencyRow>(
        "SELECT dependency.work_item_id, dependency.depends_on_id
         FROM org_dependencies dependency
         JOIN org_work_items item ON item.id = dependency.work_item_id
         WHERE item.document_id = $1
         ORDER BY dependency.work_item_id, dependency.depends_on_id",
    )
    .bind(document_id.to_string())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("list Org projected dependencies", error))?;
    for row in dependencies {
        let id = decode_work_item_id(row.work_item_id)?;
        let target = decode_work_item_id(row.depends_on_id)?;
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.dependencies.push(target);
        }
    }

    let links = sqlx::query_as::<_, ListedNoteLinkRow>(
        "SELECT link.work_item_id, link.purpose, link.note_id, link.description
         FROM org_note_links link
         JOIN org_work_items item ON item.id = link.work_item_id
         WHERE item.document_id = $1
         ORDER BY link.work_item_id, link.ordinal",
    )
    .bind(document_id.to_string())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("list Org projected note links", error))?;
    for row in links {
        let id = decode_work_item_id(row.work_item_id.clone())?;
        let link = row.into_note_link()?;
        if let Some(item) = index.get(&id).and_then(|index| items.get_mut(*index)) {
            item.note_links.push(link);
        }
    }
    Ok(())
}

fn decode_timestamp(
    raw: Option<String>,
    local: Option<String>,
    timezone: Option<String>,
    utc_timestamp: Option<i64>,
    label: &str,
) -> StorageResult<Option<StoredOrgTimestamp>> {
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

#[derive(sqlx::FromRow)]
struct WorkspaceRow {
    id: String,
    slug: String,
    display_name: String,
    description: String,
    timezone: String,
    policy_schema_version: i64,
    policy: Value,
    revision: i64,
    created_at: i64,
    updated_at: i64,
    archived_at: Option<i64>,
}

impl WorkspaceRow {
    fn into_workspace(self) -> StorageResult<OrgWorkspace> {
        let policy = serde_json::from_value(self.policy).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Corrupt,
                "decode stored Org workspace policy",
                error,
            )
        })?;
        Ok(OrgWorkspace {
            id: decode_workspace_id(self.id)?,
            slug: self.slug,
            display_name: self.display_name,
            description: self.description,
            timezone: self.timezone,
            policy_schema_version: self.policy_schema_version,
            policy,
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
            archived_at: self.archived_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DocumentRow {
    id: String,
    workspace_id: String,
    path: String,
    source: String,
    content_hash: String,
    revision: i64,
    created_at: i64,
    updated_at: i64,
}

impl DocumentRow {
    fn into_document(self) -> StorageResult<OrgDocument> {
        Ok(OrgDocument {
            id: decode_document_id(self.id)?,
            workspace_id: decode_workspace_id(self.workspace_id)?,
            path: self.path,
            source: self.source,
            content_hash: self.content_hash,
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
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
