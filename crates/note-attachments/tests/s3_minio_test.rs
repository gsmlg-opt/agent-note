use note_attachments::{AttachmentStore, S3AttachmentConfig, S3AttachmentStore};
use note_core::NoteAttachment;

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

async fn list_keys(fixture: &Fixture, prefix: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut request = fixture
            .client
            .list_objects_v2()
            .bucket(&fixture.bucket)
            .prefix(prefix)
            .max_keys(1_000);
        if let Some(token) = continuation.as_deref() {
            request = request.continuation_token(token);
        }
        let output = request.send().await.unwrap();
        keys.extend(
            output
                .contents()
                .iter()
                .filter_map(|object| object.key())
                .map(ToOwned::to_owned),
        );
        if output.is_truncated().unwrap_or(false) {
            continuation = Some(output.next_continuation_token().unwrap().to_string());
        } else {
            return keys;
        }
    }
}

async fn wait_for_no_keys(fixture: &Fixture) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if list_keys(fixture, &format!("{}/", fixture.prefix))
                .await
                .is_empty()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("staging cleanup must finish");
}

fn attachment(path: &str, content: &[u8]) -> NoteAttachment {
    NoteAttachment {
        id: path.to_string(),
        path: path.to_string(),
        mime: "application/octet-stream".into(),
        description: "integration test".into(),
        content: content.to_vec(),
    }
}

#[tokio::test]
async fn prepare_returns_metadata_without_bytes_and_abort_removes_staging() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let prepared = fixture
        .store
        .prepare("note-1", &[attachment("./nested/file.txt", b"payload")])
        .await
        .unwrap();

    assert_eq!(prepared.metadata()[0].path, "./nested/file.txt");
    assert!(prepared.metadata()[0].content.is_empty());
    prepared.abort().await.unwrap();

    assert!(list_keys(&fixture, &format!("{}/", fixture.prefix))
        .await
        .is_empty());
}

#[tokio::test]
async fn unsafe_or_duplicate_paths_fail_before_any_upload() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    for path in ["../secret", "/absolute", r"C:\secret", "C:/secret"] {
        assert!(fixture
            .store
            .prepare("note-1", &[attachment(path, b"secret")])
            .await
            .is_err());
    }
    assert!(fixture
        .store
        .prepare(
            "note-1",
            &[
                attachment("./duplicate.txt", b"first"),
                attachment("duplicate.txt", b"second"),
            ],
        )
        .await
        .is_err());
    assert!(list_keys(&fixture, &format!("{}/", fixture.prefix))
        .await
        .is_empty());
}

#[tokio::test]
async fn direct_read_fetches_only_the_requested_object() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let prepared = fixture
        .store
        .prepare(
            "note-1",
            &[
                attachment("./wanted.txt", b"wanted"),
                attachment("./sibling.txt", b"sibling"),
            ],
        )
        .await
        .unwrap();
    prepared.publish().await.unwrap();
    fixture
        .client
        .delete_object()
        .bucket(&fixture.bucket)
        .key(format!("{}/note-1/sibling.txt", fixture.prefix))
        .send()
        .await
        .unwrap();

    assert_eq!(
        fixture.store.read("note-1", "./wanted.txt").await.unwrap(),
        b"wanted"
    );
    fixture.store.remove_note("note-1").await.unwrap();
    wait_for_no_keys(&fixture).await;
}

#[tokio::test]
async fn hydrate_reads_each_attachment_and_remove_note_deletes_final_objects() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let prepared = fixture
        .store
        .prepare(
            "note-1",
            &[
                attachment("./one.txt", b"one"),
                attachment("./two.txt", b"two"),
            ],
        )
        .await
        .unwrap();
    let mut metadata = prepared.metadata().to_vec();
    prepared.publish().await.unwrap();

    fixture
        .store
        .hydrate("note-1", &mut metadata)
        .await
        .unwrap();
    assert_eq!(metadata[0].content, b"one");
    assert_eq!(metadata[1].content, b"two");
    fixture.store.remove_note("note-1").await.unwrap();
    assert!(list_keys(&fixture, &format!("{}/", fixture.prefix))
        .await
        .is_empty());
}

#[tokio::test]
async fn same_note_preparation_waits_until_the_first_set_is_resolved() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let first = fixture
        .store
        .prepare("note-1", &[attachment("./first.txt", b"first")])
        .await
        .unwrap();
    let second_store = fixture.store.clone();
    let mut second = Box::pin(async move { second_store.prepare("note-1", &[]).await.unwrap() });

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), second.as_mut())
            .await
            .is_err()
    );
    first.abort().await.unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
        .await
        .unwrap();
    second.abort().await.unwrap();
}

#[tokio::test]
async fn different_note_sets_can_be_prepared_together_for_import() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let first = fixture.store.prepare("note-1", &[]).await.unwrap();
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.store.prepare("note-2", &[]),
    )
    .await
    .unwrap()
    .unwrap();
    first.abort().await.unwrap();
    second.abort().await.unwrap();
}

#[tokio::test]
async fn dropping_an_unresolved_set_eventually_removes_its_staging_objects() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let prepared = fixture
        .store
        .prepare("note-1", &[attachment("./file.txt", b"payload")])
        .await
        .unwrap();
    assert_eq!(
        list_keys(&fixture, &format!("{}/", fixture.prefix))
            .await
            .len(),
        1
    );

    drop(prepared);

    wait_for_no_keys(&fixture).await;
}

#[tokio::test]
async fn store_info_exposes_only_the_bucket_and_prefix() {
    let Some(fixture) = fixture().await else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
    };
    let info = fixture.store.info();
    assert_eq!(info.engine, "s3");
    assert_eq!(
        info.location,
        Some(format!("s3://{}/{}", fixture.bucket, fixture.prefix))
    );
}
