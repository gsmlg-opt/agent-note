mod support;

use note_storage::StorageBackend;
use std::sync::Arc;

#[tokio::test]
async fn postgresql_satisfies_shared_storage_contracts() {
    let Some(database) = support::TestDatabase::provision_with_vector(
        "postgresql_satisfies_shared_storage_contracts",
    )
    .await
    else {
        return;
    };
    let storage = Arc::new(
        note_storage_pg::PgStorage::connect(&database.url, 10)
            .await
            .unwrap(),
    );
    let backend: Arc<dyn StorageBackend> = storage.clone();

    note_storage_contract_tests::run_storage_contracts(backend).await;

    database.cleanup(Some(&storage)).await.unwrap();
}
