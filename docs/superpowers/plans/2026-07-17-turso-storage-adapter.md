# Turso Storage Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace libSQL with the Rust Turso Database, introduce a driver-neutral storage contract for a future PostgreSQL adapter, and resolve database and attachment settings through optional TOML, environment variables, and defaults.

**Architecture:** `note-storage` becomes a SQL-free contract crate. A new `note-storage-turso` crate owns Turso connections, schema, transactions, error mapping, and exact vector retrieval. `note-pipelines::Context` receives `Arc<dyn StorageBackend>`, while the `note-server` entrypoint is the only production module that selects and constructs the concrete adapter.

**Tech Stack:** Rust 2021, `async-trait`, Turso Database `turso` 0.7.0, Tokio, Serde, TOML, Axum, tempfile-backed integration tests.

---

## Baseline

Before this plan was written, `cargo test --workspace` passed with 167 tests and
no failures. Preserve that behavior while replacing the driver.

## Final File Structure

### Contract crate

- Modify `crates/note-storage/Cargo.toml`: add contract dependencies first,
  then remove libSQL/runtime dependencies during the final cutover.
- Modify `crates/note-storage/src/lib.rs`: export driver-neutral contracts and
  records, retaining the old concrete exports only until the final cutover.
- Create `crates/note-storage/src/error.rs`: stable error categories and
  backend-neutral error wrapper.
- Create `crates/note-storage/src/records.rs`: embedding-job, note-chunk,
  backend-information, and command records.
- Create `crates/note-storage/src/repositories.rs`: focused repository traits,
  session/unit-of-work traits, transaction modes, and backend factory trait.
- Create `crates/note-storage/tests/contracts_test.rs`: dyn compatibility and
  public error-contract tests.

### Turso adapter

- Create `crates/note-storage-turso/Cargo.toml`.
- Create `crates/note-storage-turso/schema.sql`: fresh Turso schema without
  DiskANN objects.
- Create `crates/note-storage-turso/src/lib.rs`: public `TursoStorage` adapter.
- Create `crates/note-storage-turso/src/preflight.rs`: read-only SQLite-header
  marker validation.
- Create `crates/note-storage-turso/src/connection.rs`: configured connection,
  owned transaction handle, and driver error mapping.
- Create `crates/note-storage-turso/src/notes.rs`,
  `labels.rs`, `embedding_jobs.rs`, `retrieval.rs`, and `settings.rs`: SQL
  implementations of the focused contracts.
- Create `crates/note-storage-turso/tests/support/mod.rs`: concrete
  Turso-session fixtures and deterministic 1024-component test vectors.
- Move and adapt all current `crates/note-storage/tests/*.rs` coverage into
  `crates/note-storage-turso/tests/*.rs`.

### Pipeline and composition layers

- Modify `crates/note-pipelines/src/context.rs` and every pipeline module that
  currently opens a concrete connection.
- Create `crates/note-pipelines/tests/support/mod.rs`: shared Turso-backed
  integration-test fixture that returns both `Context` and the contract handle.
- Modify MCP/server fixtures to construct `TursoStorage` while production
  transports continue to receive only `Context`.
- Create `crates/note-server/src/config.rs`: pure, injected-environment runtime
  configuration resolver.
- Modify `crates/note-server/src/main.rs`: single configuration load and
  concrete adapter construction.
- Modify `crates/note-pipelines/src/system.rs`,
  `crates/note-frontend/src/state.rs`, and
  `crates/note-frontend/src/pages/system.rs`: backend-neutral system
  information.

### Cleanup and documentation

- Remove the old SQL implementation files and tests from `note-storage`.
- Remove libSQL dependencies and vector-index maintenance.
- Update `README.md`, `docs/design.md`, `Dockerfile`, and `.gitignore`.

## Task 1: Define the Driver-Neutral Storage Contract

**Files:**

- Modify: `crates/note-storage/Cargo.toml`
- Modify: `crates/note-storage/src/lib.rs`
- Create: `crates/note-storage/src/error.rs`
- Create: `crates/note-storage/src/records.rs`
- Create: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage/src/embedding_jobs.rs`
- Create: `crates/note-storage/tests/contracts_test.rs`

- [ ] **Step 1: Write the failing dyn-compatibility and error-category tests**

Add a compile-time trait-object test and a runtime error-kind test:

```rust
use note_storage::{
    StorageBackend, StorageError, StorageErrorKind, StorageSession, StorageTransaction,
};
use std::sync::Arc;

fn accepts_backend(_: Arc<dyn StorageBackend>) {}
fn accepts_session(_: Box<dyn StorageSession>) {}
fn accepts_transaction(_: Box<dyn StorageTransaction>) {}

#[test]
fn storage_contracts_are_dyn_compatible() {
    let _ = accepts_backend;
    let _ = accepts_session;
    let _ = accepts_transaction;
}

