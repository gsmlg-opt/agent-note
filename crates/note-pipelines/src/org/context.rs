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

#[derive(Clone)]
pub struct OrgContext {
    storage: Arc<dyn StorageBackend>,
    clock: Arc<dyn OrgClock>,
}

impl OrgContext {
    pub fn new(storage: Arc<dyn StorageBackend>, clock: Arc<dyn OrgClock>) -> Self {
        Self { storage, clock }
    }

    pub(crate) fn storage(&self) -> &dyn StorageBackend {
        self.storage.as_ref()
    }

    pub fn clock(&self) -> &dyn OrgClock {
        self.clock.as_ref()
    }
}
