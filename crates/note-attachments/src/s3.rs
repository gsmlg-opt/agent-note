use aws_config::{retry::RetryConfig, BehaviorVersion, Region};
use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    Client,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex as StdMutex, Weak},
};
use tokio::sync::Mutex as AsyncMutex;

const S3_PAGE_SIZE: i32 = 1_000;
const S3_DELETE_BATCH_SIZE: usize = 1_000;
const S3_MAX_ATTEMPTS: u32 = 4;

#[derive(Clone, PartialEq, Eq)]
pub struct S3AttachmentConfig {
    pub bucket: String,
    pub prefix: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub force_path_style: bool,
}

#[derive(Clone)]
pub struct S3AttachmentStore {
    client: Client,
    bucket: String,
    prefix: String,
    endpoint_configured: bool,
    coordination: Arc<NoteCoordination>,
}

#[derive(Default)]
struct NoteCoordination {
    locks: StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

impl fmt::Debug for S3AttachmentConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("S3AttachmentConfig")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint.as_ref().map(|_| "<redacted>"))
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

impl fmt::Debug for S3AttachmentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoint = self.endpoint_configured.then_some("<redacted>");
        formatter
            .debug_struct("S3AttachmentStore")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("endpoint", &endpoint)
            .finish_non_exhaustive()
    }
}

impl NoteCoordination {
    fn lock_for(&self, note_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(note_id).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(note_id.to_owned(), Arc::downgrade(&lock));
        lock
    }
}

impl S3AttachmentStore {
    pub async fn new(config: S3AttachmentConfig) -> anyhow::Result<Self> {
        let bucket = config.bucket.trim().to_owned();
        if bucket.is_empty() {
            anyhow::bail!("S3 attachment bucket must not be blank");
        }
        let prefix = normalize_prefix(&config.prefix)?;
        let region = config
            .region
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let endpoint = config
            .endpoint
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(validate_endpoint)
            .transpose()?;

        let retry = RetryConfig::standard().with_max_attempts(S3_MAX_ATTEMPTS);
        let mut loader = aws_config::defaults(BehaviorVersion::latest()).retry_config(retry);
        if let Some(region) = region {
            loader = loader.region(Region::new(region));
        }
        let shared = loader.load().await;
        let mut service =
            aws_sdk_s3::config::Builder::from(&shared).force_path_style(config.force_path_style);
        service.set_endpoint_url(None);
        if let Some(endpoint) = endpoint.as_ref() {
            service = service.endpoint_url(endpoint.as_str());
        }

        Ok(Self {
            client: Client::from_conf(service.build()),
            bucket,
            prefix,
            endpoint_configured: endpoint.is_some(),
            coordination: Arc::new(NoteCoordination::default()),
        })
    }

    async fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|error| safe_sdk_error("put object", &error))?;
        Ok(())
    }
}

fn validate_endpoint(value: String) -> anyhow::Result<url::Url> {
    let endpoint = url::Url::parse(&value)
        .map_err(|_| anyhow::anyhow!("invalid S3 attachment endpoint URL"))?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        anyhow::bail!("invalid S3 attachment endpoint URL");
    }
    Ok(endpoint)
}

fn safe_sdk_error<E>(operation: &str, error: &SdkError<E>) -> anyhow::Error
where
    E: ProvideErrorMetadata,
{
    let category = match error {
        SdkError::ConstructionFailure(_) => "construction",
        SdkError::TimeoutError(_) => "timeout",
        SdkError::DispatchFailure(_) => "dispatch",
        SdkError::ResponseError(_) => "response",
        SdkError::ServiceError(_) => match error.code() {
            Some("AccessDenied") => "access-denied",
            Some("NoSuchBucket") => "missing-bucket",
            Some("NoSuchKey") => "missing-object",
            Some("SlowDown" | "Throttling") => "throttled",
            Some("InternalError" | "ServiceUnavailable") => "service-unavailable",
            _ => "service",
        },
        _ => "unknown",
    };
    anyhow::anyhow!("S3 {operation} failed ({category})")
}