#[test]
fn storage_errors_expose_stable_categories() {
    let error = StorageError::new(
        StorageErrorKind::IncompatibleDatabase,
        "database is not an agent-note database",
    );
    assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
    assert!(error.to_string().contains("agent-note"));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```sh
cargo test -p note-storage --test contracts_test
```

Expected: compilation fails because the contract types do not exist.

- [ ] **Step 3: Add contract dependencies without breaking existing consumers**

Add these dependencies while retaining the current concrete-adapter
dependencies until the atomic consumer cutover:

```toml
[dependencies]
async-trait = "0.1.89"
note-core = { version = "0.1.0", path = "../note-core" }
thiserror = "2"
```

`anyhow`, `libsql`, `tokio`, `serde`, and `serde_json` remain temporarily
because the unchanged pipelines still use the old adapter. They are removed in
the final cutover phase.

- [ ] **Step 4: Define stable errors and shared records**

Define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    IncompatibleDatabase,
    UnsupportedSchema,
    Constraint,
    Conflict,
    Transaction,
    Unavailable,
    Corrupt,
    Operation,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StorageError {
    kind: StorageErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

pub type StorageResult<T> = Result<T, StorageError>;
```

Provide `StorageError::new`, `StorageError::with_source`, and
`StorageError::kind`. In `records.rs`, move the current `NoteChunk`,
`EmbeddingJob`, `ProcessingEmbeddingNote`, and `EmbeddingDashboardStatus`
records unchanged. Import those records from `embedding_jobs.rs` so the old
adapter and new contracts share one definition, then add:

```rust
pub const EMBEDDING_DIMENSION: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveNoteSource {
    pub id: String,
    pub content: String,
    pub note_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub engine: String,
    pub location: Option<std::path::PathBuf>,
    pub size_bytes: Option<u64>,
}

pub struct NewNote<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub created_at: i64,
    pub updated_at: i64,
    pub note_revision: i64,
    pub deleted_at: Option<i64>,
}

pub struct NoteUpdate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
    pub note_revision: i64,
}

pub struct UpsertNoteChunk<'a> {
    pub note_id: &'a str,
    pub chunk_idx: i64,
    pub content_hash: &'a str,
    pub content: &'a str,
    pub note_revision: i64,
    pub status: &'a str,
    pub updated_at: i64,
}
```

- [ ] **Step 5: Define focused repository traits**

Use `#[async_trait]`, `&self` receivers, and `StorageResult` throughout.
Create these groups, mapping each method one-for-one to the named current
operation:

```rust
#[async_trait::async_trait]
pub trait NotesRepository: Send + Sync {
    async fn insert_note(&self, note: NewNote<'_>) -> StorageResult<()>;
    async fn get_note_revision(&self, id: &str) -> StorageResult<Option<i64>>;
    async fn note_exists(&self, id: &str) -> StorageResult<bool>;
    async fn get_note(&self, id: &str) -> StorageResult<Option<note_core::Note>>;
    async fn get_note_content(&self, id: &str) -> StorageResult<Option<String>>;
    async fn update_note(&self, note: NoteUpdate<'_>) -> StorageResult<u64>;
    async fn soft_delete_note(&self, id: &str, deleted_at: i64) -> StorageResult<u64>;
    async fn get_deleted_note_content_and_revision(
        &self,
        id: &str,
    ) -> StorageResult<Option<(String, i64)>>;
    async fn restore_note(&self, id: &str, note_revision: i64) -> StorageResult<u64>;
    async fn permanently_delete_note(&self, id: &str) -> StorageResult<u64>;
    async fn list_expired_deleted_note_ids(&self, cutoff: i64) -> StorageResult<Vec<String>>;
    async fn clear_note_search_data(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_labels(&self, id: &str) -> StorageResult<()>;
    async fn clear_note_chunk_derived(&self, id: &str, chunk_idx: i64)
        -> StorageResult<()>;
    async fn clear_note_chunks_from_derived(
        &self,
        id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<()>;
    async fn list_notes(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::Note>>;
    async fn list_all_notes(&self) -> StorageResult<Vec<note_core::Note>>;
    async fn list_note_summaries(
        &self,
        selectors: &[note_core::LabelSelector],
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn list_deleted_note_summaries(&self)
        -> StorageResult<Vec<note_core::NoteListItem>>;
    async fn count_notes(&self, selectors: &[note_core::LabelSelector])
        -> StorageResult<usize>;
    async fn list_active_note_sources(&self) -> StorageResult<Vec<ActiveNoteSource>>;
}
```

Define the remaining groups explicitly:

```rust
#[async_trait::async_trait]
pub trait LabelRepository: Send + Sync {
    async fn insert_label_key(&self, key: &str, description: &str)
        -> StorageResult<()>;
    async fn insert_label_key_if_missing(
        &self,
        key: &str,
        description: &str,
    ) -> StorageResult<()>;
    async fn insert_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn list_label_keys(&self) -> StorageResult<Vec<note_core::LabelKey>>;
    async fn update_label_key(&self, key: &str, description: &str)
        -> StorageResult<()>;
    async fn update_label_key_with_type(
        &self,
        key: &str,
        description: &str,
        value_type: note_core::LabelValueType,
    ) -> StorageResult<()>;
    async fn delete_label_key(&self, key: &str) -> StorageResult<()>;
    async fn attach_label(&self, note_id: &str, key: &str, value: &str)
        -> StorageResult<()>;
    async fn labels_for_note(&self, note_id: &str)
        -> StorageResult<Vec<note_core::Label>>;
    async fn label_note_counts(&self) -> StorageResult<Vec<(String, usize)>>;
    async fn find_note_with_labels(
        &self,
        labels: &[(String, String)],
    ) -> StorageResult<Option<String>>;
}

#[async_trait::async_trait]
pub trait EmbeddingRepository: Send + Sync {
    async fn embedding_dashboard_status(&self)
        -> StorageResult<EmbeddingDashboardStatus>;
    async fn list_note_chunks(&self, note_id: &str)
        -> StorageResult<Vec<NoteChunk>>;
    async fn get_note_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<Option<NoteChunk>>;
    async fn upsert_note_chunk(&self, chunk: UpsertNoteChunk<'_>)
        -> StorageResult<()>;
    async fn mark_note_chunk_status(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        note_revision: i64,
        status: &str,
        updated_at: i64,
    ) -> StorageResult<u64>;
    async fn delete_note_chunks_from(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn chunk_embedding_exists(
        &self,
        note_id: &str,
        chunk_idx: i64,
    ) -> StorageResult<bool>;
    async fn enqueue_embedding_job(
        &self,
        note_id: &str,
        chunk_idx: i64,
        content_hash: &str,
        content: &str,
        note_revision: i64,
        now: i64,
    ) -> StorageResult<()>;
    async fn delete_stale_embedding_jobs_for_chunk(
        &self,
        note_id: &str,
        chunk_idx: i64,
        current_hash: &str,
    ) -> StorageResult<u64>;
    async fn delete_embedding_jobs_from_chunk(
        &self,
        note_id: &str,
        min_chunk_idx: i64,
    ) -> StorageResult<u64>;
    async fn claim_pending_embedding_jobs(
        &self,
        limit: usize,
        now: i64,
    ) -> StorageResult<Vec<EmbeddingJob>>;
    async fn delete_embedding_job(&self, id: i64) -> StorageResult<u64>;
    async fn fail_embedding_job(
        &self,
        id: i64,
        attempts: i64,
        max_attempts: i64,
        error: &str,
        now: i64,
    ) -> StorageResult<u64>;
    async fn requeue_processing_embedding_jobs(&self, now: i64)
        -> StorageResult<u64>;
}

#[async_trait::async_trait]
pub trait RetrievalRepository: Send + Sync {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()>;
    async fn dense_search(&self, query: &[f32], limit: usize)
        -> StorageResult<Vec<String>>;
    async fn insert_chunk_sparse_weights(
        &self,
        note_id: &str,
        chunk_idx: i64,
        weights: &[(i64, f64)],
    ) -> StorageResult<()>;
    async fn sparse_postings_query(
        &self,
        token_ids: &[i64],
        limit: usize,
    ) -> StorageResult<Vec<String>>;
}

#[async_trait::async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_system_config(&self) -> StorageResult<note_core::SystemConfig>;
    async fn set_system_config(
        &self,
        config: &note_core::SystemConfig,
    ) -> StorageResult<()>;
}
```

- [ ] **Step 6: Define sessions, transactions, and backend construction**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionMode {
    Deferred,
    Immediate,
}

pub trait StorageSession:
    NotesRepository
    + LabelRepository
    + EmbeddingRepository
    + RetrievalRepository
    + SettingsRepository
    + Send
    + Sync
{
}

impl<T> StorageSession for T where
    T: NotesRepository
        + LabelRepository
        + EmbeddingRepository
        + RetrievalRepository
        + SettingsRepository
        + Send
        + Sync
{
}

#[async_trait::async_trait]
pub trait StorageTransaction: StorageSession {
    async fn commit(self: Box<Self>) -> StorageResult<()>;
    async fn rollback(self: Box<Self>) -> StorageResult<()>;
}

#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>>;
    async fn begin(
        &self,
        mode: TransactionMode,
    ) -> StorageResult<Box<dyn StorageTransaction>>;
    async fn info(&self) -> StorageResult<BackendInfo>;
}
```

- [ ] **Step 7: Verify GREEN and contract-source isolation**

Run:

```sh
cargo test -p note-storage --test contracts_test
cargo check --workspace --all-targets
rg -n 'libsql|turso' \
  crates/note-storage/src/error.rs \
  crates/note-storage/src/records.rs \
  crates/note-storage/src/repositories.rs
```

Expected: contract tests and the existing workspace compile; the source scan
prints no matches. The temporary libSQL dependency is removed after all
consumers switch.

- [ ] **Step 8: Commit**

```sh
git add crates/note-storage
git commit -m "feat(storage): define backend-neutral contracts"
```

## Task 2: Bootstrap the Turso Adapter, Schema, and File Preflight

**Files:**

- Modify: `Cargo.toml`
- Create: `crates/note-storage-turso/Cargo.toml`
- Create: `crates/note-storage-turso/schema.sql`
- Create: `crates/note-storage-turso/src/lib.rs`
- Create: `crates/note-storage-turso/src/preflight.rs`
- Create: `crates/note-storage-turso/src/connection.rs`
- Create: `crates/note-storage-turso/tests/schema_test.rs`

- [ ] **Step 1: Add failing fresh-open, repeated-open, marker, and legacy tests**

The tests must prove:

```rust
#[tokio::test]
async fn fresh_database_is_marked_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");
    drop(TursoStorage::open(&path).await.unwrap());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..16], b"SQLite format 3\0");
    assert_eq!(u32::from_be_bytes(bytes[68..72].try_into().unwrap()), 0x414E4F54);
    assert_eq!(u32::from_be_bytes(bytes[60..64].try_into().unwrap()), 1);
    TursoStorage::open(&path).await.unwrap();
}

