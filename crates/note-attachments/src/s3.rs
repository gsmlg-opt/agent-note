use aws_config::{retry::RetryConfig, BehaviorVersion, Region};
use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
    Client,
};
use note_core::NoteAttachment;
use sha2::{Digest, Sha256};
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

struct PreparedS3Mutation {
    store: S3AttachmentStore,
    operation: S3MutationOperation,
    note_guard: Option<OwnedMutexGuard<()>>,
    cleanup_armed: bool,
}

enum S3MutationOperation {
    Put {
        staging_key: String,
        final_key: String,
    },
    Delete {
        final_key: String,
    },
}

struct S3CleanupWork {
    store: S3AttachmentStore,
    staged_keys: Vec<String>,
    staging_location: String,
    _note_guard: OwnedMutexGuard<()>,
}

struct S3SingleCleanupWork {
    store: S3AttachmentStore,
    staging_key: String,
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

    async fn put_owned(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(|error| safe_sdk_error("put object", &error));
        #[cfg(test)]
        pause_s3_operation(S3OperationPhase::Put).await;
        result?;
        Ok(())
    }

    async fn delete_key(&self, key: &str) -> anyhow::Result<()> {
        let result = self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        #[cfg(test)]
        pause_s3_operation(S3OperationPhase::Delete).await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound")) => Ok(()),
            Err(error) => Err(safe_sdk_error("delete object", &error)),
        }
    }

    #[cfg(test)]
    async fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.put_owned(key, bytes.to_vec()).await
    }

    async fn prepare_set(
        &self,
        note_id: String,
        attachments: Vec<NoteAttachment>,
    ) -> anyhow::Result<PreparedS3Set> {
        validate_note_id(&note_id)?;
        let mut canonical = Vec::with_capacity(attachments.len());
        let mut canonical_set = HashSet::with_capacity(attachments.len());
        let mut metadata = Vec::with_capacity(attachments.len());
        for attachment in &attachments {
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

        let note_guard = self.coordination.lock_for(&note_id).lock_owned().await;
        let staging_prefix = staging_prefix(
            &self.prefix,
            &note_id,
            &uuid::Uuid::new_v4().simple().to_string(),
        )?;
        let mut prepared = PreparedS3Set {
            store: self.clone(),
            note_id,
            staging_prefix,
            staged_keys: Vec::new(),
            metadata,
            note_guard: Some(note_guard),
            cleanup_armed: true,
        };
        for (attachment, relative) in attachments.into_iter().zip(canonical) {
            let key = format!("{}{relative}", prepared.staging_prefix);
            // The request may have committed remotely even if its response is
            // lost, so include the current key in idempotent cleanup first.
            prepared.staged_keys.push(key.clone());
            if let Err(error) = self.put_owned(&key, attachment.content).await {
                let cleanup = await_cleanup(prepared.schedule_cleanup()?).await;
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => {
                        Err(error.context(format!("staging cleanup also failed: {cleanup_error}")))
                    }
                };
            }
        }

        Ok(prepared)
    }

    async fn prepare_put_mutation(
        &self,
        note_id: String,
        attachment: NoteAttachment,
    ) -> anyhow::Result<PreparedS3Mutation> {
        validate_note_id(&note_id)?;
        let relative = crate::path::canonical_relative_path(&attachment.path)?;
        let final_key = final_key(&self.prefix, &note_id, &relative)?;
        let staging_prefix = staging_prefix(
            &self.prefix,
            &note_id,
            &uuid::Uuid::new_v4().simple().to_string(),
        )?;
        let staging_key = format!("{staging_prefix}{relative}");
        let note_guard = self.coordination.lock_for(&note_id).lock_owned().await;
        let mut prepared = PreparedS3Mutation {
            store: self.clone(),
            operation: S3MutationOperation::Put {
                staging_key: staging_key.clone(),
                final_key,
            },
            note_guard: Some(note_guard),
            cleanup_armed: true,
        };

        if let Err(error) = self.put_owned(&staging_key, attachment.content).await {
            let error = error.context(format!("at {}", self.key_location(&staging_key)));
            let cleanup = await_cleanup(prepared.schedule_cleanup()?).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => {
                    Err(error.context(format!("staging cleanup also failed: {cleanup_error}")))
                }
            };
        }

        Ok(prepared)
    }

    async fn prepare_delete_mutation(
        &self,
        note_id: String,
        path: String,
    ) -> anyhow::Result<PreparedS3Mutation> {
        validate_note_id(&note_id)?;
        let final_key = final_key(&self.prefix, &note_id, &path)?;
        let note_guard = self.coordination.lock_for(&note_id).lock_owned().await;
        Ok(PreparedS3Mutation {
            store: self.clone(),
            operation: S3MutationOperation::Delete { final_key },
            note_guard: Some(note_guard),
            cleanup_armed: false,
        })
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

    async fn delete_keys(&self, keys: &[String]) -> anyhow::Result<()> {
        for batch in keys.chunks(S3_DELETE_BATCH_SIZE) {
            let mut pending = batch.to_vec();
            for attempt in 0..S3_MAX_ATTEMPTS {
                let expected = pending.iter().cloned().collect::<HashSet<_>>();
                let objects = pending
                    .iter()
                    .map(|key| ObjectIdentifier::builder().key(key).build())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| anyhow::anyhow!("S3 delete objects failed (invalid key)"))?;
                let delete = Delete::builder()
                    .set_objects(Some(objects))
                    .quiet(true)
                    .build()
                    .map_err(|_| anyhow::anyhow!("S3 delete objects failed (invalid request)"))?;
                let no_sdk_retries = aws_sdk_s3::config::Builder::new()
                    .retry_config(RetryConfig::standard().with_max_attempts(1));
                let response = self
                    .client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .customize()
                    .config_override(no_sdk_retries)
                    .send()
                    .await;
                let output = match response {
                    Ok(output) => output,
                    Err(error) if retryable_sdk_error(&error) && attempt + 1 < S3_MAX_ATTEMPTS => {
                        sleep_before_delete_retry(attempt).await;
                        continue;
                    }
                    Err(error) => return Err(safe_sdk_error("delete objects", &error)),
                };
                if output.errors().is_empty() {
                    pending.clear();
                    break;
                }

                let mut retry = Vec::new();
                for error in output.errors() {
                    let Some(key) = error.key().filter(|key| expected.contains(*key)) else {
                        anyhow::bail!("S3 delete objects failed (malformed object error)");
                    };
                    if !transient_delete_code(error.code()) {
                        anyhow::bail!("S3 delete objects failed (permanent object error)");
                    }
                    retry.push(key.to_owned());
                }
                retry.sort();
                retry.dedup();
                pending = retry;
                if attempt + 1 == S3_MAX_ATTEMPTS {
                    anyhow::bail!("S3 delete objects failed (transient retries exhausted)");
                }
                sleep_before_delete_retry(attempt).await;
            }
            if !pending.is_empty() {
                anyhow::bail!("S3 delete objects failed (transient retries exhausted)");
            }
        }
        Ok(())
    }

    async fn list_keys(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        let mut seen_tokens = HashSet::new();
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .max_keys(S3_PAGE_SIZE);
            if let Some(token) = continuation.as_deref() {
                request = request.continuation_token(token);
            }
            let output = request
                .send()
                .await
                .map_err(|error| safe_sdk_error("list objects", &error))?;
            keys.extend(
                output
                    .contents()
                    .iter()
                    .filter_map(|object| object.key())
                    .map(ToOwned::to_owned),
            );
            if output.is_truncated().unwrap_or(false) {
                let token = output
                    .next_continuation_token()
                    .filter(|token| !token.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("S3 list objects failed (missing continuation token)")
                    })?
                    .to_string();
                if !seen_tokens.insert(token.clone()) {
                    anyhow::bail!("S3 list objects failed (repeated continuation token)");
                }
                continuation = Some(token);
            } else {
                break;
            }
        }
        Ok(keys)
    }

    async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<()> {
        let keys = self.list_keys(prefix).await?;
        self.delete_keys(&keys).await
    }

    async fn publish_set(&self, prepared: Box<PreparedS3Set>) -> anyhow::Result<()> {
        let final_prefix = final_prefix(&self.prefix, &prepared.note_id)?;
        let expected = prepared
            .metadata
            .iter()
            .map(|attachment| final_key(&self.prefix, &prepared.note_id, &attachment.path))
            .collect::<anyhow::Result<HashSet<_>>>()?;

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

        let obsolete = match self.list_keys(&final_prefix).await {
            Ok(keys) => keys
                .into_iter()
                .filter(|key| !expected.contains(key))
                .collect::<Vec<_>>(),
            Err(error) => {
                let staging_location = prepared.staging_location();
                eprintln!(
                    "S3 attachment replacement failed after database commit; \
                     staged objects retained at {staging_location}"
                );
                return Err(error.context(format!(
                    "database committed; staged objects retained at {staging_location}"
                )));
            }
        };
        if let Err(error) = self.delete_keys(&obsolete).await {
            let staging_location = prepared.staging_location();
            eprintln!(
                "S3 attachment replacement failed after database commit; \
                 staged objects retained at {staging_location}"
            );
            return Err(error.context(format!(
                "database committed; staged objects retained at {staging_location}"
            )));
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

    async fn publish_mutation(&self, prepared: Box<PreparedS3Mutation>) -> anyhow::Result<()> {
        match &prepared.operation {
            S3MutationOperation::Put {
                staging_key,
                final_key,
            } => {
                let copy_result = self
                    .client
                    .copy_object()
                    .copy_source(copy_source(&self.bucket, staging_key))
                    .bucket(&self.bucket)
                    .key(final_key)
                    .send()
                    .await
                    .map_err(|error| safe_sdk_error("copy object", &error));
                #[cfg(test)]
                pause_s3_operation(S3OperationPhase::Copy).await;
                if let Err(error) = copy_result {
                    let staging_location = self.key_location(staging_key);
                    let final_location = self.key_location(final_key);
                    return Err(error.context(format!(
                        "database committed; copy to {final_location} failed; \
                         staged object retained at {staging_location}"
                    )));
                }
                if let Err(error) = self.delete_key(staging_key).await {
                    let staging_location = self.key_location(staging_key);
                    return Err(error.context(format!(
                        "database committed; final object copied; \
                         staging cleanup uncertain at {staging_location}"
                    )));
                }
            }
            S3MutationOperation::Delete { final_key } => {
                if let Err(error) = self.delete_key(final_key).await {
                    return Err(error.context(format!(
                        "database committed; delete failed at {}",
                        self.key_location(final_key)
                    )));
                }
            }
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

    fn key_location(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
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

impl S3SingleCleanupWork {
    async fn run(self) -> anyhow::Result<()> {
        let result = self.store.delete_key(&self.staging_key).await;
        if let Err(error) = result.as_ref() {
            eprintln!(
                "S3 staged attachment cleanup failed at {}: {error}",
                self.staging_location
            );
        }
        result
    }
}

async fn await_cleanup(cleanup: tokio::task::JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    cleanup.await.map_err(safe_cleanup_join_error)?
}

fn safe_cleanup_join_error(_error: tokio::task::JoinError) -> anyhow::Error {
    anyhow::anyhow!("S3 staging cleanup task failed")
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

impl PreparedS3Mutation {
    fn take_cleanup_work(&mut self) -> Option<S3SingleCleanupWork> {
        if !self.cleanup_armed {
            return None;
        }
        self.cleanup_armed = false;
        let note_guard = self.note_guard.take()?;
        let S3MutationOperation::Put { staging_key, .. } = &self.operation else {
            drop(note_guard);
            return None;
        };
        Some(S3SingleCleanupWork {
            store: self.store.clone(),
            staging_key: staging_key.clone(),
            staging_location: self.store.key_location(staging_key),
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

impl Drop for PreparedS3Mutation {
    fn drop(&mut self) {
        if !self.cleanup_armed {
            return;
        }
        let staging_location = match &self.operation {
            S3MutationOperation::Put { staging_key, .. } => self.store.key_location(staging_key),
            S3MutationOperation::Delete { .. } => return,
        };
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
        await_cleanup(self.schedule_cleanup()?).await
    }
}

#[async_trait::async_trait]
impl crate::PreparedAttachmentMutation for PreparedS3Mutation {
    async fn publish(mut self: Box<Self>) -> anyhow::Result<()> {
        self.cleanup_armed = false;
        let store = self.store.clone();
        tokio::spawn(async move { store.publish_mutation(self).await })
            .await
            .map_err(|_| anyhow::anyhow!("S3 attachment mutation publication task failed"))?
    }

    async fn abort(mut self: Box<Self>) -> anyhow::Result<()> {
        await_cleanup(self.schedule_cleanup()?).await
    }
}

#[async_trait::async_trait]
impl crate::AttachmentStore for S3AttachmentStore {
    async fn put_immutable(
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
        if self.key_exists(&key).await? {
            anyhow::bail!("attachment object already exists");
        }
        let expected = crate::ObjectMetadata {
            size_bytes: request.bytes.len() as u64,
            checksum_sha256: request.checksum_sha256,
        };
        match self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .if_none_match("*")
            .metadata("checksum-sha256", &expected.checksum_sha256)
            .body(ByteStream::from(request.bytes))
            .send()
            .await
        {
            Ok(_) => {}
            Err(error)
                if error.code() == Some("PreconditionFailed")
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 412) =>
            {
                anyhow::bail!("attachment object already exists")
            }
            Err(error) => return Err(safe_sdk_error("put object", &error)),
        }
        let verified = self
            .head_key(&key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("S3 put object verification failed (missing object)"))?;
        if verified != expected {
            anyhow::bail!("S3 put object verification failed (metadata mismatch)");
        }
        Ok(crate::StoredObject {
            object_key,
            size_bytes: verified.size_bytes,
            checksum_sha256: verified.checksum_sha256,
        })
    }

    async fn read_object(&self, object_key: &str) -> anyhow::Result<Vec<u8>> {
        self.read_key(&object_key_with_prefix(&self.prefix, object_key)?)
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

    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn crate::PreparedAttachmentSet>> {
        let store = self.clone();
        let note_id = note_id.to_string();
        let attachments = attachments.to_vec();
        let prepared = tokio::spawn(async move { store.prepare_set(note_id, attachments).await })
            .await
            .map_err(|_| anyhow::anyhow!("S3 attachment preparation task failed"))??;
        Ok(Box::new(prepared))
    }

    async fn prepare_put(
        &self,
        note_id: &str,
        attachment: &NoteAttachment,
    ) -> anyhow::Result<Box<dyn crate::PreparedAttachmentMutation>> {
        let store = self.clone();
        let note_id = note_id.to_string();
        let attachment = attachment.clone();
        let prepared =
            tokio::spawn(async move { store.prepare_put_mutation(note_id, attachment).await })
                .await
                .map_err(|_| anyhow::anyhow!("S3 attachment mutation preparation task failed"))??;
        Ok(Box::new(prepared))
    }

    async fn prepare_delete(
        &self,
        note_id: &str,
        path: &str,
    ) -> anyhow::Result<Box<dyn crate::PreparedAttachmentMutation>> {
        let store = self.clone();
        let note_id = note_id.to_string();
        let path = path.to_string();
        let prepared =
            tokio::spawn(async move { store.prepare_delete_mutation(note_id, path).await })
                .await
                .map_err(|_| anyhow::anyhow!("S3 attachment mutation preparation task failed"))??;
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
        self.delete_prefix(&final_prefix(&self.prefix, note_id)?)
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

fn transient_delete_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("InternalError" | "SlowDown" | "ServiceUnavailable" | "RequestTimeout")
    )
}

fn retryable_sdk_error<E>(error: &SdkError<E>) -> bool
where
    E: ProvideErrorMetadata,
{
    matches!(
        error,
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_)
    ) || transient_delete_code(error.code())
}

async fn sleep_before_delete_retry(attempt: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(50_u64 << attempt.min(3))).await;
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

fn object_key_with_prefix(prefix: &str, object_key: &str) -> anyhow::Result<String> {
    let prefix = normalize_prefix(prefix)?;
    let object_key = crate::path::canonical_object_key(object_key)?;
    Ok(join_key(&prefix, &object_key))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum S3OperationPhase {
    Put,
    Copy,
    Delete,
}

#[cfg(test)]
struct S3OperationPause {
    phase: S3OperationPhase,
    reached: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
fn s3_operation_pauses() -> &'static StdMutex<Vec<S3OperationPause>> {
    static PAUSES: std::sync::OnceLock<StdMutex<Vec<S3OperationPause>>> =
        std::sync::OnceLock::new();
    PAUSES.get_or_init(|| StdMutex::new(Vec::new()))
}

#[cfg(test)]
fn install_s3_operation_pause(
    phase: S3OperationPhase,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
    let (resume_sender, resume_receiver) = tokio::sync::oneshot::channel();
    s3_operation_pauses()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(S3OperationPause {
            phase,
            reached: reached_sender,
            resume: resume_receiver,
        });
    (reached_receiver, resume_sender)
}

#[cfg(test)]
async fn pause_s3_operation(phase: S3OperationPhase) {
    let pause = {
        let mut pauses = s3_operation_pauses()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pauses
            .iter()
            .position(|pause| pause.phase == phase)
            .map(|index| pauses.swap_remove(index))
    };
    if let Some(pause) = pause {
        let _ = pause.reached.send(());
        let _ = pause.resume.await;
    }
}

#[cfg(test)]
mod tests {
    // AWS configuration is process-global, so the standard mutex intentionally
    // remains held for each complete asynchronous test operation.
    #![allow(clippy::await_holding_lock)]

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

    fn test_attachment(path: &str, content: &[u8]) -> NoteAttachment {
        NoteAttachment {
            id: path.into(),
            path: path.into(),
            mime: "application/octet-stream".into(),
            description: String::new(),
            content: content.to_vec(),
        }
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
    async fn immutable_put_uses_prefixed_object_key_metadata_and_head_verification() {
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
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "PUT")
                .count(),
            1
        );
        assert!(requests.iter().all(|request| {
            !request.url.path().contains("note-1") && !request.url.path().contains(".staging")
        }));
    }

    #[tokio::test]
    async fn immutable_collision_is_rejected_before_put() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("collision-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("collision-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/collision"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", "7")
                    .insert_header("x-amz-meta-checksum-sha256", checksum),
            )
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

    #[tokio::test]
    async fn immutable_put_conditionally_rejects_a_collision_after_absent_preflight() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("conditional-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("conditional-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let checksum = "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5";
        Mock::given(method("HEAD"))
            .and(path("/agent-note/generations/raced"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/generations/raced"))
            .and(header_regex("if-none-match", r"^\*$"))
            .respond_with(ResponseTemplate::new(412).set_body_raw(
                "<Error><Code>PreconditionFailed</Code><Message>private collision detail</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .put_immutable(crate::PutObjectRequest {
                object_key: "generations/raced".into(),
                bytes: b"payload".to_vec(),
                checksum_sha256: checksum.into(),
            })
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(error, "attachment object already exists");
        assert!(!error.contains("private collision detail"));
        assert!(!error.contains("conditional-access"));
        assert!(!error.contains("conditional-secret"));
    }

    #[tokio::test]
    async fn immutable_read_head_and_delete_target_one_exact_key_and_report_missing_delete() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("lifecycle-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("lifecycle-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
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
            store.head_object("generations/key").await.unwrap(),
            crate::ObjectMetadata {
                size_bytes: 7,
                checksum_sha256: checksum.into(),
            }
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
            .prepare_set("note-1".into(), vec![attachment])
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
    async fn single_put_uploads_copies_and_cleans_only_its_staging_key() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("put-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("put-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/nested/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/nested/file.txt"))
            .and(header_regex(
                "x-amz-copy-source",
                r"^agent-note/\.staging/note-1/[0-9a-f]{32}/nested/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/nested/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        test_store(server.uri())
            .await
            .prepare_put(
                "note-1",
                &test_attachment("./nested/file.txt", b"replacement"),
            )
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let staging_path = requests[0].url.path();
        assert_eq!(requests[0].method.as_str(), "PUT");
        assert!(staging_path.starts_with("/agent-note/.staging/note-1/"));
        assert!(staging_path.ends_with("/nested/file.txt"));
        assert_eq!(requests[1].method.as_str(), "PUT");
        assert_eq!(requests[1].url.path(), "/agent-note/note-1/nested/file.txt");
        assert_eq!(
            requests[1]
                .headers
                .get("x-amz-copy-source")
                .unwrap()
                .to_str()
                .unwrap(),
            staging_path.trim_start_matches('/')
        );
        assert_eq!(requests[2].method.as_str(), "DELETE");
        assert_eq!(requests[2].url.path(), staging_path);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "GET")
                .count(),
            0,
            "single put must never list the final note prefix"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "POST")
                .count(),
            0,
            "single put must never batch-delete sibling final objects"
        );
        assert!(requests
            .iter()
            .all(|request| !request.url.path().contains("sibling")));
    }

    #[tokio::test]
    async fn failed_single_put_upload_only_cleans_its_staging_key() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("failed-put-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("failed-put-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(503).set_body_raw(
                "<Error><Code>ServiceUnavailable</Code><Message>private</Message></Error>",
                "application/xml",
            ))
            .expect(4)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .prepare_put("note-1", &test_attachment("target.txt", b"payload"))
            .await
            .err()
            .expect("ambiguous staging upload must fail");
        assert!(format!("{error:#}").contains("S3 put object failed"));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 5);
        let staging_path = requests[0].url.path();
        assert!(staging_path.starts_with("/agent-note/.staging/note-1/"));
        assert!(staging_path.ends_with("/target.txt"));
        for request in &requests[..4] {
            assert_eq!(request.method.as_str(), "PUT");
            assert_eq!(request.url.path(), staging_path);
            assert!(request.headers.get("x-amz-copy-source").is_none());
        }
        assert_eq!(requests[4].method.as_str(), "DELETE");
        assert_eq!(requests[4].url.path(), staging_path);
        assert!(requests
            .iter()
            .all(|request| request.url.path() != "/agent-note/note-1/target.txt"));
    }

    #[tokio::test]
    async fn failed_single_put_copy_retains_staging_without_cleanup_delete() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("failed-copy-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("failed-copy-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/target.txt"))
            .and(header_regex(
                "x-amz-copy-source",
                r"^agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<Error><Code>AccessDenied</Code><Message>private</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .prepare_put("note-1", &test_attachment("target.txt", b"payload"))
            .await
            .unwrap()
            .publish()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("staged object retained at s3://agent-note/.staging/note-1/"));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let staging_path = requests[0].url.path();
        assert_eq!(requests[0].method.as_str(), "PUT");
        assert!(staging_path.starts_with("/agent-note/.staging/note-1/"));
        assert_eq!(requests[1].method.as_str(), "PUT");
        assert_eq!(requests[1].url.path(), "/agent-note/note-1/target.txt");
        assert_eq!(
            requests[1]
                .headers
                .get("x-amz-copy-source")
                .unwrap()
                .to_str()
                .unwrap(),
            staging_path.trim_start_matches('/')
        );
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() != "DELETE"));
    }

    #[tokio::test]
    async fn failed_single_put_staging_cleanup_happens_after_final_copy() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("failed-cleanup-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("failed-cleanup-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/target.txt"))
            .and(header_regex(
                "x-amz-copy-source",
                r"^agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/target\.txt$",
            ))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<Error><Code>AccessDenied</Code><Message>private</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .prepare_put("note-1", &test_attachment("target.txt", b"payload"))
            .await
            .unwrap()
            .publish()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "database committed; final object copied; staging cleanup uncertain at \
             s3://agent-note/.staging/note-1/"
        ));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let staging_path = requests[0].url.path();
        assert_eq!(requests[0].method.as_str(), "PUT");
        assert!(staging_path.starts_with("/agent-note/.staging/note-1/"));
        assert_eq!(requests[1].method.as_str(), "PUT");
        assert_eq!(requests[1].url.path(), "/agent-note/note-1/target.txt");
        assert_eq!(
            requests[1]
                .headers
                .get("x-amz-copy-source")
                .unwrap()
                .to_str()
                .unwrap(),
            staging_path.trim_start_matches('/')
        );
        assert_eq!(requests[2].method.as_str(), "DELETE");
        assert_eq!(requests[2].url.path(), staging_path);
    }

    #[tokio::test]
    async fn single_put_abort_deletes_staging_without_changing_the_final_object() {
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
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/note-1/file.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"old".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        store
            .prepare_put("note-1", &test_attachment("file.txt", b"new"))
            .await
            .unwrap()
            .abort()
            .await
            .unwrap();

        assert_eq!(store.read("note-1", "file.txt").await.unwrap(), b"old");
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| {
                !(request.method.as_str() == "PUT"
                    && request.url.path() == "/agent-note/note-1/file.txt")
            }));
    }

    #[tokio::test]
    async fn single_delete_targets_only_the_canonical_key_and_missing_is_success() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("delete-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("delete-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/agent-note/note-1/nested/file.txt"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/agent-note/note-1/missing.txt"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(
                "<Error><Code>NoSuchKey</Code><Message>private</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let prepared = store
            .prepare_delete("note-1", "./nested/file.txt")
            .await
            .unwrap();
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "delete preparation must not mutate S3"
        );
        prepared.publish().await.unwrap();
        store
            .prepare_delete("note-1", "missing.txt")
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() == "DELETE"));
    }

    #[tokio::test]
    async fn single_mutations_share_note_coordination_with_complete_sets() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("lock-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("lock-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let store = test_store(server.uri()).await;
        let complete = store.prepare("note-1", &[]).await.unwrap();

        let same_store = store.clone();
        let mut same =
            tokio::spawn(async move { same_store.prepare_delete("note-1", "same.txt").await });
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut same)
            .await
            .is_err());
        let different = tokio::time::timeout(
            Duration::from_secs(2),
            store.prepare_delete("note-2", "different.txt"),
        )
        .await
        .expect("different notes must remain concurrent")
        .unwrap();
        different.abort().await.unwrap();

        complete.abort().await.unwrap();
        let same = tokio::time::timeout(Duration::from_secs(2), same)
            .await
            .expect("same-note mutation must proceed after complete set")
            .unwrap()
            .unwrap();

        let complete_store = store.clone();
        let mut next_complete =
            tokio::spawn(async move { complete_store.prepare("note-1", &[]).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut next_complete)
                .await
                .is_err()
        );
        same.abort().await.unwrap();
        next_complete.await.unwrap().unwrap().abort().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_single_put_prepare_holds_lock_through_detached_cleanup() {
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
        let (put_reached, resume_put) = install_s3_operation_pause(S3OperationPhase::Put);
        let (delete_reached, resume_delete) = install_s3_operation_pause(S3OperationPhase::Delete);
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let preparing_store = store.clone();
        let preparing = tokio::spawn(async move {
            preparing_store
                .prepare_put("note-1", &test_attachment("file.txt", b"payload"))
                .await
        });
        put_reached.await.unwrap();
        preparing.abort();
        assert!(matches!(
            preparing.await,
            Err(error) if error.is_cancelled()
        ));
        let same_store = store.clone();
        let same =
            tokio::spawn(async move { same_store.prepare_delete("note-1", "next.txt").await });
        tokio::task::yield_now().await;
        assert!(!same.is_finished());
        assert!(store.coordination.lock_for("note-1").try_lock().is_err());

        resume_put.send(()).unwrap();
        delete_reached.await.unwrap();
        assert!(!same.is_finished());
        assert!(store.coordination.lock_for("note-1").try_lock().is_err());

        resume_delete.send(()).unwrap();
        let prepared = tokio::time::timeout(Duration::from_secs(2), same)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        prepared.abort().await.unwrap();
        assert!(store.coordination.lock_for("note-1").try_lock().is_ok());
    }

    #[tokio::test]
    async fn cancelled_single_put_publish_holds_lock_until_detached_publication_finishes() {
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
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/file.txt"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let prepared = store
            .prepare_put("note-1", &test_attachment("file.txt", b"payload"))
            .await
            .unwrap();
        let (copy_reached, resume_copy) = install_s3_operation_pause(S3OperationPhase::Copy);
        let (delete_reached, resume_delete) = install_s3_operation_pause(S3OperationPhase::Delete);
        let publishing = tokio::spawn(async move { prepared.publish().await });
        copy_reached.await.unwrap();
        publishing.abort();
        assert!(publishing.await.unwrap_err().is_cancelled());

        let same_store = store.clone();
        let same =
            tokio::spawn(async move { same_store.prepare_delete("note-1", "next.txt").await });
        tokio::task::yield_now().await;
        assert!(!same.is_finished());
        assert!(store.coordination.lock_for("note-1").try_lock().is_err());

        resume_copy.send(()).unwrap();
        delete_reached.await.unwrap();
        assert!(!same.is_finished());
        assert!(store.coordination.lock_for("note-1").try_lock().is_err());

        resume_delete.send(()).unwrap();
        let prepared = tokio::time::timeout(Duration::from_secs(2), same)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        prepared.abort().await.unwrap();
        assert!(store.coordination.lock_for("note-1").try_lock().is_ok());
    }

    #[tokio::test]
    async fn cancelled_single_put_abort_holds_lock_until_detached_cleanup_finishes() {
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
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        let prepared = store
            .prepare_put("note-1", &test_attachment("file.txt", b"payload"))
            .await
            .unwrap();
        let (delete_reached, resume_delete) = install_s3_operation_pause(S3OperationPhase::Delete);
        let aborting = tokio::spawn(async move { prepared.abort().await });
        delete_reached.await.unwrap();
        aborting.abort();
        assert!(aborting.await.unwrap_err().is_cancelled());

        let same_store = store.clone();
        let same =
            tokio::spawn(async move { same_store.prepare_delete("note-1", "next.txt").await });
        tokio::task::yield_now().await;
        assert!(!same.is_finished());
        assert!(store.coordination.lock_for("note-1").try_lock().is_err());

        resume_delete.send(()).unwrap();
        let prepared = tokio::time::timeout(Duration::from_secs(2), same)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        prepared.abort().await.unwrap();
        assert!(store.coordination.lock_for("note-1").try_lock().is_ok());
    }

    #[tokio::test]
    async fn single_mutation_errors_include_safe_object_locations_without_credentials() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("private-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("private-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let delete_server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/agent-note/note-1/private-name.txt"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<Error><Code>AccessDenied</Code><Message>private-server-body</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&delete_server)
            .await;

        let delete_rendered = test_store(delete_server.uri())
            .await
            .prepare_delete("note-1", "./private-name.txt")
            .await
            .unwrap()
            .publish()
            .await
            .unwrap_err()
            .to_string();

        assert!(delete_rendered.contains("s3://agent-note/note-1/private-name.txt"));

        let put_server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/private-name\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&put_server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/note-1/private-name.txt"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<Error><Code>AccessDenied</Code><Message>private-copy-body</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&put_server)
            .await;
        let put_rendered = test_store(put_server.uri())
            .await
            .prepare_put(
                "note-1",
                &test_attachment("./private-name.txt", b"private-content"),
            )
            .await
            .unwrap()
            .publish()
            .await
            .unwrap_err()
            .to_string();
        assert!(put_rendered.contains("s3://agent-note/note-1/private-name.txt"));
        assert!(put_rendered.contains("s3://agent-note/.staging/note-1/"));

        for rendered in [&delete_rendered, &put_rendered] {
            for secret in [
                "private-access",
                "private-secret",
                "private-server-body",
                "private-copy-body",
                &delete_server.uri(),
                &put_server.uri(),
            ] {
                assert!(!rendered.contains(secret));
            }
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
        Mock::given(method("GET"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
                "application/xml",
            ))
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
                "note-1".into(),
                vec![NoteAttachment {
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
                    "note-1".into(),
                    vec![NoteAttachment {
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
        let mut second =
            tokio::spawn(async move { second_store.prepare_set("note-1".into(), vec![]).await });
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
                "note-1".into(),
                vec![NoteAttachment {
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
        let mut second =
            tokio::spawn(async move { second_store.prepare_set("note-1".into(), vec![]).await });
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
                "note-1".into(),
                vec![NoteAttachment {
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
        Mock::given(method("GET"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
                "application/xml",
            ))
            .expect(1)
            .mount(&cleanup_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(503).set_body_raw(
                "<Error><Code>PrivateDeleteCode</Code><Message>private delete body</Message></Error>",
                "application/xml",
            ))
            .expect(1)
            .mount(&cleanup_server)
            .await;
        let cleanup_store = test_store(cleanup_server.uri()).await;
        let prepared = cleanup_store
            .prepare_set(
                "note-1".into(),
                vec![NoteAttachment {
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

    #[tokio::test]
    async fn publication_failure_retains_staging_and_reports_only_safe_location() {
        use wiremock::matchers::header_exists;

        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("failure-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("failure-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/tests/\.staging/note-1/[0-9a-f]{32}/file\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/agent-note/tests/note-1/file.txt"))
            .and(header_exists("x-amz-copy-source"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let store = S3AttachmentStore::new(S3AttachmentConfig {
            bucket: "agent-note".into(),
            prefix: "tests".into(),
            region: Some("us-east-1".into()),
            endpoint: Some(server.uri()),
            force_path_style: true,
        })
        .await
        .unwrap();
        let error = store
            .prepare(
                "note-1",
                &[NoteAttachment {
                    id: "file".into(),
                    path: "./file.txt".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    content: b"payload".to_vec(),
                }],
            )
            .await
            .unwrap()
            .publish()
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains(
            "database committed; staged objects retained at \
             s3://agent-note/tests/.staging/note-1/"
        ));
        assert!(!error.contains("failure-access"));
        assert!(!error.contains("failure-secret"));
        assert!(!error.contains(&server.uri()));
    }

    #[tokio::test]
    async fn truncated_list_with_an_empty_token_fails_safely() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("list-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("list-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken></NextContinuationToken></ListBucketResult>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .list_keys("note-1/")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "S3 list objects failed (missing continuation token)"
        );
    }

    #[tokio::test]
    async fn repeated_list_token_fails_safely_instead_of_looping() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("list-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("list-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken>same-token</NextContinuationToken></ListBucketResult>",
                "application/xml",
            ))
            .expect(2)
            .mount(&server)
            .await;

        let error = test_store(server.uri())
            .await
            .list_keys("note-1/")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "S3 list objects failed (repeated continuation token)"
        );
    }

    #[tokio::test]
    async fn partial_transient_delete_errors_retry_only_the_failed_key() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("delete-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("delete-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let responder_calls = calls.clone();
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(move |request: &Request| {
                let call = responder_calls.fetch_add(1, Ordering::SeqCst);
                let body = String::from_utf8_lossy(&request.body);
                if call == 0 {
                    assert!(body.contains("<Key>first</Key>"));
                    assert!(body.contains("<Key>second</Key>"));
                    ResponseTemplate::new(200).set_body_raw(
                        "<DeleteResult><Error><Key>second</Key><Code>InternalError</Code>\
                         <Message>private</Message></Error></DeleteResult>",
                        "application/xml",
                    )
                } else {
                    assert!(!body.contains("<Key>first</Key>"));
                    assert!(body.contains("<Key>second</Key>"));
                    ResponseTemplate::new(200)
                        .set_body_raw("<DeleteResult></DeleteResult>", "application/xml")
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        test_store(server.uri())
            .await
            .delete_keys(&["first".into(), "second".into()])
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn transient_delete_transport_failures_make_at_most_four_total_requests() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("delete-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("delete-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(503).set_body_raw(
                "<Error><Code>ServiceUnavailable</Code><Message>private</Message></Error>",
                "application/xml",
            ))
            .expect(4)
            .mount(&server)
            .await;

        let rendered = test_store(server.uri())
            .await
            .delete_keys(&["requested".into()])
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(rendered, "S3 delete objects failed (service-unavailable)");
        assert!(!rendered.contains("private"));
    }

    #[tokio::test]
    async fn replacement_copies_every_expected_object_before_any_list_or_delete() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("order-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("order-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(
                r"^/agent-note/\.staging/note-1/[0-9a-f]{32}/(first|second)\.txt$",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(r"^/agent-note/note-1/(first|second)\.txt$"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agent-note/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<ListBucketResult><IsTruncated>false</IsTruncated><Contents>\
                 <Key>note-1/obsolete.txt</Key></Contents></ListBucketResult>",
                "application/xml",
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/agent-note/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<DeleteResult></DeleteResult>", "application/xml"),
            )
            .expect(2)
            .mount(&server)
            .await;

        let store = test_store(server.uri()).await;
        store
            .prepare(
                "note-1",
                &[
                    NoteAttachment {
                        id: "first".into(),
                        path: "first.txt".into(),
                        mime: "text/plain".into(),
                        description: String::new(),
                        content: b"first".to_vec(),
                    },
                    NoteAttachment {
                        id: "second".into(),
                        path: "second.txt".into(),
                        mime: "text/plain".into(),
                        description: String::new(),
                        content: b"second".to_vec(),
                    },
                ],
            )
            .await
            .unwrap()
            .publish()
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let publish_requests = requests
            .iter()
            .skip_while(|request| {
                !(request.method.as_str() == "PUT"
                    && request.url.path().starts_with("/agent-note/note-1/"))
            })
            .collect::<Vec<_>>();
        assert_eq!(publish_requests.len(), 5);
        assert_eq!(publish_requests[0].method.as_str(), "PUT");
        assert_eq!(publish_requests[1].method.as_str(), "PUT");
        assert_eq!(publish_requests[2].method.as_str(), "GET");
        assert_eq!(publish_requests[3].method.as_str(), "POST");
        assert_eq!(publish_requests[4].method.as_str(), "POST");
    }

    #[tokio::test]
    async fn malformed_or_permanent_delete_errors_never_echo_server_fields() {
        let _lock = AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = AwsEnvGuard::apply(&[
            ("AWS_ACCESS_KEY_ID", Some("delete-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("delete-secret")),
            ("AWS_SESSION_TOKEN", None),
            ("AWS_EC2_METADATA_DISABLED", Some("true")),
            ("AWS_ENDPOINT_URL_S3", None),
        ]);

        for (response, expected) in [
            (
                "<DeleteResult><Error><Key>unrequested-private-key</Key>\
                 <Code>InternalError</Code><Message>private malformed message</Message>\
                 </Error></DeleteResult>",
                "S3 delete objects failed (malformed object error)",
            ),
            (
                "<DeleteResult><Error><Key>requested</Key><Code>PrivatePermanentCode</Code>\
                 <Message>private permanent message</Message></Error></DeleteResult>",
                "S3 delete objects failed (permanent object error)",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/agent-note/"))
                .respond_with(ResponseTemplate::new(200).set_body_raw(response, "application/xml"))
                .expect(1)
                .mount(&server)
                .await;

            let rendered = test_store(server.uri())
                .await
                .delete_keys(&["requested".into()])
                .await
                .unwrap_err()
                .to_string();

            assert_eq!(rendered, expected);
            for secret in [
                "unrequested-private-key",
                "PrivatePermanentCode",
                "private malformed message",
                "private permanent message",
                "delete-access",
                "delete-secret",
            ] {
                assert!(!rendered.contains(secret));
            }
        }
    }

    #[tokio::test]
    async fn cleanup_join_errors_have_a_fixed_safe_message() {
        let cleanup = tokio::spawn(std::future::pending::<()>());
        cleanup.abort();
        let join_error = cleanup.await.unwrap_err();

        let rendered = safe_cleanup_join_error(join_error).to_string();
        assert_eq!(rendered, "S3 staging cleanup task failed");
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
