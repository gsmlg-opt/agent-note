# PostgreSQL Storage Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a production PostgreSQL implementation of every `note-storage` contract, with PostgreSQL title FTS, exact 1,024-component pgvector retrieval, serialized immediate transactions, startup migration/extension verification, server composition, and real-pgvector CI coverage.

**Architecture:** Keep `note-storage` as the driver-neutral boundary and add `note-storage-pg` as a sibling of `note-storage-turso`. Move cross-backend behavioral assertions into a reusable contract-test crate, run them against both adapters, and retain backend-specific tests for PostgreSQL lifecycle, SQLSTATE mapping, advisory locking, FTS, and pgvector behavior. The server composition root selects `TursoStorage` or `PgStorage` from the typed mandatory configuration introduced by the prerequisite config/attachment plan.

**Tech Stack:** Rust 2021, Tokio, SQLx 0.9.0, PostgreSQL, pgvector PostgreSQL extension, `pgvector` Rust crate 0.4.2, Serde/JSONB, GitHub Actions service containers.

**Reference:** `docs/superpowers/specs/2026-07-18-external-adapters-design.md`, especially sections 6, 9, 10, 11, and 12.2.

---

## Execution prerequisites and scope

Implement `docs/superpowers/plans/2026-07-18-config-attachment-boundary.md` first.
This plan consumes its exact public configuration type:

```rust
pub enum DatabaseConfig {
    Embed { path: PathBuf },
    Pg { url: String, max_connections: u32 },
}
```

Create an isolated worktree before implementation:

```sh
git worktree add .trees/codex/postgresql-adapter -b codex/postgresql-adapter
cd .trees/codex/postgresql-adapter
```

Confirm the prerequisite and baseline:

```sh
rg -n 'enum DatabaseConfig|Pg \\{ url: String, max_connections: u32 \\}' \
  crates/note-server/src/config.rs
cargo fmt --all -- --check
cargo test --workspace
```

Expected: the typed `DatabaseConfig::Pg` variant exists, formatting passes, and
the native workspace tests pass. Stop if the prerequisite is absent or the
baseline fails; do not mix prerequisite implementation or unrelated repairs
into this PostgreSQL branch.

This plan does not add the remote embedding or S3 adapters. It may modify only
storage, PostgreSQL composition, PostgreSQL-facing system information,
PostgreSQL documentation, and PostgreSQL CI coverage.

## File map

### Shared behavior contracts

- Create `crates/note-storage-contract-tests/Cargo.toml`: non-published helper
  crate used only through adapter dev-dependencies.
- Create `crates/note-storage-contract-tests/src/lib.rs`: shared fixture entry
  point and deterministic helpers.
- Create `crates/note-storage-contract-tests/src/notes.rs`: note lifecycle,
  attachment metadata, labels, selectors, soft-delete, restore, and purge.
- Create `crates/note-storage-contract-tests/src/embedding.rs`: chunks and
  durable embedding-job behavior.
- Create `crates/note-storage-contract-tests/src/retrieval.rs`: title-only FTS,
  exact dense ranking, active-note filtering, validation, and ties.
- Create `crates/note-storage-contract-tests/src/settings.rs`: default and
  persisted application settings.
- Create `crates/note-storage-contract-tests/src/transactions.rs`: commit,
  rollback, drop rollback, multi-repository atomicity, and immediate
  serialization.
- Create `crates/note-storage-turso/tests/contracts_test.rs`: run the shared
  contract suite against a fresh Turso database.
- Modify `crates/note-storage-turso/Cargo.toml`: add the shared helper as a
  dev-dependency.
- Modify `crates/note-storage-turso/tests/retrieval_test.rs`: add the dense
  soft-delete regression before extracting the shared expectation.
- Modify `crates/note-storage-turso/src/retrieval.rs`: exclude soft-deleted
  notes from dense retrieval.

### PostgreSQL adapter

- Modify `Cargo.toml`: add both new crates to the native workspace.
- Create `crates/note-storage-pg/Cargo.toml`: SQLx 0.9 and pgvector 0.4.2
  dependencies.
- Create `crates/note-storage-pg/build.rs`: rerun compilation when embedded
  migrations change.
- Create `crates/note-storage-pg/migrations/0001_initial.sql`: complete
  PostgreSQL schema, generated title `tsvector`, GIN index, and
  `vector(1024)` storage without creating the extension.
- Create `crates/note-storage-pg/src/lib.rs`: `PgStorage`, startup sequence,
  contract implementations, and backend information.
- Create `crates/note-storage-pg/src/connection.rs`: pooled sessions, owned
  SQLx transactions, advisory immediate transactions, and error mapping.
- Create `crates/note-storage-pg/src/notes.rs`: `NotesRepository`.
- Create `crates/note-storage-pg/src/labels.rs`: `LabelRepository`.
- Create `crates/note-storage-pg/src/embedding_jobs.rs`:
  `EmbeddingRepository`, including atomic `SKIP LOCKED` claims.
- Create `crates/note-storage-pg/src/retrieval.rs`:
  `RetrievalRepository`, title FTS normalization, vector validation, and exact
  cosine ranking.
- Create `crates/note-storage-pg/src/settings.rs`: `SettingsRepository` backed
  by JSONB.
- Create `crates/note-storage-pg/tests/support/mod.rs`: isolated PostgreSQL
  database provisioning and cleanup for integration tests.
- Create `crates/note-storage-pg/tests/lifecycle_test.rs`: connectivity,
  missing-extension, migrations, backend info, and credential-redaction tests.
- Create `crates/note-storage-pg/tests/transactions_test.rs`: SQLx transaction
  and advisory-lock-specific behavior.
- Create `crates/note-storage-pg/tests/retrieval_test.rs`: PostgreSQL plan,
  title FTS, exact pgvector, and validation tests.
- Create `crates/note-storage-pg/tests/contracts_test.rs`: run the shared
  storage suite against PostgreSQL.

### Composition, CI, and current documentation

- Modify `crates/note-server/Cargo.toml`: depend on `note-storage-pg`.
- Modify `crates/note-server/src/main.rs`: construct the selected storage
  adapter and avoid creating local database directories for PostgreSQL.
- Modify `crates/note-server/src/system_api.rs`: assert PostgreSQL system info
  is backend-neutral and credential-free.
- Modify `.github/workflows/ci.yml`: add an Ubuntu pgvector service job and run
  the PostgreSQL suite with a real database.
- Modify `README.md`: document PostgreSQL config, required extension
  provisioning, and test command.
- Modify `docs/design.md`: record the implemented PostgreSQL adapter and exact
  retrieval semantics.

---

### Task 1: Fix Turso dense soft-delete semantics

**Files:**

- Modify: `crates/note-storage-turso/tests/retrieval_test.rs`
- Modify: `crates/note-storage-turso/src/retrieval.rs`

- [ ] **Step 1: Add the failing dense soft-delete/restore regression**

Append this test, using the existing `fixture`, `insert_named_note`, and `unit`
helpers:

```rust
#[tokio::test]
async fn dense_search_excludes_soft_deleted_notes_and_restore_reuses_vectors() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "active", "Active", "content").await;
    insert_named_note(&fixture.session, "deleted", "Deleted", "content").await;
    fixture
        .session
        .insert_chunk_embedding("active", 0, &unit(1))
        .await
        .unwrap();
    fixture
        .session
        .insert_chunk_embedding("deleted", 0, &unit(0))
        .await
        .unwrap();

    fixture
        .session
        .soft_delete_note("deleted", 10)
        .await
        .unwrap();
    assert_eq!(
        fixture.session.dense_search(&unit(0), 10).await.unwrap(),
        vec!["active"]
    );

    fixture.session.restore_note("deleted", 2).await.unwrap();
    assert_eq!(
        fixture.session.dense_search(&unit(0), 10).await.unwrap(),
        vec!["deleted", "active"]
    );
}
```

