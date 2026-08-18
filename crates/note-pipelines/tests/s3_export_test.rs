use std::sync::Arc;
use std::time::Duration;

use note_attachments::{AttachmentStore, S3AttachmentConfig, S3AttachmentStore};
use note_embedding::StubEmbedder;
use note_pipelines::{delete_note, export_json, import_json, permanently_delete_note, Context};
use note_storage::StorageBackend;

async fn s3_context() -> Option<(Context, S3AttachmentStore, tempfile::TempDir)> {
    let Ok(endpoint) = std::env::var("NOTE_TEST_MINIO_ENDPOINT") else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("notes.db"))
            .await
            .unwrap(),
    );
    let s3 = S3AttachmentStore::new(S3AttachmentConfig {
        bucket: std::env::var("NOTE_TEST_MINIO_BUCKET")
            .unwrap_or_else(|_| "agent-note-tests".into()),
        prefix: format!("pipeline-tests/{}", uuid::Uuid::new_v4()),
        region: Some("us-east-1".into()),
        endpoint: Some(endpoint),
        force_path_style: true,
    })
    .await
    .unwrap();
    let attachments: Arc<dyn AttachmentStore> = Arc::new(s3.clone());
    let ctx = Context::new(storage, Arc::new(StubEmbedder), attachments);
    Some((ctx, s3, dir))
}

#[tokio::test]
async fn json_round_trip_uses_the_selected_s3_attachment_store() {
    let Some((ctx, s3, _dir)) = s3_context().await else {
        return;
    };
    let input = r#"{
      "version": 2,
      "label_keys": [],
      "notes": [{
        "id": "imported-note",
        "title": "Imported",
        "content": "Body",
        "attachments": [{
          "id": "proof",
          "path": "./proof.bin",
          "mime": "application/octet-stream",
          "description": "binary proof",
          "content_base64": "AJ+Slv8="
        }],
        "created_at": 1000,
        "updated_at": 1000,
        "labels": []
      }]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();
    assert_eq!(stats.notes_added, 1);
    let exported = export_json(&ctx).await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(
        json["notes"][0]["attachments"][0]["content_base64"],
        "AJ+Slv8="
    );
    assert!(delete_note(&ctx, "imported-note", 1).await.unwrap());
    assert!(permanently_delete_note(&ctx, "imported-note", 2)
        .await
        .unwrap());
    assert!(s3.read("imported-note", "./proof.bin").await.is_err());
}

#[tokio::test]
async fn import_can_hold_two_prepared_s3_attachment_sets() {
    let Some((ctx, s3, _dir)) = s3_context().await else {
        return;
    };
    let input = r#"{
      "version": 2,
      "label_keys": [],
      "notes": [
        {
          "id": "note-a",
          "title": "A",
          "content": "First",
          "attachments": [{
            "id": "a",
            "path": "./a.bin",
            "mime": "application/octet-stream",
            "description": "first",
            "content_base64": "AP8="
          }],
          "created_at": 1000,
          "updated_at": 1000,
          "labels": []
        },
        {
          "id": "note-b",
          "title": "B",
          "content": "Second",
          "attachments": [{
            "id": "b",
            "path": "./b.bin",
            "mime": "application/octet-stream",
            "description": "second",
            "content_base64": "gP4="
          }],
          "created_at": 1001,
          "updated_at": 1001,
          "labels": []
        }
      ]
    }"#;

    let stats = tokio::time::timeout(Duration::from_secs(5), import_json(&ctx, input))
        .await
        .expect("two-note S3 import must not deadlock")
        .unwrap();
    assert_eq!(stats.notes_added, 2);
    assert_eq!(s3.read("note-a", "./a.bin").await.unwrap(), vec![0, 255]);
    assert_eq!(s3.read("note-b", "./b.bin").await.unwrap(), vec![128, 254]);

    let exported = export_json(&ctx).await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&exported).unwrap();
    let notes = json["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2);
    let note_a = notes
        .iter()
        .find(|note| note["id"] == "note-a")
        .expect("note-a must be exported");
    let note_b = notes
        .iter()
        .find(|note| note["id"] == "note-b")
        .expect("note-b must be exported");
    assert_eq!(
        note_a["attachments"][0]["content_base64"],
        serde_json::Value::String("AP8=".into())
    );
    assert_eq!(
        note_b["attachments"][0]["content_base64"],
        serde_json::Value::String("gP4=".into())
    );

    for note_id in ["note-a", "note-b"] {
        assert!(delete_note(&ctx, note_id, 1).await.unwrap());
        assert!(permanently_delete_note(&ctx, note_id, 2).await.unwrap());
    }
    assert!(s3.read("note-a", "./a.bin").await.is_err());
    assert!(s3.read("note-b", "./b.bin").await.is_err());
}