#[tokio::test]
async fn unmarked_sqlite_database_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let database = turso::Builder::new_local(path.to_str().unwrap())
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute("CREATE TABLE legacy (id INTEGER)", ())
        .await
        .unwrap();
    drop(connection);
    drop(database);
    let before = std::fs::read(&path).unwrap();
    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("unmarked database was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), StorageErrorKind::IncompatibleDatabase);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
```

Also test zero-length initialization, truncated-header corruption, and
unsupported `user_version`.

- [ ] **Step 2: Run schema tests and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test schema_test
```

Expected: Cargo reports that the workspace package does not exist.

- [ ] **Step 3: Add the crate and fresh schema**

Add `crates/note-storage-turso` to workspace members. Its dependencies are:

```toml
[dependencies]
async-trait = "0.1.89"
note-core = { version = "0.1.0", path = "../note-core" }
note-storage = { version = "0.1.0", path = "../note-storage" }
serde_json = "1.0.150"
tokio = { version = "1.52.3", features = ["rt"] }
turso = "0.7.0"

[dev-dependencies]
tempfile = "3.27.0"
tokio = { version = "1.52.3", features = ["full"] }
```

Copy the current logical schema but remove
`idx_note_chunk_embedding` and every `libsql_vector_idx` expression. Keep
`note_chunk_embeddings.embedding F32_BLOB(1024) NOT NULL`.

- [ ] **Step 4: Implement read-only header preflight**

Use the first 100 bytes of a nonempty file:

```rust
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const APPLICATION_ID: u32 = 0x414E4F54;
const SCHEMA_VERSION: u32 = 1;

pub(crate) fn preflight(path: &Path) -> StorageResult<Preflight> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preflight::Fresh)
        }
        Err(error) => return Err(io_error(path, error)),
        Ok(metadata) if metadata.len() == 0 => return Ok(Preflight::Fresh),
        Ok(_) => {}
    }
    let mut header = [0_u8; 100];
    std::fs::File::open(path)
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut header))
        .map_err(|error| header_error(path, error))?;
    if &header[0..16] != SQLITE_HEADER {
        return Err(StorageError::new(
            StorageErrorKind::Corrupt,
            format!("{} is not a valid SQLite database", path.display()),
        ));
    }
    let user_version = u32::from_be_bytes(header[60..64].try_into().unwrap());
    let application_id = u32::from_be_bytes(header[68..72].try_into().unwrap());
    if application_id != APPLICATION_ID {
        return Err(StorageError::new(
            StorageErrorKind::IncompatibleDatabase,
            format!(
                "{} is not an agent-note database; remove the legacy test database and restart",
                path.display()
            ),
        ));
    }
    if user_version != SCHEMA_VERSION {
        return Err(StorageError::new(
            StorageErrorKind::UnsupportedSchema,
            format!("unsupported agent-note schema version {user_version}"),
        ));
    }
    Ok(Preflight::Existing)
}
```

- [ ] **Step 5: Implement configured connections and atomic schema creation**

`TursoStorage::open` must preflight before `Builder::new_local`, configure a
connection with a 30-second busy timeout and verified foreign keys, and for a
fresh file execute:

```sql
BEGIN IMMEDIATE;
-- every statement from schema.sql
PRAGMA application_id = 1095651156;
PRAGMA user_version = 1;
COMMIT;
```

On any initialization error, execute `ROLLBACK` and preserve the primary
error. Call `cacheflush()` after commit so the marker is visible to the next
process. Every later connection must execute `PRAGMA foreign_keys = ON` and
query it back, rejecting a value other than `1`.

Expose `TursoStorage::connect(&self) -> StorageResult<TursoSession>` for
adapter integration tests. `TursoSession` owns one private, un-cloned
`turso::Connection`; Task 5 will box the same type behind the contract traits.

- [ ] **Step 6: Verify GREEN**

Run:

```sh
cargo test -p note-storage-turso --test schema_test
```

Expected: all schema/preflight tests pass.

- [ ] **Step 7: Commit**

```sh
git add Cargo.toml Cargo.lock crates/note-storage-turso
git commit -m "feat(storage): bootstrap Turso adapter"
```

## Task 3: Port Notes, Labels, Settings, and Cascade Behavior

**Files:**

- Create: `crates/note-storage-turso/src/notes.rs`
- Create: `crates/note-storage-turso/src/labels.rs`
- Create: `crates/note-storage-turso/src/settings.rs`
- Create: `crates/note-storage-turso/tests/support/mod.rs`
- Create: `crates/note-storage-turso/tests/notes_test.rs`
- Create: `crates/note-storage-turso/tests/labels_test.rs`
- Create: `crates/note-storage-turso/tests/settings_test.rs`

- [ ] **Step 1: Adapt the existing tests before porting SQL**

Copy the note, label-key, note-label, and settings behaviors into the new
adapter tests. Replace concrete free-function calls with repository methods on
a test session. Define the shared fixture and vector helper:

```rust
pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub storage: note_storage_turso::TursoStorage,
    pub session: note_storage_turso::TursoSession,
}

pub async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let storage = note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
        .await
        .unwrap();
    let session = storage.connect().await.unwrap();
    Fixture {
        _dir: dir,
        storage,
        session,
    }
}

pub async fn insert_test_note(
    session: &note_storage_turso::TursoSession,
    id: &str,
) {
    session
        .insert_note(note_storage::NewNote {
            id,
            title: id,
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

pub fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    vector[axis] = 1.0;
    vector
}
```

Add cascade tests using only contract operations:

```rust
#[tokio::test]
async fn deleting_a_note_cascades_note_labels() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture.session.insert_label_key("status", "").await.unwrap();
    fixture
        .session
        .attach_label("note-1", "status", "ready")
        .await
        .unwrap();
    fixture.session.soft_delete_note("note-1", 2).await.unwrap();
    fixture.session.permanently_delete_note("note-1").await.unwrap();
    assert!(!fixture.session.note_exists("note-1").await.unwrap());
    assert!(fixture.session.labels_for_note("note-1").await.unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_label_key_cascades_note_labels() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "note-1").await;
    fixture.session.insert_label_key("status", "").await.unwrap();
    fixture
        .session
        .attach_label("note-1", "status", "ready")
        .await
        .unwrap();
    fixture.session.delete_label_key("status").await.unwrap();
    assert!(fixture.session.labels_for_note("note-1").await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test notes_test
cargo test -p note-storage-turso --test labels_test
cargo test -p note-storage-turso --test settings_test
```

Expected: compilation fails because the repository implementations do not
exist.

- [ ] **Step 3: Port note SQL to Turso**

Move SQL behavior from `crates/note-storage/src/notes.rs`, replacing
`libsql::params!`/`libsql::Value` with
`turso::params!`/`turso::Value`. Keep attachment JSON metadata behavior,
trash filtering, typed label filtering, pagination, and ordering unchanged.
Add the new active-note query:

```sql
SELECT id, content, note_revision
FROM notes
WHERE deleted_at IS NULL
ORDER BY id
```

- [ ] **Step 4: Port label and settings SQL to Turso**

Move every current label and setting operation. Preserve strict unique-key
errors, ignore-if-present insertion, label-value types, composite duplicate
lookup, label counts, and JSON-backed `SystemConfig`.

- [ ] **Step 5: Map Turso errors to contract categories**

Use this exact mapping in `connection.rs`, matching by reference so the
original error remains available as `StorageError::source`:

```rust
match &error {
    turso::Error::Constraint(_) => StorageErrorKind::Constraint,
    turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => StorageErrorKind::Conflict,
    turso::Error::Corrupt(_) | turso::Error::NotAdb(_) => StorageErrorKind::Corrupt,
    turso::Error::IoError(_, _)
    | turso::Error::Readonly(_)
    | turso::Error::DatabaseFull(_) => StorageErrorKind::Unavailable,
    _ => StorageErrorKind::Operation,
}
```

- [ ] **Step 6: Verify GREEN**

Run the three focused adapter tests. Expected: all pass, including both
foreign-key cascade tests.

- [ ] **Step 7: Commit**

```sh
git add crates/note-storage-turso
git commit -m "feat(storage): port note and label repositories"
```

## Task 4: Port Embedding Jobs and Exact Retrieval

**Files:**

- Create: `crates/note-storage-turso/src/embedding_jobs.rs`
- Create: `crates/note-storage-turso/src/retrieval.rs`
- Create: `crates/note-storage-turso/tests/embedding_jobs_test.rs`
- Create: `crates/note-storage-turso/tests/retrieval_test.rs`

- [ ] **Step 1: Write the exact-search and dimension tests first**

Adapt sparse/job tests and add dense assertions:

```rust
#[tokio::test]
async fn dense_search_ranks_each_note_by_its_closest_chunk() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
    insert_test_note(&fixture.session, "b").await;
    fixture.session.insert_chunk_embedding("a", 0, &unit(0)).await.unwrap();
    fixture.session.insert_chunk_embedding("a", 1, &unit(3)).await.unwrap();
    fixture.session.insert_chunk_embedding("b", 0, &unit(1)).await.unwrap();
    assert_eq!(
        fixture.session.dense_search(&unit(0), 2).await.unwrap(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn equal_dense_distances_use_note_id_order() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "b").await;
    insert_test_note(&fixture.session, "a").await;
    fixture.session.insert_chunk_embedding("b", 0, &unit(0)).await.unwrap();
    fixture.session.insert_chunk_embedding("a", 0, &unit(0)).await.unwrap();
    assert_eq!(
        fixture.session.dense_search(&unit(0), 2).await.unwrap(),
        vec!["a", "b"]
    );
}

#[tokio::test]
async fn vectors_must_have_1024_components() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
    let error = fixture
        .session
        .insert_chunk_embedding("a", 0, &[0.0; 3])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Operation);
}

#[tokio::test]
async fn deleting_a_note_cascades_chunks_jobs_and_retrieval() {
    let fixture = fixture().await;
    insert_test_note(&fixture.session, "a").await;
    fixture
        .session
        .upsert_note_chunk(UpsertNoteChunk {
            note_id: "a",
            chunk_idx: 0,
            content_hash: "hash",
            content: "content",
            note_revision: 1,
            status: "pending",
            updated_at: 1,
        })
        .await
        .unwrap();
    fixture.session.insert_chunk_embedding("a", 0, &unit(0)).await.unwrap();
    fixture
        .session
        .insert_chunk_sparse_weights("a", 0, &[(7, 1.0)])
        .await
        .unwrap();
    fixture
        .session
        .enqueue_embedding_job("a", 0, "hash", "content", 1, 1)
        .await
        .unwrap();
    fixture.session.soft_delete_note("a", 2).await.unwrap();
    fixture.session.permanently_delete_note("a").await.unwrap();
    assert!(fixture.session.list_note_chunks("a").await.unwrap().is_empty());
    assert!(fixture.session.dense_search(&unit(0), 10).await.unwrap().is_empty());
    assert!(fixture
        .session
        .sparse_postings_query(&[7], 10)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .claim_pending_embedding_jobs(10, 3)
        .await
        .unwrap()
        .is_empty());
}
```

- [ ] **Step 2: Run retrieval/job tests and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test retrieval_test
cargo test -p note-storage-turso --test embedding_jobs_test
```

Expected: compilation fails because the adapter methods are missing.

- [ ] **Step 3: Port chunk and job SQL**

Move all current chunk/job behavior, including explicit row-stream drops before
writes. Preserve conditional job claim with `WHERE status = 'pending'`,
attempt increments, stale-job cleanup, failure/requeue behavior, and dashboard
status.

- [ ] **Step 4: Implement vector validation and exact dense SQL**

Reject insertion and queries unless `len() == EMBEDDING_DIMENSION` and every
component is finite. Insert JSON through `vector(?3)`. Search with:

```sql
SELECT note_id
FROM note_chunk_embeddings
GROUP BY note_id
ORDER BY MIN(vector_distance_cos(embedding, vector(?1))) ASC, note_id ASC
LIMIT ?2
```

Preserve sparse best-chunk-per-note ranking. Add `note_id ASC` after sparse
score to make ties deterministic.

- [ ] **Step 5: Verify GREEN**

Run both focused tests. Expected: job behavior, sparse behavior, vector
validation, closest-chunk ranking, deduplication, limits, and ties all pass.

- [ ] **Step 6: Commit**

```sh
git add crates/note-storage-turso
git commit -m "feat(storage): add Turso embedding and exact retrieval"
```

## Task 5: Complete Turso Sessions and Owned Transactions

**Files:**

- Modify: `crates/note-storage-turso/src/lib.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Create: `crates/note-storage-turso/tests/transactions_test.rs`
- Create: `crates/note-storage-turso/tests/backend_test.rs`

- [ ] **Step 1: Write transaction and backend tests**

Test deferred commit, immediate commit, explicit rollback, dropped-unit
rollback, immediate writer contention, 30-second configured timeout, and
backend info. Assert `PRAGMA busy_timeout` reports `30000` rather than waiting
for a 30-second contention timeout. The dropped-unit test must open a second
session after dropping the unit and prove its inserted row is absent.

- [ ] **Step 2: Run tests and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-turso --test backend_test
```

Expected: `StorageBackend` and `StorageTransaction` are not implemented.

- [ ] **Step 3: Implement one owned handle for sessions and units**

Use the existing concrete test-session type:

```rust
pub struct TursoSession {
    connection: turso::Connection,
    transaction_open: bool,
}
```

`StorageBackend::session` boxes `TursoStorage::connect`. `begin` creates a
separate `TursoSession` and executes `BEGIN DEFERRED` or `BEGIN IMMEDIATE`.
`commit(self: Box<Self>)` executes `COMMIT`; `rollback(self: Box<Self>)`
executes `ROLLBACK`. Map failures from either finalization operation to
`StorageErrorKind::Transaction`.

Do not clone the connection held by a handle. Dropping the only connection
with an open raw transaction closes that Turso core connection and supplies
the rollback safeguard; prove this with the dropped-unit test.

- [ ] **Step 4: Implement backend information**

Return:

```rust
BackendInfo {
    engine: "embed".to_string(),
    location: Some(self.path.clone()),
    size_bytes: Some(page_count.max(0) as u64 * page_size.max(0) as u64),
}
```

- [ ] **Step 5: Verify all adapter tests**

Run:

```sh
cargo test -p note-storage-turso
```

Expected: all adapter tests pass.

- [ ] **Step 6: Commit**

```sh
git add crates/note-storage-turso
git commit -m "feat(storage): implement Turso sessions and transactions"
```

## Task 6: Atomic Consumer, Configuration, and Cleanup Cutover

Changing `Context.storage` changes the compile-time type used by every
pipeline, MCP fixture, server fixture, and server mode. Phases A-F are one
atomic task: do not commit between phases, and run the full green gate before
the single cutover commit in Phase F.

### Phase A: Switch Context and Read-Only Pipelines to Storage Contracts

**Files:**

- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `crates/note-pipelines/src/context.rs`
- Modify: `crates/note-pipelines/src/get_note.rs`
- Modify: `crates/note-pipelines/src/list_notes.rs`
- Modify: `crates/note-pipelines/src/label_keys.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/src/system.rs`
- Create: `crates/note-pipelines/tests/support/mod.rs`
- Modify: `crates/note-pipelines/tests/{label_keys,search_notes,system}_test.rs`

- [ ] **Step 1: Convert fixtures first and verify RED**

Add `note-storage-turso` as a dev dependency. The shared fixture returns:

```rust
pub async fn test_context() -> (
    Context,
    Arc<dyn StorageBackend>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let backend: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("test.db")).await.unwrap(),
    );
    let ctx = Context::new(
        backend.clone(),
        Arc::new(StubEmbedder),
        dir.path().join("attachments"),
    );
    (ctx, backend, dir)
}
```

Run the three integration tests. Expected: compilation fails because `Context`
still requires concrete `Storage`.

- [ ] **Step 2: Make Context driver-neutral and attachment-explicit**

Store a private `Arc<dyn StorageBackend>`. Require `attachments_dir` in
`Context::new` and in the notifier constructor. Remove
`default_attachment_dir()` and every pipeline-side environment read. Provide a
`pub(crate) fn storage(&self) -> &dyn StorageBackend` accessor for pipeline
modules only.

- [ ] **Step 3: Convert ordinary pipeline calls**

Each operation obtains `let session = ctx.storage().session().await?;` and
calls repository methods. Search keeps one session for label filtering, dense
search, sparse search, and note hydration. Rename comments and symbols from
ANN/libSQL to exact dense/Turso-neutral terminology.

- [ ] **Step 4: Make system information backend-neutral**

Change `SystemInfo` to:

```rust
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_path: String,
}
```

Populate it from `storage.info().await?`.

- [ ] **Step 5: Check the remaining cutover failures**

Run:

```sh
cargo check -p note-pipelines --all-targets
```

Expected: read-only modules are converted; compilation remains RED only in
the transactional, queue, or import modules handled by Phases B and C.

### Phase B: Switch Note Write Workflows to Contract Transactions

**Files:**

- Modify: `crates/note-pipelines/src/save_note.rs`
- Modify: `crates/note-pipelines/src/update_note.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`

- [ ] **Step 1: Adapt behavior tests before production code**

Replace raw database helpers with the separately returned
`Arc<dyn StorageBackend>`. Use domain methods for note existence, revisions,
chunks, and jobs. Move table-layout row-count assertions into a Turso adapter
atomicity test. Keep concurrent duplicate creation, concurrent new label-key
creation, failed-write rollback, batch restore rollback, trash purge, and
attachment assertions.

- [ ] **Step 2: Run and verify RED**

Run:

```sh
cargo test -p note-pipelines --test save_note_test
```

Expected: compilation fails at concrete connections and libSQL transaction
modes.

- [ ] **Step 3: Convert save and update transactions**

Use `TransactionMode::Immediate` for create, soft delete, restore, and purge.
Use `TransactionMode::Deferred` for update. Call repository methods on the
boxed unit and explicitly `commit().await?` on success.

For every early return after a unit begins, call `rollback().await?` before
returning. Add this finalizer and structure each multi-step unit as an async
block borrowing `&*unit`, followed by exactly one finalizer call:

```rust
async fn finish_transaction<T>(
    unit: Box<dyn note_storage::StorageTransaction>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            unit.commit().await.map_err(anyhow::Error::new)?;
            Ok(value)
        }
        Err(error) => match unit.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(error.context(format!(
                "rollback also failed: {rollback_error}"
            ))),
        },
    }
}
```

Import `anyhow::Context as _` in modules that call this helper.

- [ ] **Step 4: Preserve filesystem sequencing**

Keep prepared attachment cleanup on database failure and attachment commit
after database commit. Do not introduce cross-resource transaction behavior.

- [ ] **Step 5: Check the remaining pipeline failures**

Run `cargo check -p note-pipelines --all-targets`. Expected: note write
workflows compile; remaining RED errors are limited to embedding queue or
import/export code handled by Phase C.

### Phase C: Switch Embedding Queue and Import/Export Workflows

**Files:**

- Modify: `crates/note-pipelines/src/embedding_queue.rs`
- Modify: `crates/note-pipelines/src/export.rs`
- Modify: `crates/note-pipelines/tests/export_test.rs`

- [ ] **Step 1: Adapt stale-job and import rollback tests**

Make fixtures Turso-backed and contract-only. Keep
`invalid_version_two_attachment_rolls_back_the_import` and the stale-revision
test as the RED regression gates.

- [ ] **Step 2: Run and verify RED**

Run:

```sh
cargo test -p note-pipelines embedding_queue
cargo test -p note-pipelines --test export_test
```

Expected: compilation fails at the raw libSQL connection signature and import
transaction.

- [ ] **Step 3: Convert embedding synchronization**

Change:

```rust
pub async fn sync_note_embedding_jobs(
    storage: &dyn StorageSession,
    note_id: &str,
    chunks: &[String],
    note_revision: i64,
    now: i64,
) -> anyhow::Result<usize>
```

Replace raw active-note SQL with `list_active_note_sources`. Use deferred units
for each note synchronization, embedding completion, and embedding failure.
Use an ordinary session for requeue and dashboard operations. Claim jobs
inside an immediate unit and commit the claim before calling the embedder, so
concurrent workers cannot receive the same pending job.

- [ ] **Step 4: Convert import/export**

Use ordinary sessions for export and import preflight reads. Use one deferred
unit for imported label keys and note rows. Explicitly roll back invalid
attachment or storage failures, preserve prepared-file cleanup, commit files
after database commit, then enqueue missing embeddings.

- [ ] **Step 5: Verify GREEN and remove pipeline libSQL**

Run:

```sh
cargo test -p note-pipelines
rg -n '\blibsql::|use libsql' crates/note-pipelines
```

Expected: all pipeline tests pass and `rg` prints no matches.

### Phase D: Implement Optional Runtime Configuration

**Files:**

- Modify: `crates/note-server/Cargo.toml`
- Create: `crates/note-server/src/lib.rs`
- Create: `crates/note-server/src/config.rs`
- Modify: `crates/note-server/src/main.rs`

- [ ] **Step 1: Write pure resolver tests**

Use an injected environment record, not global environment mutation:

```rust
#[derive(Default)]
struct EnvValues {
    config_path: Option<PathBuf>,
    database_engine: Option<String>,
    database_path: Option<PathBuf>,
    attachments_dir: Option<PathBuf>,
}
```

Cover defaults, absent optional `./config.toml`, explicit missing
`NOTE_CONFIG_PATH`, relative selector path, partial TOML, file-over-environment,
environment-over-default, TOML-relative values, environment/default
working-directory values, attachment directory, unknown fields, unknown
engines, malformed TOML, and parsed-but-unsupported `pg`.

- [ ] **Step 2: Run config tests and verify RED**

Run:

```sh
cargo test -p note-server --lib config::tests
```

Expected: the library target or `config` module does not exist.

- [ ] **Step 3: Implement strict TOML records and per-field resolution**

Use:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseEngine {
    Embed,
    Pg,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    attachments_dir: Option<PathBuf>,
    database: Option<FileDatabaseConfig>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabaseConfig {
    engine: Option<DatabaseEngine>,
    path: Option<PathBuf>,
}

pub struct RuntimeConfig {
    pub database_engine: DatabaseEngine,
    pub database_path: PathBuf,
    pub attachments_dir: PathBuf,
}
```

