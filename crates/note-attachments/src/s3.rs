use aws_config::{retry::RetryConfig, BehaviorVersion, Region};
use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
    Client,
};
use note_core::NoteAttachment;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex as StdMutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

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

struct PreparedS3Set {
    store: S3AttachmentStore,
    note_id: String,
    staging_prefix: String,
    staged_keys: Vec<String>,
    metadata: Vec<NoteAttachment>,
    note_guard: Option<OwnedMutexGuard<()>>,
    cleanup_armed: bool,
}

struct S3CleanupWork {
    store: S3AttachmentStore,
    staged_keys: Vec<String>,
    staging_location: String,
    _note_guard: OwnedMutexGuard<()>,
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

    async fn prepare_set(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<PreparedS3Set> {
        validate_note_id(note_id)?;
        let mut canonical = Vec::with_capacity(attachments.len());
        let mut canonical_set = HashSet::with_capacity(attachments.len());
        let mut metadata = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let relative = crate::path::canonical_relative_path(&attachment.path)?;
            if !canonical_set.insert(relative.clone()) {
                anyhow::bail!("duplicate attachment path");
            }
            canonical.push(relative);
            metadata.push(NoteAttachment {
                id: attachment.id.clone(),
                path: attachment.path.clone(),
                mime: attachment.mime.clone(),
                description: attachment.description.clone(),
                content: Vec::new(),
            });
        }

        let note_guard = self.coordination.lock_for(note_id).lock_owned().await;
        let staging_prefix = staging_prefix(
            &self.prefix,
            note_id,
            &uuid::Uuid::new_v4().simple().to_string(),
        )?;
        let mut prepared = PreparedS3Set {
            store: self.clone(),
            note_id: note_id.to_string(),
            staging_prefix,
            staged_keys: Vec::new(),
            metadata,
            note_guard: Some(note_guard),
            cleanup_armed: true,
        };
        for (attachment, relative) in attachments.iter().zip(canonical) {
            let key = format!("{}{relative}", prepared.staging_prefix);
            // The request may have committed remotely even if its response is
            // lost, so include the current key in idempotent cleanup first.
            prepared.staged_keys.push(key.clone());
            if let Err(error) = self.put(&key, &attachment.content).await {
                let cleanup = prepared.schedule_cleanup()?.await;
                return match cleanup {
                    Ok(Ok(())) => Err(error),
                    Ok(Err(cleanup_error)) => {
                        Err(error.context(format!("staging cleanup also failed: {cleanup_error}")))
                    }
                    Err(cleanup_error) => {
                        Err(error.context(format!("staging cleanup also failed: {cleanup_error}")))
                    }
                };
            }
        }

        Ok(prepared)
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

    async fn delete_keys(&self, keys: &[String]) -> anyhow::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let objects = keys
            .iter()
            .map(|key| ObjectIdentifier::builder().key(key).build())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| anyhow::anyhow!("S3 delete objects failed (invalid key)"))?;
        let delete = Delete::builder()
            .set_objects(Some(objects))
            .quiet(true)
            .build()
            .map_err(|_| anyhow::anyhow!("S3 delete objects failed (invalid request)"))?;
        let output = self
            .client
            .delete_objects()
            .bucket(&self.bucket)
            .delete(delete)
            .send()
            .await
            .map_err(|error| safe_sdk_error("delete objects", &error))?;
        if !output.errors().is_empty() {
            anyhow::bail!(
                "S3 delete objects failed for {} object(s)",
                output.errors().len()
            );
        }
        Ok(())
    }

    async fn list_keys_single_page(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(prefix)
            .max_keys(S3_PAGE_SIZE)
            .send()
            .await
            .map_err(|error| safe_sdk_error("list objects", &error))?;
        Ok(output
            .contents()
            .iter()
            .filter_map(|object| object.key())
            .map(ToOwned::to_owned)
            .collect())
    }

    async fn publish_set(&self, prepared: Box<PreparedS3Set>) -> anyhow::Result<()> {
        for attachment in &prepared.metadata {
            let relative = crate::path::canonical_relative_path(&attachment.path)?;
            let source_key = format!("{}{relative}", prepared.staging_prefix);
            let destination_key = final_key(&self.prefix, &prepared.note_id, &attachment.path)?;
            if let Err(error) = self
                .client
                .copy_object()
                .copy_source(copy_source(&self.bucket, &source_key))
                .bucket(&self.bucket)
                .key(destination_key)
                .send()
                .await
                .map_err(|error| safe_sdk_error("copy object", &error))
            {
                let staging_location = prepared.staging_location();
                eprintln!(
                    "S3 attachment publication failed after database commit; \
                     staged objects retained at {staging_location}"
                );
                return Err(error.context(format!(
                    "database committed; staged objects retained at {staging_location}"
                )));
            }
        }

        if let Err(error) = self.delete_keys(&prepared.staged_keys).await {
            let staging_location = prepared.staging_location();
            eprintln!(
                "S3 attachment publication copied final objects after database commit; \
                 staging cleanup uncertain at {staging_location}"
            );
            return Err(error.context(format!(
                "database committed; final objects copied; \
                 staging cleanup uncertain at {staging_location}"
            )));
        }
        Ok(())
    }

    fn safe_location(&self) -> String {
        let suffix = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", self.prefix)
        };
        format!("s3://{}{suffix}", self.bucket)
    }
}

