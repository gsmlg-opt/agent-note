use note_org::{WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{FixedOrgClock, OrgContext};
use note_storage::{NewOrgWorkspace, StorageBackend};
use note_storage_turso::TursoStorage;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::Arc;

pub async fn org_test_context(
    now: i64,
) -> (
    OrgContext,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
    PathBuf,
    WorkspaceId,
) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("org-test.db");
    let backend: Arc<dyn StorageBackend> = Arc::new(TursoStorage::open(&db_path).await.unwrap());
    let workspace_id = WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    backend
        .session()
        .await
        .unwrap()
        .insert_org_workspace(NewOrgWorkspace {
            id: workspace_id,
            slug: "engineering",
            display_name: "Engineering",
            description: "Org pipeline test workspace",
            timezone: "UTC",
            policy_schema_version: 1,
            policy: &WorkspacePolicy::engineering_default(),
            now: 1,
        })
        .await
        .unwrap();
    (
        OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(now))),
        backend,
        dir,
        db_path,
        workspace_id,
    )
}
