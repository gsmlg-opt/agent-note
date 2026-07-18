mod support;

use note_core::{
    DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm, SystemConfig,
    SystemConfigValidationError,
};
use note_pipelines::{get_system_config, get_system_info, update_system_config};
use support::test_context;

#[tokio::test]
async fn config_roundtrips_and_system_info_reports_storage() {
    let (ctx, _backend, dir) = test_context().await;
    let database_path = dir.path().join("test.db");
    let attachments_path = dir.path().join("attachments");
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
    assert_eq!(info.database_engine, "embed");
    assert_eq!(
        info.database_path.as_deref(),
        Some(database_path.to_string_lossy().as_ref())
    );
    assert_eq!(info.attachments_engine, "filesystem");
    assert_eq!(
        info.attachments_location.as_deref(),
        Some(attachments_path.to_string_lossy().as_ref())
    );
    assert!(info.database_size_bytes.unwrap() > 0);
}

#[tokio::test]
async fn invalid_config_is_rejected_before_storage() {
    let (ctx, _backend, _dir) = test_context().await;
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
