# Remote BGE-M3 Embedding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a production OpenAI-compatible BGE-M3 embedding adapter, preserve the local BGE-M3 path, and atomically regenerate stored vectors whenever the configured embedding fingerprint changes.

**Architecture:** `note-embedding` owns the HTTP adapter and backend metadata, while `note-server` resolves secrets and selects local versus remote execution. The driver-neutral storage contract owns fingerprint persistence and a reset-and-requeue primitive implemented by both Turso and PostgreSQL; `note-pipelines` runs the primitive and fingerprint update in one storage transaction before starting the scheduler.

**Tech Stack:** Rust 2021, Tokio, reqwest with rustls, Serde JSON, wiremock, Turso Database 0.7, PostgreSQL/sqlx + pgvector from the PostgreSQL adapter plan, Axum, Yew/Wasm.

---

## Execution prerequisites and scope

Execute after `docs/superpowers/plans/2026-07-18-config-attachment-boundary.md`.
That plan establishes `EmbeddingConfig`, mandatory configuration, the
`AttachmentStore`-based `Context`, and the backend-neutral attachment fields in
`SystemInfo`.

The PostgreSQL adapter may be created by its own plan after this document is
written. Task 4 below remains mandatory: if `crates/note-storage-pg` is absent,
land the PostgreSQL adapter plan first, then implement the exact fingerprint
methods and tests specified here. Do not silently limit fingerprint handling to
Turso.

This plan does not change retrieval weights, PostgreSQL schema outside the
embedding settings/jobs contract, attachment adapters, or import/export JSON
shape.

Before edits:

```bash
cargo fmt --all -- --check
cargo test --workspace
(cd crates/note-frontend && cargo test)
```

Expected: all commands pass. If a baseline command fails, stop and report it;
do not fold unrelated repairs into this slice.

## File map

### Remote embedding adapter

- Create `crates/note-embedding/src/openai.rs` — request/response types,
  validation, retry classification, bounded error capture, and the reqwest
  adapter.
- Create `crates/note-embedding/tests/openai_test.rs` — wiremock request,
  response-ordering, validation, retry, timeout, and redaction tests.
- Modify `crates/note-embedding/Cargo.toml` — add reqwest/serde JSON/runtime
  dependencies and wiremock test dependency.
- Modify `crates/note-embedding/src/lib.rs` — export the adapter.
- Modify `crates/note-embedding/src/embedder.rs` — define safe backend metadata
  and the stable BGE-M3 fingerprint helper.

### Blank input behavior

- Modify `crates/note-pipelines/src/chunk.rs` — return no chunks for blank
  content.
- Modify `crates/note-pipelines/src/search_notes.rs` — return no results for a
  blank query before embedding or storage access.
- Modify `crates/note-pipelines/tests/export_test.rs` — prove blank legacy
  imported content queues no embedding job.
- Modify `crates/note-pipelines/tests/search_notes_test.rs` — prove blank
  retrieval does not call the embedder.

### Fingerprint storage and regeneration

- Modify `crates/note-storage/src/repositories.rs` — add driver-neutral
  fingerprint and reset contracts.
- Modify `crates/note-storage-turso/src/settings.rs` — store the fingerprint in
  `app_settings`.
- Modify `crates/note-storage-turso/src/embedding_jobs.rs` — atomically clear
  derived vectors and recreate active-chunk jobs.
- Modify `crates/note-storage-turso/tests/settings_test.rs` — fingerprint
  persistence.
- Modify `crates/note-storage-turso/tests/embedding_jobs_test.rs` — reset,
  active/deleted filtering, and rollback behavior.
- Modify `crates/note-storage-pg/src/settings.rs` — PostgreSQL fingerprint
  persistence.
- Modify `crates/note-storage-pg/src/embedding_jobs.rs` — PostgreSQL reset and
  requeue implementation.
- Create `crates/note-storage-pg/tests/settings_test.rs` — PostgreSQL
  fingerprint persistence.
- Create `crates/note-storage-pg/tests/embedding_jobs_test.rs` — PostgreSQL
  reset and rollback parity.
- Create `crates/note-pipelines/tests/embedding_fingerprint_test.rs` — startup
  reconciliation behavior.
- Modify `crates/note-pipelines/src/embedding_queue.rs` — run reconciliation in
  one immediate transaction.
- Modify `crates/note-pipelines/src/lib.rs` — export reconciliation types and
  function.

### Configuration, composition, and system information

- Modify `crates/note-server/src/config.rs` — finish OpenAI configuration tests
  and selected-adapter validation.
- Modify `crates/note-server/src/main.rs` — select local/remote execution,
  resolve optional bearer credentials, reconcile fingerprints, and preserve
  offline import/export.
- Modify `crates/note-server/Cargo.toml` — use the completed embedding and
  PostgreSQL crates.
- Modify `crates/note-pipelines/src/context.rs` — carry safe embedding metadata.
- Modify `crates/note-pipelines/src/system.rs` — expose embedding engine, model,
  and fingerprint.
- Modify `crates/note-pipelines/tests/system_test.rs` — backend-neutral
  embedding information.
- Modify `crates/note-server/src/system_api.rs` — API assertions that secrets
  are absent.
- Modify `crates/note-frontend/src/state.rs` — deserialize embedding metadata.
- Modify `crates/note-frontend/src/pages/system.rs` — render the three safe
  embedding fields.
- Modify `README.md` and `docs/design.md` — document remote BGE-M3 setup,
  fingerprint behavior, and operational limits.