fn normalize_prefix(prefix: &str) -> anyhow::Result<String> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    if prefix.contains('\\')
        || prefix
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        anyhow::bail!("invalid S3 attachment prefix");
    }
    Ok(prefix.to_string())
}

fn validate_note_id(note_id: &str) -> anyhow::Result<()> {
    if note_id.is_empty()
        || note_id == "."
        || note_id == ".."
        || note_id == ".staging"
        || note_id.contains('/')
        || note_id.contains('\\')
    {
        anyhow::bail!("invalid note id for attachment key");
    }
    Ok(())
}

fn join_key(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{prefix}/{suffix}")
    }
}

fn final_prefix(prefix: &str, note_id: &str) -> anyhow::Result<String> {
    validate_note_id(note_id)?;
    let prefix = normalize_prefix(prefix)?;
    Ok(format!("{}/", join_key(&prefix, note_id)))
}

fn final_key(prefix: &str, note_id: &str, path: &str) -> anyhow::Result<String> {
    let relative = crate::path::canonical_relative_path(path)?;
    Ok(format!("{}{relative}", final_prefix(prefix, note_id)?))
}

fn staging_prefix(prefix: &str, note_id: &str, stage_id: &str) -> anyhow::Result<String> {
    validate_note_id(note_id)?;
    validate_note_id(stage_id)?;
    let prefix = normalize_prefix(prefix)?;
    Ok(format!(
        "{}/",
        join_key(&prefix, &format!(".staging/{note_id}/{stage_id}"))
    ))
}

