# S3-Compatible Attachments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an AWS S3, MinIO, R2, and S3-compatible attachment adapter that uses the existing prepared-write boundary, the standard AWS credential chain, bounded retries, canonical keys, direct single-object reads, and safe operational errors.

**Architecture:** `note-attachments` owns an `S3AttachmentStore` that stages complete note attachment sets under unique object prefixes, while the existing pipelines continue to commit database metadata before publishing or aborting the prepared set. Publication copies staged objects to canonical final keys, removes obsolete final objects and then staging objects; list and delete helpers paginate and batch safely. `note-server` only translates resolved `AttachmentConfig::S3` into the adapter, so transports and pipelines remain backend-neutral.

**Tech Stack:** Rust 2021 on Rust 1.95, AWS SDK for Rust (`aws-config` 1.9.0 and `aws-sdk-s3` 1.138.1, both MSRV 1.94.1), Tokio, async-trait, wiremock 0.6.5, MinIO Docker integration tests, Axum, Yew/Wasm.

---

## References, prerequisite, and accepted limitation

- Approved design: `docs/superpowers/specs/2026-07-18-external-adapters-design.md`, especially sections 8, 9, 10, 11, and 12.4.
- Required prerequisite: complete `docs/superpowers/plans/2026-07-18-config-attachment-boundary.md` first. This plan relies on its exact object-safe `AttachmentStore`, `PreparedAttachmentSet`, `AttachmentStoreInfo`, `FilesystemAttachmentStore`, typed `AttachmentConfig::S3`, async pipeline orchestration, backend-neutral System UI, and direct `get_note_attachment` pipeline.
- AWS retry semantics: `RetryConfig::with_max_attempts` counts the initial request. This plan sets `S3_MAX_ATTEMPTS = 4`, meaning one initial request plus at most three retries. See <https://docs.aws.amazon.com/sdk-for-rust/latest/dg/retries.html>.
- Credential behavior: production construction calls `aws_config::defaults(BehaviorVersion::latest())` and does not inject credentials, preserving the standard environment/profile/web-identity/container/instance credential chain. See <https://docs.aws.amazon.com/sdk-for-rust/latest/dg/credproviders.html>.

The database commit **must remain before final S3 publication**. PostgreSQL/Turso and S3 do not share a transaction. If final copy, obsolete-object deletion, or staging cleanup fails after the database commit, the write returns an error, prints only the safe `s3://bucket/staging-key-prefix` location, and retains staging objects for manual repair. The database may temporarily reference unavailable final objects. This release deliberately adds **no durable outbox, reconciliation worker, immutable generation indirection, or automatic repair**. Similarly, a failed S3 cleanup after permanent database deletion may leave orphan objects, but it must never delete objects for a retained note.

## Execution baseline

Run from the prerequisite implementation checkout:

```bash
test -f crates/note-attachments/src/filesystem.rs
test -f crates/note-attachments/src/path.rs
rg -n "trait AttachmentStore|trait PreparedAttachmentSet" \
  crates/note-attachments/src/lib.rs
cargo fmt --all -- --check
cargo test -p note-attachments
cargo test -p note-pipelines
cargo test -p note-server
```

Expected: the prerequisite files and traits exist and every command passes. If not, stop and finish or repair only the prerequisite plan; do not build S3 around the old filesystem path API.

## File map

### S3 adapter

- Modify `crates/note-attachments/Cargo.toml` — pin Rust-1.95-compatible AWS SDK dependencies and add wiremock test support.
- Modify `crates/note-attachments/src/lib.rs` — export `S3AttachmentConfig` and `S3AttachmentStore`.
- Modify `crates/note-attachments/src/path.rs` — expose one canonical relative-path function shared by filesystem and S3.
- Modify `crates/note-attachments/src/filesystem.rs` — consume the shared canonical path rather than maintaining divergent validation.
- Create `crates/note-attachments/src/s3.rs` — SDK construction, safe errors, canonical key composition, staging, publish/abort, direct reads, pagination, and delete batching.
- Create `crates/note-attachments/tests/s3_minio_test.rs` — MinIO behavior tests, gated by `NOTE_TEST_MINIO_ENDPOINT`.

### Composition and cross-adapter workflows

- Modify `crates/note-server/src/main.rs` — construct S3 from resolved endpoint, region, bucket, prefix, and path-style fields.
- Modify `crates/note-server/src/system_api.rs` — assert safe S3 system information contains no endpoint or credentials.
- Create `crates/note-pipelines/tests/s3_export_test.rs` — JSON import/export attachment round trip through Turso plus S3.

### CI and documentation

- Modify `.github/workflows/ci.yml` — add a pinned MinIO Docker integration job on Ubuntu.
- Modify `README.md` — S3 configuration, standard credentials, key layout, retries, and consistency limitation.
- Modify `docs/design.md` — attachment adapter contract and operational ordering.

Do not change attachment JSON, REST, MCP, or frontend workflow shapes. The prerequisite already makes the System page backend-neutral and makes the raw download endpoint read one object.

---

### Task 1: Share canonical path and key primitives

**Files:**

- Modify: `crates/note-attachments/Cargo.toml`
- Modify: `crates/note-attachments/src/lib.rs`
- Modify: `crates/note-attachments/src/path.rs`
- Modify: `crates/note-attachments/src/filesystem.rs`
- Create: `crates/note-attachments/src/s3.rs`
- Test: unit tests in `crates/note-attachments/src/path.rs` and `crates/note-attachments/src/s3.rs`

- [ ] **Step 1: Write failing canonicalization tests**