---

### Task 1: Make blank content and blank retrieval endpoint-free

**Files:**

- Modify: `crates/note-pipelines/src/chunk.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/tests/export_test.rs`
- Modify: `crates/note-pipelines/tests/search_notes_test.rs`

- [ ] **Step 1: Write the failing blank-content test**

Add to `crates/note-pipelines/src/chunk.rs`:

```rust
#[test]
fn blank_markdown_yields_no_chunks() {
    assert!(chunk_content("").is_empty());
    assert!(chunk_content(" \n\t\r\n").is_empty());
}
```

Add to `crates/note-pipelines/tests/export_test.rs`. Use import because normal
note creation correctly rejects blank content, while legacy export data can
still contain it:

```rust
#[tokio::test]
async fn blank_imported_content_creates_no_chunks_or_embedding_jobs() {
    let (ctx, backend, _dir) = test_context().await;
    let input = r#"{
        "version": 2,
        "label_keys": [],
        "notes": [{
            "id": "blank-legacy",
            "title": "Title only",
            "content": " \n\t",
            "attachments": [],
            "created_at": 1000,
            "updated_at": 1000,
            "labels": []
        }]
    }"#;

    let stats = import_json(&ctx, input).await.unwrap();
    assert_eq!(stats.embedding_jobs_queued, 0);
    let session = backend.session().await.unwrap();
    assert!(session
        .list_note_chunks("blank-legacy")
        .await
        .unwrap()
        .is_empty());
    assert!(session
        .claim_pending_embedding_jobs(10, chrono::Utc::now().timestamp())
        .await
        .unwrap()
        .is_empty());
}
```

- [ ] **Step 2: Write the failing blank-query test**

In `crates/note-pipelines/tests/search_notes_test.rs`, add an embedder that
panics if called and build the usual Turso/filesystem context around it:

```rust
struct PanicEmbedder;

#[async_trait::async_trait]
impl note_embedding::Embedder for PanicEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<note_embedding::DenseVector> {
        panic!("blank retrieval must not call the embedder")
    }
}

#[tokio::test]
async fn blank_query_returns_no_results_without_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn note_storage::StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
        note_attachments::FilesystemAttachmentStore::new(
            dir.path().join("attachments"),
        ),
    );
    let ctx = note_pipelines::Context::new(
        storage,
        Arc::new(PanicEmbedder),
        attachments,
    );

    assert!(search_notes(&ctx, " \n\t", 10).await.unwrap().is_empty());
}
```

Import `std::sync::Arc`; the prerequisite attachment plan defines the
`FilesystemAttachmentStore::new(PathBuf)` constructor used above.

- [ ] **Step 3: Run focused tests and verify RED**

```bash
cargo test -p note-pipelines chunk::tests::blank_markdown_yields_no_chunks
cargo test -p note-pipelines --test export_test blank_imported_content
cargo test -p note-pipelines --test search_notes_test blank_query
```

Expected: the chunk assertion fails because blank input currently becomes one
empty chunk, the import test observes a queued job, and the search test panics
inside `PanicEmbedder`.

- [ ] **Step 4: Implement the two early returns**

Make `chunk_content` start with:

```rust
pub fn chunk_content(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return Vec::new();
    }

    let chunks: Vec<String> = MarkdownSplitter::new(CHUNK_CAPACITY_CHARS)
        .chunks(content)
        .map(str::to_string)
        .collect();

    chunks
}
```

The splitter cannot return zero chunks after the explicit blank check, so
remove the old `vec![content.to_string()]` fallback.

Make `search_notes_filtered` return before embedding, selector parsing, or
opening a storage session:

```rust
if limit == 0 || query.trim().is_empty() {
    return Ok(Vec::new());
}
```

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-pipelines chunk::tests
cargo test -p note-pipelines --test export_test blank_imported_content
cargo test -p note-pipelines --test search_notes_test blank_query
git add crates/note-pipelines/src/chunk.rs \
  crates/note-pipelines/src/search_notes.rs \
  crates/note-pipelines/tests/export_test.rs \
  crates/note-pipelines/tests/search_notes_test.rs
git commit -m "fix(embedding): skip blank embedding inputs"
```

Expected: every focused test passes and the commit succeeds.

---

### Task 2: Implement the OpenAI-compatible BGE-M3 adapter

**Files:**

- Create: `crates/note-embedding/src/openai.rs`
- Create: `crates/note-embedding/tests/openai_test.rs`
- Modify: `crates/note-embedding/Cargo.toml`
- Modify: `crates/note-embedding/src/embedder.rs`
- Modify: `crates/note-embedding/src/lib.rs`

- [ ] **Step 1: Add dependencies and write failing request-shape tests**

Add:

```toml
[dependencies]
reqwest = { version = "0.13.4", default-features = false, features = ["json", "rustls", "stream"] }
serde_json = "1.0.150"

[dev-dependencies]
wiremock = "0.6.5"
```

In `tests/openai_test.rs`, define:

```rust
use note_embedding::{
    Embedder, OpenAiCompatibleConfig, OpenAiCompatibleEmbedder,
    DEFAULT_EMBEDDING_DIMENSION,
};
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn vector(value: f32) -> Vec<f32> {
    vec![value; DEFAULT_EMBEDDING_DIMENSION]
}