- [ ] **Step 2: Run the regression and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test retrieval_test \
  dense_search_excludes_soft_deleted_notes_and_restore_reuses_vectors -- --exact
```

Expected: FAIL because the first dense result is `deleted`; the existing query
groups embeddings without joining active notes.

- [ ] **Step 3: Filter exact dense retrieval through active notes**

Replace the Turso dense query with:

```sql
SELECT e.note_id
FROM note_chunk_embeddings e
JOIN notes n ON n.id = e.note_id
WHERE n.deleted_at IS NULL
GROUP BY e.note_id
ORDER BY MIN(vector_distance_cos(e.embedding, vector(?1))) ASC, e.note_id ASC
LIMIT ?2
```

Keep the existing 1,024-component finite-vector validation and checked limit.
Do not delete embeddings on soft-delete; restore must make the retained vectors
searchable again.

- [ ] **Step 4: Run focused Turso retrieval tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-storage-turso --test retrieval_test
```

Expected: all retrieval tests pass, including active filtering after
soft-delete and reuse after restore.

- [ ] **Step 5: Commit**

```sh
git add crates/note-storage-turso/src/retrieval.rs \
  crates/note-storage-turso/tests/retrieval_test.rs
git commit -m "fix(storage): exclude deleted notes from dense retrieval"
```

### Task 2: Add the reusable storage contract-test crate

**Files:**

- Modify: `Cargo.toml`
- Create: `crates/note-storage-contract-tests/Cargo.toml`
- Create: `crates/note-storage-contract-tests/src/lib.rs`
- Create: `crates/note-storage-contract-tests/src/notes.rs`
- Create: `crates/note-storage-contract-tests/src/embedding.rs`
- Create: `crates/note-storage-contract-tests/src/retrieval.rs`
- Create: `crates/note-storage-contract-tests/src/settings.rs`
- Create: `crates/note-storage-contract-tests/src/transactions.rs`
- Modify: `crates/note-storage-turso/Cargo.toml`
- Create: `crates/note-storage-turso/tests/contracts_test.rs`

- [ ] **Step 1: Register the helper crate and write its public entry point**

Add `"crates/note-storage-contract-tests"` to workspace members and create:

```toml
[package]
name = "note-storage-contract-tests"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
note-core = { path = "../note-core" }
note-storage = { path = "../note-storage" }
tokio = { version = "1.52.3", features = ["time"] }
```

`src/lib.rs` must contain:

```rust
mod embedding;
mod notes;
mod retrieval;
mod settings;
mod transactions;

use note_storage::StorageBackend;
use std::sync::Arc;

pub async fn run_storage_contracts(storage: Arc<dyn StorageBackend>) {
    notes::run(storage.clone()).await;
    embedding::run(storage.clone()).await;
    retrieval::run(storage.clone()).await;
    settings::run(storage.clone()).await;
    transactions::run(storage).await;
}

pub(crate) fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    vector[axis] = 1.0;
    vector
}
```

- [ ] **Step 2: Write the note and label behavior contract**

In `src/notes.rs`, define `pub(crate) async fn run(storage:
Arc<dyn StorageBackend>)`. Use fresh IDs prefixed with `contract-notes-` and
exercise these exact public calls and results:

```rust
let session = storage.session().await.unwrap();
session.insert_note(NewNote {
    id: "contract-notes-active",
    title: "Contract title",
    content: "Contract body",
    attachments: &[NoteAttachment {
        id: "meta".into(),
        path: "meta.json".into(),
        mime: "application/json".into(),
        description: "metadata".into(),
        content: b"not stored".to_vec(),
    }],
    created_at: 100,
    updated_at: 100,
    note_revision: 1,
    deleted_at: None,
}).await.unwrap();
```

Assert all of the following in the function with ordinary `assert_eq!` or
`assert!` statements:

```text
get_note returns title/content/timestamps and attachment metadata with empty bytes
get_note_revision returns 1; note_exists returns true
update_note changes title/content/metadata/revision only while active
insert_label_key_with_type/list/update preserve LabelValueType
insert_label_key_if_missing does not replace an existing type or description
attach_label/labels_for_note round-trip the value
duplicate label key and duplicate note-label attachment are Constraint errors
find_note_with_labels requires every supplied key/value and ignores key order
list/count/summaries honor ANDed typed selectors, limit, and offset
negative limits mean unbounded and negative offsets mean zero
soft_delete hides get/list/count/find and makes label counts zero
get_deleted_note_content_and_revision and deleted summaries still expose the row
restore clears deleted_at, sets the requested revision, and restores visibility
list_expired_deleted_note_ids orders by deleted_at then id
permanently_delete_note only deletes soft-deleted rows and cascades labels
list_active_note_sources returns active rows ordered by id
```

Use `parse_label_selectors("priority>=2&status=ready")` for the ANDed typed
selector assertion. Keep comparison semantics in `note-core`; the contract
requires adapter-equivalent results, not backend-specific SQL comparison.

- [ ] **Step 3: Write chunk, job, retrieval, settings, and transaction contracts**

Implement the remaining modules with these concrete entry signatures:

```rust
pub(crate) async fn run(storage: Arc<dyn StorageBackend>)
```

`embedding.rs` must insert a note plus two chunks and assert:

```text
upsert/list/get preserve chunk order, hash, content, revision, status, and time
conditional mark returns 0 for stale hash/revision and 1 for the current pair
enqueue is idempotent for the same note/chunk/hash unless status is processing
claim(0) returns empty and changes nothing
claim selects pending jobs by created_at then id and increments attempts
claim does not return the same processing job twice
fail below max attempts requeues; fail at max attempts becomes terminal
stale/trailing cleanup preserves processing jobs
requeue_processing makes interrupted work claimable
delete_embedding_job reports 1 then 0
dashboard counts only fully current embedded active notes
clear_note_chunk_derived and clear_note_chunks_from_derived remove vectors
clear_note_search_data removes chunks, jobs, and vectors
```

`retrieval.rs` must use `unit(0)`, `unit(1)`, and this normalized diagonal
vector:

```rust
let mut diagonal = vec![0.0; note_storage::EMBEDDING_DIMENSION];
diagonal[0] = 0.8;
diagonal[1] = 0.6;
```

Assert:

```text
insert_chunk_embedding rejects 1023, 1025, NaN, and infinity
dense_search rejects the same invalid query shapes
exact cosine search keeps only the best chunk per note
equal cosine distances break by note ID
limit zero returns empty
soft-deleted notes are absent and restored notes reuse retained vectors
title_search is title-only and never matches body-only terms
title updates replace old terms; soft-delete hides; restore reveals
punctuation is normalized as literal OR terms
blank/punctuation-only query and limit zero return empty
equal title ranks break by note ID
permanent deletion cascades chunks, jobs, labels, and retrieval data
```

`settings.rs` must assert `SystemConfig::default()`, then persist and retrieve:

```rust
SystemConfig {
    duplicate_check: DuplicateCheckConfig {
        enabled: true,
        rules: vec![DuplicateCheckRule {
            terms: vec![
                DuplicateCheckTerm {
                    key: "skill-name".into(),
                    value: None,
                },
                DuplicateCheckTerm {
                    key: "kind".into(),
                    value: Some("skill".into()),
                },
            ],
        }],
    },
}
```