In `path.rs`, make the shared test contract explicit:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_relative_paths_drop_only_current_directory_components() {
        assert_eq!(canonical_relative_path("./docs/read me.txt").unwrap(), "docs/read me.txt");
        assert_eq!(canonical_relative_path("docs/./read.md").unwrap(), "docs/read.md");
    }

    #[test]
    fn canonical_relative_paths_reject_unsafe_or_empty_inputs() {
        for value in ["", ".", "../secret", "/absolute", r"C:\secret", r"dir\secret"] {
            assert!(canonical_relative_path(value).is_err(), "{value:?} must fail");
        }
    }
}
```

In the new `s3.rs`, start with tests for exact final and staging keys:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        for prefix in ["../attachments", "safe//unsafe", r"safe\unsafe"] {
            assert!(normalize_prefix(prefix).is_err(), "{prefix:?} must fail");
        }
        for note_id in ["", "../note", "note/other", ".staging"] {
            assert!(final_prefix("", note_id).is_err(), "{note_id:?} must fail");
        }
    }

    #[test]
    fn copy_source_percent_encodes_segments_but_preserves_path_separators() {
        assert_eq!(
            copy_source("agent-note", "attachments/note-1/read me.txt"),
            "agent-note/attachments/note-1/read%20me.txt"
        );
    }
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
cargo test -p note-attachments canonical_relative_paths
cargo test -p note-attachments s3::tests::key_composition
```

Expected: the first test fails or does not compile because the prerequisite helper is not yet the canonical `String` API; the second command fails because `s3.rs` and its key helpers do not exist.

- [ ] **Step 3: Pin the current AWS SDK dependencies**

Add these exact entries to `crates/note-attachments/Cargo.toml`:

```toml
[dependencies]
aws-config = "=1.9.0"
aws-sdk-s3 = "=1.138.1"
url = "2"
urlencoding = "2.1.3"

[dev-dependencies]
wiremock = "0.6.5"
```

Keep the prerequisite `anyhow`, `async-trait`, `note-core`, `tokio`, and `uuid` dependencies. Do not add `aws-credential-types` to production: the production constructor must use the standard chain rather than static credentials.

- [ ] **Step 4: Implement the shared canonical relative path**

Use one implementation in `path.rs`:

```rust
use std::path::{Component, Path};

pub(crate) fn canonical_relative_path(value: &str) -> anyhow::Result<String> {
    if value.is_empty() || value.contains('\\') {
        anyhow::bail!("invalid attachment path");
    }

    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("attachment path is not UTF-8"))?;
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("invalid attachment path");
            }
        }
    }

    if parts.is_empty() {
        anyhow::bail!("attachment path is empty");
    }
    Ok(parts.join("/"))
}
```

Change `filesystem.rs` to call `canonical_relative_path` and then push each `/`-separated segment under its configured root. Preserve all prerequisite filesystem tests; this is a shared-validator refactor, not a behavior expansion.

- [ ] **Step 5: Implement exact key helpers**

At the top of `s3.rs`, add:

```rust
const S3_PAGE_SIZE: i32 = 1_000;
const S3_DELETE_BATCH_SIZE: usize = 1_000;
const S3_MAX_ATTEMPTS: u32 = 4;

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
```

In `lib.rs`, declare `mod s3;`. Keep it private until Task 2 defines the public
configuration and store types.

- [ ] **Step 6: Run path and key tests and verify GREEN**

```bash
cargo fmt --all
cargo test -p note-attachments path::tests
cargo test -p note-attachments s3::tests::key_composition
cargo test -p note-attachments s3::tests::prefix_and_note_id
cargo test -p note-attachments s3::tests::copy_source
```

Expected: all named tests pass; existing filesystem tests also still compile.

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates/note-attachments
git commit -m "feat(attachments): add canonical S3 key primitives"
```

---

### Task 2: Construct the SDK client with standard credentials and bounded retries

**Files:**

- Modify: `crates/note-attachments/src/s3.rs`
- Test: unit tests in `crates/note-attachments/src/s3.rs`

- [ ] **Step 1: Write failing SDK configuration and safe-error tests**

Add tests using wiremock. Serialize environment mutation with a process-local mutex:

```rust
#[cfg(test)]
static AWS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn explicit_endpoint_region_path_style_and_standard_credentials_sign_requests() {
    use wiremock::matchers::{header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = AWS_ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    std::env::set_var("AWS_ACCESS_KEY_ID", "chain-access");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "chain-secret");
    std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");

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
        bucket: "agent-note".into(),
        prefix: "tests".into(),
        region: Some("us-east-1".into()),
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

#[test]
fn safe_errors_keep_category_and_code_but_never_source_text() {
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
```

Add constructor tests proving non-HTTP(S) endpoint URLs and URLs with userinfo,
query strings, or fragments are rejected with
`invalid S3 attachment endpoint URL` and never echo a username/password
sentinel. Also prove outer bucket/region whitespace is normalized once. Set
`AWS_ENDPOINT_URL_S3` to a wiremock sentinel and prove a config with
`endpoint: None` does not inherit it; the typed config resolver already owns
the allowed `NOTE_S3_ENDPOINT` fallback.

Use a small test-only RAII environment guard that snapshots
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
`AWS_EC2_METADATA_DISABLED`, and `AWS_ENDPOINT_URL_S3`, applies the test values
while `AWS_ENV_LOCK` is held, and restores/removes every variable in `Drop`.
Use this guard in every shown `set_var` test; no test may leave AWS environment
mutations behind for a later test.

Add a retry responder and test. It must fail the first three requests and succeed on the fourth:

```rust
#[derive(Clone)]
struct FailThenSucceed {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    failures: usize,
}

impl wiremock::Respond for FailThenSucceed {
    fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call < self.failures {
            wiremock::ResponseTemplate::new(503)
        } else {
            wiremock::ResponseTemplate::new(200)
        }
    }
}

#[tokio::test]
async fn sdk_uses_one_initial_attempt_plus_three_transient_retries() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer};

    let _guard = AWS_ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    std::env::set_var("AWS_ACCESS_KEY_ID", "retry-access");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "retry-secret");
    std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

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

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
}
```

Define the helper used by the retry test:

```rust
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
```

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
cargo test -p note-attachments s3::tests::explicit_endpoint_region
cargo test -p note-attachments s3::tests::safe_errors
cargo test -p note-attachments s3::tests::sdk_uses_one_initial
```