impl PreparedS3Set {
    fn staging_location(&self) -> String {
        format!("s3://{}/{}", self.store.bucket, self.staging_prefix)
    }

    fn take_cleanup_work(&mut self) -> Option<S3CleanupWork> {
        if !self.cleanup_armed {
            return None;
        }
        self.cleanup_armed = false;
        let note_guard = self.note_guard.take()?;
        if self.staged_keys.is_empty() {
            drop(note_guard);
            return None;
        }
        Some(S3CleanupWork {
            store: self.store.clone(),
            staged_keys: std::mem::take(&mut self.staged_keys),
            staging_location: self.staging_location(),
            _note_guard: note_guard,
        })
    }

    fn schedule_cleanup(&mut self) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("S3 staging cleanup could not be scheduled"))?;
        let work = self.take_cleanup_work();
        Ok(runtime.spawn(async move {
            let Some(work) = work else {
                return Ok(());
            };
            work.run().await
        }))
    }
}

impl S3CleanupWork {
    async fn run(self) -> anyhow::Result<()> {
        let result = self.store.delete_keys(&self.staged_keys).await;
        if let Err(error) = result.as_ref() {
            eprintln!(
                "S3 staged attachment cleanup failed at {}: {error}",
                self.staging_location
            );
        }
        result
    }
}

impl Drop for PreparedS3Set {
    fn drop(&mut self) {
        if !self.cleanup_armed {
            return;
        }
        let staging_location = self.staging_location();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.cleanup_armed = false;
            eprintln!("S3 staged attachment cleanup could not be scheduled at {staging_location}");
            return;
        };
        if let Some(work) = self.take_cleanup_work() {
            runtime.spawn(async move {
                let _ = work.run().await;
            });
        }
    }
}

#[async_trait::async_trait]
impl crate::PreparedAttachmentSet for PreparedS3Set {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(mut self: Box<Self>) -> anyhow::Result<()> {
        // The database is already committed. From this point onward, caller
        // cancellation must neither stop publication nor invoke pre-commit
        // staging cleanup.
        self.cleanup_armed = false;
        let store = self.store.clone();
        tokio::spawn(async move { store.publish_set(self).await })
            .await
            .map_err(|_| anyhow::anyhow!("S3 attachment publication task failed"))?
    }

    async fn abort(mut self: Box<Self>) -> anyhow::Result<()> {
        self.schedule_cleanup()?
            .await
            .map_err(|_| anyhow::anyhow!("S3 staging cleanup task failed"))?
    }
}

