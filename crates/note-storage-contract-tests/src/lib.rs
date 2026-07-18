mod embedding;
mod notes;
mod retrieval;
mod settings;
mod transactions;

use note_storage::StorageBackend;
use std::sync::Arc;

pub async fn run_storage_contracts(storage: Arc<dyn StorageBackend>) {
    notes::run(storage.clone()).await;
    embedding::run(storage.clone()).await;
    retrieval::run(storage.clone()).await;
    settings::run(storage.clone()).await;
    transactions::run(storage).await;
}

pub(crate) fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    vector[axis] = 1.0;
    vector
}
