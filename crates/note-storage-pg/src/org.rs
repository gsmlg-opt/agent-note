use crate::connection::map_sqlx_error;
use crate::PgSession;
use note_org::{DocumentId, WorkspaceId, WorkspacePolicy};
use note_storage::{
    CompareAndSwap, NewOrgDocument, NewOrgWorkspace, OrgDocument, OrgDocumentUpdate, OrgRepository,
    OrgWorkspace, OrgWorkspaceUpdate, StorageError, StorageErrorKind, StorageResult,
};
use serde_json::Value;
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