fn config(server: &MockServer) -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig {
        base_url: server.uri(),
        model: "bge-m3".into(),
        bearer_token: None,
        timeout: Duration::from_secs(1),
        max_retries: 0,
    }
}
```

Add `unauthenticated_batch_uses_openai_shape` with this mock and assertion:

```rust
Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .and(body_json(serde_json::json!({
        "model": "bge-m3",
        "input": ["first", "second"],
        "encoding_format": "float"
    })))
    .and(header("content-type", "application/json"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "data": [
            {"index": 0, "embedding": vector(0.1)},
            {"index": 1, "embedding": vector(0.2)}
        ]
    })))
    .mount(&server)
    .await;

let adapter = OpenAiCompatibleEmbedder::new(config(&server)).unwrap();
let output = adapter
    .embed_batch(&["first".into(), "second".into()])
    .await
    .unwrap();
assert_eq!(output, vec![vector(0.1), vector(0.2)]);
let requests = server.received_requests().await.unwrap();
assert!(!requests[0].headers.contains_key("authorization"));
```

Add `optional_bearer_auth_and_configured_model_are_sent` using model
`private-bge-m3`, token `secret-token`, a
`header("authorization", "Bearer secret-token")` matcher, and a one-item
response. The request body must still omit `dimensions`.

- [ ] **Step 2: Write failing ordering and validation tests**

Use direct mock responses for these exact cases:

```text
response data indices [1, 0] -> returned vectors are reordered [0, 1]
response count 1 for two inputs -> error contains "expected 2 items, got 1"
indices [0, 0] -> error contains "duplicate response index 0"
index 2 for two inputs -> error contains "response index 2 out of range 0..2"
1023 components -> error contains "expected 1024 components, got 1023"
empty request slice -> error contains "embedding input batch is empty"
one input equal to whitespace -> error contains "embedding input 1 is blank"
```

For finite-value coverage, put this unit test beside the private response
validator in `src/openai.rs`:

```rust
#[test]
fn rejects_non_finite_components_before_returning_vectors() {
    let error = validate_response_items(
        1,
        vec![EmbeddingResponseItem {
            index: 0,
            embedding: vec![f32::NAN; DEFAULT_EMBEDDING_DIMENSION],
        }],
    )
    .unwrap_err();
    assert!(error.to_string().contains("non-finite component"));
}
```

- [ ] **Step 3: Write failing retry, timeout, and redaction tests**

Add:

```rust
#[tokio::test]
async fn retries_429_and_5xx_then_returns_success() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = calls.clone();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
                    .set_body_string("temporarily unavailable")
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": [{"index": 0, "embedding": vector(0.4)}]
                }))
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let mut cfg = config(&server);
    cfg.max_retries = 2;
    let output = OpenAiCompatibleEmbedder::new(cfg)
        .unwrap()
        .embed("retry me")
        .await
        .unwrap();
    assert_eq!(output, vector(0.4));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
```

Import `Arc`, `AtomicUsize`, and `Ordering`. Use stateful responders like this
for response sequences; `up_to_n_times` is an expectation bound, not a
sequencing mechanism. Add separate tests proving:

- HTTP 429 is retried once and a following success is returned;
- HTTP 400 is attempted once even when `max_retries = 3`;
- a delayed response exceeding a 20 ms timeout is retried exactly
  `max_retries + 1` times and returns an error containing `timed out`;
- a 401 body containing both `secret-token` and 8,000 `x` characters produces
  an error that contains `response body [REDACTED]`, contains neither
  `secret-token` nor `secret-`, and is shorter than 5,000 characters.

- [ ] **Step 4: Run adapter tests and verify RED**

```bash
cargo test -p note-embedding --test openai_test
```

Expected: compilation fails because `OpenAiCompatibleConfig` and
`OpenAiCompatibleEmbedder` do not exist.

- [ ] **Step 5: Define metadata and the adapter API**

Append to `embedder.rs`:

```rust
pub const DEFAULT_BGE_M3_MODEL: &str = "bge-m3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingBackendInfo {
    pub engine: String,
    pub model: String,
    pub fingerprint: String,
}

pub fn embedding_fingerprint(model: &str) -> String {
    format!(
        "{}:{}",
        model.trim(),
        crate::rpc::DEFAULT_EMBEDDING_DIMENSION
    )
}

impl EmbeddingBackendInfo {
    pub fn local_bge_m3() -> Self {
        Self {
            engine: "local".into(),
            model: DEFAULT_BGE_M3_MODEL.into(),
            fingerprint: embedding_fingerprint(DEFAULT_BGE_M3_MODEL),
        }
    }

    pub fn openai(model: impl Into<String>) -> Self {
        let model = model.into().trim().to_owned();
        Self {
            engine: "openai".into(),
            fingerprint: embedding_fingerprint(&model),
            model,
        }
    }
}
```

Use this public adapter configuration:

```rust
#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub model: String,
    pub bearer_token: Option<String>,
    pub timeout: Duration,
    pub max_retries: u32,
}

