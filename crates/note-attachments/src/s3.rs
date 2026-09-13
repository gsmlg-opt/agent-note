use aws_config::{retry::RetryConfig, BehaviorVersion, Region};
use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    Client,
};
use sha2::{Digest, Sha256};
use std::fmt;
use tokio::io::{AsyncRead, AsyncReadExt};

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
        formatter
            .debug_struct("S3AttachmentStore")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field(
                "endpoint",
                &self.endpoint_configured.then_some("<redacted>"),
            )
            .finish_non_exhaustive()
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
        })
    }

    async fn put_immutable_object(
        &self,
        request: crate::PutObjectRequest,
    ) -> anyhow::Result<crate::StoredObject> {
        let object_key = crate::path::canonical_object_key(&request.object_key)?;
        if !valid_sha256(&request.checksum_sha256) {
            anyhow::bail!("invalid expected SHA-256 checksum");
        }
        let actual_checksum = format!("{:x}", Sha256::digest(&request.bytes));
        if actual_checksum != request.checksum_sha256 {
            anyhow::bail!("attachment object checksum does not match expected SHA-256");
        }
        let key = object_key_with_prefix(&self.prefix, &object_key)?;
        let expected = crate::ObjectMetadata {
            size_bytes: request.bytes.len() as u64,
            checksum_sha256: request.checksum_sha256,
        };
        if self.key_exists(&key).await? {
            return self
                .classify_existing_object(object_key, &key, &expected)
                .await;
        }

        let put = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .if_none_match("*")
            .metadata("checksum-sha256", &expected.checksum_sha256)
            .body(ByteStream::from(request.bytes))
            .send()
            .await;
        match put {
            Ok(_) => self.verify_object(object_key, &key, &expected).await,
            Err(error)
                if error.code() == Some("PreconditionFailed")
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 412) =>
            {
                self.classify_existing_object(object_key, &key, &expected)
                    .await
            }
            Err(error) => Err(safe_sdk_error("put object", &error)),
        }
    }

    async fn verify_object(
        &self,
        object_key: String,
        key: &str,
        expected: &crate::ObjectMetadata,
    ) -> anyhow::Result<crate::StoredObject> {
        let metadata = self
            .head_key(key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("S3 put object verification failed (missing object)"))?;
        let bytes = self.read_key(key).await?;
        let checksum = format!("{:x}", Sha256::digest(&bytes));
        if metadata != *expected
            || bytes.len() as u64 != expected.size_bytes
            || checksum != expected.checksum_sha256
        {
            anyhow::bail!("S3 put object verification failed (content mismatch)");
        }
        Ok(crate::StoredObject {
            object_key,
            size_bytes: metadata.size_bytes,
            checksum_sha256: metadata.checksum_sha256,
        })
    }

    async fn classify_existing_object(
        &self,
        object_key: String,
        key: &str,
        expected: &crate::ObjectMetadata,
    ) -> anyhow::Result<crate::StoredObject> {
        match self.verify_object(object_key, key, expected).await {
            Ok(stored) => Ok(stored),
            Err(_) => anyhow::bail!("attachment object already exists"),
        }
    }

    async fn read_key(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| safe_sdk_error("get object", &error))?;
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|_| anyhow::anyhow!("S3 get object body failed (stream)"))?;
        Ok(bytes.into_bytes().to_vec())
    }

    async fn read_key_bounded(&self, key: &str, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
        let output = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => output,
            Err(error)
                if matches!(error.code(), Some("NoSuchKey" | "NotFound" | "404"))
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404) =>
            {
                return Err(crate::BoundedReadError::Missing.into())
            }
            Err(_) => return Err(crate::BoundedReadError::StorageFailure.into()),
        };
        if output
            .content_length()
            .and_then(|length| u64::try_from(length).ok())
            .is_some_and(|length| length > max_bytes)
        {
            return Err(crate::BoundedReadError::LimitExceeded.into());
        }

        read_stream_bounded(output.body.into_async_read(), max_bytes).await
    }

    async fn head_key(&self, key: &str) -> anyhow::Result<Option<crate::ObjectMetadata>> {
        let output = match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => output,
            Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound" | "404")) => {
                return Ok(None)
            }
            Err(error) => return Err(safe_sdk_error("head object", &error)),
        };
        let size_bytes = output
            .content_length()
            .and_then(|size| u64::try_from(size).ok())
            .ok_or_else(|| anyhow::anyhow!("S3 head object failed (invalid content length)"))?;
        let checksum_sha256 = output
            .metadata()
            .and_then(|metadata| metadata.get("checksum-sha256"))
            .filter(|checksum| valid_sha256(checksum))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("S3 head object failed (missing checksum metadata)"))?;
        Ok(Some(crate::ObjectMetadata {
            size_bytes,
            checksum_sha256,
        }))
    }

    async fn key_exists(&self, key: &str) -> anyhow::Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound" | "404")) => {
                Ok(false)
            }
            Err(error) => Err(safe_sdk_error("head object", &error)),
        }
    }

    async fn delete_key(&self, key: &str) -> anyhow::Result<()> {
        match self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound")) => Ok(()),
            Err(error) => Err(safe_sdk_error("delete object", &error)),
        }
    }

    fn safe_location(&self) -> String {
        let trimmed_prefix = self.prefix.trim_matches('/');
        let suffix = if trimmed_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", trimmed_prefix)
        };
        format!("s3://{}{suffix}", self.bucket)
    }
}