Expected: compilation fails because `S3AttachmentConfig`, `S3AttachmentStore::new`, `put`, and `safe_sdk_error` do not exist.

- [ ] **Step 3: Define the public configuration and store**

Add to `s3.rs`:

```rust
use aws_config::{retry::RetryConfig, BehaviorVersion, Region};
use aws_sdk_s3::{
    error::{ProvideErrorMetadata, SdkError},
    primitives::ByteStream,
    Client,
};
use note_core::NoteAttachment;

#[derive(Debug, Clone, PartialEq, Eq)]
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
    coordination: std::sync::Arc<NoteCoordination>,
}
```

In `lib.rs`, publicly export the completed types:

```rust
pub use s3::{S3AttachmentConfig, S3AttachmentStore};
```

The constructor is:

```rust
impl S3AttachmentStore {
    pub async fn new(config: S3AttachmentConfig) -> anyhow::Result<Self> {
        let bucket = config.bucket.trim().to_owned();
        if bucket.is_empty() {
            anyhow::bail!("S3 attachment bucket must not be blank");
        }
        let prefix = normalize_prefix(&config.prefix)?;
        let retry = RetryConfig::standard().with_max_attempts(S3_MAX_ATTEMPTS);
        let mut loader = aws_config::defaults(BehaviorVersion::latest()).retry_config(retry);
        if let Some(region) = config
            .region
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            loader = loader.region(Region::new(region));
        }
        let shared = loader.load().await;
        let mut service = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(config.force_path_style);
        service.set_endpoint_url(None);
        if let Some(endpoint) = config
            .endpoint
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            let endpoint = url::Url::parse(&endpoint)
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
            service = service.endpoint_url(endpoint.as_str());
        }

        Ok(Self {
            client: Client::from_conf(service.build()),
            bucket,
            prefix,
            coordination: std::sync::Arc::new(NoteCoordination::default()),
        })
    }
}
```

Do not call `.credentials_provider(...)`. The optional explicit region overrides region discovery; otherwise `aws_config` retains its normal region provider chain. The S3 service builder applies the resolved endpoint and path-style choice after loading shared credentials/config.

`NoteCoordination` uses the same minimal per-note pattern as the corrected
filesystem adapter: a standard-mutex-protected map from note ID to
`Weak<tokio::sync::Mutex<()>>`, pruning dead entries during lookup. Store
clones share it. Same-note operations serialize without retaining one lock
forever per historical note.

- [ ] **Step 4: Add redacted error categories and the internal put operation**

Use the SDK error variant plus service code, never `DisplayErrorContext`, `Debug`, the raw response, source error, endpoint URL, or environment:

```rust
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

impl S3AttachmentStore {
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
```

This uses the SDK standard retry classifier: transient dispatch/timeouts, throttling, and retryable 5xx responses may retry; permanent authentication, authorization, validation, and other non-retryable service errors do not.

- [ ] **Step 5: Run the focused tests and verify GREEN**

```bash
cargo fmt --all
cargo test -p note-attachments s3::tests::explicit_endpoint_region
cargo test -p note-attachments s3::tests::safe_errors
cargo test -p note-attachments s3::tests::sdk_uses_one_initial
```

Expected: path-style requests reach wiremock, the authorization scope contains `us-east-1`, safe errors contain no source secret, and exactly four total transient attempts occur.

- [ ] **Step 6: Commit**

```bash
git add Cargo.lock crates/note-attachments
git commit -m "feat(attachments): configure bounded S3 client"
```

---

### Task 3: Prepare, abort, and direct-read staged objects

**Files:**

- Modify: `crates/note-attachments/src/s3.rs`
- Create: `crates/note-attachments/tests/s3_minio_test.rs`

- [ ] **Step 1: Start a local MinIO test service**

```bash
docker rm -f agent-note-minio 2>/dev/null || true
docker run -d --name agent-note-minio \
  -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin \
  -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio:RELEASE.2025-07-18T21-56-31Z server /data
ready=0
for _ in $(seq 1 60); do
  if curl --fail --silent http://127.0.0.1:9000/minio/health/live >/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  docker logs agent-note-minio
  exit 1
fi
docker run --rm --network host \
  -e MC_HOST_local=http://minioadmin:minioadmin@127.0.0.1:9000 \
  minio/mc:RELEASE.2025-07-21T05-28-08Z \
  mb --ignore-existing local/agent-note-tests
```

Expected: the health check succeeds and bucket `agent-note-tests` exists.

- [ ] **Step 2: Write failing staging, abort, path, and direct-read integration tests**

Create `s3_minio_test.rs` with a gated fixture that also constructs a raw test
client for assertions without widening the production adapter API:

```rust
use note_attachments::{
    AttachmentStore, S3AttachmentConfig, S3AttachmentStore,
};
use note_core::NoteAttachment;

struct Fixture {
    store: S3AttachmentStore,
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

async fn fixture() -> Option<Fixture> {
    let endpoint = std::env::var("NOTE_TEST_MINIO_ENDPOINT").ok()?;
    let bucket = std::env::var("NOTE_TEST_MINIO_BUCKET")
        .unwrap_or_else(|_| "agent-note-tests".into());
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
async fn unsafe_paths_fail_before_any_upload() {
    let Some(fixture) = fixture().await else { return };
    for path in ["../secret", "/absolute", r"C:\secret"] {
        assert!(fixture
            .store
            .prepare("note-1", &[attachment(path, b"secret")])
            .await
            .is_err());
    }
    assert!(list_keys(&fixture, &format!("{}/", fixture.prefix))
        .await
        .is_empty());
}

#[tokio::test]
async fn direct_read_fetches_only_the_requested_object() {
    let Some(fixture) = fixture().await else { return };
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
        fixture
            .store
            .read("note-1", "./wanted.txt")
            .await
            .unwrap(),
        b"wanted"
    );
}

#[tokio::test]
async fn same_note_preparation_waits_until_the_first_set_is_resolved() {
    let Some(fixture) = fixture().await else { return };
    let first = fixture
        .store
        .prepare("note-1", &[attachment("./first.txt", b"first")])
        .await
        .unwrap();
    let second_store = fixture.store.clone();
    let mut second = Box::pin(async move {
        second_store.prepare("note-1", &[]).await.unwrap()
    });

    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(100),
        second.as_mut(),
    )
    .await
    .is_err());
    first.abort().await.unwrap();
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        second,
    )
    .await
    .unwrap();
    second.abort().await.unwrap();
}

#[tokio::test]
async fn different_note_sets_can_be_prepared_together_for_import() {
    let Some(fixture) = fixture().await else { return };
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
```