pub struct OpenAiCompatibleEmbedder {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    model: String,
    authorization: Option<reqwest::header::HeaderValue>,
    max_retries: u32,
}
```

Do not derive `Debug` for either type because configuration may contain the
token. When a token is present, `new` builds `Bearer {token}` as a
`HeaderValue`, calls `set_sensitive(true)`, and drops the original token after
building the header. `authorization.is_some()` selects safe omission of
authenticated response bodies. Normalize the model once with
`trim()` and use that exact value for both requests and fingerprints; reject a
blank model. Parse `base_url` first with an error that does not echo the input,
reject URL username/password components plus query strings and fragments, then
append `/v1/embeddings` through `reqwest::Url` path-segment APIs rather than
string concatenation. Reject `max_retries > 10`, and construct a reqwest client
with the configured timeout.

- [ ] **Step 6: Implement request/response validation**

Use exact wire types:

```rust
#[derive(serde::Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    encoding_format: &'static str,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingResponseItem>,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponseItem {
    index: usize,
    embedding: Vec<f32>,
}
```

`validate_response_items(expected, items)` allocates
`Vec<Option<DenseVector>>` of `expected`, rejects count mismatch first,
rejects out-of-range and duplicate indices, validates exactly 1,024
components, checks `component.is_finite()` for every value, and returns the
slots in ascending index order.

Implement `Embedder` so `embed` delegates to `embed_batch`; `embed_batch`
rejects an empty slice and rejects each `text.trim().is_empty()` with its
zero-based input index before performing any HTTP request.

- [ ] **Step 7: Implement bounded retry and safe errors**

For each attempt, rebuild the reqwest request. Clone the sensitive
`Authorization` header only when it is present. Retry only:

```rust
fn retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retryable_transport(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}
```

Map terminal transport failures to fixed category text such as
`embedding request timed out` or `embedding request connection failed`; do not
attach or format the reqwest source because it includes the endpoint URL.

Use `for attempt in 0..=max_retries` so the attempt count cannot overflow.
Before a retry, derive the exponential factor with saturating arithmetic and
cap the exponent before shifting; sleep for a jittered delay in the inclusive
range `50 * 2^(n-1)..100 * 2^(n-1)` milliseconds, with both bounds capped at
2 seconds. Derive jitter from `SystemTime::now()` nanoseconds; it need not be
seeded or exposed.

When authentication is configured, never render any response-body bytes in an
error; use the fixed text `response body [REDACTED]`. This avoids leaking a
token prefix that crosses a truncation boundary. For unauthenticated requests,
read at most 4,096 response-body bytes from `bytes_stream()` and append
`...[truncated]` when more data exists. Decode with `String::from_utf8_lossy`.
Never include request headers, the reqwest request debug value, or the raw
token in errors.

Export the module from `lib.rs`:

```rust
pub mod openai;
pub use openai::*;
```

- [ ] **Step 8: Verify GREEN and commit**

```bash
cargo fmt --all
cargo test -p note-embedding --test openai_test
cargo test -p note-embedding openai::tests
cargo test -p note-embedding
git add Cargo.lock crates/note-embedding
git commit -m "feat(embedding): add OpenAI-compatible BGE-M3 adapter"
```

Expected: every embedding test passes; wiremock confirms batching, optional
auth, retry bounds, response reordering, 1,024 finite components, and
redaction.

---

### Task 3: Add the fingerprint contract and Turso implementation

**Files:**

- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/settings.rs`
- Modify: `crates/note-storage-turso/src/embedding_jobs.rs`
- Modify: `crates/note-storage-turso/tests/settings_test.rs`
- Modify: `crates/note-storage-turso/tests/embedding_jobs_test.rs`

- [ ] **Step 1: Write failing Turso fingerprint tests**

In `settings_test.rs`, add:

```rust
#[tokio::test]
async fn embedding_fingerprint_defaults_to_none_and_persists() {
    let fixture = fixture().await;
    assert_eq!(
        fixture.session.get_embedding_fingerprint().await.unwrap(),
        None
    );
    fixture
        .session
        .set_embedding_fingerprint("bge-m3:1024")
        .await
        .unwrap();
    assert_eq!(
        fixture.session.get_embedding_fingerprint().await.unwrap().as_deref(),
        Some("bge-m3:1024")
    );
}
```

In `embedding_jobs_test.rs`, create active and soft-deleted notes with one
embedded chunk each, insert vectors, then assert:

```rust
let queued = fixture
    .session
    .reset_embeddings_for_regeneration(2000)
    .await
    .unwrap();
assert_eq!(queued, 1);
assert!(!fixture
    .session
    .chunk_embedding_exists("active", 0)
    .await
    .unwrap());
assert_eq!(
    fixture
        .session
        .get_note_chunk("active", 0)
        .await
        .unwrap()
        .unwrap()
        .status,
    "pending"
);
let jobs = fixture
    .session
    .claim_pending_embedding_jobs(10, 2001)
    .await
    .unwrap();
assert_eq!(jobs.len(), 1);
assert_eq!(jobs[0].note_id, "active");
```