`transactions.rs` must assert deferred commit, immediate commit, explicit
rollback, drop rollback, and atomic rollback across `notes`, `note_chunks`,
`embedding_jobs`, `note_chunk_embeddings`, and `note_labels`. For immediate
serialization, hold the first transaction and keep the second future alive
while polling it:

```rust
let second_storage = storage.clone();
let mut second = Box::pin(async move {
    second_storage
        .begin(TransactionMode::Immediate)
        .await
        .unwrap()
});
assert!(
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        second.as_mut(),
    )
    .await
    .is_err()
);
```

Commit the first transaction, then require
`tokio::time::timeout(Duration::from_secs(2), second)` to return the second
transaction and roll it back. IDs in every module must use that module's prefix
so the single suite can run sequentially in one database.

- [ ] **Step 4: Compile the backend-neutral helper crate**

Run:

```sh
cargo test -p note-storage-contract-tests
```

Expected: the crate compiles and has no standalone tests. The assertions are
deliberately inert until Step 5 supplies the first concrete backend runner.

- [ ] **Step 5: Run the contract against Turso**

Add to `note-storage-turso` dev-dependencies:

```toml
note-storage-contract-tests = { path = "../note-storage-contract-tests" }
```

Create `crates/note-storage-turso/tests/contracts_test.rs`:

```rust
use note_storage::StorageBackend;
use note_storage_turso::TursoStorage;
use std::sync::Arc;

#[tokio::test]
async fn turso_satisfies_shared_storage_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(TursoStorage::open(dir.path().join("contracts.db")).await.unwrap());

    note_storage_contract_tests::run_storage_contracts(storage).await;
}
```

Run:

```sh
cargo fmt --all
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso
```

Expected: the shared contract and all existing Turso-specific tests pass.

- [ ] **Step 6: Commit**

```sh
git add Cargo.toml Cargo.lock \
  crates/note-storage-contract-tests \
  crates/note-storage-turso/Cargo.toml \
  crates/note-storage-turso/tests/contracts_test.rs
git commit -m "test(storage): share backend behavior contracts"
```

### Task 3: Create PostgreSQL lifecycle, extension verification, and migrations

**Files:**

- Modify: `Cargo.toml`
- Create: `crates/note-storage-pg/Cargo.toml`
- Create: `crates/note-storage-pg/build.rs`
- Create: `crates/note-storage-pg/migrations/0001_initial.sql`
- Create: `crates/note-storage-pg/src/lib.rs`
- Create: `crates/note-storage-pg/src/connection.rs`
- Create: `crates/note-storage-pg/tests/support/mod.rs`
- Create: `crates/note-storage-pg/tests/lifecycle_test.rs`

- [ ] **Step 1: Register the crate with exact dependency versions**

Add `"crates/note-storage-pg"` to workspace members and create:

```toml
[package]
name = "note-storage-pg"
version = "0.1.0"
edition.workspace = true

[dependencies]
async-trait = "0.1.89"
note-core = { path = "../note-core" }
note-storage = { path = "../note-storage" }
pgvector = { version = "0.4.2", features = ["sqlx"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
sqlx = { version = "0.9.0", default-features = false, features = [
    "json",
    "macros",
    "migrate",
    "postgres",
    "runtime-tokio",
    "tls-rustls",
] }
tokio = { version = "1.52.3", features = ["sync"] }

[dev-dependencies]
note-storage-contract-tests = { path = "../note-storage-contract-tests" }
tokio = { version = "1.52.3", features = ["full"] }
url = "2"
uuid = { version = "1", features = ["v4"] }
```

Create `build.rs`:

```rust
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
```

SQLx 0.9 supplies `PgPoolOptions`, owned pool transactions, and embedded
`migrate!`; pgvector 0.4.2 supports SQLx versions `>=0.8,<0.10` and its `sqlx`
feature supplies `Vector` bindings.

- [ ] **Step 2: Write lifecycle tests before the adapter exists**

In `tests/support/mod.rs`, implement:

```rust
pub const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

pub fn configured_url() -> Option<String> {
    std::env::var(TEST_DATABASE_URL_ENV).ok()
}
```

In `tests/lifecycle_test.rs`, add tests which return early when
`TEST_DATABASE_URL` is absent and otherwise assert:

```text
an unreachable URL maps to Unavailable and does not print its password
max_connections zero is rejected before attempting a network connection
a fresh test database without CREATE EXTENSION vector fails with an error
  containing "PostgreSQL extension vector is not installed"
after test provisioning runs CREATE EXTENSION vector, PgStorage::connect runs
  every embedded migration and all expected tables/indexes exist
opening the same migrated database repeatedly is safe
BackendInfo is engine "pg" with location None and size_bytes None
```

The test provisioner must parse `TEST_DATABASE_URL` with `url::Url`, generate
`agent_note_test_<uuid simple>` (only ASCII letters, digits, and underscore),
connect to the configured administrative database, execute:

```sql
CREATE DATABASE "agent_note_test_<uuid>"
```

and return the derived database URL. Its explicit async cleanup must close the
adapter pool, terminate remaining connections, and execute:

```sql
DROP DATABASE IF EXISTS "agent_note_test_<uuid>" WITH (FORCE)
```

Provision pgvector for normal fixtures with a separate administrative
connection to the fresh database:

```sql
CREATE EXTENSION vector
```

The application adapter must never execute that statement.

- [ ] **Step 3: Run lifecycle tests and verify RED**

Run:

```sh
cargo test -p note-storage-pg --test lifecycle_test
```

Expected: compilation fails because `PgStorage` and the PostgreSQL migration do
not exist.

- [ ] **Step 4: Write the complete initial migration**

Create `migrations/0001_initial.sql` with exactly these objects:

```sql
CREATE TABLE notes (
    id text PRIMARY KEY,
    title text NOT NULL,
    content text NOT NULL,
    attachments jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    deleted_at bigint,
    title_fts tsvector GENERATED ALWAYS AS
        (to_tsvector('simple'::regconfig, title)) STORED
);

CREATE INDEX idx_notes_title_fts ON notes USING gin (title_fts);

CREATE TABLE label_keys (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key text NOT NULL UNIQUE,
    description text NOT NULL,
    value_type text NOT NULL DEFAULT 'text'
);

CREATE TABLE note_labels (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id bigint NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value text NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);

CREATE INDEX idx_note_labels_key_value
    ON note_labels(label_key_id, value);

CREATE TABLE note_chunks (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    chunk_hash text NOT NULL,
    content text NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    status text NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE INDEX idx_note_chunks_hash ON note_chunks(chunk_hash);
CREATE INDEX idx_note_chunks_status ON note_chunks(status);

CREATE TABLE embedding_jobs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    chunk_hash text NOT NULL,
    content text NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    status text NOT NULL,
    attempts bigint NOT NULL,
    error text,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    UNIQUE(note_id, chunk_idx, chunk_hash)
);

CREATE INDEX idx_embedding_jobs_status
    ON embedding_jobs(status, created_at, id);

CREATE TABLE note_chunk_embeddings (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    embedding vector(1024) NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE TABLE app_settings (
    key text PRIMARY KEY,
    value jsonb NOT NULL
);
```

Do not add HNSW, IVFFlat, or any other ANN index. SQLx owns the
`_sqlx_migrations` metadata table.

- [ ] **Step 5: Implement startup in the required order**

`src/lib.rs` exposes:

```rust
pub struct PgStorage {
    pool: sqlx::PgPool,
}

impl PgStorage {
    pub async fn connect(url: &str, max_connections: u32) -> StorageResult<Self>;
    pub async fn close(&self);
    pub fn backend_info(&self) -> BackendInfo;
}
```

