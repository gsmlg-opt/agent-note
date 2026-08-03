use crate::connection::map_turso_error;
use crate::TursoSession;
use note_org::{DocumentId, NoteLink, WorkItemId, WorkItemType, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgWorkspace, OrgDocument, OrgDocumentUpdate,
    OrgProjectedWorkItem, OrgRepository, OrgWorkspace, OrgWorkspaceUpdate, StorageError,
    StorageErrorKind, StorageResult, StoredOrgTimestamp,
};
use std::collections::{HashMap, HashSet};
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
        let order = validate_document_projection(self, document_id, items).await?;
        upsert_projection(self, items, &order).await
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
            items.push(decode_projected_item(self, &row).await?);
        }
        Ok(items)
    }

    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()> {
        let order = validate_workspace_rebuild(self, workspace_id, items).await?;
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
        upsert_projection(self, items, &order).await
    }
}

async fn validate_document_projection(
    session: &TursoSession,
    document_id: DocumentId,
    items: &[OrgProjectedWorkItem],
) -> StorageResult<Vec<usize>> {
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
) -> StorageResult<Vec<usize>> {
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
) -> StorageResult<Vec<usize>> {
    let mut existing = HashMap::<WorkItemId, (WorkspaceId, DocumentId, Option<WorkItemId>)>::new();
    let mut rows = session
        .connection
        .query(
            "SELECT id, workspace_id, document_id, parent_id FROM org_work_items",
            (),
        )
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
        existing.insert(id, (stored_workspace, document, parent));
    }

    let mut ids = HashSet::new();
    let mut source_orders = HashSet::new();
    for item in items {
        if !ids.insert(item.id) {
            return Err(projection_constraint(
                "duplicate Org projected work item id",
            ));
        }
        if !source_orders.insert((item.document_id, item.source_order)) {
            return Err(projection_constraint(
                "duplicate source order in Org document projection",
            ));
        }
        if let Some((stored_workspace, stored_document, _)) = existing.get(&item.id) {
            if *stored_workspace != workspace_id
                || (!rebuilding && *stored_document != item.document_id)
            {
                return Err(projection_constraint(
                    "Org projected work item id is owned by another workspace or document",
                ));
            }
        }
    }

    let mut parents = existing
        .iter()
        .filter(|(_, (stored_workspace, _, _))| *stored_workspace == workspace_id)
        .map(|(id, (_, _, parent))| (*id, *parent))
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
                .is_some_and(|(stored_workspace, _, _)| *stored_workspace == workspace_id);
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

    let mut order = (0..items.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let item = &items[*index];
        (depths[&item.id], item.source_order, item.id)
    });
    Ok(order)
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
        for tag in &item.tags {
            session
                .connection
                .execute(
                    "INSERT INTO org_work_item_tags (work_item_id, tag) VALUES (?1, ?2)",
                    turso::params![id.clone(), tag.clone()],
                )
                .await
                .map_err(|error| map_turso_error("insert Org projected work item tag", error))?;
        }
        for dependency in &item.dependencies {
            session
                .connection
                .execute(
                    "INSERT INTO org_dependencies (work_item_id, depends_on_id) VALUES (?1, ?2)",
                    turso::params![id.clone(), dependency.to_string()],
                )
                .await
                .map_err(|error| {
                    map_turso_error("insert Org projected work item dependency", error)
                })?;
        }
        for (ordinal, link) in item.note_links.iter().enumerate() {
            session
                .connection
                .execute(
                    "INSERT INTO org_note_links (
                         work_item_id, ordinal, purpose, note_id, description
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    turso::params![
                        id.clone(),
                        ordinal as i64,
                        link.purpose.clone(),
                        link.note_id.to_string(),
                        link.description.clone()
                    ],
                )
                .await
                .map_err(|error| map_turso_error("insert Org projected note link", error))?;
        }
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

async fn decode_projected_item(
    session: &TursoSession,
    row: &turso::Row,
) -> StorageResult<OrgProjectedWorkItem> {
    let id = decode_work_item_id(
        row.get(0)
            .map_err(|error| map_turso_error("decode Org projected work item id", error))?,
    )?;
    let priority = row
        .get::<Option<String>>(8)
        .map_err(|error| map_turso_error("decode Org projected priority", error))?
        .map(|value| decode_priority(&value))
        .transpose()?;
    let mut item = OrgProjectedWorkItem {
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
    };
    item.tags = query_strings(
        session,
        "SELECT tag FROM org_work_item_tags WHERE work_item_id = ?1 ORDER BY tag",
        id,
        "list Org projected tags",
    )
    .await?;
    item.dependencies = query_strings(
        session,
        "SELECT depends_on_id FROM org_dependencies WHERE work_item_id = ?1 ORDER BY depends_on_id",
        id,
        "list Org projected dependencies",
    )
    .await?
    .into_iter()
    .map(decode_work_item_id)
    .collect::<StorageResult<Vec<_>>>()?;
    let mut links = session
        .connection
        .query(
            "SELECT purpose, note_id, description FROM org_note_links
             WHERE work_item_id = ?1 ORDER BY ordinal",
            turso::params![id.to_string()],
        )
        .await
        .map_err(|error| map_turso_error("list Org projected note links", error))?;
    while let Some(link) = links
        .next()
        .await
        .map_err(|error| map_turso_error("read Org projected note links", error))?
    {
        let note_id_text: String = link
            .get(1)
            .map_err(|error| map_turso_error("decode Org projected note id", error))?;
        item.note_links.push(NoteLink {
            purpose: link
                .get(0)
                .map_err(|error| map_turso_error("decode Org projected note purpose", error))?,
            note_id: note_id_text.parse().map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Corrupt,
                    format!("decode stored Org note id {note_id_text:?}"),
                    error,
                )
            })?,
            description: link
                .get(2)
                .map_err(|error| map_turso_error("decode Org projected note description", error))?,
        });
    }
    Ok(item)
}

async fn query_strings(
    session: &TursoSession,
    sql: &str,
    id: WorkItemId,
    context: &str,
) -> StorageResult<Vec<String>> {
    let mut rows = session
        .connection
        .query(sql, turso::params![id.to_string()])
        .await
        .map_err(|error| map_turso_error(context, error))?;
    let mut values = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error(context, error))?
    {
        values.push(
            row.get(0)
                .map_err(|error| map_turso_error(context, error))?,
        );
    }
    Ok(values)
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