fn copy_source(bucket: &str, key: &str) -> String {
    std::iter::once(bucket)
        .chain(key.split('/'))
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    // AWS configuration is process-global, so the standard mutex intentionally
    // remains held for each complete asynchronous test operation.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::{
        collections::HashMap,
        ffi::OsString,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use wiremock::{
        matchers::{header_regex, method, path},
        Mock, MockServer, Request, Respond, ResponseTemplate,
    };

    static AWS_ENV_LOCK: Mutex<()> = Mutex::new(());
    const AWS_ENV_NAMES: [&str; 5] = [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_EC2_METADATA_DISABLED",
        "AWS_ENDPOINT_URL_S3",
    ];

    struct AwsEnvGuard {
        original: HashMap<&'static str, Option<OsString>>,
    }

    impl AwsEnvGuard {
        fn apply(values: &[(&'static str, Option<&str>)]) -> Self {
            let original = AWS_ENV_NAMES
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect();
            let values: HashMap<_, _> = values.iter().copied().collect();
            for name in AWS_ENV_NAMES {
                match values.get(name).copied().flatten() {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            Self { original }
        }
    }

    impl Drop for AwsEnvGuard {
        fn drop(&mut self) {
            for name in AWS_ENV_NAMES {
                match self.original.remove(name).flatten() {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[derive(Clone)]
    struct FailThenSucceed {
        calls: Arc<AtomicUsize>,
        failures: usize,
    }

    impl Respond for FailThenSucceed {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200)
            }
        }
    }

    async fn test_store(endpoint: String) -> S3AttachmentStore {
        S3AttachmentStore::new(S3AttachmentConfig {
            bucket: "agent-note".into(),
            prefix: String::new(),
            region: Some("us-east-1".into()),
            endpoint: Some(endpoint),
            force_path_style: true,
        })
        .await
        .unwrap()
    }

    #[test]
    fn operation_limits_match_s3_api_boundaries() {
        assert_eq!(S3_PAGE_SIZE, 1_000);
        assert_eq!(S3_DELETE_BATCH_SIZE, 1_000);
        assert_eq!(S3_MAX_ATTEMPTS, 4);
    }

    #[test]
    fn key_composition_is_canonical_with_or_without_a_configured_prefix() {
        assert_eq!(
            final_key("attachments", "note-1", "./docs/read me.txt").unwrap(),
            "attachments/note-1/docs/read me.txt"
        );
        assert_eq!(
            final_key("", "note-1", "file.txt").unwrap(),
            "note-1/file.txt"
        );
        assert_eq!(
            staging_prefix("attachments/", "note-1", "stage-1").unwrap(),
            "attachments/.staging/note-1/stage-1/"
        );
    }

    #[test]
    fn prefix_and_note_id_cannot_escape_the_owned_namespace() {
        for prefix in [
            "../attachments",
            "safe//unsafe",
            r"safe\unsafe",
            "safe/./unsafe",
        ] {
            assert!(normalize_prefix(prefix).is_err(), "{prefix:?} must fail");
        }
        for note_id in [
            "",
            ".",
            "..",
            "../note",
            "note/other",
            r"note\other",
            ".staging",
        ] {
            assert!(final_prefix("", note_id).is_err(), "{note_id:?} must fail");
        }
    }

    #[test]
    fn stage_id_cannot_escape_the_owned_namespace() {
        for stage_id in [
            "",
            ".",
            "..",
            "../stage",
            "stage/other",
            r"stage\other",
            ".staging",
        ] {
            assert!(
                staging_prefix("", "note-1", stage_id).is_err(),
                "{stage_id:?} must fail"
            );
        }
    }

    #[test]
    fn copy_source_percent_encodes_segments_but_preserves_path_separators() {
        assert_eq!(
            copy_source("agent-note", "attachments/note-1/read me.txt"),
            "agent-note/attachments/note-1/read%20me.txt"
        );
        assert_eq!(
            copy_source("agent note", "attachments/note+1/100%.txt"),
            "agent%20note/attachments/note%2B1/100%25.txt"
        );
    }

    #[tokio::test]
    async fn explicit_endpoint_region_path_style_and_standard_credentials_sign_requests() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("chain-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("chain-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/agent-note/tests/.staging/note-1/stage-1/file.txt"))
            .and(header_regex(
                "authorization",
                r"Credential=chain-access/.*/us-east-1/s3/aws4_request",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: " agent-note ".into(),
            prefix: "tests".into(),
            region: Some(" us-east-1 ".into()),
            endpoint: Some(server.uri()),
            force_path_style: true,
        })
        .await
        .unwrap();

        store
            .put("tests/.staging/note-1/stage-1/file.txt", b"payload")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sdk_uses_one_initial_attempt_plus_three_transient_retries() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("retry-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("retry-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));

        Mock::given(method("PUT"))
            .and(path("/agent-note/retry/file.txt"))
            .respond_with(FailThenSucceed {
                calls: calls.clone(),
                failures: 3,
            })
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        store.put("retry/file.txt", b"payload").await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn safe_errors_keep_category_but_never_source_text() {
        use aws_sdk_s3::error::SdkError;
        use aws_sdk_s3::operation::get_object::GetObjectError;

        let error = SdkError::<GetObjectError>::construction_failure(anyhow::anyhow!(
            "AWS_ACCESS_KEY_ID=secret-access AWS_SECRET_ACCESS_KEY=secret-key"
        ));
        let rendered = safe_sdk_error("get object", &error).to_string();

        assert_eq!(rendered, "S3 get object failed (construction)");
        assert!(!rendered.contains("secret-access"));
        assert!(!rendered.contains("secret-key"));
    }

    #[tokio::test]
    async fn endpoint_validation_is_fixed_and_never_echoes_input() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", None),
            ("AWS_SECRET_ACCESS_KEY", None),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);

        for endpoint in [
            "ftp://example.test",
            "https://",
            "https://sentinel-user:sentinel-password@example.test",
            "https://example.test?sentinel-query",
            "https://example.test#sentinel-fragment",
            "not a URL",
        ] {
            let error = S3AttachmentStore::new(S3AttachmentConfig {
                bucket: "bucket".into(),
                prefix: String::new(),
                region: Some("us-east-1".into()),
                endpoint: Some(endpoint.into()),
                force_path_style: false,
            })
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "invalid S3 attachment endpoint URL");
            assert!(!error.to_string().contains("sentinel"));
        }
    }

    #[tokio::test]
    async fn explicit_none_endpoint_ignores_aws_service_endpoint_environment() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let server = MockServer::start().await;
        let sentinel_endpoint = server.uri();
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("endpoint-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("endpoint-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", Some(&sentinel_endpoint)),
        ]);

        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: "bucket".into(),
            prefix: String::new(),
            // This produces a dispatch error without reaching an HTTP server
            // when the SDK's normal S3 endpoint resolver is used.
            region: Some("invalid region".into()),
            endpoint: None,
            force_path_style: true,
        })
        .await
        .unwrap();

        let error = tokio::time::timeout(Duration::from_secs(2), store.put("probe", b"payload"))
            .await
            .expect("normal endpoint resolution must fail without network delay")
            .unwrap_err();
        assert_eq!(error.to_string(), "S3 put object failed (dispatch)");
        assert!(server.received_requests().await.unwrap().is_empty());
        let rendered = format!("{store:?}");
        assert!(rendered.contains("endpoint: None"));
        assert!(!rendered.contains(&sentinel_endpoint));
    }

    #[tokio::test]
    async fn construction_succeeds_without_environment_credentials_when_metadata_is_disabled() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", None),
            ("AWS_SECRET_ACCESS_KEY", None),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);

        let store = tokio::time::timeout(
            Duration::from_secs(2),
            S3AttachmentStore::new(S3AttachmentConfig {
                bucket: "bucket".into(),
                prefix: String::new(),
                region: None,
                endpoint: None,
                force_path_style: false,
            }),
        )
        .await
        .expect("constructor must complete while instance metadata is disabled")
        .unwrap();

        let clone = store.clone();
        let first = store.coordination.lock_for("note-1");
        let second = clone.coordination.lock_for("note-1");
        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        drop(second);

        let _other = store.coordination.lock_for("note-2");
        let locks = store
            .coordination
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!locks.contains_key("note-1"));
    }

    #[test]
    fn environment_guard_restores_every_aws_variable() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let originals = [
            ("AWS_ACCESS_KEY_ID", Some("original-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("original-secret")),
            ("AWS_SESSION_TOKEN", Some("original-session")),
            ("AWS_EC2_METADATA_DISABLED", Some("original-metadata")),
            ("AWS_ENDPOINT_URL_S3", Some("https://original.example.test")),
        ];
        let _outer = AwsEnvGuard::apply(&originals);
        {
            let _inner = AwsEnvGuard::apply(&[
                ("AWS_ACCESS_KEY_ID", Some("temporary-access")),
                ("AWS_SECRET_ACCESS_KEY", None),
                ("AWS_SESSION_TOKEN", None),
                ("AWS_EC2_METADATA_DISABLED", Some("true")),
                (
                    "AWS_ENDPOINT_URL_S3",
                    Some("https://temporary.example.test"),
                ),
            ]);
            assert_eq!(
                std::env::var("AWS_ACCESS_KEY_ID").unwrap(),
                "temporary-access"
            );
            assert!(std::env::var_os("AWS_SECRET_ACCESS_KEY").is_none());
        }
        for (name, expected) in originals {
            assert_eq!(std::env::var(name).ok().as_deref(), expected);
        }
    }

    #[tokio::test]
    async fn debug_output_redacts_explicit_endpoints() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", None),
            ("AWS_SECRET_ACCESS_KEY", None),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let endpoint = "https://private-endpoint.example.test";
        let config = S3AttachmentConfig {
            bucket: "bucket".into(),
            prefix: String::new(),
            region: Some("us-east-1".into()),
            endpoint: Some(endpoint.into()),
            force_path_style: false,
        };
        let config_debug = format!("{config:?}");
        assert!(config_debug.contains("endpoint: Some(\"<redacted>\")"));
        assert!(!config_debug.contains(endpoint));

        let store = S3AttachmentStore::new(config).await.unwrap();
        let store_debug = format!("{store:?}");
        assert!(store_debug.contains("endpoint: Some(\"<redacted>\")"));
        assert!(!store_debug.contains(endpoint));
    }
}