Resolve each field as file value, then injected environment value, then
`embed`/`dev-data/notes.db`/`dev-data/attachments`. Resolve relative
`NOTE_CONFIG_PATH` from `cwd`, TOML setting paths from the selected file's
parent, and environment/default setting paths from `cwd`.

Export the resolver with `pub mod config;` from the small library target and
import it from the binary as `note_server::config`. This keeps resolver tests
independent from temporary binary compile failures elsewhere in the atomic
cutover.

Add `RuntimeConfig::validate_supported`, which succeeds for `Embed` and returns
the exact `PostgreSQL storage is not supported in this release` error for
`Pg`. The resolver still parses and returns `Pg`; startup calls this validation
before creating directories or a backend.

- [ ] **Step 4: Wire a single composition-root backend**

After the internal embedding-worker early return, load runtime configuration
once, reject `DatabaseEngine::Pg` with
`PostgreSQL storage is not supported in this release`, create both data
directories, and construct:

```rust
let storage: Arc<dyn note_storage::StorageBackend> =
    Arc::new(note_storage_turso::TursoStorage::open(&config.database_path).await?);
```

Clone that handle into export, import, stdio, and HTTP contexts. Remove
`db_path_from_env`, `attachments_dir_from_env`,
`--optimize-vector-index`, and `--vacuum`.