`connect` must:

```rust
if max_connections == 0 {
    return Err(StorageError::new(
        StorageErrorKind::Operation,
        "PostgreSQL max_connections must be greater than zero",
    ));
}

let pool = PgPoolOptions::new()
    .max_connections(max_connections)
    .connect(url)
    .await
    .map_err(map_connect_error)?;

let extension: Option<String> = sqlx::query_scalar(
    "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
)
.fetch_optional(&pool)
.await
.map_err(|error| map_sqlx_error("verify PostgreSQL vector extension", error))?;

if extension.is_none() {
    pool.close().await;
    return Err(StorageError::new(
        StorageErrorKind::UnsupportedSchema,
        "PostgreSQL extension vector is not installed; provision pgvector before starting agent-note",
    ));
}

sqlx::migrate!("./migrations")
    .run(&pool)
    .await
    .map_err(map_migrate_error)?;

Ok(Self { pool })
```

Task 3 needs a compiling initial mapper before Task 4 adds the full SQLSTATE
table:

```rust
fn map_connect_error(error: sqlx::Error) -> StorageError {
    let kind = match &error {
        sqlx::Error::Database(database)
            if database
                .code()
                .as_deref()
                .is_some_and(|code| {
                    code.starts_with("08")
                        || matches!(code, "57P01" | "57P02" | "57P03")
                }) =>
        {
            StorageErrorKind::Unavailable
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        _ => StorageErrorKind::Operation,
    };
    StorageError::new(kind, "connect to PostgreSQL")
}

pub(crate) fn map_sqlx_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = match &error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        _ => StorageErrorKind::Operation,
    };
    StorageError::with_source(kind, context, error)
}

fn map_migrate_error(error: sqlx::migrate::MigrateError) -> StorageError {
    use sqlx::migrate::MigrateError;

    let kind = match &error {
        MigrateError::VersionMissing(_)
        | MigrateError::VersionMismatch(_)
        | MigrateError::VersionNotPresent(_)
        | MigrateError::VersionTooOld(_, _)
        | MigrateError::VersionTooNew(_, _)
        | MigrateError::Dirty(_) => StorageErrorKind::UnsupportedSchema,
        _ => StorageErrorKind::Operation,
    };
    StorageError::with_source(kind, "migrate PostgreSQL schema", error)
}
```

`map_connect_error` deliberately omits the SQLx source chain because
configuration errors may echo the connection URL. It preserves the stable
availability/operation category without allowing a password into formatted
startup errors. Task 4 must refactor it to call the full
`classify_sqlx_error` while keeping this no-source connect policy.

Return backend information from the concrete adapter:

```rust
pub fn backend_info(&self) -> BackendInfo {
    BackendInfo {
            engine: "pg".into(),
            location: None,
            size_bytes: None,
    }
}
```

Do not implement `StorageBackend` yet. Its `StorageSession` return type requires
`PgSession` to implement every repository supertrait; Task 8 adds the backend
and transaction trait implementations only after Tasks 5–7 complete all five
repositories.

Migration version/checksum/dirty-state failures map to
`StorageErrorKind::UnsupportedSchema`. Task 4 expands ordinary SQLx mapping
without changing the migration categories. Never include `url` in an error
message.

- [ ] **Step 6: Run lifecycle tests and verify GREEN**

Run without a configured database:

```sh
cargo fmt --all
cargo test -p note-storage-pg --test lifecycle_test
```

Expected: tests compile and report pass after explicitly skipping because
`TEST_DATABASE_URL` is absent.

Run against a local pgvector service:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test lifecycle_test -- --nocapture
```

Expected: missing-extension verification, migrations, repeated startup,
credential redaction, and backend info all pass against isolated databases.

- [ ] **Step 7: Commit**

```sh
git add Cargo.toml Cargo.lock crates/note-storage-pg
git commit -m "feat(storage-pg): verify pgvector and run migrations"
```

### Task 4: Implement SQLx sessions, error mapping, and advisory transactions

**Files:**

- Modify: `crates/note-storage-pg/src/connection.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`

- [ ] **Step 1: Write failing connection-module transaction tests**

In `connection.rs`'s `#[cfg(test)]` module, connect directly to
`TEST_DATABASE_URL` when configured and add:

```text
dropping an unfinished SQLx Transaction rolls back before pool reuse
two deferred transactions may begin concurrently
two immediate transactions serialize until the holder commits or rolls back
commit/rollback on a non-transaction session return Transaction errors
unique, foreign-key, and check/not-null violations map to Constraint
SQLSTATE 40001, 40P01, and 55P03 map to Conflict
pool timeout/closed, IO, TLS, protocol, and SQLSTATE class 08 map to Unavailable
```

The immediate serialization test uses the same 100 ms red window and two second
green window as the shared contract. Also query from the first transaction:

```sql
SELECT pg_try_advisory_xact_lock(4703814459809813573)
```

Expected: `true` because PostgreSQL transaction-scoped advisory locks are
reentrant for the holder.

- [ ] **Step 2: Run transaction tests and verify RED**

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg connection::tests
```

Expected: compilation or assertion failures because `PgSession`, transaction
state, advisory locking, and SQLSTATE mapping are incomplete.

- [ ] **Step 3: Implement owned session state**

In `connection.rs`, use an owned pool transaction so drop has SQLx's documented
rollback behavior:

```rust
const IMMEDIATE_ADVISORY_LOCK_KEY: i64 = 0x4147_4E54_4E4F_5445;

enum PgConnectionState {
    Session(sqlx::pool::PoolConnection<sqlx::Postgres>),
    Transaction(sqlx::Transaction<'static, sqlx::Postgres>),
}

pub struct PgSession {
    state: tokio::sync::Mutex<Option<PgConnectionState>>,
}

impl PgConnectionState {
    fn connection(&mut self) -> &mut sqlx::PgConnection {
        match self {
            Self::Session(connection) => connection.as_mut(),
            Self::Transaction(transaction) => &mut **transaction,
        }
    }
}
```

Use these constructors:

```rust
pub(crate) async fn connect(pool: &PgPool) -> StorageResult<Self> {
    let connection = pool
        .acquire()
        .await
        .map_err(|error| map_sqlx_error("acquire PostgreSQL session", error))?;
    Ok(Self {
        state: Mutex::new(Some(PgConnectionState::Session(connection))),
    })
}

pub(crate) async fn begin(
    pool: PgPool,
    mode: TransactionMode,
) -> StorageResult<Self> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| map_transaction_error("begin PostgreSQL transaction", error))?;
    if mode == TransactionMode::Immediate {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(IMMEDIATE_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                map_transaction_error("acquire immediate PostgreSQL advisory lock", error)
            })?;
    }
    Ok(Self {
        state: Mutex::new(Some(PgConnectionState::Transaction(transaction))),
    })
}
```

Expose a concrete session for adapter tests and repository implementations:

```rust
impl PgStorage {
    pub async fn connect_session(&self) -> StorageResult<PgSession> {
        PgSession::connect(&self.pool).await
    }
}
```

Each repository method locks `state`, rejects `None` as
`StorageErrorKind::Transaction`, obtains `state.connection()`, executes its
query, and drops the mutex guard only after rows are fully decoded.

Implement explicit finalization by taking the state out of the mutex:

```rust
async fn finish(self: Box<Self>, commit: bool) -> StorageResult<()> {
    let state = self.state.lock().await.take();
    let Some(PgConnectionState::Transaction(transaction)) = state else {
        return Err(StorageError::new(
            StorageErrorKind::Transaction,
            "finalize PostgreSQL storage session without an open transaction",
        ));
    };
    if commit {
        transaction
            .commit()
            .await
            .map_err(|error| map_transaction_error("commit PostgreSQL transaction", error))
    } else {
        transaction
            .rollback()
            .await
            .map_err(|error| map_transaction_error("rollback PostgreSQL transaction", error))
    }
}
```

- [ ] **Step 4: Implement stable SQLx error categories**

Use the database error's SQLSTATE without copying driver text into the public
message:

```rust
pub(crate) fn map_sqlx_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = classify_sqlx_error(&error);
    StorageError::with_source(kind, context, error)
}

