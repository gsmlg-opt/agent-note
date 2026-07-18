# Mandatory Config and Attachment Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `dev-data/config.toml` mandatory, generate it only for implicit debug/dev startup, and replace pipeline filesystem attachment access with an async adapter while preserving local behavior.

**Architecture:** `note-server` resolves a typed, active-adapter configuration with file → environment → default precedence. A new `note-attachments` crate owns an object-safe attachment contract and filesystem implementation; `note-pipelines::Context` receives the adapter rather than a path.

**Tech Stack:** Rust 2021, Tokio, Serde/TOML, async-trait, tempfile, existing Turso storage and Axum/Yew tests.

---

## File map

**Create**

- `crates/note-attachments/Cargo.toml` — attachment crate dependencies.
- `crates/note-attachments/src/lib.rs` — public contract and exports.
- `crates/note-attachments/src/path.rs` — canonical relative-path validation.
- `crates/note-attachments/src/filesystem.rs` — filesystem adapter and prepared write.
- `crates/note-attachments/tests/filesystem_test.rs` — adapter behavior tests.
- `crates/note-pipelines/src/note_attachments.rs` — pipeline orchestration and hydration.

**Modify**

- `Cargo.toml` — add the attachment crate to the workspace.
- `crates/note-server/Cargo.toml` — add `note-attachments` and production `tempfile`.
- `crates/note-server/src/config.rs` — mandatory typed configuration and generation.
- `crates/note-server/src/main.rs` — build the filesystem adapter from config.
- `crates/note-pipelines/Cargo.toml` — depend on `note-attachments`.
- `crates/note-pipelines/src/context.rs` — inject `Arc<dyn AttachmentStore>`.
- `crates/note-pipelines/src/lib.rs` — replace the filesystem-only module.
- `crates/note-pipelines/src/save_note.rs` — publish or abort prepared attachments.
- `crates/note-pipelines/src/update_note.rs` — update and remove attachment sets.
- `crates/note-pipelines/src/get_note.rs` — hydrate note attachments.
- `crates/note-pipelines/src/list_notes.rs` — hydrate listed notes.
- `crates/note-pipelines/src/search_notes.rs` — hydrate retrieved notes.
- `crates/note-pipelines/src/export.rs` — prepare imports and hydrate exports.
- `crates/note-pipelines/src/system.rs` — report attachment adapter information.
- `crates/note-pipelines/tests/support/mod.rs` — construct a filesystem adapter for integration tests.
- `crates/note-pipelines/tests/{save_note_test,export_test,system_test}.rs` — preserve attachment behavior assertions.
- `crates/note-server/src/notes_api.rs` — update contexts and single-object download.
- `crates/note-server/src/labels_api.rs` — update test contexts.
- `crates/note-server/src/system_api.rs` — update contexts and system info assertions.
- `crates/note-server/tests/labels_api_test.rs` — update test contexts.
- `crates/note-server/tests/notes_api_test.rs` — update test contexts.
- `crates/note-mcp/src/http.rs` — update test contexts.
- `crates/note-mcp/src/stdio.rs` — update test contexts.
- `crates/note-mcp/tests/tools_test.rs` — update test contexts.
- `crates/note-frontend/src/state.rs` and `crates/note-frontend/src/pages/system.rs` — render backend-neutral attachment info.
- `README.md` and `docs/design.md` — document mandatory local config and attachment boundary.

**Delete**

- `crates/note-pipelines/src/attachment_files.rs` — behavior moves to `note-attachments` and `note_attachments.rs`.

### Task 1: Mandatory typed configuration

**Files:**

- Modify: `crates/note-server/src/config.rs`
- Modify: `crates/note-server/Cargo.toml`

- [ ] **Step 1: Write failing configuration tests**

Replace the old optional-config expectations with tests covering:

```rust
#[test]
fn debug_policy_creates_the_implicit_config_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let config = resolve_runtime_config(dir.path(), EnvValues::default(), true).unwrap();
    let path = dir.path().join("dev-data/config.toml");

    assert!(path.is_file());
    assert_eq!(std::fs::read_to_string(path).unwrap(), DEFAULT_DEV_CONFIG);
    assert_eq!(
        config.database,
        DatabaseConfig::Embed {
            path: dir.path().join("dev-data/notes.db"),
        }
    );
    assert_eq!(
        config.attachments,
        AttachmentConfig::Filesystem {
            path: dir.path().join("dev-data/attachments"),
        }
    );
}

#[test]
fn release_policy_rejects_a_missing_implicit_config() {
    let dir = tempfile::tempdir().unwrap();
    let error = resolve_runtime_config(dir.path(), EnvValues::default(), false).unwrap_err();
    assert!(error.to_string().contains("dev-data/config.toml"));
    assert!(!dir.path().join("dev-data/config.toml").exists());
}

#[test]
fn explicit_missing_config_is_never_generated() {
    let dir = tempfile::tempdir().unwrap();
    let error = resolve_runtime_config(
        dir.path(),
        EnvValues {
            config_path: Some("custom/config.toml".into()),
            ..EnvValues::default()
        },
        true,
    )
    .unwrap_err();

    assert!(error.to_string().contains("custom/config.toml"));
    assert!(!dir.path().join("custom/config.toml").exists());
}
```