#[async_trait::async_trait]
impl crate::AttachmentStore for S3AttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn crate::PreparedAttachmentSet>> {
        let store = self.clone();
        let note_id = note_id.to_string();
        let attachments = attachments.to_vec();
        let prepared = tokio::spawn(async move { store.prepare_set(&note_id, &attachments).await })
            .await
            .map_err(|_| anyhow::anyhow!("S3 attachment preparation task failed"))??;
        Ok(Box::new(prepared))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        validate_note_id(note_id)?;
        let note_lock = self.coordination.lock_for(note_id);
        let _guard = note_lock.lock().await;
        self.read_key(&final_key(&self.prefix, note_id, path)?)
            .await
    }

    async fn hydrate(
        &self,
        note_id: &str,
        attachments: &mut [NoteAttachment],
    ) -> anyhow::Result<()> {
        validate_note_id(note_id)?;
        let note_lock = self.coordination.lock_for(note_id);
        let _guard = note_lock.lock().await;
        for attachment in attachments {
            attachment.content = self
                .read_key(&final_key(&self.prefix, note_id, &attachment.path)?)
                .await?;
        }
        Ok(())
    }

    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
        validate_note_id(note_id)?;
        let note_lock = self.coordination.lock_for(note_id);
        let _guard = note_lock.lock().await;
        let keys = self
            .list_keys_single_page(&final_prefix(&self.prefix, note_id)?)
            .await?;
        self.delete_keys(&keys).await
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
        matchers::{body_string_contains, header_regex, method, path, path_regex},
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

    #[derive(Clone)]
    struct BlockFirstResponse {
        calls: Arc<AtomicUsize>,
        reached: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        response: ResponseTemplate,
    }

    impl Respond for BlockFirstResponse {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                if let Some(reached) = self
                    .reached
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = reached.send(());
                }
                return self.response.clone().set_delay(Duration::from_millis(500));
            }
            self.response.clone()
        }
    }

    fn block_first_response(
        response: ResponseTemplate,
    ) -> (
        BlockFirstResponse,
        tokio::sync::oneshot::Receiver<()>,
        Arc<AtomicUsize>,
    ) {
        let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        (
            BlockFirstResponse {
                calls: calls.clone(),
                reached: Arc::new(Mutex::new(Some(reached_sender))),
                response,
            },
            reached_receiver,
            calls,
        )
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

    #[tokio::test]
    async fn ambiguous_failed_put_deletes_the_current_staging_key() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("cleanup-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("cleanup-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/private-name\.txt$",
            ))
            .respond_with(ResponseTemplate::new(503).set_body_string("private server detail"))
            .expect(4)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .and(body_string_contains("private-name.txt</Key>"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<DeleteResult></DeleteResult>", "application/xml"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let attachment = NoteAttachment {
            id: "private-id".into(),
            path: "private-name.txt".into(),
            mime: "text/plain".into(),
            description: String::new(),
            content: b"private content".to_vec(),
        };
        let error = store
            .prepare_set("note-1", &[attachment])
            .await
            .err()
            .expect("ambiguous PUT must fail");

        let rendered = error.to_string();
        assert_eq!(rendered, "S3 put object failed (service)");
        for secret in [
            "private-name.txt",
            "private server detail",
            "cleanup-access",
            "cleanup-secret",
            &server.uri(),
        ] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test]
    async fn cancelled_publish_keeps_the_note_locked_until_ordered_publication_finishes() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("publish-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("publish-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let (copy_response, copy_reached, copy_calls) =
            block_first_response(ResponseTemplate::new(200));

        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/file.txt"))
            .respond_with(copy_response)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .and(body_string_contains(".staging/note-1/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<DeleteResult></DeleteResult>", "application/xml"),
            )
            .expect(2)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let first = store
            .prepare_set(
                "note-1",
                &[NoteAttachment {
                    id: "file".into(),
                    path: "file.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"old".to_vec(),
                }],
            )
            .await
            .unwrap();
        let publish = tokio::spawn(async move {
            let prepared: Box<dyn crate::PreparedAttachmentSet> = Box::new(first);
            prepared.publish().await
        });
        tokio::time::timeout(Duration::from_secs(2), copy_reached)
            .await
            .expect("first copy must start")
            .expect("copy responder must signal");
        publish.abort();
        assert!(publish.await.unwrap_err().is_cancelled());

        let second_store = store.clone();
        let mut second = tokio::spawn(async move {
            second_store
                .prepare_set(
                    "note-1",
                    &[NoteAttachment {
                        id: "file".into(),
                        path: "file.txt".into(),
                        mime: "text/plain".into(),
                        description: String::new(),
                        content: b"new".to_vec(),
                    }],
                )
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "the next same-note prepare must wait for detached publication"
        );

        let second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("second prepare must proceed after first publication")
            .unwrap()
            .unwrap();
        let prepared: Box<dyn crate::PreparedAttachmentSet> = Box::new(second);
        prepared.publish().await.unwrap();
        assert_eq!(copy_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancelled_prepare_detaches_cleanup_and_holds_the_note_lock() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("prepare-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("prepare-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let (put_response, put_reached, _) = block_first_response(ResponseTemplate::new(200));
        let (delete_response, delete_reached, _) = block_first_response(
            ResponseTemplate::new(200)
                .set_body_raw("<DeleteResult></DeleteResult>", "application/xml"),
        );

        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(put_response)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .and(body_string_contains("file.txt</Key>"))
            .respond_with(delete_response)
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let preparing_store = store.clone();
        let preparing = tokio::spawn(async move {
            crate::AttachmentStore::prepare(
                &preparing_store,
                "note-1",
                &[NoteAttachment {
                    id: "file".into(),
                    path: "file.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"payload".to_vec(),
                }],
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), put_reached)
            .await
            .expect("PUT must start")
            .expect("PUT responder must signal");
        preparing.abort();
        assert!(matches!(preparing.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(Duration::from_secs(2), delete_reached)
            .await
            .expect("cancelled prepare must schedule cleanup")
            .expect("delete responder must signal");

        let second_store = store.clone();
        let mut second = tokio::spawn(async move { second_store.prepare_set("note-1", &[]).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "cleanup must retain the note lock"
        );
        let mut second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("second prepare must proceed after cleanup")
            .unwrap()
            .unwrap();
        second.schedule_cleanup().unwrap().await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_abort_detaches_cleanup_and_holds_the_note_lock() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("abort-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("abort-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let (delete_response, delete_reached, _) = block_first_response(
            ResponseTemplate::new(200)
                .set_body_raw("<DeleteResult></DeleteResult>", "application/xml"),
        );

        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .and(body_string_contains("file.txt</Key>"))
            .respond_with(delete_response)
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let prepared = store
            .prepare_set(
                "note-1",
                &[NoteAttachment {
                    id: "file".into(),
                    path: "file.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"payload".to_vec(),
                }],
            )
            .await
            .unwrap();
        let aborting = tokio::spawn(async move {
            let prepared: Box<dyn crate::PreparedAttachmentSet> = Box::new(prepared);
            prepared.abort().await
        });
        tokio::time::timeout(Duration::from_secs(2), delete_reached)
            .await
            .expect("abort cleanup must start")
            .expect("delete responder must signal");
        aborting.abort();
        assert!(aborting.await.unwrap_err().is_cancelled());

        let second_store = store.clone();
        let mut second = tokio::spawn(async move { second_store.prepare_set("note-1", &[]).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "detached abort must retain the note lock"
        );
        let mut second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("second prepare must proceed after abort cleanup")
            .unwrap()
            .unwrap();
        second.schedule_cleanup().unwrap().await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn publication_errors_report_copy_and_cleanup_phases_without_secrets() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("phase-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("phase-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);

        let copy_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/private-name\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&copy_server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/private-name.txt"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                "<Error><Code>PrivateCopyCode</Code><Message>private copy body</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&copy_server)
            .await;
        let copy_store = test_store(copy_server.uri()).await;
        let prepared = copy_store
            .prepare_set(
                "note-1",
                &[NoteAttachment {
                    id: "private-id".into(),
                    path: "private-name.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"private content".to_vec(),
                }],
            )
            .await
            .unwrap();
        let prepared: Box<dyn crate::PreparedAttachmentSet> = Box::new(prepared);
        let copy_error = prepared.publish().await.unwrap_err();
        let copy_rendered = format!("{copy_error:#}");
        assert!(copy_rendered.contains("staged objects retained at s3://agent-note/.staging/"));
        assert!(!copy_rendered.contains("staging cleanup uncertain"));
        assert_eq!(
            copy_server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.method.as_str() == "POST")
                .count(),
            0
        );

        let cleanup_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/private-name\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&cleanup_server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/private-name.txt"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&cleanup_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(503).set_body_raw(
                "<Error><Code>PrivateDeleteCode</Code><Message>private delete body</Message></Error>",
                "application/xml",
            ))
            .expect(4)
            .mount(&cleanup_server)
            .await;
        let cleanup_store = test_store(cleanup_server.uri()).await;
        let prepared = cleanup_store
            .prepare_set(
                "note-1",
                &[NoteAttachment {
                    id: "private-id".into(),
                    path: "private-name.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"private content".to_vec(),
                }],
            )
            .await
            .unwrap();
        let prepared: Box<dyn crate::PreparedAttachmentSet> = Box::new(prepared);
        let cleanup_error = prepared.publish().await.unwrap_err();
        let cleanup_rendered = format!("{cleanup_error:#}");
        assert!(cleanup_rendered
            .contains("final objects copied; staging cleanup uncertain at s3://agent-note/"));
        assert!(!cleanup_rendered.contains("staged objects retained"));

        for rendered in [&copy_rendered, &cleanup_rendered] {
            for secret in [
                "private-name.txt",
                "private copy body",
                "PrivateCopyCode",
                "private delete body",
                "PrivateDeleteCode",
                "phase-access",
                "phase-secret",
                &copy_server.uri(),
                &cleanup_server.uri(),
            ] {
                assert!(!rendered.contains(secret));
            }
        }
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