fn classify_sqlx_error(error: &sqlx::Error) -> StorageErrorKind {
    match error {
        sqlx::Error::Database(database) => {
            let code = database.code();
            match code.as_deref() {
                Some("23502" | "23503" | "23505" | "23514" | "23P01") => {
                    StorageErrorKind::Constraint
                }
                Some("40001" | "40P01" | "55P03") => StorageErrorKind::Conflict,
                Some(code) if code.starts_with("08") => StorageErrorKind::Unavailable,
                Some("57P01" | "57P02" | "57P03") => StorageErrorKind::Unavailable,
                _ => StorageErrorKind::Operation,
            }
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        sqlx::Error::BeginFailed => StorageErrorKind::Transaction,
        _ => StorageErrorKind::Operation,
    }
}

fn map_connect_error(error: sqlx::Error) -> StorageError {
    StorageError::new(classify_sqlx_error(&error), "connect to PostgreSQL")
}

fn map_transaction_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = match classify_sqlx_error(&error) {
        StorageErrorKind::Operation => StorageErrorKind::Transaction,
        specific => specific,
    };
    StorageError::with_source(kind, context, error)
}
```

Because `sqlx::Error` is non-exhaustive, retain the wildcard. Public context
strings must name the operation but never contain the configured URL or
credentials. Begin, commit, and rollback keep specific constraint, conflict,
and availability categories; only otherwise-unclassified transaction failures
fall back to `Transaction`.

- [ ] **Step 5: Run transaction tests and verify GREEN**

Run:

```sh
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg connection::tests
```

Expected: owned-transaction drop rollback, deferred concurrency, advisory
immediate serialization, invalid finalization, and SQLSTATE category tests
pass without requiring repository implementations.

- [ ] **Step 6: Commit**

```sh
git add crates/note-storage-pg/src/connection.rs \
  crates/note-storage-pg/src/lib.rs
git commit -m "feat(storage-pg): add advisory transactions"
```

### Task 5: Implement notes and labels repositories

**Files:**

- Create: `crates/note-storage-pg/src/notes.rs`
- Create: `crates/note-storage-pg/src/labels.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`
- Create: `crates/note-storage-pg/tests/repositories_test.rs`

- [ ] **Step 1: Write failing note and label tests**

Port the note and label assertions from the shared contract into focused
PostgreSQL smoke tests with one active note, one deleted note, typed selectors,
JSONB attachment metadata, a duplicate label key, duplicate label attachment,
restore, and cascade purge.

Additionally assert the database representation directly:

```sql
SELECT jsonb_typeof(attachments), attachments->0->>'path'
FROM notes
WHERE id = 'pg-jsonb'
```

Expected row: `("array", "meta.json")`; stored JSON must omit attachment
content bytes. Open a separate `sqlx::PgPool` against the isolated fixture URL
for this adapter-internal representation assertion; do not expose a raw
connection through the public storage contract.

- [ ] **Step 2: Run repository tests and verify RED**

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test repositories_test
```

Expected: compilation fails because `NotesRepository` and `LabelRepository`
are not implemented for `PgSession`.

- [ ] **Step 3: Implement every `NotesRepository` method**

Implement these unchanged trait signatures in `notes.rs`:

```rust
insert_note(NewNote<'_>) -> StorageResult<()>
get_note_revision(&str) -> StorageResult<Option<i64>>
note_exists(&str) -> StorageResult<bool>
get_note(&str) -> StorageResult<Option<Note>>
get_note_content(&str) -> StorageResult<Option<String>>
update_note(NoteUpdate<'_>) -> StorageResult<u64>
soft_delete_note(&str, i64) -> StorageResult<u64>
get_deleted_note_content_and_revision(&str) -> StorageResult<Option<(String, i64)>>
restore_note(&str, i64) -> StorageResult<u64>
permanently_delete_note(&str) -> StorageResult<u64>
list_expired_deleted_note_ids(i64) -> StorageResult<Vec<String>>
clear_note_search_data(&str) -> StorageResult<()>
clear_note_labels(&str) -> StorageResult<()>
clear_note_chunk_derived(&str, i64) -> StorageResult<()>
clear_note_chunks_from_derived(&str, i64) -> StorageResult<()>
list_notes(&[LabelSelector], Option<i64>, Option<i64>) -> StorageResult<Vec<Note>>
list_all_notes() -> StorageResult<Vec<Note>>
list_note_summaries(&[LabelSelector], Option<i64>, Option<i64>)
    -> StorageResult<Vec<NoteListItem>>
list_deleted_note_summaries() -> StorageResult<Vec<NoteListItem>>
count_notes(&[LabelSelector]) -> StorageResult<usize>
list_active_note_sources() -> StorageResult<Vec<ActiveNoteSource>>
```

Use positional PostgreSQL binds (`$1`, `$2`, ...), `query`, `query_scalar`, and
`QueryResult::rows_affected()`. The mutation predicates remain:

```sql
UPDATE notes
SET title = $2, content = $3, attachments = $4,
    updated_at = $5, note_revision = $6
WHERE id = $1 AND deleted_at IS NULL

UPDATE notes SET deleted_at = $2
WHERE id = $1 AND deleted_at IS NULL

UPDATE notes SET deleted_at = NULL, note_revision = $2
WHERE id = $1 AND deleted_at IS NOT NULL

DELETE FROM notes
WHERE id = $1 AND deleted_at IS NOT NULL
```

Serialize attachments to `serde_json::Value` with fields `id`, `path`, `mime`,
and `description` only. Decode the JSONB array into `NoteAttachment` values
with `content: Vec::new()`.

For list methods, preserve current adapter semantics: fetch active rows ordered
by `created_at DESC`; attach labels; apply `label_matches_selector` in Rust when
selectors are nonempty; paginate after selector filtering. For empty selectors
normalize a negative limit to `None` (PostgreSQL `LIMIT NULL`, meaning
unbounded) and a negative offset to zero before binding SQL
`LIMIT $1 OFFSET $2`. Add focused and shared-contract assertions for both
negative cases. `list_all_notes` includes deleted rows.

`PgSession` uses a non-reentrant mutex. Scope and drop the state guard after
each note-row query before calling `labels_for_note`; likewise collect list
rows and release the guard before attaching labels. Never call one repository
method while the same task still holds the session state guard.

- [ ] **Step 4: Implement every `LabelRepository` method**

Implement these signatures:

```rust
insert_label_key
insert_label_key_if_missing
insert_label_key_with_type
list_label_keys
update_label_key
update_label_key_with_type
delete_label_key
attach_label
labels_for_note
label_note_counts
find_note_with_labels
```

Use these PostgreSQL statements for adapter-specific operations:

```sql
INSERT INTO label_keys (key, description, value_type)
VALUES ($1, $2, $3)

INSERT INTO label_keys (key, description, value_type)
VALUES ($1, $2, 'text')
ON CONFLICT (key) DO NOTHING

INSERT INTO note_labels (note_id, label_key_id, value)
SELECT $1, id, $3 FROM label_keys WHERE key = $2
```