Add selected-adapter and precedence tests using these resolved shapes:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseConfig {
    Embed { path: PathBuf },
    Pg { url: String, max_connections: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingConfig {
    Local,
    OpenAi {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        timeout_secs: u64,
        max_retries: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentConfig {
    Filesystem { path: PathBuf },
    S3 {
        bucket: String,
        prefix: String,
        region: Option<String>,
        endpoint: Option<String>,
        force_path_style: bool,
    },
}
```

Tests must prove:

- a complete config value wins over its environment fallback;
- an omitted value uses environment, then default;
- paths resolve from the config parent;
- inactive PostgreSQL/OpenAI/S3 fields are not required;
- active adapters reject blank required fields;
- unknown TOML fields fail;
- `api_key_env` stores only an environment-variable name.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p note-server --lib config::tests
```

Expected: compilation or assertion failures because the old loader still treats `config.toml` as optional and exposes flat runtime fields.

- [ ] **Step 3: Implement typed resolution and atomic debug generation**

Use these constants and public runtime root:

```rust
const DEFAULT_CONFIG_PATH: &str = "dev-data/config.toml";
const DEFAULT_DEV_CONFIG: &str = r#"[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub config_path: PathBuf,
    pub database: DatabaseConfig,
    pub embedding: EmbeddingConfig,
    pub attachments: AttachmentConfig,
}
```

`load_runtime_config()` calls:

```rust
resolve_runtime_config(&cwd, env, cfg!(debug_assertions))
```

For a missing implicit debug config:

1. create the parent directory;
2. create a `tempfile::NamedTempFile` in that parent;
3. write and sync `DEFAULT_DEV_CONFIG`;
4. call `persist_noclobber`;
5. if another process won with `AlreadyExists`, discard the temp file and read the winner;
6. never overwrite an existing file.

Keep raw file structs `#[serde(deny_unknown_fields)]` and optional so resolution remains per-field. Resolve these environment fallbacks:

```text
NOTE_CONFIG_PATH
NOTE_DB_ENGINE
NOTE_DB_PATH
DATABASE_URL
NOTE_DB_MAX_CONNECTIONS
NOTE_EMBEDDING_ENGINE
NOTE_EMBEDDING_BASE_URL
NOTE_EMBEDDING_MODEL
NOTE_EMBEDDING_API_KEY_ENV
NOTE_EMBEDDING_TIMEOUT_SECS
NOTE_EMBEDDING_MAX_RETRIES
NOTE_ATTACHMENTS_ENGINE
NOTE_ATTACHMENTS_DIR
NOTE_S3_BUCKET
NOTE_S3_PREFIX
AWS_REGION
NOTE_S3_ENDPOINT
NOTE_S3_FORCE_PATH_STYLE
```

Use defaults `embed`, `local`, `filesystem`, `bge-m3`, 30 seconds, 3 retries, 10 PostgreSQL connections, empty S3 prefix, and `false` path style. Do not read the value named by `api_key_env` in the config parser.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p note-server --lib config::tests
```

Expected: all config tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/note-server/Cargo.toml crates/note-server/src/config.rs Cargo.lock
git commit -m "feat(config): require runtime config file"
```

### Task 2: Attachment contract and filesystem adapter

**Files:**

- Create: `crates/note-attachments/Cargo.toml`
- Create: `crates/note-attachments/src/lib.rs`
- Create: `crates/note-attachments/src/path.rs`
- Create: `crates/note-attachments/src/filesystem.rs`
- Create: `crates/note-attachments/tests/filesystem_test.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write filesystem contract tests**

Use the desired public API:

```rust
let store = FilesystemAttachmentStore::new(dir.path().join("attachments"));
let prepared = store.prepare("note-1", &attachments).await.unwrap();
assert!(!dir.path().join("attachments/note-1/file.txt").exists());
assert_eq!(prepared.metadata()[0].content, Vec::<u8>::new());

prepared.publish().await.unwrap();
assert_eq!(
    store.read("note-1", "./file.txt").await.unwrap(),
    b"content"
);
```

Separate tests must prove:

- `abort()` removes temporary data without publishing;
- publishing a replacement removes obsolete files;
- `remove_note()` removes the complete note directory;
- `../`, absolute, and platform-prefix paths are rejected;
- `info()` returns engine `filesystem` and the configured root;
- an empty attachment set replaces the old set with no files.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p note-attachments
```

Expected: Cargo reports that the workspace package does not exist.

- [ ] **Step 3: Define the object-safe contract**

In `src/lib.rs` define:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentStoreInfo {
    pub engine: String,
    pub location: Option<String>,
}

#[async_trait::async_trait]
pub trait PreparedAttachmentSet: Send {
    fn metadata(&self) -> &[note_core::NoteAttachment];
    async fn publish(self: Box<Self>) -> anyhow::Result<()>;
    async fn abort(self: Box<Self>) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
pub trait AttachmentStore: Send + Sync {
    async fn prepare(
        &self,
        note_id: &str,
        attachments: &[note_core::NoteAttachment],
    ) -> anyhow::Result<Box<dyn PreparedAttachmentSet>>;

    async fn read(&self, note_id: &str, path: &str) -> anyhow::Result<Vec<u8>>;
    async fn hydrate(
        &self,
        note_id: &str,
        attachments: &mut [note_core::NoteAttachment],
    ) -> anyhow::Result<()> {
        for attachment in attachments {
            attachment.content = self.read(note_id, &attachment.path).await?;
        }
        Ok(())
    }
    async fn remove_note(&self, note_id: &str) -> anyhow::Result<()>;
    fn info(&self) -> AttachmentStoreInfo;
}
```

Expose `FilesystemAttachmentStore` and a crate-visible canonical path helper.

- [ ] **Step 4: Implement the filesystem adapter**

Port the existing staging behavior to `tokio::fs`:

```rust
pub struct FilesystemAttachmentStore {
    root: PathBuf,
}

struct PreparedFilesystemSet {
    final_dir: PathBuf,
    temp_dir: Option<PathBuf>,
    metadata: Vec<NoteAttachment>,
}
```

`prepare` writes to `.<note-id>-<uuid>.tmp`; `publish` removes the old final directory and renames the temp directory; for an empty set it removes the old final directory without renaming. `abort` removes only the temp directory. Construct every path by iterating `std::path::Component` and accepting only `Normal` and `CurDir`.

- [ ] **Step 5: Run the attachment tests and verify GREEN**

Run:

```bash
cargo test -p note-attachments
cargo fmt --all -- --check
```

Expected: all attachment tests pass and formatting is clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/note-attachments
git commit -m "feat(attachments): add filesystem adapter"
```

### Task 3: Migrate pipelines to AttachmentStore

**Files:**

- Create: `crates/note-pipelines/src/note_attachments.rs`
- Delete: `crates/note-pipelines/src/attachment_files.rs`
- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `crates/note-pipelines/src/context.rs`
- Modify: `crates/note-pipelines/src/lib.rs`
- Modify: `crates/note-pipelines/src/save_note.rs`
- Modify: `crates/note-pipelines/src/update_note.rs`
- Modify: `crates/note-pipelines/src/get_note.rs`
- Modify: `crates/note-pipelines/src/list_notes.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/src/export.rs`
- Modify: `crates/note-pipelines/src/system.rs`
- Modify: `crates/note-pipelines/src/embedding_queue.rs`
- Modify: `crates/note-pipelines/tests/support/mod.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-pipelines/tests/export_test.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`

- [ ] **Step 1: Write failing adapter-injection and single-read tests**

Add a recording test store implementing `AttachmentStore` and assert:

```rust
let note = get_note(&ctx, "note-1").await.unwrap().unwrap();
assert_eq!(note.attachments[0].content, b"payload");
assert_eq!(recording_store.reads(), vec![("note-1", "./file.txt")]);
```

Add `get_note_attachment` coverage:

```rust
let attachment = get_note_attachment(&ctx, "note-1", "file.txt")
    .await
    .unwrap()
    .unwrap();
assert_eq!(attachment.content, b"payload");
assert_eq!(recording_store.read_count(), 1);
```

The test store should fail if pipelines assume a filesystem path.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p note-pipelines --test save_note_test --test export_test --test system_test
```

Expected: compilation fails because `Context` still requires `PathBuf` and attachment operations are synchronous filesystem functions.

- [ ] **Step 3: Inject the attachment adapter**

Change `Context` to:

```rust
pub struct Context {
    storage: Arc<dyn StorageBackend>,
    pub embedder: Arc<dyn Embedder>,
    attachments: Arc<dyn AttachmentStore>,
    embedding_job_notifier: Option<Arc<dyn EmbeddingJobNotifier>>,
}

pub fn attachments(&self) -> &dyn AttachmentStore {
    self.attachments.as_ref()
}
```

Both constructors accept `Arc<dyn AttachmentStore>`.

In `note_attachments.rs`, add:

```rust
pub async fn hydrate_note_attachments(
    ctx: &Context,
    note: &mut Note,
) -> anyhow::Result<()> {
    ctx.attachments()
        .hydrate(&note.id, &mut note.attachments)
        .await
}
```

Add `get_note_attachment(ctx, note_id, requested_path)` that loads note metadata without first hydrating all files, matches the normalized attachment path, reads only the selected object, and returns `Ok(None)` for a missing note or path.

- [ ] **Step 4: Convert write/delete/import flows to async prepared sets**

For save and update:

```rust
let prepared = ctx.attachments().prepare(note_id, attachments).await?;
// use prepared.metadata() inside the storage transaction
match finish_transaction(transaction, transaction_result).await {
    Ok(value) => {
        prepared.publish().await?;
        Ok(value)
    }
    Err(error) => {
        if let Err(abort_error) = prepared.abort().await {
            return Err(error.context(format!(
                "attachment abort also failed: {abort_error}"
            )));
        }
        Err(error)
    }
}
```

Await hydration in get/list/search/export. Await `remove_note` after permanent deletion and purge. Preserve the existing order: database commit precedes attachment publish; database deletion precedes attachment removal.

Refactor import separately so it never holds a database transaction across
filesystem or future S3 uploads:

1. preclassify duplicate note IDs within the import so only the first
   occurrence is a preparation candidate and later occurrences count as
   skipped without acquiring the same note's attachment guard;
2. decode every candidate attachment and prepare each unique attachment set;
3. begin the database transaction only after all preparation succeeds;
4. recheck note existence in the transaction, persist inserted-note metadata
   using `prepared.metadata()`, and mark raced/existing candidates skipped;
5. commit the database transaction;
6. publish sets for inserted notes and abort sets for skipped notes;
7. on preparation or database failure, abort every set prepared so far.

- [ ] **Step 5: Update test fixtures**

Replace every pipeline test path argument with:

```rust
Arc::new(FilesystemAttachmentStore::new(
    dir.path().join("attachments"),
))
```

Keep assertions against the same filesystem paths so the adapter migration proves behavior did not change.

- [ ] **Step 6: Run pipeline tests and verify GREEN**

Run:

```bash
cargo test -p note-pipelines
```

Expected: all pipeline unit and integration tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/note-pipelines crates/note-attachments Cargo.lock
git commit -m "refactor(pipelines): inject attachment storage"
```

### Task 4: Wire local composition and backend-neutral system info

**Files:**

- Modify: `crates/note-server/Cargo.toml`
- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/labels_api.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Modify: `crates/note-server/tests/labels_api_test.rs`
- Modify: `crates/note-server/tests/notes_api_test.rs`
- Modify: `crates/note-mcp/src/http.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`
- Modify: `crates/note-pipelines/src/system.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/pages/system.rs`

- [ ] **Step 1: Write failing server and system-info tests**

Update SystemInfo expectations to:

```json
{
  "database_engine": "embed",
  "database_path": ".../test.db",
  "database_size_bytes": 123,
  "attachments_engine": "filesystem",
  "attachments_location": ".../attachments"
}
```

Add a server unit test that only creates local directories for:

```rust
DatabaseConfig::Embed { .. }
AttachmentConfig::Filesystem { .. }
```

Add/update the raw attachment endpoint test to verify one requested attachment is returned without requiring all sibling attachment objects to be readable.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p note-server
cargo test -p note-mcp
cd crates/note-frontend && cargo test
```

Expected: compilation failures from old context constructors and old `attachments_path` fields.

- [ ] **Step 3: Construct local adapters from typed config**

In the composition root, match the currently implemented local variants:

```rust
let storage: Arc<dyn StorageBackend> = match &config.database {
    DatabaseConfig::Embed { path } => {
        Arc::new(note_storage_turso::TursoStorage::open(path).await?)
    }
    DatabaseConfig::Pg { .. } => {
        anyhow::bail!("PostgreSQL storage adapter is not implemented yet")
    }
};

let attachments: Arc<dyn AttachmentStore> = match &config.attachments {
    AttachmentConfig::Filesystem { path } => {
        Arc::new(FilesystemAttachmentStore::new(path.clone()))
    }
    AttachmentConfig::S3 { .. } => {
        anyhow::bail!("S3 attachment adapter is not implemented yet")
    }
};
```

Keep local embedding behavior for `EmbeddingConfig::Local`; return a precise not-yet-implemented error for `EmbeddingConfig::OpenAi` until its plan lands. Import/export use `StubEmbedder` and the configured attachment adapter.

- [ ] **Step 4: Make system information backend-neutral**

Change the pipeline DTO to:

```rust
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    pub attachments_location: Option<String>,
}
```

Populate attachment values from `ctx.attachments().info()`. Update the frontend DTO and render “Attachments engine” plus an optional “Attachments location”.

Change `notes_api::get_attachment_handler` to call the pipeline's single-object `get_note_attachment`.

- [ ] **Step 5: Update server and MCP context fixtures and run GREEN**

Run:

```bash
cargo test -p note-server
cargo test -p note-mcp
cd crates/note-frontend && cargo test
```

Expected: all focused suites pass.

- [ ] **Step 6: Commit**

```bash
git add crates/note-server crates/note-mcp crates/note-pipelines crates/note-frontend Cargo.lock
git commit -m "feat(server): compose attachment adapters"
```

### Task 5: Documentation and slice verification

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Document the new local startup contract**

Document:

- debug/dev automatic creation of `dev-data/config.toml`;
- release and explicit-path missing-file failures;
- paths relative to the config parent;
- file → environment → default precedence;
- `[database]`, `[embedding]`, and `[attachments]`;
- filesystem attachment adapter behavior;
- PostgreSQL, OpenAI-compatible, and S3 values as reserved configuration that subsequent implementation plans activate.

- [ ] **Step 2: Run slice completion gates**

Run:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cd crates/note-frontend && cargo test
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
git diff --check
```

Expected: every command exits successfully.

- [ ] **Step 3: Commit**

```bash
git add README.md docs/design.md crates/note-server/src/config.rs
git commit -m "docs(config): describe mandatory adapter config"
```
