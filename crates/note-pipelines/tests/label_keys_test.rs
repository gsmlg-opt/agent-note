mod support;

use note_core::LabelValueType;
use note_pipelines::{define_label_key, define_label_key_with_type, list_label_keys};
use support::test_context;

#[tokio::test]
async fn define_then_list_roundtrips() {
    let (ctx, _backend, _dir) = test_context().await;
    define_label_key(&ctx, "status", "Workflow status")
        .await
        .unwrap();
    let keys = list_label_keys(&ctx).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].value_type, LabelValueType::Text);
}

#[tokio::test]
async fn define_with_type_roundtrips() {
    let (ctx, _backend, _dir) = test_context().await;
    define_label_key_with_type(&ctx, "version", "Release version", LabelValueType::Version)
        .await
        .unwrap();
    let keys = list_label_keys(&ctx).await.unwrap();
    assert_eq!(keys[0].value_type, LabelValueType::Version);
}

#[tokio::test]
async fn empty_description_is_allowed() {
    let (ctx, _backend, _dir) = test_context().await;
    define_label_key(&ctx, "status", "").await.unwrap();

    let keys = list_label_keys(&ctx).await.unwrap();
    assert_eq!(keys[0].description, "");
}