If the final insert affects zero rows, return
`StorageErrorKind::Operation` with `unknown label key: <key>`.

For exact-label duplicate lookup, bind the requested pairs as JSONB and avoid
dynamic SQL placeholders:

```sql
WITH requested AS (
    SELECT key, value
    FROM jsonb_to_recordset($1::jsonb) AS item(key text, value text)
)
SELECT nl.note_id
FROM requested r
JOIN label_keys lk ON lk.key = r.key
JOIN note_labels nl ON nl.label_key_id = lk.id AND nl.value = r.value
JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
GROUP BY nl.note_id
HAVING COUNT(*) = (SELECT COUNT(*) FROM requested)
ORDER BY nl.note_id
LIMIT 1
```

Return `None` without a query for an empty request. Parse `value_type` through
`LabelValueType::from_str`.

- [ ] **Step 5: Verify notes and labels**

Run:

```sh
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test repositories_test
```

Expected: note and label behavior passes through concrete `PgSession` values.

- [ ] **Step 6: Commit**

```sh
git add crates/note-storage-pg/src \
  crates/note-storage-pg/tests/repositories_test.rs
git commit -m "feat(storage-pg): implement notes and labels"
```

### Task 6: Implement chunks, atomic embedding jobs, and settings

**Files:**

- Create: `crates/note-storage-pg/src/embedding_jobs.rs`
- Create: `crates/note-storage-pg/src/settings.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`
- Modify: `crates/note-storage-pg/tests/repositories_test.rs`

- [ ] **Step 1: Add failing embedding-job and settings tests**

Add tests for the exact chunk/job/settings behaviors listed in Task 2. Add a
PostgreSQL concurrency test which enqueues ten jobs, begins two deferred
transactions, calls `claim_pending_embedding_jobs(5, now)` concurrently, and
asserts that the returned ID sets are disjoint and contain all ten IDs.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test repositories_test embedding
```

Expected: compilation fails because `EmbeddingRepository` and
`SettingsRepository` are not implemented.

- [ ] **Step 3: Implement chunk and dashboard operations**

Implement every `EmbeddingRepository` signature from
`crates/note-storage/src/repositories.rs`. Use the same status strings and
ordering as Turso. Required upsert:

```sql
INSERT INTO note_chunks (
    note_id, chunk_idx, chunk_hash, content, note_revision, status, updated_at
)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (note_id, chunk_idx) DO UPDATE SET
    chunk_hash = EXCLUDED.chunk_hash,
    content = EXCLUDED.content,
    note_revision = EXCLUDED.note_revision,
    status = EXCLUDED.status,
    updated_at = EXCLUDED.updated_at
```

Required enqueue semantics:

```sql
INSERT INTO embedding_jobs (
    note_id, chunk_idx, chunk_hash, content, note_revision,
    status, attempts, error, created_at, updated_at
)
VALUES ($1, $2, $3, $4, $5, 'pending', 0, NULL, $6, $6)
ON CONFLICT (note_id, chunk_idx, chunk_hash) DO UPDATE SET
    content = EXCLUDED.content,
    note_revision = EXCLUDED.note_revision,
    status = 'pending',
    error = NULL,
    updated_at = EXCLUDED.updated_at
WHERE embedding_jobs.status <> 'processing'
```

Dashboard queries must exactly retain current semantics: count active notes
that have current chunks and no stale/non-embedded chunks; return the oldest
processing active note ordered by job `updated_at, id`.

- [ ] **Step 4: Implement atomic `SKIP LOCKED` claims**

Convert `usize` to `i64` with a checked conversion, return empty immediately
for zero, and execute this single statement on the caller's current
connection/transaction:

```sql
WITH candidates AS MATERIALIZED (
    SELECT id
    FROM embedding_jobs
    WHERE status = 'pending'
    ORDER BY created_at, id
    FOR UPDATE SKIP LOCKED
    LIMIT $1
),
claimed AS (
    UPDATE embedding_jobs AS job
    SET status = 'processing',
        attempts = job.attempts + 1,
        error = NULL,
        updated_at = $2
    FROM candidates
    WHERE job.id = candidates.id
    RETURNING job.id, job.note_id, job.chunk_idx, job.chunk_hash,
              job.content, job.note_revision, job.attempts, job.created_at
)
SELECT id, note_id, chunk_idx, chunk_hash, content, note_revision, attempts
FROM claimed
ORDER BY created_at, id
```

This is the only PostgreSQL claim implementation. Do not reproduce Turso's
select-then-loop update.

- [ ] **Step 5: Implement JSONB application settings**

Use:

```sql
SELECT value FROM app_settings WHERE key = 'system_config'

INSERT INTO app_settings (key, value)
VALUES ('system_config', $1)
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value
```

Bind `serde_json::Value`, return `SystemConfig::default()` when absent, and map
serialization/deserialization failures to `StorageErrorKind::Operation`
without database credentials.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```sh
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test repositories_test
```

Expected: chunks, dashboard, job lifecycle, concurrent disjoint claim, and
settings tests pass.

- [ ] **Step 7: Commit**

```sh
git add crates/note-storage-pg/src/embedding_jobs.rs \
  crates/note-storage-pg/src/settings.rs \
  crates/note-storage-pg/src/lib.rs \
  crates/note-storage-pg/tests/repositories_test.rs
git commit -m "feat(storage-pg): persist embedding jobs and settings"
```

### Task 7: Add PostgreSQL title FTS and exact pgvector retrieval

**Files:**

- Create: `crates/note-storage-pg/src/retrieval.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`
- Create: `crates/note-storage-pg/tests/retrieval_test.rs`

- [ ] **Step 1: Write failing vector validation and exact-ranking tests**

Use 1,024-component vectors and assert:

```rust
assert_eq!(
    session.dense_search(&unit(0), 10).await.unwrap(),
    vec!["a", "b", "c"]
);
```

Arrange `a` with two chunks so its best chunk wins, arrange `b` and `c` at the
same cosine distance so IDs break the tie, soft-delete the closest note, and
assert active filtering. Test both insertion and query validation for 1,023,
1,025, `f32::NAN`, `f32::INFINITY`, and `f32::NEG_INFINITY`.

Query `pg_indexes` and assert no index definition for
`note_chunk_embeddings` contains `hnsw` or `ivfflat`.

- [ ] **Step 2: Write failing title FTS tests**

Cover all of:

```text
title term matches while the same body-only term does not
two normalized literal terms use OR semantics
punctuation and tsquery operators in user input cannot alter query structure
blank and punctuation-only queries return empty without SQL errors
updated titles drop old terms and gain new terms
soft-delete hides and restore reveals without rebuilding title data
equal rank orders by note ID
EXPLAIN contains a Bitmap Index Scan or Index Scan using idx_notes_title_fts
```

For the plan assertion, insert enough nonmatching rows and execute:

```sql
SET LOCAL enable_seqscan = off
```

inside a transaction before `EXPLAIN`, so the GIN index use is deterministic
for the small test database. Use a separate SQLx test transaction against the
isolated fixture URL for `pg_indexes`, `SET LOCAL`, and `EXPLAIN`; keep raw
database access out of `PgStorage`'s public API.

- [ ] **Step 3: Run retrieval tests and verify RED**

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test retrieval_test
```

Expected: compilation fails because `RetrievalRepository` is not implemented.

- [ ] **Step 4: Implement shared vector validation and insertion**

Define:

```rust
fn validate_vector(vector: &[f32]) -> StorageResult<()> {
    if vector.len() != EMBEDDING_DIMENSION {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            format!(
                "embedding vector must contain exactly {EMBEDDING_DIMENSION} components, got {}",
                vector.len()
            ),
        ));
    }
    if vector.iter().any(|component| !component.is_finite()) {
        return Err(StorageError::new(
            StorageErrorKind::Operation,
            "embedding vector components must all be finite",
        ));
    }
    Ok(())
}
```

After validation bind `pgvector::Vector::from(embedding.to_vec())`:

```sql
INSERT INTO note_chunk_embeddings (note_id, chunk_idx, embedding)
VALUES ($1, $2, $3)
ON CONFLICT (note_id, chunk_idx) DO UPDATE
SET embedding = EXCLUDED.embedding
```

The upsert makes regeneration replace one derived chunk vector atomically.

- [ ] **Step 5: Implement exact cosine retrieval**

Validate before acquiring/locking the session, checked-convert the limit, and
return empty for limit zero. Use:

```sql
SELECT embedding.note_id
FROM note_chunk_embeddings AS embedding
JOIN notes ON notes.id = embedding.note_id
WHERE notes.deleted_at IS NULL
GROUP BY embedding.note_id
ORDER BY MIN(embedding.embedding <=> $1) ASC, embedding.note_id ASC
LIMIT $2
```

Bind `pgvector::Vector` and `i64`. Do not add an ANN index or use `<->`
Euclidean distance; `<=>` is exact cosine distance.

- [ ] **Step 6: Implement normalized literal title OR search**

Normalize in Rust:

```rust
fn normalize_title_query(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("'{term}'"))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" | "))
}
```

This strips PostgreSQL query operators, quotes every remaining alphanumeric
lexeme, and supplies only application-owned `|` separators. Return empty for
blank input, punctuation-only input, or zero limit. Query:

```sql
WITH query AS (
    SELECT to_tsquery('simple'::regconfig, $1) AS terms
)
SELECT notes.id
FROM notes
CROSS JOIN query
WHERE notes.deleted_at IS NULL
  AND notes.title_fts @@ query.terms
ORDER BY ts_rank_cd(notes.title_fts, query.terms) DESC, notes.id ASC
LIMIT $2
```

The generated `title_fts` column updates automatically with `title`; neither
content nor labels enter the FTS document.

- [ ] **Step 7: Run retrieval tests and verify GREEN**

Run:

```sh
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test retrieval_test
```

Expected: exact dense ranking, dimensional/finite validation, title-only FTS,
literal OR normalization, active filtering, deterministic ties, and GIN plan
assertions pass.

- [ ] **Step 8: Commit**

```sh
git add crates/note-storage-pg/src/retrieval.rs \
  crates/note-storage-pg/src/lib.rs \
  crates/note-storage-pg/tests/retrieval_test.rs
git commit -m "feat(storage-pg): add exact vector and title retrieval"
```

### Task 8: Prove PostgreSQL satisfies every shared storage contract

**Files:**

- Modify: `crates/note-storage-pg/src/connection.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`
- Create: `crates/note-storage-pg/tests/transactions_test.rs`
- Create: `crates/note-storage-pg/tests/contracts_test.rs`
- Modify: `crates/note-storage-pg/tests/support/mod.rs`

- [ ] **Step 1: Write failing contract-facing transaction tests**

Create `tests/transactions_test.rs` against an isolated pgvector fixture.
Through `StorageBackend::begin`, assert deferred commit, immediate commit,
explicit rollback, drop rollback, and multi-repository atomic rollback. The
atomic case must insert one note, chunk, job, vector, and label, deliberately
trigger a duplicate label constraint, roll back, then assert all five
note-owned tables contain zero rows while the preexisting label key remains.

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test transactions_test
```

Expected: compilation fails because the complete concrete session does not yet
implement `StorageTransaction` and `PgStorage` does not yet implement
`StorageBackend`.

- [ ] **Step 2: Add the trait implementations now that all repositories exist**

In `connection.rs`:

```rust
#[async_trait::async_trait]
impl StorageTransaction for PgSession {
    async fn commit(self: Box<Self>) -> StorageResult<()> {
        self.finish(true).await
    }

    async fn rollback(self: Box<Self>) -> StorageResult<()> {
        self.finish(false).await
    }
}
```

In `lib.rs`:

```rust
#[async_trait::async_trait]
impl StorageBackend for PgStorage {
    async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
        Ok(Box::new(self.connect_session().await?))
    }

    async fn begin(
        &self,
        mode: TransactionMode,
    ) -> StorageResult<Box<dyn StorageTransaction>> {
        Ok(Box::new(PgSession::begin(self.pool.clone(), mode).await?))
    }

    async fn info(&self) -> StorageResult<BackendInfo> {
        Ok(self.backend_info())
    }
}
```

Run:

```sh
cargo fmt --all
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test transactions_test
```

Expected: all commit, rollback, drop, atomicity, and immediate transaction
tests pass.

- [ ] **Step 3: Add the PostgreSQL shared-suite runner**

Create:

```rust
mod support;

use note_storage::StorageBackend;
use std::sync::Arc;

#[tokio::test]
async fn postgresql_satisfies_shared_storage_contracts() {
    let Some(database) = support::TestDatabase::provision_with_vector().await else {
        eprintln!("skipping: TEST_DATABASE_URL is not configured");
        return;
    };
    let storage = Arc::new(
        note_storage_pg::PgStorage::connect(database.url(), 10)
            .await
            .unwrap(),
    );
    let backend: Arc<dyn StorageBackend> = storage.clone();

    note_storage_contract_tests::run_storage_contracts(backend).await;

    storage.close().await;
    drop(storage);
    database.drop().await;
}
```

The fixture's `drop().await` must execute even on the normal successful path.
CI databases are ephemeral, but local repeated runs must not accumulate test
databases.

- [ ] **Step 4: Run the suite and inspect any semantic mismatch**

Run:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test contracts_test -- --nocapture
```

Expected: the suite passes without backend-specific conditionals. Raw Turso
and PostgreSQL FTS scores are allowed to differ; ranked note IDs and all
behavioral semantics must match.

- [ ] **Step 5: Run both adapters' full storage suites**

Run:

```sh
cargo test -p note-storage-turso
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg
```

Expected: every Turso and PostgreSQL unit, integration, and shared contract
test passes.

- [ ] **Step 6: Commit**

```sh
git add crates/note-storage-pg/src/connection.rs \
  crates/note-storage-pg/src/lib.rs \
  crates/note-storage-pg/tests/contracts_test.rs \
  crates/note-storage-pg/tests/transactions_test.rs \
  crates/note-storage-pg/tests/support/mod.rs
git commit -m "test(storage-pg): run shared backend contracts"
```

### Task 9: Compose PostgreSQL in the server without leaking credentials

**Files:**

- Modify: `crates/note-server/Cargo.toml`
- Modify: `crates/note-server/src/config.rs`
- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/src/system_api.rs`

- [ ] **Step 1: Write failing composition and system-info tests**

Extract a storage builder with an injectable config and add tests for:

```rust
async fn build_storage(config: &DatabaseConfig)
    -> anyhow::Result<Arc<dyn StorageBackend>>;
```

Test that:

```text
DatabaseConfig::Embed creates/opens only the configured Turso path
database.max_connections = 0 is rejected during active PostgreSQL config resolution
the local-directory helper creates a database parent only for Embed
an injected backend reporting engine "pg" serializes database_engine "pg",
  database_path null, and database_size_bytes null
an unreachable PgStorage connection error never contains the PostgreSQL URL,
  username, or password