- [ ] **Step 5: Verify the resolver and identify remaining server fixtures**

Run:

```sh
cargo test -p note-server --lib config::tests
cargo test -p note-server tests::
```

Expected: the isolated configuration tests pass; any remaining binary/test
compile errors are confined to MCP/server fixtures converted in Phase E.

### Phase E: Adapt Transports and the System UI

**Files:**

- Modify: `crates/note-mcp/Cargo.toml`
- Modify: `crates/note-mcp/src/http.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`
- Modify: `crates/note-server/src/{labels_api,notes_api,system_api}.rs`
- Modify: `crates/note-server/tests/{labels_api,notes_api}_test.rs`
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/pages/system.rs`

- [ ] **Step 1: Convert transport test fixtures and verify RED**

Add `note-storage-turso` only where a test or the server composition root needs
it. Test helpers may return a separate `Arc<dyn StorageBackend>` for setup and
assertions; handlers and MCP server structs receive only `Context`.

Run MCP and server tests. Expected: compilation fails at concrete `Storage`
fixtures and direct `ctx.storage` usage.

- [ ] **Step 2: Replace test-only raw storage calls**

Use pipeline calls for user-visible behavior and contract sessions for job or
note setup that has no pipeline equivalent. Rewrite the libSQL-specific
comment in `labels_api.rs` around `StorageErrorKind::Constraint`.

- [ ] **Step 3: Update system-info JSON and UI**

Deserialize optional database path/size and display:

```rust
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_path: String,
}
```

Always render the engine. Render path and size rows only when present, so a
future PostgreSQL adapter does not require fake filesystem values.

- [ ] **Step 4: Verify GREEN**

Run:

```sh
cargo test -p note-mcp
cargo test -p note-server
cd crates/note-frontend && cargo test
```

Expected: all transport and frontend tests pass.

### Phase F: Remove the Old Adapter, Update Active Documentation, and Commit

**Files:**

- Delete: `crates/note-storage/schema.sql`
- Delete: `crates/note-storage/src/{db,notes,label_keys,note_labels,embedding_jobs,retrieval,settings}.rs`
- Delete: `crates/note-storage/tests/{schema,notes,label_keys,note_labels,embedding_jobs,retrieval,settings}_test.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/design.md`
- Modify: `Dockerfile`
- Modify: `.gitignore`

- [ ] **Step 1: Remove concrete files and dependencies**

Delete the old libSQL implementation after every consumer uses contracts.
Remove all libSQL packages from manifests/lockfile. Keep
`note-storage-turso` as a workspace member and the server's concrete
production dependency.

- [ ] **Step 2: Add example configuration and ignore local config**

Document:

```toml
attachments_dir = "dev-data/attachments"