The raw client exists only in integration-test code. Production exports remain
limited to the adapter configuration, adapter, and attachment traits.

- [ ] **Step 3: Run the integration tests and verify RED**

```bash
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test -- --nocapture
```

Expected: compilation fails because `AttachmentStore` is not implemented for `S3AttachmentStore` and no prepared S3 set exists.

- [ ] **Step 4: Implement staged preparation and abort**

Add:

```rust
struct PreparedS3Set {
    store: S3AttachmentStore,
    note_id: String,
    staging_prefix: String,
    staged_keys: Vec<String>,
    metadata: Vec<NoteAttachment>,
    _note_guard: tokio::sync::OwnedMutexGuard<()>,
}
```

Implement `prepare` with this exact ordering:

```rust
async fn prepare_set(
    &self,
    note_id: &str,
    attachments: &[NoteAttachment],
) -> anyhow::Result<PreparedS3Set> {
    validate_note_id(note_id)?;
    let note_guard = self.coordination.lock_for(note_id).lock_owned().await;
    let mut canonical = Vec::with_capacity(attachments.len());
    let mut metadata = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let relative = crate::path::canonical_relative_path(&attachment.path)?;
        canonical.push(relative);
        metadata.push(NoteAttachment {
            id: attachment.id.clone(),
            path: attachment.path.clone(),
            mime: attachment.mime.clone(),
            description: attachment.description.clone(),
            content: Vec::new(),
        });
    }

    let staging_prefix = staging_prefix(
        &self.prefix,
        note_id,
        &uuid::Uuid::new_v4().simple().to_string(),
    )?;
    let mut uploaded = Vec::new();
    for (attachment, relative) in attachments.iter().zip(canonical) {
        let key = format!("{staging_prefix}{relative}");
        uploaded.push(key.clone());
        if let Err(error) = self.put(&key, &attachment.content).await {
            let cleanup = self.delete_keys(&uploaded).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error.context(format!(
                    "staging cleanup also failed: {cleanup_error}"
                ))),
            };
        }
    }

    Ok(PreparedS3Set {
        store: self.clone(),
        note_id: note_id.to_string(),
        staging_prefix,
        staged_keys: uploaded,
        metadata,
        _note_guard: note_guard,
    })
}
```

Validate the complete path set before the first upload. Empty sets are valid and still return a logical unique staging prefix.
Holding the owned per-note guard in the prepared value serializes the complete
prepare → caller database transaction → publish/abort window, so same-note
handlers cannot commit in one order and publish in another. Different notes
remain independent, which is required because import prepares multiple note
sets before starting its database transaction.

Add a wiremock regression where the first PUT exhausts retries with an
ambiguous server failure and assert that `DeleteObjects` is still called with
that current key. A PUT may have committed remotely even when its response is
lost, so the key enters `uploaded` before awaiting `put`; cleanup deletion is
idempotent.

Implement the trait:

```rust
#[async_trait::async_trait]
impl crate::PreparedAttachmentSet for PreparedS3Set {
    fn metadata(&self) -> &[NoteAttachment] {
        &self.metadata
    }

    async fn publish(self: Box<Self>) -> anyhow::Result<()> {
        let store = self.store.clone();
        store.publish_set(*self).await
    }

    async fn abort(self: Box<Self>) -> anyhow::Result<()> {
        self.store.delete_keys(&self.staged_keys).await
    }
}
```

- [ ] **Step 5: Implement one-object reads and backend information**

```rust
impl S3AttachmentStore {
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

    fn safe_location(&self) -> String {
        let suffix = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", self.prefix)
        };
        format!("s3://{}{suffix}", self.bucket)
    }
}

#[async_trait::async_trait]
impl crate::AttachmentStore for S3AttachmentStore {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[NoteAttachment],
    ) -> anyhow::Result<Box<dyn crate::PreparedAttachmentSet>> {
        Ok(Box::new(self.prepare_set(note_id, attachments).await?))
    }

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        validate_note_id(note_id)?;
        let note_lock = self.coordination.lock_for(note_id);
        let _guard = note_lock.lock().await;
        self.read_key(&final_key(&self.prefix, note_id, path)?).await
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
```

Override `hydrate` so one complete note hydration cannot interleave with
publication. The direct attachment pipeline still calls `read` exactly once.

- [ ] **Step 6: Implement the initial private list/delete helpers**

Implement the minimal single-request delete used by abort and initial
publication:

```rust
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

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
```

Add this deliberately single-page helper:

```rust
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
```

These keep abort, publication, and removal functional for ordinary sets. Task
4's >1,000-object red test drives both the continuation-token loop and
multi-object delete batching.

Add the first publishing implementation needed by the prepared-set trait. It
copies every staged object to its canonical final key, retains staging on a
copy error, and removes the known staged keys only after all copies succeed:

```rust
async fn publish_set(&self, prepared: PreparedS3Set) -> anyhow::Result<()> {
    let publish_result = async {
        for attachment in &prepared.metadata {
            let relative = crate::path::canonical_relative_path(&attachment.path)?;
            let source_key = format!("{}{relative}", prepared.staging_prefix);
            let destination_key =
                final_key(&self.prefix, &prepared.note_id, &attachment.path)?;
            self.client
                .copy_object()
                .copy_source(copy_source(&self.bucket, &source_key))
                .bucket(&self.bucket)
                .key(destination_key)
                .send()
                .await
                .map_err(|error| safe_sdk_error("copy object", &error))?;
        }
        self.delete_keys(&prepared.staged_keys).await
    }
    .await;

    if let Err(error) = publish_result {
        let staging_location = format!("s3://{}/{}", self.bucket, prepared.staging_prefix);
        eprintln!(
            "S3 attachment publication failed after database commit; \
             staged objects retained at {staging_location}"
        );
        return Err(error.context(format!(
            "database committed; staged objects retained at {staging_location}"
        )));
    }
    Ok(())
}
```

Task 4 extends this method with replacement semantics: it identifies and
removes obsolete final keys before deleting staging.

- [ ] **Step 7: Run the MinIO tests and verify GREEN**

```bash
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test -- --nocapture
```

Expected: metadata has no bytes, unsafe paths upload nothing, abort removes staging, and reading `wanted.txt` succeeds even after its sibling object is removed.

- [ ] **Step 8: Commit**

```bash
git add crates/note-attachments
git commit -m "feat(attachments): stage and read S3 objects"
```

---

### Task 4: Publish replacements and paginate bounded deletions

**Files:**

- Modify: `crates/note-attachments/src/s3.rs`
- Modify: `crates/note-attachments/tests/s3_minio_test.rs`

- [ ] **Step 1: Write failing publication and replacement tests**

Add:

```rust
#[tokio::test]
async fn publish_uses_canonical_final_keys_and_removes_staging() {
    let Some(fixture) = fixture().await else { return };
    let prepared = fixture
        .store
        .prepare("note-1", &[attachment("./nested/read me.txt", b"payload")])
        .await
        .unwrap();
    prepared.publish().await.unwrap();

    let keys = list_keys(&fixture, &format!("{}/", fixture.prefix)).await;
    assert_eq!(keys.len(), 1);
    assert!(keys[0].ends_with("/note-1/nested/read me.txt"));
    assert!(!keys[0].contains("/.staging/"));
    assert_eq!(
        fixture
            .store
            .read("note-1", "./nested/read me.txt")
            .await
            .unwrap(),
        b"payload"
    );
}

#[tokio::test]
async fn replacement_overwrites_retained_paths_and_removes_obsolete_paths() {
    let Some(fixture) = fixture().await else { return };
    fixture
        .store
        .prepare(
            "note-1",
            &[
                attachment("./retained.txt", b"old"),
                attachment("./obsolete.txt", b"remove"),
            ],
        )
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    fixture
        .store
        .prepare("note-1", &[attachment("./retained.txt", b"new")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    assert_eq!(
        fixture
            .store
            .read("note-1", "./retained.txt")
            .await
            .unwrap(),
        b"new"
    );
    assert!(fixture
        .store
        .read("note-1", "./obsolete.txt")
        .await
        .is_err());
    assert_eq!(
        list_keys(&fixture, &format!("{}/note-1/", fixture.prefix))
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn publishing_an_empty_set_removes_every_final_object() {
    let Some(fixture) = fixture().await else { return };
    fixture
        .store
        .prepare("note-1", &[attachment("./old.txt", b"old")])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();
    fixture
        .store
        .prepare("note-1", &[])
        .await
        .unwrap()
        .publish()
        .await
        .unwrap();

    assert!(list_keys(&fixture, &format!("{}/note-1/", fixture.prefix))
        .await
        .is_empty());
}
```

Add this wiremock regression to the `s3.rs` unit-test module, where
`AWS_ENV_LOCK` is already defined, proving the approved failure state:

```rust
#[tokio::test]
async fn publication_failure_retains_staging_and_reports_only_safe_location() {
    use wiremock::matchers::{header_exists, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _guard = AWS_ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    std::env::set_var("AWS_ACCESS_KEY_ID", "failure-access");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "failure-secret");
    std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");

    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/agent-note/tests/\.staging/note-1/[a-f0-9]+/file\.txt$",
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
        .and(path("/agent-note"))
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
```

The zero-POST assertion proves publication did not run multi-delete cleanup on
the staged object after the final copy failed.

- [ ] **Step 2: Write the >1,000-object pagination and delete-batch test**

Add a test-only helper that seeds final objects with the raw fixture client in
bounded groups:

```rust
async fn seed_final_objects(
    fixture: &Fixture,
    note_id: &str,
    count: usize,
) {
    for chunk_start in (0..count).step_by(32) {
        let chunk_end = (chunk_start + 32).min(count);
        let mut set = tokio::task::JoinSet::new();
        for index in chunk_start..chunk_end {
            let client = fixture.client.clone();
            let bucket = fixture.bucket.clone();
            let key = format!(
                "{}/{note_id}/item-{index:04}.bin",
                fixture.prefix
            );
            set.spawn(async move {
                client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(aws_sdk_s3::primitives::ByteStream::from_static(b"x"))
                    .send()
                    .await
                    .unwrap();
            });
        }
        while let Some(result) = set.join_next().await {
            result.unwrap();
        }
    }
}
```

Then add:

```rust
#[tokio::test]
async fn remove_note_lists_every_page_and_deletes_in_at_most_one_thousand_key_batches() {
    let Some(fixture) = fixture().await else { return };
    seed_final_objects(&fixture, "note-big", 1_005).await;
    assert_eq!(
        list_keys(&fixture, &format!("{}/note-big/", fixture.prefix))
            .await
            .len(),
        1_005
    );

    fixture.store.remove_note("note-big").await.unwrap();

    assert!(list_keys(&fixture, &format!("{}/note-big/", fixture.prefix))
        .await
        .is_empty());
}
```

