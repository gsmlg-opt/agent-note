use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SearchConfig, SystemConfig,
};
use note_storage::StorageBackend;
use std::sync::Arc;

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    assert_eq!(
        session.get_system_config().await.unwrap(),
        SystemConfig::default()
    );
    let config = SystemConfig {
        sync: Default::default(),
        category_labels: Vec::new(),
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: vec![
                    DuplicateCheckTerm {
                        key: "skill-name".into(),
                        value: None,
                    },
                    DuplicateCheckTerm {
                        key: "kind".into(),
                        value: Some("skill".into()),
                    },
                ],
            }],
        },
        search: SearchConfig {
            minimum_score: 0.025,
        },
    };
    session.set_system_config(&config).await.unwrap();
    assert_eq!(session.get_system_config().await.unwrap(), config);
}

/// Replication records share the exact business transaction, with deterministic key pagination.
pub async fn run_sync_records(storage: Arc<dyn StorageBackend>) {
    use note_storage::TransactionMode;
    let tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record("sync-contract", "a", &serde_json::json!({"value":1}))
        .await
        .unwrap();
    tx.put_sync_record("sync-contract", "b", &serde_json::json!({"value":2}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let session = storage.session().await.unwrap();
    assert_eq!(
        session
            .list_sync_records("sync-contract", None, 1)
            .await
            .unwrap(),
        vec![("a".into(), serde_json::json!({"value":1}))]
    );
    assert_eq!(
        session
            .list_sync_records("sync-contract", Some("a"), 1)
            .await
            .unwrap(),
        vec![("b".into(), serde_json::json!({"value":2}))]
    );
    let tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    tx.put_sync_record("sync-contract", "a", &serde_json::json!({"value":3}))
        .await
        .unwrap();
    tx.delete_sync_record("sync-contract", "b").await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        session.get_sync_record("sync-contract", "a").await.unwrap(),
        Some(serde_json::json!({"value":1}))
    );
    assert!(session
        .get_sync_record("sync-contract", "b")
        .await
        .unwrap()
        .is_some());
    assert!(session
        .get_sync_record("other-sync-contract", "a")
        .await
        .unwrap()
        .is_none());
}
