use note_org::{WorkspaceId, WorkspacePolicy};
use note_pipelines::org::{
    FixedOrgClock, FixedOrgCursorSigner, OrgClaimPhase, OrgClaimTestHook, OrgContext, OrgError,
    OrgTokenSource, OrgWorkflowPhase, OrgWorkflowTestHook,
};
use note_storage::{
    BackendInfo, NewOrgWorkspace, StorageBackend, StorageResult, StorageSession,
    StorageTransaction, TransactionMode,
};
use note_storage_turso::TursoStorage;
use sqlx::postgres::PgPoolOptions;
use sqlx::AssertSqlSafe;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::Mutex;
use url::Url;

pub struct DeterministicTokenSource {
    values: Mutex<std::collections::VecDeque<String>>,
}

pub struct BeginBarrierStorageBackend {
    inner: Arc<dyn StorageBackend>,
    gate: Mutex<Option<BeginBarrierGate>>,
}

struct BeginBarrierGate {
    barrier: Arc<tokio::sync::Barrier>,
    remaining: usize,
}

impl BeginBarrierStorageBackend {
    pub fn new(inner: Arc<dyn StorageBackend>) -> Self {
        Self {
            inner,
            gate: Mutex::new(None),
        }
    }

    pub fn arm_immediate_pair(&self) {
        let mut gate = self.gate.lock().unwrap();
        assert!(gate.is_none(), "begin barrier is already armed");
        *gate = Some(BeginBarrierGate {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            remaining: 2,
        });
    }
}

#[async_trait::async_trait]
impl StorageBackend for BeginBarrierStorageBackend {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
        self.inner.session().await
    }

    async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>> {
        let barrier = if mode == TransactionMode::Immediate {
            let mut gate = self.gate.lock().unwrap();
            let barrier = gate.as_ref().map(|state| state.barrier.clone());
            let disarm = gate.as_mut().is_some_and(|state| {
                state.remaining -= 1;
                state.remaining == 0
            });
            if disarm {
                *gate = None;
            }
            barrier
        } else {
            None
        };
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
        self.inner.begin(mode).await
    }

    async fn info(&self) -> StorageResult<BackendInfo> {
        self.inner.info().await
    }
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
        OrgContext::new(backend.clone(), Arc::new(FixedOrgClock::new(now)))
            .with_cursor_signer(Arc::new(FixedOrgCursorSigner::new([0x5a; 32]))),
        backend,
        dir,
        db_path,
        workspace_id,
    )
}

pub struct PgAcceptanceDatabase {
    url: String,
    admin_url: String,
    database_name: String,
}

impl PgAcceptanceDatabase {
    pub async fn provision(test_name: &str) -> Option<Self> {
        let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("skipping {test_name}: TEST_DATABASE_URL is not set");
            return None;
        };
        let database_name = format!("agent_note_test_{}", uuid::Uuid::new_v4().simple());
        let url = derived_database_url(&admin_url, &database_name);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connect to PostgreSQL administrative database");
        sqlx::query(AssertSqlSafe(format!(
            r#"CREATE DATABASE "{database_name}""#
        )))
        .execute(&admin)
        .await
        .expect("create isolated PostgreSQL acceptance database");
        admin.close().await;

        let provisioned = async {
            let isolated = PgPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .map_err(|error| error.to_string())?;
            let result = sqlx::query("CREATE EXTENSION vector")
                .execute(&isolated)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            isolated.close().await;
            result
        }
        .await;
        if let Err(error) = provisioned {
            let cleanup = force_drop_database(&admin_url, &database_name).await;
            panic!(
                "provision isolated PostgreSQL acceptance database: {error}; cleanup: {cleanup:?}"
            );
        }

        Some(Self {
            url,
            admin_url,
            database_name,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn cleanup(self) -> Result<(), String> {
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .map_err(|error| error.to_string())?;
        let connection_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM pg_stat_activity WHERE datname = $1")
                .bind(&self.database_name)
                .fetch_one(&admin)
                .await
                .map_err(|error| error.to_string())?;
        sqlx::query(AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.database_name
        )))
        .execute(&admin)
        .await
        .map_err(|error| error.to_string())?;
        admin.close().await;
        if connection_count == 0 {
            Ok(())
        } else {
            Err(format!(
                "isolated PostgreSQL acceptance database retained {connection_count} connection(s)"
            ))
        }
    }
}

async fn force_drop_database(admin_url: &str, database_name: &str) -> Result<(), String> {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .map_err(|error| error.to_string())?;
    sqlx::query(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(&admin)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{database_name}" WITH (FORCE)"#
    )))
    .execute(&admin)
    .await
    .map_err(|error| error.to_string())?;
    admin.close().await;
    Ok(())
}

fn derived_database_url(admin_url: &str, database_name: &str) -> String {
    let mut parsed = Url::parse(admin_url).expect("TEST_DATABASE_URL must be a valid URL");
    let query_pairs = parsed
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "dbname" {
                database_name.to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    parsed.set_path(&format!("/{database_name}"));
    parsed.set_fragment(None);
    if query_pairs.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(query_pairs);
    }
    parsed.to_string()
}
