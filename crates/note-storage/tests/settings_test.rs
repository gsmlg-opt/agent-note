use note_core::{DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SystemConfig};
use note_storage::{get_system_config, set_system_config, Storage};

#[tokio::test]
async fn system_config_defaults_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    assert_eq!(
        get_system_config(&conn).await.unwrap(),
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
    set_system_config(&conn, &config).await.unwrap();
    drop(conn);
    drop(storage);

    let reopened = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    assert_eq!(
        get_system_config(&reopened.connect().unwrap())
            .await
            .unwrap(),
        config
    );
}

#[tokio::test]
async fn storage_reports_absolute_database_path_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();

    assert_eq!(storage.path(), path);
    assert!(storage.size_bytes().await.unwrap() > 0);
}
