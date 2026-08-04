use note_org::{WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    FixedOrgClock, OrgClaimPhase, OrgClaimTestHook, OrgContext, OrgError, OrgTokenSource,
    OrgWorkflowPhase, OrgWorkflowTestHook,
};
use note_storage::{NewOrgWorkspace, StorageBackend};
use note_storage_turso::TursoStorage;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::Mutex;

pub struct DeterministicTokenSource {
    values: Mutex<std::collections::VecDeque<String>>,
}

impl DeterministicTokenSource {
    pub fn new(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            values: Mutex::new(values.into_iter().map(Into::into).collect()),
        }
    }
}

impl OrgTokenSource for DeterministicTokenSource {
    fn generate_token(&self) -> Result<String, OrgError> {
        self.values
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| OrgError::invalid_input("deterministic token source exhausted"))
    }
}

pub struct FailOnceAtPhase {
    phase: OrgClaimPhase,
    fired: std::sync::atomic::AtomicBool,
}

impl FailOnceAtPhase {
    pub fn new(phase: OrgClaimPhase) -> Self {
        Self {
            phase,
            fired: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl OrgClaimTestHook for FailOnceAtPhase {
    fn after_phase(&self, phase: OrgClaimPhase) -> Result<(), OrgError> {
        if phase == self.phase && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(OrgError::new(
                note_pipelines::org::OrgErrorCode::StorageFailure,
                "injected claim failure",
                serde_json::json!({"phase": format!("{phase:?}")}),
                true,
            ));
        }
        Ok(())
    }
}

pub struct FailOnceAtWorkflowPhase {
    phase: OrgWorkflowPhase,
    fired: std::sync::atomic::AtomicBool,
}

impl FailOnceAtWorkflowPhase {
    pub fn new(phase: OrgWorkflowPhase) -> Self {
        Self {
            phase,
            fired: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl OrgWorkflowTestHook for FailOnceAtWorkflowPhase {
    fn after_phase(&self, phase: OrgWorkflowPhase) -> Result<(), OrgError> {
        if phase == self.phase && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(OrgError::new(
                note_pipelines::org::OrgErrorCode::StorageFailure,
                "injected workflow failure",
                serde_json::json!({"phase": format!("{phase:?}")}),
                true,
            ));
        }
        Ok(())
    }
}

pub struct FailAtWorkflowPhaseOccurrence {
    phase: OrgWorkflowPhase,
    occurrence: usize,
    seen: std::sync::atomic::AtomicUsize,
}

impl FailAtWorkflowPhaseOccurrence {
    pub fn new(phase: OrgWorkflowPhase, occurrence: usize) -> Self {
        assert!(occurrence > 0);
        Self {
            phase,
            occurrence,
            seen: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl OrgWorkflowTestHook for FailAtWorkflowPhaseOccurrence {
    fn after_phase(&self, phase: OrgWorkflowPhase) -> Result<(), OrgError> {
        if phase == self.phase
            && self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.occurrence
        {
            return Err(OrgError::new(
                note_pipelines::org::OrgErrorCode::StorageFailure,
                "injected workflow occurrence failure",
                serde_json::json!({
                    "phase": format!("{phase:?}"),
                    "occurrence": self.occurrence,
                }),
                true,
            ));
        }
        Ok(())
    }
}

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
