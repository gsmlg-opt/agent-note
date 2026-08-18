mod attachment_operations;
mod bulk_labels;
mod embedding;
mod notes;
mod org;
mod retrieval;
mod settings;
mod transactions;

use note_storage::StorageBackend;
use std::sync::Arc;

/// Runs the contracts once against a fresh database.
///
/// Fixed IDs and global settings are intentional, so the backend must not
/// contain prior data and this function must not be called twice on it.
pub async fn run_storage_contracts(storage: Arc<dyn StorageBackend>) {
    notes::run(storage.clone()).await;
    attachment_operations::run(storage.clone()).await;
    org::run(storage.clone()).await;
    embedding::run(storage.clone()).await;
    retrieval::run(storage.clone()).await;
    settings::run(storage.clone()).await;
    bulk_labels::run(storage.clone()).await;
    transactions::run(storage).await;
}

/// Runs barrier-driven lease uniqueness and workspace-capacity races against a
/// fresh backend database.
pub async fn run_org_claim_races(storage: Arc<dyn StorageBackend>) {
    transactions::run_org_claim_races(storage).await;
}

/// Runs the proof-only active lease validation contract against a fresh
/// backend database.
pub async fn run_org_lease_proof_validation(storage: Arc<dyn StorageBackend>) {
    transactions::run_org_lease_proof_validation(storage).await;
}

/// Proves that workflow-specific event names round-trip through backend decoding.
pub async fn run_org_workflow_event_decoding(storage: Arc<dyn StorageBackend>) {
    org::run_org_workflow_event_decoding(storage).await;
}

pub(crate) fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    vector[axis] = 1.0;
    vector
}
