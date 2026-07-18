use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::Arc;

#[tokio::test]
async fn turso_satisfies_shared_storage_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("contracts.db"))
            .await
            .unwrap(),
    );

    note_storage_contract_tests::run_storage_contracts(storage).await;
}
