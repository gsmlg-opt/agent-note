use note_attachments::{
    AttachmentStore, BoundedReadError, DeleteObjectOutcome, PutObjectRequest, S3AttachmentConfig,
    S3AttachmentStore,
};

struct Fixture {
    store: S3AttachmentStore,
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

async fn fixture() -> Option<Fixture> {
    let endpoint = std::env::var("NOTE_TEST_MINIO_ENDPOINT").ok()?;
    let bucket =
        std::env::var("NOTE_TEST_MINIO_BUCKET").unwrap_or_else(|_| "agent-note-tests".into());
    let prefix = format!("tests/{}", uuid::Uuid::new_v4());
    let store = S3AttachmentStore::new(S3AttachmentConfig {
        bucket: bucket.clone(),
        prefix: prefix.clone(),
        region: Some("us-east-1".into()),
        endpoint: Some(endpoint.clone()),
        force_path_style: true,
    })
    .await
    .unwrap();
    let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new("us-east-1"))
        .load()
        .await;
    let client = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::config::Builder::from(&shared)
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    );
    Some(Fixture {
        store,
        client,
        bucket,
        prefix,
    })
}

#[tokio::test]
async fn immutable_object_lifecycle_round_trips_and_deletes_exact_key() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
    let stored = fixture
        .store
        .put_immutable(PutObjectRequest {
            object_key: "objects/01/file.bin".into(),
            bytes: b"payload".to_vec(),
            checksum_sha256: checksum.into(),
        })
        .await
        .unwrap();

    assert_eq!(stored.object_key, "objects/01/file.bin");
    assert_eq!(
        fixture.store.read_object(&stored.object_key).await.unwrap(),
        b"payload"
    );
    assert_eq!(
        fixture
            .store
            .read_object_bounded(&stored.object_key, 7)
            .await
            .unwrap(),
        b"payload"
    );
    let error = fixture
        .store
        .read_object_bounded(&stored.object_key, 6)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoundedReadError>(),
        Some(&BoundedReadError::LimitExceeded)
    );
    assert_eq!(
        fixture
            .store
            .head_object(&stored.object_key)
            .await
            .unwrap()
            .size_bytes,
        7
    );
    assert_eq!(
        fixture
            .store
            .delete_object(&stored.object_key)
            .await
            .unwrap(),
        DeleteObjectOutcome::Deleted
    );
    assert_eq!(
        fixture
            .store
            .delete_object(&stored.object_key)
            .await
            .unwrap(),
        DeleteObjectOutcome::AlreadyAbsent
    );
}

#[tokio::test]
async fn legacy_read_and_delete_target_only_the_legacy_key() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let key = format!("{}/note-1/nested/legacy.txt", fixture.prefix);
    fixture
        .client
        .put_object()
        .bucket(&fixture.bucket)
        .key(&key)
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b"legacy"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        fixture
            .store
            .read_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        b"legacy"
    );
    assert_eq!(
        fixture
            .store
            .read_legacy_bounded("note-1", "nested/legacy.txt", 6)
            .await
            .unwrap(),
        b"legacy"
    );
    let error = fixture
        .store
        .read_legacy_bounded("note-1", "nested/legacy.txt", 5)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoundedReadError>(),
        Some(&BoundedReadError::LimitExceeded)
    );
    assert_eq!(
        fixture
            .store
            .delete_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        DeleteObjectOutcome::Deleted
    );
    assert_eq!(
        fixture
            .store
            .delete_legacy("note-1", "nested/legacy.txt")
            .await
            .unwrap(),
        DeleteObjectOutcome::AlreadyAbsent
    );
    assert!(fixture
        .store
        .delete_legacy("../escape", "file")
        .await
        .is_err());
}