[database]
engine = "embed"
path = "dev-data/notes.db"
```

Add `/config.toml` to `.gitignore`; do not ignore example documentation.
Document `NOTE_CONFIG_PATH`, `NOTE_DB_ENGINE`, `NOTE_DB_PATH`, and
`NOTE_ATTACHMENTS_DIR`, including file-over-environment-over-default
precedence and relative path bases.

- [ ] **Step 3: Replace ANN and maintenance documentation**

Describe exact linear cosine search, closest-chunk note ranking, the accepted
performance trade-off, the clean-break handling for test databases, and the
future `pg` engine value. Remove every vector-index maintenance command.
Change Docker comments from libSQL/ANN language to Turso/exact retrieval;
existing environment defaults remain valid fallback configuration.

- [ ] **Step 4: Run full verification**

Run:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cd crates/note-frontend && cargo test
cargo tree --workspace
rg -n '\blibsql::|use libsql|libsql_vector_idx|vector_top_k|optimize-vector-index' \
  Cargo.toml Cargo.lock crates README.md docs/design.md Dockerfile
git diff --check
```

Expected: formatting, checks, native tests, and frontend tests pass. The final
`rg` prints no matches. Historical files under `docs/superpowers/` are excluded
because they remain historical records.

- [ ] **Step 5: Commit**

```sh
git add Cargo.toml Cargo.lock .gitignore Dockerfile README.md docs/design.md crates
git commit -m "feat(storage): replace libSQL with Turso Database"
```

## Task 7: Final Architecture and Dependency Gate

**Files:**

- Verify all files changed by Tasks 1-6

- [ ] **Step 1: Verify the concrete-adapter boundary**

Run:

```sh
rg -n 'note_storage_turso|turso::' crates \
  -g '*.rs' -g 'Cargo.toml'
```

Expected production matches: `note-storage-turso` itself and the narrow
`note-server` composition root. Other matches may occur only in test fixtures
or dev-dependencies.

- [ ] **Step 2: Verify repository cleanliness**

Run:

```sh
git status --short --branch
git log --oneline --decorate -12
```

Expected: no uncommitted changes and a sequence of focused conventional
commits for the contract, Turso adapter slices, and the atomic
consumer/configuration/documentation cutover.
