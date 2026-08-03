use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, OrgDocument, OrgDocumentUpdate,
    OrgEvent, OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate, StorageError,
    StorageErrorKind, StorageResult, StoredOrgOperation, StoredOrgTimestamp,
};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, QueryBuilder};
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr as _;

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
        let row = sqlx::query_as::<_, EventRow>(
            "WITH next_sequence AS (
                 UPDATE org_workspaces
                 SET last_event_sequence=last_event_sequence+1
                 WHERE id=$1
                 RETURNING last_event_sequence
             )
             INSERT INTO org_events (
                 id, workspace_id, sequence, subject_kind, subject_id,
                 actor_id, event_type, occurred_at, summary, metadata
             )
             SELECT $2, $1, last_event_sequence, $3, $4, $5, $6, $7, $8, $9
             FROM next_sequence
             RETURNING id, workspace_id, sequence, subject_kind, subject_id,
                       actor_id, event_type, occurred_at, summary, metadata",
        )
        .bind(event.workspace_id.to_string())
        .bind(event.id)
        .bind(event.subject_kind)
        .bind(event.subject_id)
        .bind(event.actor_id)
        .bind(event.event_type)
        .bind(event.occurred_at)
        .bind(event.summary)
        .bind(event.metadata)
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
                    actor_id, event_type, occurred_at, summary, metadata
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
    event_type: String,
    occurred_at: i64,
    summary: String,
    metadata: Value,
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
            event_type: self.event_type,
            occurred_at: self.occurred_at,
            summary: self.summary,
            metadata: self.metadata,
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

async fn validate_projection(
    connection: &mut PgConnection,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
    rebuilding: bool,
) -> StorageResult<ProjectionPlan> {
    let mut identity_query = QueryBuilder::<Postgres>::new(
        "SELECT id, workspace_id, document_id, parent_id, source_order
         FROM org_work_items WHERE workspace_id = ",
    );
    identity_query.push_bind(workspace_id.to_string());
    if !items.is_empty() {
        identity_query.push(" OR id IN (");
        let mut ids = identity_query.separated(", ");
        for item in items {
            ids.push_bind(item.id.to_string());
        }
        ids.push_unseparated(")");
    }
    let rows = identity_query
        .build_query_as::<ProjectionIdentityRow>()
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| map_sqlx_error("query existing Org projection identities", error))?;
    let mut existing = HashMap::<WorkItemId, ExistingProjection>::new();
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

    let mut depths = HashMap::new();
    for item in items {
        let mut path = HashSet::new();
        let mut current = item.id;
        let mut depth = 0usize;
        loop {
            if !path.insert(current) {
                return Err(projection_constraint(
                    "cyclic Org projected parent structure",
                ));
            }
            let Some(parent) = parents.get(&current).copied().flatten() else {
                break;
            };
            depth += 1;
            current = parent;
        }
        depths.insert(item.id, depth);
    }
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
    for chunk in order.chunks(1_000) {
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
    for chunk in tags.chunks(1_000) {
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
    for chunk in dependencies.chunks(1_000) {
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
    for chunk in links.chunks(1_000) {
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
