mod support;

use note_core::{DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SystemConfig};
use note_storage::SettingsRepository;
use note_storage_turso::TursoStorage;
use support::{fixture, Fixture};

#[tokio::test]
async fn system_config_defaults_and_persists() {
    let Fixture {
        _dir,
        storage,
        session,
    } = fixture().await;

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
                        key: "skill-name".to_string(),
                        value: None,
                    },
                    DuplicateCheckTerm {
                        key: "version".to_string(),
                        value: None,
                    },
                ],
            }],
        },
    };
    session.set_system_config(&config).await.unwrap();
    drop(session);
    drop(storage);

    let reopened = TursoStorage::open(_dir.path().join("test.db"))
        .await
        .unwrap();
    let session = reopened.connect().await.unwrap();
    assert_eq!(session.get_system_config().await.unwrap(), config);
}