This is intentionally a real MinIO test: it proves that the second list page is consumed and that the SDK accepts both delete requests (1,000 plus 5).

- [ ] **Step 3: Run the new tests and verify RED**

```bash
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test -- --nocapture
```

Expected: publication tests fail because `publish_set` is absent or
incomplete; the large delete test leaves the final five keys because the
single-page helper sees only the first 1,000.

- [ ] **Step 4: Implement paginated listing**

```rust
impl S3AttachmentStore {
    async fn list_keys(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        let mut seen_tokens = std::collections::HashSet::new();
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
                        anyhow::anyhow!(
                            "S3 list objects failed (missing continuation token)"
                        )
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
}
```

Add wiremock cases for truncated responses with an empty token and with the
same token repeated on the next page; both must terminate with the static safe
errors above rather than loop.

Delete `list_keys_single_page` and change `remove_note` to:

```rust
async fn remove_note(&self, note_id: &str) -> anyhow::Result<()> {
    validate_note_id(note_id)?;
    let note_lock = self.coordination.lock_for(note_id);
    let _guard = note_lock.lock().await;
    self.delete_prefix(&final_prefix(&self.prefix, note_id)?).await
}
```

- [ ] **Step 5: Implement <=1,000-key multi-delete batches**

```rust
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

fn transient_delete_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("InternalError" | "SlowDown" | "ServiceUnavailable" | "RequestTimeout")
    )
}

fn retryable_sdk_error<E: ProvideErrorMetadata>(error: &SdkError<E>) -> bool {
    matches!(
        error,
        SdkError::TimeoutError(_)
            | SdkError::DispatchFailure(_)
            | SdkError::ResponseError(_)
    ) || matches!(
        error.code(),
        Some("InternalError" | "SlowDown" | "ServiceUnavailable" | "RequestTimeout")
    )
}

impl S3AttachmentStore {
    async fn delete_keys(&self, keys: &[String]) -> anyhow::Result<()> {
        for batch in keys.chunks(S3_DELETE_BATCH_SIZE) {
            if batch.is_empty() {
                continue;
            }
            let mut pending = batch.to_vec();
            for attempt in 0..S3_MAX_ATTEMPTS {
                let expected = pending.iter().cloned().collect::<std::collections::HashSet<_>>();
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
                let no_transport_retries = aws_sdk_s3::config::Builder::new()
                    .retry_config(RetryConfig::standard().with_max_attempts(1));
                let response = self
                    .client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .customize()
                    .config_override(no_transport_retries)
                    .send()
                    .await;
                let output = match response {
                    Ok(output) => output,
                    Err(error)
                        if retryable_sdk_error(&error)
                            && attempt + 1 < S3_MAX_ATTEMPTS =>
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50_u64 << attempt.min(3),
                        ))
                        .await;
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
                tokio::time::sleep(std::time::Duration::from_millis(
                    50_u64 << attempt.min(3),
                ))
                .await;
            }
        }
        Ok(())
    }

    async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<()> {
        let keys = self.list_keys(prefix).await?;
        self.delete_keys(&keys).await
    }
}
```

The custom operation override disables nested SDK transport retries for this
application-level loop, so a batch makes at most four total requests. Add a
wiremock test returning HTTP 200 with one `InternalError` entry followed by
success and assert two requests. Add a malicious/permanent code test and
assert the error contains only the static category above. Never include
returned object keys, raw service codes, or service messages in an error; they
are server-controlled and an attachment filename may contain sensitive data.

- [ ] **Step 6: Implement database-after publication semantics**

`publish_set` is called only after the prerequisite pipeline commits the database:

```rust
impl S3AttachmentStore {
    async fn publish_set(&self, prepared: PreparedS3Set) -> anyhow::Result<()> {
        let final_prefix = final_prefix(&self.prefix, &prepared.note_id)?;
        let expected = prepared
            .metadata
            .iter()
            .map(|attachment| {
                final_key(&self.prefix, &prepared.note_id, &attachment.path)
            })
            .collect::<anyhow::Result<std::collections::HashSet<_>>>()?;

        let publish_result = async {
            for attachment in &prepared.metadata {
                let relative = crate::path::canonical_relative_path(&attachment.path)?;
                let source_key = format!("{}{relative}", prepared.staging_prefix);
                let destination_key =
                    final_key(&self.prefix, &prepared.note_id, &attachment.path)?;
                self.client
                    .copy_object()
                    .copy_source(copy_source(&self.bucket, &source_key))
                    .bucket(&self.bucket)
                    .key(destination_key)
                    .send()
                    .await
                    .map_err(|error| safe_sdk_error("copy object", &error))?;
            }

            let obsolete = self
                .list_keys(&final_prefix)
                .await?
                .into_iter()
                .filter(|key| !expected.contains(key))
                .collect::<Vec<_>>();
            self.delete_keys(&obsolete).await?;
            self.delete_keys(&prepared.staged_keys).await
        }
        .await;

        if let Err(error) = publish_result {
            let staging_location =
                format!("s3://{}/{}", self.bucket, prepared.staging_prefix);
            eprintln!(
                "S3 attachment publication failed after database commit; \
                 staged objects retained at {staging_location}"
            );
            return Err(error.context(format!(
                "database committed; staged objects retained at {staging_location}"
            )));
        }
        Ok(())
    }
}
```

Do not abort or delete staging on any `publish_set` error. Copying retained paths before listing/deleting obsolete paths ensures a cleanup failure cannot delete a newly expected path. Staging is removed only after every copy and obsolete-object delete succeeds.

- [ ] **Step 7: Run all attachment tests and verify GREEN**

```bash
cargo fmt --all
cargo test -p note-attachments
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test -- --nocapture
```

Expected: unit, filesystem regression, retry, staging, direct-read, replacement, empty-set, and 1,005-object deletion tests all pass.

- [ ] **Step 8: Commit**