Add a transaction test that sets an old fingerprint and an embedding, calls
`reset_embeddings_for_regeneration` plus `set_embedding_fingerprint` through
an immediate transaction, rolls it back, then proves the old fingerprint and
embedding still exist.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p note-storage-turso --test settings_test embedding_fingerprint
cargo test -p note-storage-turso --test embedding_jobs_test reset_embeddings
```

Expected: compilation fails because the three contract methods do not exist.

- [ ] **Step 3: Extend the driver-neutral traits**

Add to `SettingsRepository`:

```rust
async fn get_embedding_fingerprint(&self) -> StorageResult<Option<String>>;
async fn set_embedding_fingerprint(&self, fingerprint: &str) -> StorageResult<()>;
```

Add to `EmbeddingRepository`:

```rust
/// Removes every derived vector, marks active chunks pending, replaces all
/// embedding jobs with one pending job per active chunk, and returns the
/// number of queued jobs. Call this inside a storage transaction together
/// with set_embedding_fingerprint.
async fn reset_embeddings_for_regeneration(&self, now: i64) -> StorageResult<u64>;
```

The method is deliberately a single backend operation. Do not expose a
pipeline sequence of individually committed delete/update/insert calls.

- [ ] **Step 4: Implement Turso fingerprint settings**

Use key `embedding_fingerprint`. `get_embedding_fingerprint` executes:

```sql
SELECT value FROM app_settings WHERE key = ?1
```

and returns `None` for no row. `set_embedding_fingerprint` rejects a blank
fingerprint with `StorageErrorKind::Operation`, then executes:

```sql
INSERT INTO app_settings (key, value) VALUES (?1, ?2)
ON CONFLICT(key) DO UPDATE SET value = excluded.value
```

Keep system-config JSON under its existing `system_config` key; do not merge
the two values.

- [ ] **Step 5: Implement Turso reset and requeue**

In `TursoSession::reset_embeddings_for_regeneration`, execute these statements
on `self.connection` in this order:

```sql
DELETE FROM note_chunk_embeddings
```

```sql
DELETE FROM embedding_jobs
```

```sql
UPDATE note_chunks
SET status = 'pending', updated_at = ?1
WHERE EXISTS (
    SELECT 1 FROM notes
    WHERE notes.id = note_chunks.note_id
      AND notes.deleted_at IS NULL
)
```

```sql
INSERT INTO embedding_jobs (
    note_id, chunk_idx, chunk_hash, content, note_revision,
    status, attempts, error, created_at, updated_at
)
SELECT
    chunks.note_id, chunks.chunk_idx, chunks.chunk_hash, chunks.content,
    chunks.note_revision, 'pending', 0, NULL, ?1, ?1
FROM note_chunks AS chunks
JOIN notes ON notes.id = chunks.note_id
WHERE notes.deleted_at IS NULL
```

Return the affected-row count from the final insert. This clears vectors for
deleted notes too, but only active notes receive jobs.

- [ ] **Step 6: Verify GREEN and commit**

```bash
cargo fmt --all
cargo test -p note-storage --test contracts_test
cargo test -p note-storage-turso --test settings_test
cargo test -p note-storage-turso --test embedding_jobs_test
git add crates/note-storage/src/repositories.rs \
  crates/note-storage-turso/src/settings.rs \
  crates/note-storage-turso/src/embedding_jobs.rs \
  crates/note-storage-turso/tests/settings_test.rs \
  crates/note-storage-turso/tests/embedding_jobs_test.rs
git commit -m "feat(storage): persist embedding fingerprints"
```

Expected: contract compilation and all Turso settings/job tests pass, including
rollback preservation.

---

### Task 4: Implement the same contract for PostgreSQL

**Files:**

- Modify: `crates/note-storage-pg/src/settings.rs`
- Modify: `crates/note-storage-pg/src/embedding_jobs.rs`
- Create: `crates/note-storage-pg/tests/settings_test.rs`
- Create: `crates/note-storage-pg/tests/embedding_jobs_test.rs`

- [ ] **Step 1: Copy the behavioral tests to the PostgreSQL fixture**

Add tests with the same names and assertions as Task 3:

```text
embedding_fingerprint_defaults_to_none_and_persists
reset_embeddings_requeues_only_active_chunks
rolled_back_embedding_reset_preserves_fingerprint_vectors_and_jobs
```

Declare `mod support;` in both new test files and use the PostgreSQL plan's
real isolated `PgStorage`/`PgSession` fixture plus a local 1,024-component
vector helper. Set the test database's fingerprint to
`old-model:1024`, roll back a transaction containing reset plus
`bge-m3:1024`, and assert the value remains `old-model:1024`.

- [ ] **Step 2: Run focused PostgreSQL tests and verify RED**

With the PostgreSQL test URL used by that adapter plan:

```bash
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test settings_test embedding_fingerprint
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test embedding_jobs_test reset_embeddings
```

Expected: compilation fails because `PgSession` does not implement the new
trait methods.

- [ ] **Step 3: Implement PostgreSQL settings**

Use the existing PostgreSQL executor held by `PgSession`. The migration defines
`app_settings.value` as JSONB, so decode and bind `serde_json::Value`, not a
raw SQL string:

```sql
SELECT value FROM app_settings WHERE key = $1
```

```sql
INSERT INTO app_settings (key, value) VALUES ($1, $2)
ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value
```

Fetch with `fetch_optional`, return `None` for no row, and require a JSON string
when a row exists. Store `serde_json::Value::String(fingerprint.to_owned())`.
Map SQLx errors through the adapter's existing
`map_sqlx_error("query/store embedding fingerprint", error)` and malformed
stored JSON through `StorageErrorKind::Operation`. Apply the same blank-value
rejection as Turso.

- [ ] **Step 4: Implement PostgreSQL reset and requeue**

Execute through the session/transaction's existing sqlx executor:

```sql
DELETE FROM note_chunk_embeddings;
DELETE FROM embedding_jobs;
UPDATE note_chunks AS chunks
SET status = 'pending', updated_at = $1
FROM notes
WHERE notes.id = chunks.note_id
  AND notes.deleted_at IS NULL;
INSERT INTO embedding_jobs (
    note_id, chunk_idx, chunk_hash, content, note_revision,
    status, attempts, error, created_at, updated_at
)
SELECT
    chunks.note_id, chunks.chunk_idx, chunks.chunk_hash, chunks.content,
    chunks.note_revision, 'pending', 0, NULL, $1, $1
