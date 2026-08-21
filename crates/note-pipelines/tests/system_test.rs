mod support;

use note_core::{
    CategoryLabelConfigError, DuplicateCheckConfig, DuplicateCheckRule, DuplicateCheckTerm,
    SearchConfig, SystemConfig, SystemConfigValidationError,
};
use note_pipelines::{
    category_label_summaries, define_label_key, delete_label_key, get_system_config,
    get_system_info, list_label_keys, update_system_config, CategoryLabelSummary,
    CategoryLabelValueSummary,
};
use note_storage::NewNote;
use support::test_context;

#[tokio::test]
async fn config_roundtrips_and_system_info_reports_storage() {
    let (ctx, _backend, dir) = test_context().await;
    let database_path = dir.path().join("test.db");
    let attachments_path = dir.path().join("attachments");
    let config = SystemConfig {
        category_labels: Vec::new(),
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule {
                terms: vec![DuplicateCheckTerm {
                    key: "version".to_string(),
                    value: None,
                }],
            }],
        },
        search: SearchConfig::default(),
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
    assert_eq!(info.embedding_engine, "local");
    assert_eq!(info.embedding_model, "bge-m3");
    assert_eq!(info.embedding_fingerprint, "bge-m3:1024");
    assert!(info.database_size_bytes.unwrap() > 0);
}

#[tokio::test]
async fn invalid_config_is_rejected_before_storage() {
    let (ctx, _backend, _dir) = test_context().await;
    let config = SystemConfig {
        category_labels: Vec::new(),
        duplicate_check: DuplicateCheckConfig {
            enabled: true,
            rules: vec![DuplicateCheckRule { terms: vec![] }],
        },
        search: SearchConfig::default(),
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

#[tokio::test]
async fn category_keys_must_exist_and_configured_keys_cannot_be_deleted() {
    let (ctx, _backend, _dir) = test_context().await;
    let configured = SystemConfig {
        category_labels: vec!["project".to_string()],
        search: SearchConfig::default(),
        ..SystemConfig::default()
    };

    let error = update_system_config(&ctx, &configured).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<CategoryLabelConfigError>(),
        Some(&CategoryLabelConfigError::UnknownKey {
            key: "project".to_string(),
        })
    );
    assert_eq!(
        get_system_config(&ctx).await.unwrap(),
        SystemConfig::default()
    );

    define_label_key(&ctx, "project", "Project").await.unwrap();
    update_system_config(&ctx, &configured).await.unwrap();

    let error = delete_label_key(&ctx, "project").await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<CategoryLabelConfigError>(),
        Some(&CategoryLabelConfigError::ConfiguredKeyCannotBeDeleted {
            key: "project".to_string(),
        })
    );

    update_system_config(&ctx, &SystemConfig::default())
        .await
        .unwrap();
    delete_label_key(&ctx, "project").await.unwrap();
    assert!(list_label_keys(&ctx).await.unwrap().is_empty());
}

#[tokio::test]
async fn category_summaries_preserve_config_order_and_include_empty_categories() {
    let (ctx, backend, _dir) = test_context().await;
    define_label_key(&ctx, "project", "Project").await.unwrap();
    define_label_key(&ctx, "team", "Owning team").await.unwrap();
    define_label_key(&ctx, "empty-category", "Empty category")
        .await
        .unwrap();

    let session = backend.session().await.unwrap();
    for (index, id, labels) in [
        (
            1,
            "category-a",
            vec![("project", "yellow-dog"), ("team", "platform")],
        ),
        (2, "category-b", vec![("project", "yellow-dog")]),
        (3, "category-c", vec![("project", "sigma")]),
        (4, "category-deleted", vec![("project", "archived-only")]),
    ] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content: id,
                attachments: &[],
                created_at: index,
                updated_at: index,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
        for (key, value) in labels {
            session.attach_label(id, key, value).await.unwrap();
        }
    }
    session
        .soft_delete_note("category-deleted", 1, 5)
        .await
        .unwrap();

    update_system_config(
        &ctx,
        &SystemConfig {
            category_labels: vec![
                "team".to_string(),
                "empty-category".to_string(),
                "project".to_string(),
            ],
            search: SearchConfig::default(),
            ..SystemConfig::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        category_label_summaries(&ctx).await.unwrap(),
        vec![
            CategoryLabelSummary {
                key: "team".to_string(),
                description: "Owning team".to_string(),
                values: vec![CategoryLabelValueSummary {
                    value: "platform".to_string(),
                    count: 1,
                }],
            },
            CategoryLabelSummary {
                key: "empty-category".to_string(),
                description: "Empty category".to_string(),
                values: Vec::new(),
            },
            CategoryLabelSummary {
                key: "project".to_string(),
                description: "Project".to_string(),
                values: vec![
                    CategoryLabelValueSummary {
                        value: "yellow-dog".to_string(),
                        count: 2,
                    },
                    CategoryLabelValueSummary {
                        value: "sigma".to_string(),
                        count: 1,
                    },
                ],
            },
        ]
    );
}

#[tokio::test]
async fn no_configured_categories_returns_an_empty_summary() {
    let (ctx, _backend, _dir) = test_context().await;

    assert_eq!(category_label_summaries(&ctx).await.unwrap(), Vec::new());
}

#[tokio::test]
async fn category_summaries_reject_stale_configured_keys() {
    let (ctx, backend, _dir) = test_context().await;
    let session = backend.session().await.unwrap();
    session
        .set_system_config(&SystemConfig {
            category_labels: vec!["missing".to_string()],
            search: SearchConfig::default(),
            ..SystemConfig::default()
        })
        .await
        .unwrap();

    let error = category_label_summaries(&ctx).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<CategoryLabelConfigError>(),
        Some(&CategoryLabelConfigError::UnknownKey {
            key: "missing".to_string(),
        })
    );
}

#[tokio::test]
async fn category_summaries_reject_duplicate_keys_from_stored_config() {
    let (ctx, backend, _dir) = test_context().await;
    define_label_key(&ctx, "project", "Project").await.unwrap();
    let session = backend.session().await.unwrap();
    session
        .set_system_config(&SystemConfig {
            category_labels: vec!["project".to_string(), "project".to_string()],
            search: SearchConfig::default(),
            ..SystemConfig::default()
        })
        .await
        .unwrap();

    let error = category_label_summaries(&ctx).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<SystemConfigValidationError>(),
        Some(&SystemConfigValidationError::DuplicateCategoryLabel {
            key: "project".to_string(),
        })
    );

    let valid = SystemConfig {
        category_labels: vec!["project".to_string()],
        search: SearchConfig::default(),
        ..SystemConfig::default()
    };
    update_system_config(&ctx, &valid).await.unwrap();
    assert_eq!(get_system_config(&ctx).await.unwrap(), valid);
}
