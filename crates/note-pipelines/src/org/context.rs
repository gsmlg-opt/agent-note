use super::{OrgError, OrgErrorCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use note_storage::StorageBackend;
use std::sync::Arc;

pub trait OrgClock: Send + Sync {
    fn now(&self) -> i64;
}

#[derive(Debug)]
pub struct FixedOrgClock {
    now: i64,
}

impl FixedOrgClock {
    pub fn new(now: i64) -> Self {
        Self { now }
    }
}

impl OrgClock for FixedOrgClock {
    fn now(&self) -> i64 {
        self.now
    }
}

#[derive(Debug, Default)]
pub struct SystemOrgClock;

impl OrgClock for SystemOrgClock {
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

pub trait OrgTokenSource: Send + Sync {
    fn generate_token(&self) -> Result<String, OrgError>;
}

#[derive(Debug, Default)]
pub struct SystemOrgTokenSource;

impl OrgTokenSource for SystemOrgTokenSource {
    fn generate_token(&self) -> Result<String, OrgError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| {
            OrgError::new(
                OrgErrorCode::StorageFailure,
                "Secure Org fencing token generation failed",
                serde_json::json!({}),
                true,
            )
        })?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgClaimPhase {
    SourceEdit,
    AttemptAllocation,
    LeaseInsert,
    Events,
    ContextHydration,
    OperationWrite,
}

pub trait OrgClaimTestHook: Send + Sync {
    fn after_phase(&self, phase: OrgClaimPhase) -> Result<(), OrgError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgWorkflowPhase {
    Proof,
    SourceEdit,
    AttemptUpdate,
    LeaseUpdate,
    Events,
    ContextHydration,
    OperationWrite,
}

pub trait OrgWorkflowTestHook: Send + Sync {
    fn after_phase(&self, phase: OrgWorkflowPhase) -> Result<(), OrgError>;
}

#[derive(Debug, Default)]
struct NoopOrgWorkflowTestHook;

impl OrgWorkflowTestHook for NoopOrgWorkflowTestHook {
    fn after_phase(&self, _phase: OrgWorkflowPhase) -> Result<(), OrgError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct NoopOrgClaimTestHook;

impl OrgClaimTestHook for NoopOrgClaimTestHook {
    fn after_phase(&self, _phase: OrgClaimPhase) -> Result<(), OrgError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct OrgContext {
    storage: Arc<dyn StorageBackend>,
    clock: Arc<dyn OrgClock>,
    token_source: Arc<dyn OrgTokenSource>,
    claim_test_hook: Arc<dyn OrgClaimTestHook>,
    workflow_test_hook: Arc<dyn OrgWorkflowTestHook>,
}

impl OrgContext {
    pub fn new(storage: Arc<dyn StorageBackend>, clock: Arc<dyn OrgClock>) -> Self {
        Self {
            storage,
            clock,
            token_source: Arc::new(SystemOrgTokenSource),
            claim_test_hook: Arc::new(NoopOrgClaimTestHook),
            workflow_test_hook: Arc::new(NoopOrgWorkflowTestHook),
        }
    }

    pub fn with_token_source(mut self, token_source: Arc<dyn OrgTokenSource>) -> Self {
        self.token_source = token_source;
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn OrgClock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_claim_test_hook(mut self, hook: Arc<dyn OrgClaimTestHook>) -> Self {
        self.claim_test_hook = hook;
        self
    }

    pub fn with_workflow_test_hook(mut self, hook: Arc<dyn OrgWorkflowTestHook>) -> Self {
        self.workflow_test_hook = hook;
        self
    }

    pub(crate) fn storage(&self) -> &dyn StorageBackend {
        self.storage.as_ref()
    }

    pub fn clock(&self) -> &dyn OrgClock {
        self.clock.as_ref()
    }

    pub(crate) fn token_source(&self) -> &dyn OrgTokenSource {
        self.token_source.as_ref()
    }

    pub(crate) fn after_claim_phase(&self, phase: OrgClaimPhase) -> Result<(), OrgError> {
        self.claim_test_hook.after_phase(phase)
    }

    pub(crate) fn after_workflow_phase(&self, phase: OrgWorkflowPhase) -> Result<(), OrgError> {
        self.workflow_test_hook.after_phase(phase)
    }
}
