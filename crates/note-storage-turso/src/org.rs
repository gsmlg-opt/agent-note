use crate::connection::map_turso_error;
use crate::TursoSession;
use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgEvent, NewOrgWorkspace, OrgDocument, OrgDocumentUpdate,
    OrgEvent, OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate, StorageError,
    StorageErrorKind, StorageResult, StoredOrgOperation, StoredOrgTimestamp,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr as _;

const WORKSPACE_COLUMNS: &str =
    "id, slug, display_name, description, timezone, policy_schema_version, policy, revision, created_at, updated_at, archived_at";
const DOCUMENT_COLUMNS: &str =
    "id, workspace_id, path, source, content_hash, revision, created_at, updated_at";
const WORK_ITEM_COLUMNS: &str =
    "id, workspace_id, document_id, parent_id, source_order, item_type, title, state, priority,
     scheduled_raw, scheduled_local, scheduled_timezone, scheduled_utc,
     deadline_raw, deadline_local, deadline_timezone, deadline_utc,
     assignee, requires_review, created_at";
const EVENT_COLUMNS: &str =
    "id, workspace_id, sequence, subject_kind, subject_id, actor_id, event_type, occurred_at, summary, metadata";

#[async_trait::async_trait]
impl OrgRepository for TursoSession {
    async fn insert_org_workspace(&self, value: NewOrgWorkspace<'_>) -> StorageResult<()> {
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
        let sql = format!("SELECT {WORKSPACE_COLUMNS} FROM org_workspaces WHERE id = ?1");
        query_optional_workspace(self, &sql, turso::params![id.to_string()]).await
    }

    async fn get_org_workspace_by_slug(&self, slug: &str) -> StorageResult<Option<OrgWorkspace>> {
        let sql = format!("SELECT {WORKSPACE_COLUMNS} FROM org_workspaces WHERE slug = ?1");
        query_optional_workspace(self, &sql, turso::params![slug]).await
    }

    async fn list_org_workspaces(
        &self,
        include_archived: bool,
    ) -> StorageResult<Vec<OrgWorkspace>> {
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
        let sql = format!("SELECT {DOCUMENT_COLUMNS} FROM org_documents WHERE id = ?1");
        query_optional_document(self, &sql, turso::params![id.to_string()]).await
    }

    async fn list_org_documents(
        &self,
        workspace_id: WorkspaceId,
    ) -> StorageResult<Vec<OrgDocument>> {
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

    async fn replace_org_document_projection(
        &self,
        document_id: DocumentId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let plan = validate_document_projection(self, document_id, items).await?;
        stage_source_orders(self, &plan.staged_existing).await?;
        upsert_projection(self, items, &plan.order).await
    }

    async fn list_org_document_projection(
        &self,
        document_id: DocumentId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>> {
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

    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
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
        let metadata = serde_json::to_string(event.metadata).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize Org event metadata",
                error,
            )
        })?;
        let sql = format!(
            "INSERT INTO org_events (
                 id, workspace_id, sequence, subject_kind, subject_id,
                 actor_id, event_type, occurred_at, summary, metadata
             )
             SELECT ?1, workspace.id, workspace.last_event_sequence+1,
                    ?3, ?4, ?5, ?6, ?7, ?8, ?9
             FROM org_workspaces workspace
             WHERE workspace.id=?2
             RETURNING {EVENT_COLUMNS}"
        );
        let mut rows = self
            .connection
            .query(
                &sql,
                turso::params![
                    event.id,
                    event.workspace_id.to_string(),
                    event.subject_kind,
                    event.subject_id,
                    event.actor_id,
                    event.event_type,
                    event.occurred_at,
                    event.summary,
                    metadata
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
        // Turso rolls back an unfinished writer when its Statement is dropped.
        // Drive RETURNING to Done so success commits, while cancellation before
        // this point drops `rows` and atomically aborts both trigger and insert.
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

    async fn list_org_events(
        &self,
        workspace_id: WorkspaceId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>> {
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

    async fn insert_org_operation(&self, operation: &StoredOrgOperation) -> StorageResult<()> {
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
        .get(9)
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
        event_type: row
            .get(6)
            .map_err(|error| map_turso_error("decode Org event type", error))?,
        occurred_at: row
            .get(7)
            .map_err(|error| map_turso_error("decode Org event occurred at", error))?,
        summary: row
            .get(8)
            .map_err(|error| map_turso_error("decode Org event summary", error))?,
        metadata,
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

async fn validate_projection(
    session: &TursoSession,
    workspace_id: WorkspaceId,
    items: &[OrgProjectedWorkItem],
    rebuilding: bool,
) -> StorageResult<ProjectionPlan> {
    let mut identity_sql = String::from(
        "SELECT id, workspace_id, document_id, parent_id, source_order
         FROM org_work_items WHERE workspace_id = ?1",
    );
    let mut identity_params = vec![turso::Value::from(workspace_id.to_string())];
    if !items.is_empty() {
        identity_sql.push_str(" OR id IN (");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                identity_sql.push_str(", ");
            }
            identity_sql.push('?');
            identity_sql.push_str(&(index + 2).to_string());
            identity_params.push(turso::Value::from(item.id.to_string()));
        }
        identity_sql.push(')');
    }
    let mut existing = HashMap::<WorkItemId, ExistingProjection>::new();
    let mut rows = session
        .connection
        .query(&identity_sql, turso::params_from_iter(identity_params))
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
    drop(rows);

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
    for chunk in rows.chunks(500) {
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