FROM note_chunks AS chunks
JOIN notes ON notes.id = chunks.note_id
WHERE notes.deleted_at IS NULL;
```

Run the statements separately on the same `PgSession` executor so an enclosing
`PgTransaction` controls atomicity. Return `rows_affected()` from the insert.
Do not begin or commit an inner transaction in the repository method.

- [ ] **Step 5: Verify PostgreSQL parity and commit**

```bash
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test settings_test
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test embedding_jobs_test
git add crates/note-storage-pg/src/settings.rs \
  crates/note-storage-pg/src/embedding_jobs.rs \
  crates/note-storage-pg/tests/settings_test.rs \
  crates/note-storage-pg/tests/embedding_jobs_test.rs
git commit -m "feat(storage-pg): support embedding regeneration"
```

Expected: PostgreSQL passes the same persistence, active/deleted filtering,
queue replacement, vector deletion, and rollback assertions as Turso.

---

### Task 5: Reconcile fingerprints atomically in pipelines

**Files:**

- Create: `crates/note-pipelines/tests/embedding_fingerprint_test.rs`
- Modify: `crates/note-pipelines/src/embedding_queue.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the three failing reconciliation tests**

Use a Turso-backed context and expose:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintReconciliation {
    Initialized,
    Unchanged,
    Regenerated { queued_jobs: usize },
}

pub async fn reconcile_embedding_fingerprint(
    storage: &dyn note_storage::StorageBackend,
    configured_fingerprint: &str,
    now: i64,
) -> anyhow::Result<FingerprintReconciliation>;
```

Tests must assert:

```rust
assert_eq!(
    reconcile_embedding_fingerprint(storage.as_ref(), "bge-m3:1024", 1000)
        .await
        .unwrap(),
    FingerprintReconciliation::Initialized
);
```

for a missing setting, without deleting an already-present legacy vector;

```rust
assert_eq!(
    reconcile_embedding_fingerprint(storage.as_ref(), "bge-m3:1024", 1001)
        .await
        .unwrap(),
    FingerprintReconciliation::Unchanged
);
```

for an equal setting, without changing jobs or vectors; and:

```rust
assert_eq!(
    reconcile_embedding_fingerprint(storage.as_ref(), "bge-m3:1024", 1002)
        .await
        .unwrap(),
    FingerprintReconciliation::Regenerated { queued_jobs: 1 }
);
```

after storing `other-model:1024`, with the old vector removed, one active chunk
pending, exactly one pending job, and the new fingerprint persisted.

The missing-setting behavior intentionally adopts `bge-m3:1024` without
regeneration because all pre-fingerprint Agent Note builds produced BGE-M3
1,024-dimensional vectors.

- [ ] **Step 2: Run tests and verify RED**

```bash
cargo test -p note-pipelines --test embedding_fingerprint_test
```

Expected: compilation fails because the reconciliation API does not exist.

- [ ] **Step 3: Implement one-transaction reconciliation**

Use exactly one immediate transaction:

```rust
pub async fn reconcile_embedding_fingerprint(
    storage: &dyn note_storage::StorageBackend,
    configured_fingerprint: &str,
    now: i64,
) -> anyhow::Result<FingerprintReconciliation> {
    if configured_fingerprint.trim().is_empty() {
        anyhow::bail!("embedding fingerprint must not be blank");
    }

    let transaction = storage.begin(TransactionMode::Immediate).await?;
    let result = async {
        match transaction.get_embedding_fingerprint().await? {
            None => {
                transaction
                    .set_embedding_fingerprint(configured_fingerprint)
                    .await?;
                anyhow::Ok(FingerprintReconciliation::Initialized)
            }
            Some(stored) if stored == configured_fingerprint => {
                anyhow::Ok(FingerprintReconciliation::Unchanged)
            }
            Some(_) => {
                let queued_jobs = transaction
                    .reset_embeddings_for_regeneration(now)
                    .await? as usize;
                transaction
                    .set_embedding_fingerprint(configured_fingerprint)
                    .await?;
                anyhow::Ok(FingerprintReconciliation::Regenerated {
                    queued_jobs,
                })
            }
        }
    }
    .await;

    crate::save_note::finish_transaction(transaction, result).await
}
```

Export the enum and function from `lib.rs`.

- [ ] **Step 4: Run Turso and PostgreSQL pipeline coverage**

```bash
cargo test -p note-pipelines --test embedding_fingerprint_test
cargo test -p note-storage-turso --test embedding_jobs_test
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test embedding_jobs_test
```

Expected: all tests pass. A changed fingerprint never exposes a new stored
fingerprint with old vectors because both changes commit or roll back together.

- [ ] **Step 5: Commit**

```bash
git add crates/note-pipelines/src/embedding_queue.rs \
  crates/note-pipelines/src/lib.rs \
  crates/note-pipelines/tests/embedding_fingerprint_test.rs
git commit -m "feat(pipelines): regenerate changed embedding spaces"
```

---

### Task 6: Wire selected configuration and preserve offline import/export

**Files:**

- Modify: `crates/note-server/src/config.rs`
- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/Cargo.toml`
- Modify: `crates/note-pipelines/src/context.rs`

- [ ] **Step 1: Finish failing config and startup tests**

In `config.rs`, retain the prerequisite plan's public shape:

```rust
EmbeddingConfig::OpenAi {
    base_url: String,
    model: String,
    api_key_env: Option<String>,
    timeout_secs: u64,
    max_retries: u32,
}
```

Add tests proving:

```text
[embedding] engine="openai", base_url="http://127.0.0.1:8000"
-> model "bge-m3", no api_key_env, timeout 30, max_retries 3

file model/timeout/max_retries values
-> override NOTE_EMBEDDING_MODEL, NOTE_EMBEDDING_TIMEOUT_SECS,
   NOTE_EMBEDDING_MAX_RETRIES

openai with missing or blank base_url
-> error names embedding.base_url

openai with timeout_secs = 0 or max_retries > 10
-> error names embedding.timeout_secs or embedding.max_retries

model = " bge-m3 "
-> normalized once to "bge-m3" for both runtime request and fingerprint

local engine with absent OpenAI fields
-> accepted

api_key_env = "EMBEDDING_API_KEY"
-> resolved config contains the name only
```

Normalize `model` with `trim().to_owned()`, require `timeout_secs > 0`, and
reject `max_retries > 10` during active OpenAI config resolution so errors name
the exact TOML fields before runtime construction.

In `main.rs`, add pure tests around:

```rust
fn resolve_bearer_token(
    api_key_env: Option<&str>,
    get_env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<String>>;
```

No name returns `None`; a configured name returns its nonblank value; missing
or blank values return an error containing the variable name but not any other
environment values.

Add `wiremock = "0.6.5"` to `note-server` dev-dependencies for the remote
startup test in Step 6.

- [ ] **Step 2: Run focused server tests and verify RED**

```bash
cargo test -p note-server --lib config::tests
cargo test -p note-server --bin note-server resolve_bearer_token
```

Expected: the config tests fail if the prerequisite OpenAI fields remain
reserved-only, and the binary test fails because token resolution does not
exist.

- [ ] **Step 3: Carry safe embedding metadata through Context**

Add:

```rust
embedding_info: note_embedding::EmbeddingBackendInfo,
```

to `Context`, plus:

```rust
pub fn embedding_info(&self) -> &note_embedding::EmbeddingBackendInfo {
    &self.embedding_info
}
```

Keep `Context::new` as the test/import convenience constructor and initialize
it with `EmbeddingBackendInfo::local_bge_m3()`. Extend
`Context::with_embedding_job_notifier` to accept an explicit
`EmbeddingBackendInfo` immediately after the embedder argument. Update its
production call sites in `main.rs`; tests using `Context::new` do not need
mechanical edits.

- [ ] **Step 4: Select local or remote runtime**

Replace the environment-only `EmbeddingExecutionMode` switch with:

```rust
struct RunningEmbedding {
    embedder: Arc<dyn note_embedding::Embedder>,
    info: EmbeddingBackendInfo,
    process_runtime: Option<Box<dyn ProcessRuntimeHandle>>,
    wake: Arc<Notify>,
}

#[async_trait::async_trait]
trait ProcessRuntimeHandle: Send {
    async fn shutdown(self: Box<Self>);
}

async fn start_embedding_runtime(
    config: &note_server::config::EmbeddingConfig,
) -> anyhow::Result<RunningEmbedding>
```

Implement `ProcessRuntimeHandle` for `ProcessWorkerRuntime` by consuming the
boxed concrete runtime and awaiting its existing `shutdown()`. This narrow
lifecycle seam also permits a unit-test fake; it does not abstract embedding
requests or worker construction.

For `Local`, start `ProcessWorkerRuntime` exactly as today and return:

```rust
RunningEmbedding {
    embedder: process_runtime.embedder(),
    info: EmbeddingBackendInfo::local_bge_m3(),
    process_runtime: Some(Box::new(process_runtime)),
    wake: Arc::new(Notify::new()),
}
```

For `OpenAi`, resolve the optional token, construct:

```rust
let info = EmbeddingBackendInfo::openai(model.clone());
let embedder = OpenAiCompatibleEmbedder::new(OpenAiCompatibleConfig {
    base_url: base_url.clone(),
    model: model.clone(),
    bearer_token,
    timeout: Duration::from_secs(*timeout_secs),
    max_retries: *max_retries,
})?;
```

and return it with `process_runtime: None`. Remove
`NOTE_EMBEDDING_MODE=remote`; `[embedding].engine` is the sole execution
selector. Local worker tuning environment variables remain supported.

- [ ] **Step 5: Put fingerprint reconciliation in startup order**

For HTTP and stdio modes:

1. construct storage and attachment adapters;
2. construct the selected embedding runtime;
3. call `reconcile_embedding_fingerprint` with
   `embedding.info.fingerprint`;
4. log only `Initialized`, `Unchanged`, or the regenerated job count;
5. construct `Context`;
6. start the scheduler and transports.

Do not log the base URL with userinfo, bearer token, named environment value,
or full remote error body.

If fingerprint reconciliation or any later startup step fails after a local
`ProcessWorkerRuntime` has started, take its boxed `process_runtime` handle and
await `shutdown()` before returning the startup error. Put this in a small
`reconcile_started_embedding(storage, &mut RunningEmbedding)` helper used by
both HTTP and stdio startup paths. Unit-test it with a fake
`ProcessRuntimeHandle` that flips an `Arc<AtomicBool>` and a failing
reconciliation backend; assert the handle is taken and the flag is set. The
existing `embedding_worker_process_test.rs` continues to prove that the real
concrete runtime terminates its child, so the unit test need not spawn the
binary test harness.

