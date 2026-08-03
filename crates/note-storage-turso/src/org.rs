use crate::connection::map_turso_error;
use crate::TursoSession;
use note_org::{DocumentId, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgWorkspace, OrgDocument, OrgDocumentUpdate, OrgRepository,
    OrgWorkspace, OrgWorkspaceUpdate, StorageError, StorageErrorKind, StorageResult,
};
use std::str::FromStr as _;

const WORKSPACE_COLUMNS: &str =
    "id, slug, display_name, description, timezone, policy_schema_version, policy, revision, created_at, updated_at, archived_at";
const DOCUMENT_COLUMNS: &str =
    "id, workspace_id, path, source, content_hash, revision, created_at, updated_at";

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