```

Use a password sentinel such as `do-not-render-this-secret` and assert it is
absent from both successful System API JSON and formatted startup errors. Use
an immediately refused loopback port for the error test; do not require a
configured PostgreSQL service in the ordinary `note-server` unit suite.

Validate zero in both layers: config resolution returns an error naming
`database.max_connections`, while `PgStorage::connect(url, 0)` remains safe for
direct crate callers and fails before parsing or contacting the URL.

The concrete PostgreSQL lifecycle tests from Tasks 3 and 8 already exercise
connection construction against a real service. Task 9's startup smoke test
below proves the selected composition branch end to end.

- [ ] **Step 2: Run server tests and verify RED**

Run:

```sh
cargo test -p note-server
```

Expected: compilation/assertion failure because `note-server` does not depend
on or construct `PgStorage`.

- [ ] **Step 3: Add the adapter dependency and compose the selected backend**

Add:

```toml
note-storage-pg = { version = "0.1.0", path = "../note-storage-pg" }
```

Implement:

```rust
async fn build_storage(
    config: &note_server::config::DatabaseConfig,
) -> anyhow::Result<Arc<dyn StorageBackend>> {
    match config {
        DatabaseConfig::Embed { path } => {
            Ok(Arc::new(note_storage_turso::TursoStorage::open(path).await?))
        }
        DatabaseConfig::Pg {
            url,
            max_connections,
        } => Ok(Arc::new(
            note_storage_pg::PgStorage::connect(url, *max_connections).await?,
        )),
    }
}
```

Replace the PostgreSQL “not implemented” branch from the prerequisite plan.
Call the local directory helper only for:

```rust
if let DatabaseConfig::Embed { path } = &config.database {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
}
```

The attachment boundary continues to create filesystem directories when that
adapter is selected. Import, export, stdio, and HTTP modes all receive the same
configured storage object; import still uses `StubEmbedder` and only queues
jobs.

- [ ] **Step 4: Verify server composition and safe system information**

Run:

```sh
cargo fmt --all
cargo test -p note-server
```

Then run a PostgreSQL startup smoke test:

```sh
PGPASSWORD=postgres createdb \
  --host=127.0.0.1 --port=5432 --username=postgres agent_note
PGPASSWORD=postgres psql \
  --host=127.0.0.1 --port=5432 --username=postgres --dbname=agent_note \
  --command='CREATE EXTENSION vector'
CONFIG_DIR="$(mktemp -d)"
cat >"$CONFIG_DIR/config.toml" <<'TOML'
[database]
engine = "pg"
url = "postgresql://postgres:postgres@127.0.0.1:5432/agent_note"
max_connections = 10

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
TOML
NOTE_CONFIG_PATH="$CONFIG_DIR/config.toml" cargo run -p note-server -- --export
PGPASSWORD=postgres dropdb \
  --host=127.0.0.1 --port=5432 --username=postgres agent_note
```

Expected: startup verifies connectivity/pgvector, runs migrations, and exports
JSON without creating a local database file or printing the URL/password. The
provisioning commands are test infrastructure; application code still never
executes `CREATE EXTENSION`.

- [ ] **Step 5: Commit**

```sh
git add crates/note-server/Cargo.toml \
  crates/note-server/src/config.rs \
  crates/note-server/src/main.rs \
  crates/note-server/src/system_api.rs \
  Cargo.lock
git commit -m "feat(server): compose PostgreSQL storage"
```

### Task 10: Add a real pgvector CI service and current documentation

**Files:**

- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Add the Linux-only PostgreSQL integration job**

Keep the existing cross-platform check matrix. Add:

```yaml
  postgres-storage:
    name: PostgreSQL storage contracts
    runs-on: ubuntu-latest
    services:
      postgres:
        image: pgvector/pgvector:0.8.2-pg17
        env:
          POSTGRES_DB: postgres
          POSTGRES_USER: postgres
          POSTGRES_PASSWORD: postgres
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U postgres -d postgres"
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5
    env:
      TEST_DATABASE_URL: postgresql://postgres:postgres@127.0.0.1:5432/postgres
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run Turso storage suite
        run: cargo test -p note-storage-turso
      - name: Run PostgreSQL storage suite
        run: cargo test -p note-storage-pg
```

The test fixture, not the application, executes `CREATE EXTENSION vector` in
its isolated databases. The image supplies extension files; the adapter only
verifies extension presence.

- [ ] **Step 2: Document operator setup and exact retrieval**

Add this complete example to `README.md`:

```toml
[database]
engine = "pg"
url = "postgresql://user:password@db/agent_note"
max_connections = 10

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
```

Document operator provisioning:

```sql
CREATE EXTENSION vector;
```

State that the application role needs normal schema/migration privileges but
Agent Note never installs the extension, PostgreSQL credentials are not shown
in System information, title retrieval uses the `simple` dictionary and a GIN
index, and dense retrieval uses exact cosine `<=>` over `vector(1024)` with no
ANN index.

Update `docs/design.md` to identify `note-storage-pg` as the complete external
adapter and retain the retrieval split: title FTS plus content vectors fused by
the existing weighted RRF pipeline.

- [ ] **Step 3: Run workflow syntax and documentation checks**

Run:

```sh
git diff --check
rg -n 'pgvector/pgvector:0.8.2-pg17|TEST_DATABASE_URL|cargo test -p note-storage-pg' \
  .github/workflows/ci.yml
rg -n 'CREATE EXTENSION vector|vector\\(1024\\)|<=>' README.md docs/design.md
```

Expected: no whitespace errors; CI contains the pgvector service and test
command; both current docs describe extension provisioning and exact
1,024-dimensional cosine retrieval.

- [ ] **Step 4: Commit**

```sh
git add .github/workflows/ci.yml README.md docs/design.md
git commit -m "ci(storage): test PostgreSQL with pgvector"
```

### Task 11: Run completion gates

**Files:**

- Verify only; no planned file changes

- [ ] **Step 1: Run formatting and native checks**

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
```

Expected: both commands exit zero on the complete native workspace, including
the shared contract helper and PostgreSQL adapter.

- [ ] **Step 2: Run all native tests with PostgreSQL enabled**

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --workspace
```

Expected: all native tests pass; PostgreSQL tests provision isolated databases,
install pgvector only as test infrastructure, run migrations, and clean up.

- [ ] **Step 3: Run frontend regression gates**

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
```

Expected: frontend tests and Wasm check pass; this storage plan introduces no
new frontend workflow.

- [ ] **Step 4: Inspect the final diff and credential safety**

```sh
git diff --check
git status --short --branch
git log --oneline --decorate -12
rg -n 'CREATE EXTENSION' crates/note-storage-pg/src \
  crates/note-storage-pg/migrations && exit 1 || true
rg -n 'hnsw|ivfflat' crates/note-storage-pg/migrations && exit 1 || true
```

Expected: only planned files changed; commits are focused; application source
and migrations never install pgvector; no ANN index exists.

- [ ] **Step 5: Perform a final PostgreSQL startup check**

With a pre-provisioned test database and mandatory config:

```sh
NOTE_CONFIG_PATH=/absolute/path/to/postgresql-config.toml \
  cargo run -p note-server -- --export > /tmp/agent-note-export.json
jq -e '.version and (.notes | type == "array")' /tmp/agent-note-export.json
```

Expected: startup connects, verifies `vector`, applies migrations, exports valid
JSON, and logs no credentials.

- [ ] **Step 6: Commit any verification-only metadata only if it changed**

No commit is expected. If Cargo updated `Cargo.lock` solely because the exact
dependency resolution occurred during final verification, inspect it, then:

```sh
git add Cargo.lock
git commit -m "build(storage-pg): lock PostgreSQL dependencies"
```

Do not create an empty commit.