```bash
git add Cargo.lock crates/note-attachments
git commit -m "feat(attachments): publish and replace S3 sets"
```

---

### Task 5: Compose S3 and prove system/import/export flows

**Files:**

- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Create: `crates/note-pipelines/tests/s3_export_test.rs`

- [ ] **Step 1: Write a failing S3 composition test**

Refactor the prerequisite attachment match into:

```rust
async fn build_attachment_store(
    config: &config::AttachmentConfig,
) -> anyhow::Result<std::sync::Arc<dyn note_attachments::AttachmentStore>>;
```

Add:

```rust
#[tokio::test]
async fn s3_attachment_config_constructs_a_safe_backend() {
    let _env = TestEnvGuard::set("AWS_EC2_METADATA_DISABLED", "true");
    let store = build_attachment_store(&AttachmentConfig::S3 {
        bucket: "agent-note".into(),
        prefix: "attachments".into(),
        region: Some("us-east-1".into()),
        endpoint: Some("http://minio.internal:9000".into()),
        force_path_style: true,
    })
    .await
    .unwrap();

    assert_eq!(
        store.info(),
        note_attachments::AttachmentStoreInfo {
            engine: "s3".into(),
            location: Some("s3://agent-note/attachments".into()),
        }
    );
}
```

The test must not set access-key or secret-key values; client construction is lazy and must not require credentials before an operation.
`TestEnvGuard` restores the prior metadata setting at test exit.

- [ ] **Step 2: Write a failing System API redaction test**

Add this complete test to `system_api.rs`:

```rust
#[tokio::test]
async fn info_reports_safe_s3_location_without_endpoint_or_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
        note_attachments::S3AttachmentStore::new(
            note_attachments::S3AttachmentConfig {
                bucket: "agent-note".into(),
                prefix: "attachments".into(),
                region: Some("us-east-1".into()),
                endpoint: Some("http://minio.internal:9000".into()),
                force_path_style: true,
            },
        )
        .await
        .unwrap(),
    );
    let ctx = Arc::new(Context::new(
        storage,
        Arc::new(StubEmbedder),
        attachments,
    ));
    let response = system_router()
        .with_state(ctx)
        .oneshot(request("GET", "/api/system/info", ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(info["attachments_engine"], "s3");
    assert_eq!(info["attachments_location"], "s3://agent-note/attachments");
    let rendered = serde_json::to_string(&info).unwrap();
    assert!(!rendered.contains("minio.internal"));
    assert!(!rendered.contains("AWS_ACCESS_KEY_ID"));
    assert!(!rendered.contains("AWS_SECRET_ACCESS_KEY"));
    assert!(!rendered.contains("minioadmin"));
}
```

System information deliberately reports only `s3://bucket/prefix`, not the endpoint, region, path-style flag, profile, or credentials.

- [ ] **Step 3: Write the failing S3 import/export round-trip test**

Create `s3_export_test.rs`:

```rust
use std::sync::Arc;

use note_attachments::{AttachmentStore, S3AttachmentConfig, S3AttachmentStore};
use note_embedding::StubEmbedder;
use note_pipelines::{
    delete_note, export_json, import_json, permanently_delete_note, Context,
};
use note_storage::StorageBackend;

#[tokio::test]
async fn json_round_trip_uses_the_selected_s3_attachment_store() {
    let Ok(endpoint) = std::env::var("NOTE_TEST_MINIO_ENDPOINT") else {
        eprintln!("skipped: NOTE_TEST_MINIO_ENDPOINT is unset");
        return;
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
    assert!(delete_note(&ctx, "imported-note").await.unwrap());
    assert!(permanently_delete_note(&ctx, "imported-note")
        .await
        .unwrap());
    assert!(s3.read("imported-note", "./proof.bin").await.is_err());
}
```

`note-pipelines` already has `uuid` as a normal dependency and its
`ImportStats` contract exposes `notes_added`, so this test needs no new
pipeline dependency.

Add a second integration test whose JSON contains two distinct note IDs with
attachments. Wrap `import_json` in a five-second timeout, assert
`notes_added == 2`, and export/read both payloads. This proves the real import
orchestration can hold two per-note prepared S3 sets simultaneously rather
than testing only the adapter in isolation.

- [ ] **Step 4: Run the focused tests and verify RED**

```bash
cargo test -p note-server s3_attachment_config_constructs
cargo test -p note-server system_api::tests
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-pipelines --test s3_export_test -- --nocapture
```

Expected: the server still rejects `AttachmentConfig::S3`, and the pipeline integration target does not yet exist.

- [ ] **Step 5: Wire resolved S3 fields into the adapter**

Implement:

```rust
async fn build_attachment_store(
    config: &config::AttachmentConfig,
) -> anyhow::Result<Arc<dyn note_attachments::AttachmentStore>> {
    match config {
        config::AttachmentConfig::Filesystem { path } => Ok(Arc::new(
            note_attachments::FilesystemAttachmentStore::new(path.clone()),
        )),
        config::AttachmentConfig::S3 {
            bucket,
            prefix,
            region,
            endpoint,
            force_path_style,
        } => Ok(Arc::new(
            note_attachments::S3AttachmentStore::new(
                note_attachments::S3AttachmentConfig {
                    bucket: bucket.clone(),
                    prefix: prefix.clone(),
                    region: region.clone(),
                    endpoint: endpoint.clone(),
                    force_path_style: *force_path_style,
                },
            )
            .await?,
        )),
    }
}
```

Call it once from the composition root for server, stdio, import, and export modes. Preserve the prerequisite rule that only `Filesystem` causes a local attachment directory to be created. Do not add S3 bucket creation: deployment owns bucket provisioning.

- [ ] **Step 6: Run composition, system, and cross-adapter workflow tests**

```bash
cargo test -p note-server
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-pipelines --test s3_export_test -- --nocapture
```