async fn read_stream_bounded<R>(reader: R, max_bytes: u64) -> anyhow::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let initial_capacity = usize::try_from(max_bytes.min(64 * 1024)).unwrap_or(64 * 1024);
    let mut bytes = Vec::with_capacity(initial_capacity);
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| anyhow::Error::from(crate::BoundedReadError::StorageFailure))?;
    if bytes.len() as u64 > max_bytes {
        return Err(crate::BoundedReadError::LimitExceeded.into());
    }
    Ok(bytes)
}

#[async_trait::async_trait]
impl crate::AttachmentStore for S3AttachmentStore {
    async fn put_immutable(
        &self,
        request: crate::PutObjectRequest,
    ) -> anyhow::Result<crate::StoredObject> {
        let store = self.clone();
        tokio::spawn(async move { store.put_immutable_object(request).await })
            .await
            .map_err(|_| anyhow::anyhow!("S3 immutable object put task failed"))?
    }

    async fn read_object(&self, object_key: &str) -> anyhow::Result<Vec<u8>> {
        self.read_key(&object_key_with_prefix(&self.prefix, object_key)?)
            .await
    }

    async fn read_object_bounded(
        &self,
        object_key: &str,
        max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        self.read_key_bounded(
            &object_key_with_prefix(&self.prefix, object_key)?,
            max_bytes,
        )
        .await
    }

    async fn head_object(&self, object_key: &str) -> anyhow::Result<crate::ObjectMetadata> {
        self.head_key(&object_key_with_prefix(&self.prefix, object_key)?)
            .await?
            .ok_or_else(|| anyhow::anyhow!("attachment object does not exist"))
    }

    async fn delete_object(&self, object_key: &str) -> anyhow::Result<crate::DeleteObjectOutcome> {
        let key = object_key_with_prefix(&self.prefix, object_key)?;
        if !self.key_exists(&key).await? {
            return Ok(crate::DeleteObjectOutcome::AlreadyAbsent);
        }
        self.delete_key(&key).await?;
        Ok(crate::DeleteObjectOutcome::Deleted)
    }

    async fn delete_legacy(
        &self,
        note_id: &str,
        user_path: &str,
    ) -> anyhow::Result<crate::DeleteObjectOutcome> {
        let key = final_key(&self.prefix, note_id, user_path)?;
        if !self.key_exists(&key).await? {
            return Ok(crate::DeleteObjectOutcome::AlreadyAbsent);
        }
        self.delete_key(&key).await?;
        Ok(crate::DeleteObjectOutcome::Deleted)
    }

    async fn read_legacy(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.read_key(&final_key(&self.prefix, note_id, path)?)
            .await
    }

    async fn read_legacy_bounded(
        &self,
        note_id: &str,
        path: &str,
        max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        self.read_key_bounded(&final_key(&self.prefix, note_id, path)?, max_bytes)
            .await
    }

    fn info(&self) -> crate::AttachmentStoreInfo {
        crate::AttachmentStoreInfo {
            engine: "s3".into(),
            location: Some(self.safe_location()),
        }
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
        SdkError::ConstructionFailure(_) => "request construction",
        SdkError::TimeoutError(_) => "timeout",
        SdkError::DispatchFailure(_) => "transport",
        SdkError::ResponseError(_) => "response",
        SdkError::ServiceError(_) => "service",
        _ => "unknown",
    };
    anyhow::anyhow!("S3 {operation} failed ({category})")
}

