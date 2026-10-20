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

    let tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    for index in 0..96 {
        let key = format!("audit-contract-{index:03}");
        let value = serde_json::json!({"event":{"event_id":key,"version":1}});
        tx.put_sync_record("member", &key, &value).await.unwrap();
        if index % 3 == 0 {
            tx.put_sync_record("pending", &key, &value).await.unwrap();
        }
    }
    // A different pending event must not hide the captured member from audit.
    tx.put_sync_record(
        "pending",
        "audit-contract-001",
        &serde_json::json!({"event":{"event_id":"later-event","version":2}}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut after = None;
    let mut audit = Vec::new();
    loop {
        let page = session
            .list_sync_audit_records(after.as_deref(), 7)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 7);
        after = page.last().map(|row| row.0.clone());
        audit.extend(page.into_iter().map(|row| row.0));
    }
    assert_eq!(audit.len(), 64);
    assert!(audit.contains(&"audit-contract-001".into()));
    assert!(audit
        .iter()
        .all(|key| key.rsplit('-').next().unwrap().parse::<usize>().unwrap() % 3 != 0));
    assert!(audit.windows(2).all(|pair| pair[0] < pair[1]));
}
