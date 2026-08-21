mod support;

use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SearchConfig, SystemConfig,
};
use note_storage::{SettingsRepository, StorageErrorKind};
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
        category_labels: Vec::new(),
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
        search: SearchConfig::default(),
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

#[tokio::test]
async fn embedding_fingerprint_defaults_to_none_and_persists() {
    let Fixture {
        _dir,
        storage,
        session,
    } = fixture().await;

    assert_eq!(session.get_embedding_fingerprint().await.unwrap(), None);
    session
        .set_embedding_fingerprint("bge-m3:1024")
        .await
        .unwrap();
    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
    drop(session);
    drop(storage);

    let reopened = TursoStorage::open(_dir.path().join("test.db"))
        .await
        .unwrap();
    let session = reopened.connect().await.unwrap();
    assert_eq!(
        session
            .get_embedding_fingerprint()
            .await
            .unwrap()
            .as_deref(),
        Some("bge-m3:1024")
    );
}

#[tokio::test]
async fn embedding_fingerprint_rejects_blank_values() {
    let fixture = fixture().await;

    let error = fixture
        .session
        .set_embedding_fingerprint(" \t\n")
        .await
        .unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::Operation);
    assert_eq!(
        fixture.session.get_embedding_fingerprint().await.unwrap(),
        None
    );
}