fn normalize_prefix(prefix: &str) -> anyhow::Result<String> {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let canonical = crate::path::canonical_object_key(trimmed)?;
    Ok(format!("{canonical}/"))
}

fn validate_note_id(note_id: &str) -> anyhow::Result<()> {
    if note_id.is_empty()
        || note_id == "."
        || note_id == ".."
        || note_id
            .bytes()
            .any(|byte| byte == b'/' || byte == b'\\' || byte == 0)
    {
        anyhow::bail!("invalid attachment note id");
    }
    Ok(())
}

fn join_key(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_owned()
    } else {
        format!("{prefix}{suffix}")
    }
}

fn object_key_with_prefix(prefix: &str, object_key: &str) -> anyhow::Result<String> {
    let object_key = crate::path::canonical_object_key(object_key)?;
    Ok(join_key(prefix, &object_key))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn final_key(prefix: &str, note_id: &str, path: &str) -> anyhow::Result<String> {
    validate_note_id(note_id)?;
    let path = crate::path::canonical_relative_path(path)?;
    Ok(join_key(prefix, &format!("{note_id}/{path}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AttachmentStore;
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
    struct MissingThenObjectMetadata {
        calls: Arc<AtomicUsize>,
        checksum: String,
        size_bytes: u64,
    }

    impl Respond for MissingThenObjectMetadata {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(404)
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-length", self.size_bytes.to_string())
                    .insert_header("x-amz-meta-checksum-sha256", self.checksum.as_str())
            }
        }
    }

    #[derive(Clone)]
    struct FailThenSucceed {
        calls: Arc<AtomicUsize>,
        failures: usize,
        success: ResponseTemplate,
    }

    impl Respond for FailThenSucceed {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                ResponseTemplate::new(503)
            } else {
                self.success.clone()
            }
        }
    }

    fn test_env(access: &str, secret: &str) -> AwsEnvGuard {
        AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some(access)),
            ("AWS_SECRET_ACCESS_KEY", Some(secret)),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ])
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
    fn key_composition_is_canonical_and_contained() {
        assert_eq!(normalize_prefix(" /owned/ ").unwrap(), "owned/");
        assert_eq!(
            object_key_with_prefix("owned/", "v2/abc").unwrap(),
            "owned/v2/abc"
        );
        assert!(object_key_with_prefix("owned/", "../escape").is_err());
        assert!(final_key("owned/", "../note", "file.txt").is_err());
        assert!(final_key("owned/", "note", "../file.txt").is_err());
    }

    #[test]
    fn debug_output_redacts_endpoint() {
        let config = S3AttachmentConfig {
            bucket: "bucket".into(),
            prefix: "prefix".into(),
            region: None,
            endpoint: Some("https://secret.example.test".into()),
            force_path_style: false,
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret.example.test"));
    }

    #[tokio::test]
    async fn immutable_put_uses_exact_prefixed_key_and_verifies_metadata_and_content() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("object-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("object-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        let head_calls = Arc::new(AtomicUsize::new(0));

        Mock::given(method("HEAD"))
            .and(path("/agent-note/configured/generations/key.bin"))
            .respond_with(MissingThenObjectMetadata {
                calls: head_calls.clone(),
                checksum: checksum.to_string(),
                size_bytes: 7,
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/configured/generations/key.bin"))
            .and(header_regex("x-amz-meta-checksum-sha256", checksum))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/configured/generations/key.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: "agent-note".into(),
            prefix: "configured".into(),
            region: Some("us-east-1".into()),
            endpoint: Some(server.uri()),
            force_path_style: true,
        })
        .await
        .unwrap();
        let stored = store
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/key.bin".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap();

        assert_eq!(stored.object_key, "generations/key.bin");
        assert_eq!(stored.size_bytes, 7);
        assert_eq!(stored.checksum_sha256, checksum);
        assert_eq!(head_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn immutable_collision_is_rejected_before_put() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("collision-access", "collision-secret");
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/collision"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", "7")
                    .insert_header("x-amz-meta-checksum-sha256", checksum),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/collision"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/collision".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "PUT"));
    }

    async fn mount_conditional_collision(
        server: &MockServer,
        key: &str,
        checksum: &str,
        bytes: &'static [u8],
    ) {
        Mock::given(method("HEAD"))
            .and(path(format!("/agent-note/{key}")))
            .respond_with(MissingThenObjectMetadata {
                calls: Arc::new(AtomicUsize::new(0)),
                checksum: checksum.into(),
                size_bytes: bytes.len() as u64,
            })
            .expect(2)
            .mount(server)
            .await;
        Mock::given(method("PUT"))
            .and(path(format!("/agent-note/{key}")))
            .and(header_regex("if-none-match", r"^\*$"))
            .respond_with(ResponseTemplate::new(412).set_body_raw(
                "<Error><Code>PreconditionFailed</Code></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/agent-note/{key}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn conditional_412_is_idempotent_only_for_exact_matching_content() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("conditional-access", "conditional-secret");
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";

        let matching = MockServer::start().await;
        mount_conditional_collision(&matching, "generations/matching", checksum, b"payload").await;
        let stored = test_store(matching.uri())
            .await
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/matching".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap();
        assert_eq!(stored.object_key, "generations/matching");

        let mismatched = MockServer::start().await;
        mount_conditional_collision(&mismatched, "generations/mismatch", checksum, b"corrupt")
            .await;
        let error = test_store(mismatched.uri())
            .await
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/mismatch".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "attachment object already exists");
    }

    #[tokio::test]
    async fn immutable_put_rejects_substituted_content_after_successful_put() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("verify-access", "verify-secret");
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/corrupt"))
            .respond_with(MissingThenObjectMetadata {
                calls: Arc::new(AtomicUsize::new(0)),
                checksum: checksum.into(),
                size_bytes: 7,
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/generations/corrupt"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/corrupt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"corrupt".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/corrupt".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "S3 put object verification failed (content mismatch)"
        );
    }

    #[tokio::test]
    async fn cancelled_immutable_put_finishes_publication_and_verification() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("cancel-access", "cancel-secret");
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/cancelled"))
            .respond_with(MissingThenObjectMetadata {
                calls: Arc::new(AtomicUsize::new(0)),
                checksum: checksum.into(),
                size_bytes: 7,
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/generations/cancelled"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(150)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/cancelled"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let store = test_store(server.uri()).await;
        let task = tokio::spawn(async move {
            store
                .put_immutable(crate::PutObjectRequest {
                    object_key: "generations/cancelled".into(),
                    bytes: b"payload".to_vec(),
                    checksum_sha256: checksum.into(),
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        task.abort();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.method.as_str() == "GET")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn immutable_lifecycle_targets_one_exact_key_and_reports_missing_delete() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("lifecycle-access", "lifecycle-secret");
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/key"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", "7")
                    .insert_header("x-amz-meta-checksum-sha256", checksum),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/agent-note/generations/key"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/missing"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let store = test_store(server.uri()).await;

        assert_eq!(
            store.read_object("generations/key").await.unwrap(),
            b"payload"
        );
        assert_eq!(
            store
                .head_object("generations/key")
                .await
                .unwrap()
                .size_bytes,
            7
        );
        assert_eq!(
            store.delete_object("generations/key").await.unwrap(),
            crate::DeleteObjectOutcome::Deleted
        );
        assert_eq!(
            store.delete_object("generations/missing").await.unwrap(),
            crate::DeleteObjectOutcome::AlreadyAbsent
        );
    }

    #[tokio::test]
    async fn bounded_immutable_reads_accept_exact_limit_and_reject_advertised_oversize() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("bounded-access", "bounded-secret");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/exact"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"12345678".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/large"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"123456789".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let store = test_store(server.uri()).await;

        assert_eq!(
            store
                .read_object_bounded("generations/exact", 8)
                .await
                .unwrap(),
            b"12345678"
        );
        assert_eq!(
            store
                .read_object_bounded("generations/large", 8)
                .await
                .unwrap_err()
                .to_string(),
            "attachment object exceeds bounded read limit"
        );
    }

    #[tokio::test]
    async fn bounded_reads_reject_chunked_oversize_bodies_without_content_length() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("bounded-stream-access", "bounded-stream-secret");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/chunked"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("transfer-encoding", "chunked")
                    .set_body_bytes(b"123456789".to_vec()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let store = test_store(server.uri()).await;

        let error = store
            .read_object_bounded("generations/chunked", 8)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::BoundedReadError>(),
            Some(crate::BoundedReadError::LimitExceeded)
        ));
    }

    #[tokio::test]
    async fn bounded_reads_distinguish_missing_keys_from_service_failures() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("bounded-errors-access", "bounded-errors-secret");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(
                "<Error><Code>NoSuchKey</Code><Message>missing</Message></Error>",
                "application/xml",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/failure"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let store = test_store(server.uri()).await;

        let missing = store
            .read_object_bounded("generations/missing", 8)
            .await
            .unwrap_err();
        assert!(matches!(
            missing.downcast_ref::<crate::BoundedReadError>(),
            Some(crate::BoundedReadError::Missing)
        ));
        let storage = store
            .read_object_bounded("generations/failure", 8)
            .await
            .unwrap_err();
        assert!(matches!(
            storage.downcast_ref::<crate::BoundedReadError>(),
            Some(crate::BoundedReadError::StorageFailure)
        ));
    }

    #[tokio::test]
    async fn streaming_ceiling_rejects_a_body_larger_than_advertised() {
        let advertised_length = 1_u64;
        let hard_limit = 8_u64;
        let body = std::io::Cursor::new(b"123456789".to_vec());
        assert!(body.get_ref().len() as u64 > advertised_length);
        let error = read_stream_bounded(body, hard_limit).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::BoundedReadError>(),
            Some(crate::BoundedReadError::LimitExceeded)
        ));
    }

    #[tokio::test]
    async fn legacy_delete_targets_only_the_validated_note_path_key() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("legacy-access", "legacy-secret");
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/agent-note/configured/note-1/nested/file.txt"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/agent-note/configured/note-1/nested/file.txt"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: "agent-note".into(),
            prefix: "configured".into(),
            region: Some("us-east-1".into()),
            endpoint: Some(server.uri()),
            force_path_style: true,
        })
        .await
        .unwrap();

        assert_eq!(
            store
                .delete_legacy("note-1", "nested/file.txt")
                .await
                .unwrap(),
            crate::DeleteObjectOutcome::Deleted
        );
        assert!(store.delete_legacy("../escape", "file").await.is_err());
        assert!(store.delete_legacy("note-1", "../escape").await.is_err());
    }

    #[tokio::test]
    async fn sdk_retries_transient_immutable_reads_at_most_four_total_attempts() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("retry-access", "retry-secret");
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/agent-note/generations/retry"))
            .respond_with(FailThenSucceed {
                calls: calls.clone(),
                failures: 3,
                success: ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()),
            })
            .mount(&server)
            .await;

        assert_eq!(
            test_store(server.uri())
                .await
                .read_object("generations/retry")
                .await
                .unwrap(),
            b"payload"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn endpoint_validation_is_fixed_and_never_echoes_input() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("endpoint-access", "endpoint-secret");
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
    async fn explicit_endpoint_region_path_style_and_credentials_apply_to_immutable_reads() {
        let _lock = AWS_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _env = test_env("chain-access", "chain-secret");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/configured/generations/key"))
            .and(header_regex(
                "authorization",
                r"Credential=chain-access/.*/us-east-1/s3/aws4_request",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let endpoint = server.uri();
        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: " agent-note ".into(),
            prefix: "configured".into(),
            region: Some(" us-east-1 ".into()),
            endpoint: Some(endpoint.clone()),
            force_path_style: true,
        })
        .await
        .unwrap();

        assert_eq!(
            store.read_object("generations/key").await.unwrap(),
            b"payload"
        );
        let debug = format!("{store:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&endpoint));
    }

    #[test]
    fn sdk_errors_keep_category_without_source_secrets() {
        use aws_sdk_s3::operation::get_object::GetObjectError;

        let error = SdkError::<GetObjectError>::construction_failure(anyhow::anyhow!(
            "AWS_ACCESS_KEY_ID=secret-access AWS_SECRET_ACCESS_KEY=secret-key"
        ));
        let rendered = safe_sdk_error("get object", &error).to_string();
        assert_eq!(rendered, "S3 get object failed (request construction)");
        assert!(!rendered.contains("secret-access"));
        assert!(!rendered.contains("secret-key"));
    }
}