Expected: S3 composes from all five resolved fields, system JSON exposes only the safe engine/location, and import/export reproduces binary attachment bytes through S3. Import uses `StubEmbedder`, queues jobs, and makes no embedding endpoint call.

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates/note-server crates/note-pipelines
git commit -m "feat(server): compose S3 attachment storage"
```

---

### Task 6: Add MinIO CI, document operations, and run completion gates

**Files:**

- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Add a dedicated Ubuntu MinIO integration job**

Append:

```yaml
  s3-integration:
    name: S3 integration (MinIO)
    runs-on: ubuntu-latest
    env:
      AWS_ACCESS_KEY_ID: minioadmin
      AWS_SECRET_ACCESS_KEY: minioadmin
      AWS_EC2_METADATA_DISABLED: "true"
      NOTE_TEST_MINIO_ENDPOINT: http://127.0.0.1:9000
      NOTE_TEST_MINIO_BUCKET: agent-note-tests

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.95.0
      - uses: Swatinem/rust-cache@v2
      - name: Start MinIO
        run: |
          docker run -d --name agent-note-minio \
            -p 9000:9000 \
            -e MINIO_ROOT_USER=minioadmin \
            -e MINIO_ROOT_PASSWORD=minioadmin \
            minio/minio:RELEASE.2025-07-18T21-56-31Z server /data
          ready=0
          for _ in $(seq 1 60); do
            if curl --fail --silent \
              http://127.0.0.1:9000/minio/health/live >/dev/null; then
              ready=1
              break
            fi
            sleep 1
          done
          if [ "$ready" -ne 1 ]; then
            docker logs agent-note-minio
            exit 1
          fi
          docker run --rm --network host \
            -e MC_HOST_local=http://minioadmin:minioadmin@127.0.0.1:9000 \
            minio/mc:RELEASE.2025-07-21T05-28-08Z \
            mb --ignore-existing local/agent-note-tests
      - name: Test S3 attachment adapter
        run: cargo test --locked -p note-attachments --test s3_minio_test -- --nocapture
      - name: Test S3 import and export
        run: cargo test --locked -p note-pipelines --test s3_export_test -- --nocapture
      - name: Print MinIO logs on failure
        if: failure()
        run: docker logs agent-note-minio
```

The existing macOS/Windows/Ubuntu `rust-check` matrix remains unchanged. Docker-backed tests run only in the Ubuntu job and skip locally when `NOTE_TEST_MINIO_ENDPOINT` is absent.

- [ ] **Step 2: Document exact S3 configuration and credentials**

Add this example to `README.md` and describe the same fields in `docs/design.md`:

```toml
[attachments]
engine = "s3"
bucket = "agent-note"
prefix = "attachments"
region = "us-east-1"
endpoint = "http://127.0.0.1:9000"
force_path_style = true
```

Document:

- AWS S3 normally omits `endpoint` and usually leaves `force_path_style = false`;
- MinIO and similar local services set an endpoint and commonly require path style;
- `region` may be omitted to use the AWS region provider chain;
- credentials never belong in TOML and come from the standard AWS chain, including `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`, shared profiles, web identity, container credentials, and instance roles;
- final keys are `<prefix>/<note-id>/<canonical-relative-path>`, or `<note-id>/<canonical-relative-path>` when prefix is empty;
- writes upload a complete set under a unique `.staging` prefix, commit metadata, copy to final keys, remove obsolete keys, and remove staging;
- the SDK permits four total attempts (initial plus at most three retries) for SDK-classified transient failures;
- reads of one attachment issue one `GetObject`; full note/export hydration reads all declared objects;
- list operations consume every continuation token and delete requests contain at most 1,000 keys;
- permanent deletion commits database removal before object cleanup, so cleanup failure may leave safe orphan objects;
- final publication happens after database commit, can leave temporarily unreadable metadata on failure, retains the safe staging location for manual repair, and has no durable outbox/reconciler in this release;
- System information shows only engine `s3` and `s3://bucket/prefix`, never endpoint userinfo, profiles, access keys, secret keys, or session tokens.

- [ ] **Step 3: Verify the Rust 1.95 dependency floor**

```bash
rustc --version
cargo tree -p note-attachments -i aws-config
cargo tree -p note-attachments -i aws-sdk-s3
cargo check --locked -p note-attachments
```

Expected: Rust is 1.95.x; Cargo resolves `aws-config v1.9.0` and `aws-sdk-s3 v1.138.1`; `note-attachments` checks successfully. Both pinned SDK crates declare `rust-version = 1.94.1`, so they are within the project toolchain.

- [ ] **Step 4: Run the full completion gates**

With MinIO still running:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test -- --nocapture
AWS_ACCESS_KEY_ID=minioadmin \
AWS_SECRET_ACCESS_KEY=minioadmin \
AWS_EC2_METADATA_DISABLED=true \
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-pipelines --test s3_export_test -- --nocapture
(cd crates/note-frontend && cargo test)
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
git diff --check
```

Expected: every command exits successfully. The workspace suite retains filesystem behavior, MinIO proves the S3 lifecycle and cross-adapter transfer, frontend tests retain the backend-neutral System rendering, the Wasm target checks, and the diff has no whitespace errors.

- [ ] **Step 5: Inspect the final diff for security and scope**

```bash
git diff -- \
  crates/note-attachments \
  crates/note-server/src/main.rs \
  crates/note-server/src/system_api.rs \
  crates/note-pipelines/tests/s3_export_test.rs \
  .github/workflows/ci.yml README.md docs/design.md
rg -n "AWS_SECRET_ACCESS_KEY|AWS_SESSION_TOKEN|minioadmin|credentials_provider" \
  crates README.md docs/design.md
```

Expected: production Rust contains no literal credentials and never injects a credentials provider; `minioadmin` appears only in tests/CI/documented local MinIO commands; no transport payload or frontend workflow changed.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml README.md docs/design.md
git commit -m "docs(attachments): document S3 operations"
```
