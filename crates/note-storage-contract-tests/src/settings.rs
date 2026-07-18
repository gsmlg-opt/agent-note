use note_core::{DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SystemConfig};
use note_storage::StorageBackend;
use std::sync::Arc;

pub(crate) async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    assert_eq!(
        session.get_system_config().await.unwrap(),
        SystemConfig::default()
    );
    let config = SystemConfig {
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
    };
    session.set_system_config(&config).await.unwrap();
    assert_eq!(session.get_system_config().await.unwrap(), config);
}
