use super::{OrgError, OrgErrorCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use note_storage::StorageBackend;
use sha2::Sha256;
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

/// Signs opaque cursor state for integrity only. This is not an authentication
/// or authorization boundary.
pub trait OrgCursorSigner: Send + Sync {
    fn sign(&self, payload: &[u8]) -> Result<[u8; 32], OrgError>;
    fn verify(&self, payload: &[u8], signature: &[u8]) -> Result<bool, OrgError>;
}

struct HmacOrgCursorSigner {
    key: [u8; 32],
}

impl HmacOrgCursorSigner {
    fn process_random() -> Result<Self, ()> {
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key).map_err(|_| ())?;
        Ok(Self { key })
    }
}

impl OrgCursorSigner for HmacOrgCursorSigner {
    fn sign(&self, payload: &[u8]) -> Result<[u8; 32], OrgError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| cursor_key_error())?;
        mac.update(payload);
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify(&self, payload: &[u8], signature: &[u8]) -> Result<bool, OrgError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| cursor_key_error())?;
        mac.update(payload);
        Ok(mac.verify_slice(signature).is_ok())
    }
}

/// Deterministic signer for tests and explicitly controlled composition roots.
/// The key is intentionally absent from Debug and serialization surfaces.
pub struct FixedOrgCursorSigner(HmacOrgCursorSigner);

impl FixedOrgCursorSigner {
    pub fn new(key: [u8; 32]) -> Self {
        Self(HmacOrgCursorSigner { key })
    }
}

impl OrgCursorSigner for FixedOrgCursorSigner {
    fn sign(&self, payload: &[u8]) -> Result<[u8; 32], OrgError> {
        self.0.sign(payload)
    }

    fn verify(&self, payload: &[u8], signature: &[u8]) -> Result<bool, OrgError> {
        self.0.verify(payload, signature)
    }
}

struct UnavailableOrgCursorSigner;

impl OrgCursorSigner for UnavailableOrgCursorSigner {
    fn sign(&self, _payload: &[u8]) -> Result<[u8; 32], OrgError> {
        Err(cursor_key_error())
    }

    fn verify(&self, _payload: &[u8], _signature: &[u8]) -> Result<bool, OrgError> {
        Err(cursor_key_error())
    }
}

fn cursor_key_error() -> OrgError {
    OrgError::new(
        OrgErrorCode::StorageFailure,
        "Org cursor integrity key is unavailable",
        serde_json::json!({}),
        true,
    )
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
    ProjectionUpdate,
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
    cursor_signer: Arc<dyn OrgCursorSigner>,
}

impl OrgContext {
    /// Creates a context with an ephemeral cursor-integrity key.
    ///
    /// Cursors issued by this context are valid only for the lifetime of the
    /// server process that owns it and are intentionally invalid after restart.
    pub fn new(storage: Arc<dyn StorageBackend>, clock: Arc<dyn OrgClock>) -> Self {
        // The default key is intentionally ephemeral: cursors are scoped to
        // this server process and become invalid after restart.
        let cursor_signer: Arc<dyn OrgCursorSigner> = HmacOrgCursorSigner::process_random()
            .map(|signer| Arc::new(signer) as Arc<dyn OrgCursorSigner>)
            .unwrap_or_else(|_| Arc::new(UnavailableOrgCursorSigner));
        Self {
            storage,
            clock,
            token_source: Arc::new(SystemOrgTokenSource),
            claim_test_hook: Arc::new(NoopOrgClaimTestHook),
            workflow_test_hook: Arc::new(NoopOrgWorkflowTestHook),
            cursor_signer,
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

    /// Replaces the ephemeral signer, primarily for deterministic tests or a
    /// composition root that explicitly manages cursor-key lifetime.
    pub fn with_cursor_signer(mut self, signer: Arc<dyn OrgCursorSigner>) -> Self {
        self.cursor_signer = signer;
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

    pub(crate) fn cursor_signer(&self) -> &dyn OrgCursorSigner {
        self.cursor_signer.as_ref()
    }

    pub(crate) fn after_claim_phase(&self, phase: OrgClaimPhase) -> Result<(), OrgError> {
        self.claim_test_hook.after_phase(phase)
    }

    pub(crate) fn after_workflow_phase(&self, phase: OrgWorkflowPhase) -> Result<(), OrgError> {
        self.workflow_test_hook.after_phase(phase)
    }
}
