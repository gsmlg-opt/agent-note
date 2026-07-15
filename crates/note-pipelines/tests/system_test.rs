use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SystemConfig,
    SystemConfigValidationError,
};
use note_embedding::StubEmbedder;
use note_pipelines::{get_system_config, get_system_info, update_system_config, Context};
use note_storage::Storage;
use std::sync::Arc;

#[tokio::test]
async fn config_roundtrips_and_system_info_reports_storage() {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("test.db");
    let attachments_path = dir.path().join("attachments");
    let storage = Storage::open_local(database_path.to_str().unwrap())
        .await
        .unwrap();
    let ctx =
        Context::with_attachment_dir(Arc::new(storage), Arc::new(StubEmbedder), &attachments_path);
    let config = SystemConfig {
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: vec![DuplicateCheckTerm {
                    key: "version".to_string(),
                    value: None,
                }],
            }],
        },
    };

    update_system_config(&ctx, &config).await.unwrap();
    assert_eq!(get_system_config(&ctx).await.unwrap(), config);

    let info = get_system_info(&ctx).await.unwrap();
    assert_eq!(info.database_path, database_path.to_string_lossy());
    assert_eq!(info.attachments_path, attachments_path.to_string_lossy());
    assert!(info.database_size_bytes > 0);
}

#[tokio::test]
async fn invalid_config_is_rejected_before_storage() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));
    let config = SystemConfig {
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule { terms: vec![] }],
        },
    };

    let error = update_system_config(&ctx, &config).await.unwrap_err();
    assert!(error
        .downcast_ref::<SystemConfigValidationError>()
        .is_some());
    assert_eq!(
        get_system_config(&ctx).await.unwrap(),
        SystemConfig::default()
    );
}