For `--import` and `--export`, keep `StubEmbedder`, do not call
`start_embedding_runtime`, do not resolve `api_key_env`, and do not reconcile
the fingerprint. Import still persists chunks/jobs through the configured
storage and attachments adapters; the next normal server start performs
fingerprint reconciliation and processes the queue.

- [ ] **Step 6: Run selected-runtime tests and verify GREEN**

Add a wiremock-backed server test proving remote startup does not spawn a local
worker and can embed one request. Add an import test with
`api_key_env = "UNSET_FOR_TEST"` proving import succeeds and queues jobs
without contacting the endpoint.

Run:

```bash
cargo fmt --all
cargo test -p note-server
cargo test -p note-pipelines --test export_test
```

Expected: all tests pass; normal remote mode uses HTTP, while import/export do
not require either embedding implementation to be available.

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates/note-server crates/note-pipelines/src/context.rs
git commit -m "feat(server): select remote BGE-M3 embeddings"
```

---

### Task 7: Report safe embedding system information

**Files:**

- Modify: `crates/note-pipelines/src/system.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/pages/system.rs`

- [ ] **Step 1: Write failing pipeline, API, and frontend tests**

Extend the post-attachment-plan DTO with:

```rust
pub embedding_engine: String,
pub embedding_model: String,
pub embedding_fingerprint: String,
```

The pipeline test must assert local defaults:

```rust
assert_eq!(info.embedding_engine, "local");
assert_eq!(info.embedding_model, "bge-m3");
assert_eq!(info.embedding_fingerprint, "bge-m3:1024");
```

The System API test must serialize an OpenAI context and assert:

```rust
assert_eq!(info["embedding_engine"], "openai");
assert_eq!(info["embedding_model"], "bge-m3");
assert_eq!(info["embedding_fingerprint"], "bge-m3:1024");
let body = String::from_utf8(bytes.to_vec()).unwrap();
assert!(!body.contains("secret-token"));
assert!(!body.contains("EMBEDDING_API_KEY"));
assert!(!body.contains("authorization"));
```

Extend the frontend deserialization test with those three JSON fields.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p note-pipelines --test system_test
cargo test -p note-server system_api::tests::info
(cd crates/note-frontend && cargo test state::tests::system_info)
```

Expected: compilation or deserialization fails because the fields are absent.

- [ ] **Step 3: Populate and render only safe metadata**

In `get_system_info`, copy engine/model/fingerprint from
`ctx.embedding_info()`. Do not add base URL, API-key environment name, token,
HTTP headers, timeout, or retry configuration to `SystemInfo`.

Mirror the fields in `crates/note-frontend/src/state.rs`. In the existing
Storage information definition list, render:

```rust
<div>
    <dt>{ "Embedding engine" }</dt>
    <dd>{ info.embedding_engine.clone() }</dd>
</div>
<div>
    <dt>{ "Embedding model" }</dt>
    <dd>{ info.embedding_model.clone() }</dd>
</div>
<div>
    <dt>{ "Embedding fingerprint" }</dt>
    <dd>{ info.embedding_fingerprint.clone() }</dd>
</div>
```

Keep this backend-neutral; do not add a configuration editing workflow.

- [ ] **Step 4: Verify GREEN and commit**

```bash
cargo fmt --all
cargo test -p note-pipelines --test system_test
cargo test -p note-server system_api::tests::info
(cd crates/note-frontend && cargo test)
git add crates/note-pipelines/src/system.rs \
  crates/note-pipelines/tests/system_test.rs \
  crates/note-server/src/system_api.rs \
  crates/note-frontend/src/state.rs \
  crates/note-frontend/src/pages/system.rs
git commit -m "feat(system): report embedding backend metadata"
```

Expected: backend and frontend tests pass and API bodies contain no credential
material.

---

### Task 8: Document and verify the complete embedding slice

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Document exact remote configuration**

Add this runnable example:

```toml
[embedding]
engine = "openai"
base_url = "http://embedding.internal:8000"
model = "bge-m3"
api_key_env = "EMBEDDING_API_KEY"
timeout_secs = 30
max_retries = 3
```

Document:

- the adapter posts batches to `/v1/embeddings` with
  `encoding_format = "float"` and no `dimensions`;
- `api_key_env` is optional and names an environment variable rather than
  containing a secret;
- responses must contain one uniquely indexed, finite 1,024-component vector
  per input;
- connection errors, timeouts, 429, and 5xx use bounded retries;
- blank retrieval returns no results and blank note content creates no
  embedding job;
- local and default remote BGE-M3 share `bge-m3:1024`;
- a changed fingerprint atomically removes old vectors and queues every active
  chunk;
- import/export do not contact the embedding service;
- System information exposes only engine, model, and fingerprint.

- [ ] **Step 2: Run adapter and cross-backend gates**

```bash
cargo test -p note-embedding
cargo test -p note-storage-turso
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg
cargo test -p note-pipelines
cargo test -p note-server
```

Expected: all focused suites pass, including wiremock and both storage
implementations.

- [ ] **Step 3: Run repository completion gates**

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
(cd crates/note-frontend && cargo test)
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
git diff --check
```

Expected: every command exits successfully with no formatting, native,
frontend, Wasm, or whitespace failures.

- [ ] **Step 4: Commit documentation**

```bash
git add README.md docs/design.md
git commit -m "docs(embedding): describe remote BGE-M3 operation"
```

Expected: the final commit succeeds and `git status --short` is empty.
