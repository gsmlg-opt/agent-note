# MCP Note Summaries and Standalone Attachment Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep MCP note discovery and detail responses bounded by returning summaries or attachment metadata only, and move attachment byte reads, writes, and deletes into three explicit single-attachment tools.

**Architecture:** Preserve the existing fully hydrated REST/export workflows, but add metadata-only and field-only pipeline paths for MCP. Extend storage with partial note-field and attachment-metadata updates, and extend both attachment adapters with prepared single-object put/delete mutations that share the existing per-note publication lock. MCP schemas then expose summary/detail types plus `put_note_attachment`, `get_note_attachment_content`, and `delete_note_attachment`.

**Tech Stack:** Rust 2021, Tokio, async-trait, Turso, PostgreSQL/sqlx, filesystem and AWS S3 attachment adapters, rmcp, schemars, serde, Base64.

---

## Scope and approved contract

- Approved design: `docs/superpowers/specs/2026-07-19-mcp-note-summary-attachment-management-design.md`.
- `list_notes`: `id`, `title`, `labels`, `created_at`, `updated_at`.
- `semantic_search`: `id`, `title`, `score`, `labels`, `created_at`, `updated_at`.
- `get_note` and `update_note`: note content plus attachment metadata (`id`, `path`, `mime`, `description`), never attachment bytes.
- MCP `save_note` and `update_note` no longer accept inline attachments.
- New tools: `put_note_attachment`, `get_note_attachment_content`, and `delete_note_attachment`.
- Attachment identity is `attachment_id`; an existing id cannot change its normalized path.
- REST, frontend, export, and existing complete-set attachment workflows retain their current contracts.

Do not add projections, compatibility flags, bulk operations, attachment sizes, checksums, ranges, streaming, or REST API changes.

## Execution baseline

Run from the repository root:

```bash
git status --short --branch
cargo fmt --all -- --check
cargo test -p note-attachments
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-pipelines
cargo test -p note-mcp
```

Expected: all commands pass before feature work. Preserve unrelated changes, including other untracked design or plan documents. PostgreSQL integration tests may visibly skip when `TEST_DATABASE_URL` is absent; do not treat a documented skip as a pass with database coverage.

## File map

### Core and storage

- Modify `crates/note-core/src/types.rs` — make search results carry `NoteListItem`.
- Modify `crates/note-core/src/validation.rs` — expose attachment-only validation.
- Modify `crates/note-storage/src/records.rs` — add partial-update records.
- Modify `crates/note-storage/src/repositories.rs` — add partial-update repository methods.
- Modify `crates/note-storage-contract-tests/src/notes.rs` — shared behavior tests.
- Modify `crates/note-storage-turso/src/notes.rs` — Turso partial updates.
- Modify `crates/note-storage-pg/src/notes.rs` — PostgreSQL partial updates.

### Attachment adapters and pipelines

- Modify `crates/note-attachments/src/lib.rs` — prepared single-object mutation contract.
- Modify `crates/note-attachments/src/filesystem.rs` — staged single-file put/delete.
- Modify `crates/note-attachments/src/s3.rs` — staged single-object put/delete.
- Modify `crates/note-attachments/tests/filesystem_test.rs` — filesystem mutation behavior.
- Modify `crates/note-attachments/tests/s3_minio_test.rs` — MinIO integration behavior.
- Modify `crates/note-pipelines/src/get_note.rs` — metadata-only note read.
- Modify `crates/note-pipelines/src/search_notes.rs` — summary search results without hydration.
- Modify `crates/note-pipelines/src/update_note.rs` — field-only update path.
- Modify `crates/note-pipelines/src/edit_note.rs` — preserve attachments through field-only update.
- Modify `crates/note-pipelines/src/note_attachments.rs` — standalone attachment workflows.
- Modify `crates/note-pipelines/tests/search_notes_test.rs` — summary/no-hydration assertions.
- Modify `crates/note-pipelines/tests/save_note_test.rs` — partial update and test-double support.
- Modify `crates/note-pipelines/tests/export_test.rs` — test-double support.
- Modify `crates/note-pipelines/tests/support/events.rs` — new repository/store forwarding.
- Create `crates/note-pipelines/tests/attachment_management_test.rs` — attachment CRUD tests.

### MCP and documentation

- Modify `crates/note-mcp/src/tools.rs` — transport-independent summary/detail and attachment tools.
- Modify `crates/note-mcp/src/stdio.rs` — schemas, handlers, advertised tool list, and error mapping.
- Modify `crates/note-mcp/tests/tools_test.rs` — tool-layer contract tests.
- Modify `README.md` — document the new MCP contracts.

---

### Task 1: Add storage partial-update contracts

**Files:**

- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-contract-tests/src/notes.rs`
- Modify: `crates/note-storage-turso/src/notes.rs`
- Modify: `crates/note-storage-pg/src/notes.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] **Step 1: Write failing shared storage-contract assertions**

In `crates/note-storage-contract-tests/src/notes.rs`, extend the active-note section with two independent updates:

```rust
assert_eq!(
    session
        .update_note_fields(NoteFieldsUpdate {
            id: "contract-notes-active",
            title: "Fields only",
            content: "Fields body",
            updated_at: 120,
            note_revision: 3,
        })
        .await
        .unwrap(),
    1
);
let fields_only = session.get_note("contract-notes-active").await.unwrap().unwrap();
assert_eq!(fields_only.title, "Fields only");
assert_eq!(fields_only.content, "Fields body");
assert_eq!(fields_only.attachments[0].id, "updated");
assert_eq!(fields_only.updated_at, 120);
assert_eq!(
    session.get_note_revision("contract-notes-active").await.unwrap(),
    Some(3)
);

let metadata_only = NoteAttachment {
    id: "standalone".into(),
    path: "standalone.bin".into(),
    mime: "application/octet-stream".into(),
    description: "standalone metadata".into(),
    content: b"must not be stored".to_vec(),
};
assert_eq!(
    session
        .update_note_attachments(AttachmentMetadataUpdate {
            id: "contract-notes-active",
            attachments: std::slice::from_ref(&metadata_only),
            updated_at: 130,
        })
        .await
        .unwrap(),
    1
);
let metadata_only = session.get_note("contract-notes-active").await.unwrap().unwrap();
assert_eq!(metadata_only.title, "Fields only");
assert_eq!(metadata_only.content, "Fields body");
assert_eq!(metadata_only.attachments[0].id, "standalone");
assert!(metadata_only.attachments[0].content.is_empty());
assert_eq!(metadata_only.updated_at, 130);
assert_eq!(
    session.get_note_revision("contract-notes-active").await.unwrap(),
    Some(3)
);
```

Import the two new record types. Also assert both methods return `0` for an absent or soft-deleted note.

- [ ] **Step 2: Run the focused contracts and verify RED**

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: compilation fails because `NoteFieldsUpdate`, `AttachmentMetadataUpdate`, and the repository methods do not exist.

- [ ] **Step 3: Add exact record and repository APIs**

In `crates/note-storage/src/records.rs`:

```rust
pub struct NoteFieldsUpdate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub updated_at: i64,
    pub note_revision: i64,
}

pub struct AttachmentMetadataUpdate<'a> {
    pub id: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
}
```

In `NotesRepository`:

```rust
async fn update_note_fields(&self, note: NoteFieldsUpdate<'_>) -> StorageResult<u64>;
async fn update_note_attachments(
    &self,
    note: AttachmentMetadataUpdate<'_>,
) -> StorageResult<u64>;
```

Update the forwarding macro invocation in `crates/note-pipelines/tests/support/events.rs` at the same time so downstream test crates continue compiling.

- [ ] **Step 4: Implement Turso partial updates**

Use separate SQL statements in `crates/note-storage-turso/src/notes.rs`:

```sql
UPDATE notes
SET title = ?2, content = ?3, updated_at = ?4, note_revision = ?5
WHERE id = ?1 AND deleted_at IS NULL
```

and:

```sql
UPDATE notes
SET attachments = ?2, updated_at = ?3
WHERE id = ?1 AND deleted_at IS NULL
```

Serialize attachment metadata with the existing `serialize_attachments`; it already strips content bytes.

- [ ] **Step 5: Implement PostgreSQL partial updates**

Use equivalent `$1`-style SQL in `crates/note-storage-pg/src/notes.rs`. Bind the existing JSONB serialization result for attachment metadata. Do not change schemas or migrations.

- [ ] **Step 6: Run storage verification**

```bash
cargo fmt --all -- --check
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: Turso passes. PostgreSQL passes when `TEST_DATABASE_URL` is configured, otherwise emits its documented skip message.

- [ ] **Step 7: Commit**

```bash
git add crates/note-storage crates/note-storage-contract-tests crates/note-storage-turso crates/note-storage-pg crates/note-pipelines/tests/support/events.rs
git commit -m "feat(storage): add partial note updates"
```

### Task 2: Add prepared single-object mutations to the filesystem adapter

**Files:**

- Modify: `crates/note-attachments/src/lib.rs`
- Modify: `crates/note-attachments/src/filesystem.rs`
- Modify: `crates/note-attachments/tests/filesystem_test.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-pipelines/tests/export_test.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] **Step 1: Write failing filesystem mutation tests**

Add tests named:

- `prepared_put_publishes_one_file_without_rewriting_siblings`
- `aborted_put_leaves_the_previous_file_and_siblings_unchanged`
- `prepared_delete_removes_only_the_selected_file`
- `aborted_delete_keeps_the_selected_file`
- `full_set_and_single_object_mutations_share_the_same_note_lock`
- `single_object_mutations_for_different_notes_can_coexist`

For the first test, publish `a.txt` and `b.txt` as a complete set, prepare a put replacing only `a.txt`, publish it, then assert `a.txt == new` and `b.txt == old`. For delete, assert the parent directory is pruned only when empty and the note directory remains when siblings exist.

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
cargo test -p note-attachments --test filesystem_test prepared_put
cargo test -p note-attachments --test filesystem_test prepared_delete
cargo test -p note-attachments --test filesystem_test full_set_and_single_object
```

Expected: compilation fails because the single-object mutation trait methods do not exist.

- [ ] **Step 3: Add the object-safe prepared mutation contract**

In `crates/note-attachments/src/lib.rs`:

```rust
#[async_trait::async_trait]
pub trait PreparedAttachmentMutation: Send {
    async fn publish(self: Box<Self>) -> anyhow::Result<()>;
    async fn abort(self: Box<Self>) -> anyhow::Result<()>;
}
```

Extend `AttachmentStore`:

```rust
async fn prepare_put(
    &self,
    note_id: &str,
    attachment: &note_core::NoteAttachment,
) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>>;

async fn prepare_delete(
    &self,
    note_id: &str,
    path: &str,
) -> anyhow::Result<Box<dyn PreparedAttachmentMutation>>;
```

Add minimal inert implementations to the three pipeline test doubles in `save_note_test.rs`, `export_test.rs`, and `support/events.rs`. The controlled test double must record `prepare_put`, `prepare_delete`, `publish`, and `abort` distinctly; do not silently route them through whole-set `prepare`.

- [ ] **Step 4: Implement staged filesystem put**

Add a prepared mutation enum or two small prepared structs that retain the same `OwnedMutexGuard<()>` used by `PreparedFilesystemSet`.

Preparation must:

1. Resolve and lock the note directory through `NoteCoordination`.
2. Canonicalize the requested relative path.
3. Reject unsafe/symlinked destination components.
4. Write one temporary file under the configured attachment root on the same filesystem.
5. Retain cleanup state and the note lock until publish or abort.

Publication must atomically replace only the target file. If a target exists, rename it to a unique backup first, promote the staged file, then remove the backup. On promotion failure, restore the backup. Do not rename or enumerate the whole note directory.

- [ ] **Step 5: Implement prepared filesystem delete**

Preparation validates the canonical path, acquires the same per-note lock, and captures the final path without deleting it. Publication removes only that file and prunes empty parent directories up to, but not including, the configured attachment root. A missing physical file is success so idempotent cleanup remains possible. Abort only releases the lock.

- [ ] **Step 6: Preserve cancellation and drop cleanup**

Match the existing complete-set guarantees:

- dropping or aborting a prepared put removes its temporary data;
- a held prepared put/delete blocks a same-note full-set or single-object prepare;
- mutation of another note is not blocked;
- no symlink traversal is introduced.

- [ ] **Step 7: Run adapter verification**

```bash
cargo fmt --all -- --check
cargo test -p note-attachments --test filesystem_test
cargo test -p note-pipelines --no-run
```

Expected: filesystem tests pass and all attachment-store test doubles implement the expanded trait.

- [ ] **Step 8: Commit**

```bash
git add crates/note-attachments/src/lib.rs crates/note-attachments/src/filesystem.rs crates/note-attachments/tests/filesystem_test.rs crates/note-pipelines/tests
git commit -m "feat(attachments): prepare single-file mutations"
```

### Task 3: Implement prepared single-object S3 mutations

**Files:**

- Modify: `crates/note-attachments/src/s3.rs`
- Modify: `crates/note-attachments/tests/s3_minio_test.rs`

- [ ] **Step 1: Write failing wiremock/unit tests**

Add tests proving:

- put uploads one staging key, copies it to one final key, and removes only that staging key;
- put does not list or delete sibling final objects;
- abort deletes the staged key and leaves the final key unchanged;
- delete issues a delete for exactly one canonical final key;
- missing-object delete succeeds;
- single-object mutations serialize with complete-set mutations for the same note;
- cancellation retains the note lock until detached publication or cleanup finishes;
- safe errors expose bucket/key location but no credentials.

- [ ] **Step 2: Run focused S3 tests and verify RED**

```bash
cargo test -p note-attachments s3::tests::single_object
```

Expected: the new tests fail because `S3AttachmentStore` has no single-object implementations.

- [ ] **Step 3: Implement prepared S3 put**

Reuse the existing note coordination map, canonical key helpers, safe location formatting, cleanup scheduling, copy helper, and detached Tokio task pattern.

Preparation uploads exactly one object to a unique staging key and returns a prepared mutation holding the same-note lock. Publication copies that staging key to:

```text
<prefix>/<note_id>/<canonical relative path>
```

then deletes the staging key. It must not list the final note prefix and must not delete obsolete siblings.

- [ ] **Step 4: Implement prepared S3 delete**

Preparation canonicalizes the final key and holds the same-note lock without performing deletion. Publication deletes exactly that final key. S3 delete's missing-object behavior is treated as success. Abort releases the lock without an object mutation.

- [ ] **Step 5: Add MinIO integration coverage**

In `crates/note-attachments/tests/s3_minio_test.rs`, create a complete set with two objects, replace one through `prepare_put`, and delete one through `prepare_delete`. Assert the sibling remains byte-for-byte unchanged and no staging keys remain.

- [ ] **Step 6: Run S3 verification**

```bash
cargo fmt --all -- --check
cargo test -p note-attachments
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test
```

Expected: unit/wiremock tests pass. Run the MinIO command when the configured service is available; otherwise record it as not run, not passed.

- [ ] **Step 7: Commit**

```bash
git add crates/note-attachments/src/s3.rs crates/note-attachments/tests/s3_minio_test.rs
git commit -m "feat(attachments): support single-object S3 mutations"
```

### Task 4: Add metadata-only reads, summary search, and field-only note updates

**Files:**

- Modify: `crates/note-core/src/types.rs`
- Modify: `crates/note-pipelines/src/get_note.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/src/update_note.rs`
- Modify: `crates/note-pipelines/src/edit_note.rs`
- Modify: `crates/note-pipelines/tests/search_notes_test.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`

- [ ] **Step 1: Write failing no-hydration and preservation tests**

Add a test attachment store whose `read` and `hydrate` methods panic. Verify:

```rust
let detail = get_note_metadata(&ctx, &note.id).await.unwrap().unwrap();
assert_eq!(detail.content, "Body");
assert_eq!(detail.attachments[0].id, "blob");
assert!(detail.attachments[0].content.is_empty());

let hits = search_notes(&ctx, "Findable", 5).await.unwrap();
assert_eq!(hits[0].note.labels, expected_labels);
assert_eq!(hits[0].note.created_at, note.created_at);
assert_eq!(hits[0].note.updated_at, note.updated_at);
```

Add a field-only update test that starts with a stored attachment, updates title/content/labels, and asserts:

- attachment metadata is preserved;
- attachment-store `prepare`, `prepare_put`, `prepare_delete`, `read`, and `hydrate` are not called;
- note revision and embedding jobs change exactly as for a content update;
- `edit_note` also preserves attachment metadata without hydrating bytes.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p note-pipelines --test search_notes_test
cargo test -p note-pipelines --test save_note_test field_only
```

Expected: search attempts hydration, `get_note_metadata`/field-only update do not exist, or result types lack summary fields.

- [ ] **Step 3: Add metadata-only note reads**

In `get_note.rs`, retain the existing hydrated `get_note` and add:

```rust
pub async fn get_note_metadata(ctx: &Context, id: &str) -> anyhow::Result<Option<Note>> {
    let session = ctx.storage().session().await?;
    Ok(session.get_note(id).await?)
}
```

Use this function for MCP-oriented reads. Do not change REST/export callers that rely on hydrated content.

- [ ] **Step 4: Change search results to summaries**

Change `note_core::SearchResult.note` from `Note` to `NoteListItem`. In `search_notes.rs`, map each ranked stored note to:

```rust
NoteListItem {
    id: note.id,
    title: note.title,
    labels: note.labels,
    created_at: note.created_at,
    updated_at: note.updated_at,
    deleted_at: note.deleted_at,
}
```

Delete the attachment hydration loop and its misleading comment. Keep ranking, filtering, and REST-visible id/title behavior unchanged.

- [ ] **Step 5: Add a field-only update entrypoint**

Refactor the existing update orchestration behind two public entrypoints:

```rust
pub async fn update_note(
    ctx: &Context,
    id: &str,
    input: SaveNoteInput,
) -> anyhow::Result<Option<Note>>;

pub async fn update_note_fields(
    ctx: &Context,
    id: &str,
    input: UpdateNoteFieldsInput,
) -> anyhow::Result<Option<Note>>;
```

with:

```rust
pub struct UpdateNoteFieldsInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
}
```

Share validation, label resolution, chunking, revision increment, and embedding-job synchronization. The complete-set path keeps `AttachmentStore::prepare` plus `NotesRepository::update_note`. The field-only path never prepares attachments and calls `NotesRepository::update_note_fields`, leaving the attachment JSON column untouched.

After a successful field-only commit, return `get_note_metadata(ctx, id)` so the caller receives the current attachment metadata rather than a pre-transaction snapshot.

- [ ] **Step 6: Route line editing through the field-only path**

Change `edit_note.rs` to load `get_note_metadata` and call `update_note_fields`. It still updates title/content/labels and embedding state, but no longer hydrates or republishes attachment bytes.

- [ ] **Step 7: Run pipeline and REST regression checks**

```bash
cargo fmt --all -- --check
cargo test -p note-pipelines --test search_notes_test
cargo test -p note-pipelines --test save_note_test
cargo test -p note-server --test notes_api_test
```

Expected: pipeline tests pass; existing REST search and complete-note update behavior remains intact.

- [ ] **Step 8: Commit**

```bash
git add crates/note-core/src/types.rs crates/note-pipelines
git commit -m "feat(pipelines): add metadata-only note workflows"
```

### Task 5: Implement standalone attachment pipelines

**Files:**

- Modify: `crates/note-core/src/validation.rs`
- Modify: `crates/note-pipelines/src/note_attachments.rs`
- Create: `crates/note-pipelines/tests/attachment_management_test.rs`

- [ ] **Step 1: Write failing attachment workflow tests**

Create focused tests for:

1. New `attachment_id` plus unique normalized path returns `created: true`.
2. Existing id and same normalized path replaces bytes/metadata and returns `created: false`.
3. Existing id with a changed normalized path is rejected without mutation.
4. New id colliding with another normalized path is rejected without mutation.
5. Empty id/MIME and absolute, traversal, URL, or empty paths are rejected.
6. Get resolves by id and reads only the selected path.
7. Delete removes only the selected metadata/object and is idempotent.
8. Missing note behavior matches the approved contract.
9. Put/delete change `updated_at`, preserve `note_revision`, preserve chunks, and enqueue no embedding jobs.
10. Put/delete never call `hydrate` and never rewrite sibling objects.
11. Failed transaction aborts the prepared mutation.
12. Publication occurs after metadata commit.

Use an old explicit timestamp in the inserted note so `updated_at` can be asserted deterministically. Drain the original embedding queue before attachment mutation, then assert `drain_embedding_jobs(&ctx, 10) == 0`.

- [ ] **Step 2: Run the new test target and verify RED**

```bash
cargo test -p note-pipelines --test attachment_management_test
```

Expected: compilation fails because the pipeline functions and result/error types do not exist.

- [ ] **Step 3: Expose attachment-only validation**

Make `validate_attachments` public in `crates/note-core/src/validation.rs` and preserve all current behavior. The put pipeline validates a one-element slice before staging; uniqueness against existing metadata is checked transactionally.

- [ ] **Step 4: Add pipeline data and error types**

In `note_attachments.rs`:

```rust
pub struct PutNoteAttachmentResult {
    pub attachment: NoteAttachment,
    pub created: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttachmentMutationError {
    NoteNotFound(String),
    AttachmentNotFound { note_id: String, attachment_id: String },
    AttachmentPathChange { attachment_id: String },
    AttachmentPathCollision { path: String },
}
```

Implement `Display` and `std::error::Error` manually. Keep validation failures as `note_core::ValidationError` so transports can distinguish caller errors from storage failures.

- [ ] **Step 5: Implement put with transactional identity checks**

Add:

```rust
pub async fn put_note_attachment(
    ctx: &Context,
    note_id: &str,
    attachment: NoteAttachment,
) -> anyhow::Result<PutNoteAttachmentResult>;
```

Required order:

1. Validate the single attachment.
2. Call `prepare_put`; this stages one object and acquires the same-note attachment lock.
3. Begin an `Immediate` storage transaction.
4. Re-read the active note while the lock and transaction are held.
5. Resolve the id and compare normalized paths.
6. Reject path changes or collisions with another id.
7. Replace or append metadata with `content` cleared.
8. Call `update_note_attachments` with the current timestamp.
9. Commit metadata.
10. Publish the staged object.

On any pre-commit error, roll back and abort the prepared mutation while preserving the primary error. Do not change note revision, chunks, labels, or embedding jobs.

- [ ] **Step 6: Implement content read by attachment id**

Add:

```rust
pub async fn get_note_attachment_by_id(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<Option<NoteAttachment>>;
```

Load stored metadata, find the exact id, and call the existing path-based single-object read only for that attachment. Return `None` for a missing note or id; the MCP handler will turn it into a clear not-found error.

- [ ] **Step 7: Implement idempotent delete**

Add:

```rust
pub async fn delete_note_attachment(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<bool>;
```

First resolve the stored path. If the note or id is absent, return `false`. Then call `prepare_delete` to acquire the same-note lock. Begin an `Immediate` transaction and re-read the note. If the id disappeared, roll back, abort, and return `false`. If its normalized path changed, abort and retry resolution once or return a typed conflict; never delete a newly assigned path accidentally.

Commit metadata removal before publishing physical deletion. A physical cleanup failure may leave an orphan object but must not restore active metadata.

- [ ] **Step 8: Run pipeline verification**

```bash
cargo fmt --all -- --check
cargo test -p note-core validation
cargo test -p note-pipelines --test attachment_management_test
cargo test -p note-pipelines
```

Expected: all pipeline tests pass, including existing export and complete-set attachment workflows.

- [ ] **Step 9: Commit**

```bash
git add crates/note-core/src/validation.rs crates/note-pipelines/src/note_attachments.rs crates/note-pipelines/tests/attachment_management_test.rs
git commit -m "feat(pipelines): manage individual attachments"
```

### Task 6: Replace MCP tool-layer full-note reuse

**Files:**

- Modify: `crates/note-mcp/src/tools.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`

- [ ] **Step 1: Rewrite tool-layer tests to the approved contract**

Replace inline-attachment round-trip tests with:

- save input has no attachment field;
- put text and binary attachments after saving;
- get/update return attachment metadata with no byte field in their Rust type;
- update preserves attachments without an attachment-store read;
- `read_note_lines_tool` reads the body without hydrating attachment bytes;
- list returns only summary fields;
- semantic search includes labels and timestamps;
- content read returns bytes only for the selected id;
- delete is idempotent and leaves siblings intact.

The compile-time absence of `content` from `AttachmentMetadataData` and absence of `attachments` from `SaveNoteToolInput`/`UpdateNoteToolInput` is part of the contract.

- [ ] **Step 2: Run the tool tests and verify RED**

```bash
cargo test -p note-mcp --test tools_test
```

Expected: tests fail to compile until the tool-layer types and wrappers are changed.

- [ ] **Step 3: Define distinct transport-independent data types**

Use separate types rather than one optional-field response:

```rust
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttachmentMetadataData {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct NoteSummaryData {
    pub id: String,
    pub title: String,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct NoteDetailData {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<AttachmentMetadataData>,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}
```

Keep byte-bearing input/output types confined to the standalone attachment tools.

- [ ] **Step 4: Remove inline attachments from save/update**

`SaveNoteToolInput` contains only title/content/labels and passes `attachments: vec![]` to the existing save pipeline. `UpdateNoteToolInput` contains id/title/content/labels and calls `update_note_fields`.

`get_note_tool` calls `get_note_metadata`. `list_notes_tool` calls `list_note_summaries`. Map attachment metadata without copying a byte vector.

`read_note_lines_tool` must also use `get_note_metadata` (or the content-only pipeline) instead of the hydrated `get_note`; reading note lines must not perform attachment-store I/O.

- [ ] **Step 5: Expand semantic-search results**

Add labels, `created_at`, and `updated_at` to `SemanticSearchToolResult`; map them from the new summary-valued `SearchResult`.

- [ ] **Step 6: Add attachment tool wrappers**

Define:

```rust
pub struct PutNoteAttachmentToolInput {
    pub note_id: String,
    pub attachment_id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
    pub content: Vec<u8>,
}

pub struct PutNoteAttachmentToolOutput {
    pub created: bool,
    pub attachment: AttachmentMetadataData,
}

pub struct GetNoteAttachmentContentToolInput {
    pub note_id: String,
    pub attachment_id: String,
}

pub struct AttachmentContentData {
    pub attachment: AttachmentMetadataData,
    pub content: Vec<u8>,
}
```

Add wrappers for put/get/delete that delegate to the new pipeline functions. The delete wrapper returns `bool`.

- [ ] **Step 7: Run tool-layer verification**

```bash
cargo fmt --all -- --check
cargo test -p note-mcp --test tools_test
```

- [ ] **Step 8: Commit**

```bash
git add crates/note-mcp/src/tools.rs crates/note-mcp/tests/tools_test.rs
git commit -m "refactor(mcp): use bounded note response types"
```

### Task 7: Expose the new MCP schemas and handlers

**Files:**

- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Write failing router and serialization tests**

Update `server_builds_and_lists_all_tools` to require exactly the existing eight tools plus:

```text
put_note_attachment
get_note_attachment_content
delete_note_attachment
```

Add schema and serialized JSON assertions:

- `save_note` and `update_note` input schemas have no `attachments` property.
- `list_notes` items have exactly summary properties and no `content`/`attachments`.
- `semantic_search` hits contain the six approved fields.
- `get_note` and `update_note` attachment items have exactly `id`, `path`, `mime`, `description`.
- attachment metadata schemas have no `content` or `content_base64`.
- put requires at least one of `content`/`content_base64`.
- UTF-8 content response serializes `content` and omits `content_base64`.
- binary content response serializes `content_base64` and omits `content`.
- delete response is `{ "deleted": true|false }`.

- [ ] **Step 2: Run stdio tests and verify RED**

```bash
cargo test -p note-mcp stdio::tests
```

Expected: old schemas still advertise inline attachments and the three handlers are missing.

- [ ] **Step 3: Split summary, detail, and metadata schemas**

Replace `NoteResponse`/`AttachmentResponseSchema` reuse with:

```rust
pub struct AttachmentMetadataSchema {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
}

pub struct NoteSummaryResponse {
    pub id: String,
    pub title: String,
    pub labels: Vec<LabelSchema>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NoteDetailResponse {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<AttachmentMetadataSchema>,
    pub labels: Vec<LabelSchema>,
    pub created_at: i64,
    pub updated_at: i64,
}
```

`NoteListResponse.notes` becomes `Vec<NoteSummaryResponse>`. Get/update return `NoteDetailResponse`.

- [ ] **Step 4: Remove inline attachment request schemas from save/update**

Delete `attachments` from `SaveNoteRequest`, `UpdateNoteRequest`, and their conversions. Keep a byte-bearing `PutNoteAttachmentRequest`:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend(
    "anyOf" = [
        {"required": ["content"]},
        {"required": ["content_base64"]}
    ]
))]
pub struct PutNoteAttachmentRequest {
    pub note_id: String,
    pub attachment_id: String,
    pub path: String,
    pub mime: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub content_base64: Option<String>,
}
```

Its `TryFrom` uses `note_core::decode_attachment_content` and maps invalid Base64, missing content, or mismatched dual representations to `ErrorData::invalid_params`.

- [ ] **Step 5: Add put/get/delete response schemas**

Put returns:

```rust
pub struct PutNoteAttachmentResponse {
    pub created: bool,
    pub attachment: AttachmentMetadataSchema,
}
```

Get content returns metadata and exactly one representation:

```rust
pub struct GetNoteAttachmentContentResponse {
    pub attachment: AttachmentMetadataSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}
```

Construct it with UTF-8 text when `std::str::from_utf8` succeeds; otherwise use canonical padded Base64. Never populate both options.

- [ ] **Step 6: Register handlers with exact tool names**

Use:

```rust
#[tool(
    name = "put_note_attachment",
    description = "Add or replace one note attachment by attachment id."
)]
#[tool(
    name = "get_note_attachment_content",
    description = "Fetch one note attachment's content by attachment id."
)]
#[tool(
    name = "delete_note_attachment",
    description = "Delete one note attachment by attachment id."
)]
```

Get maps a missing note/id to a clear `note attachment not found` MCP error. Put maps `AttachmentMutationError::NoteNotFound` explicitly. Delete remains idempotent and returns `deleted: false`.

Update `ServerInfo.instructions` and tool descriptions so they no longer claim inline attachments are accepted or returned.

- [ ] **Step 7: Map caller faults to invalid params**

Extend the error mapper to return `ErrorData::invalid_params` for:

- `note_core::ValidationError`;
- attachment path-change and path-collision errors;
- transport decoding errors.

Keep storage and adapter failures as internal errors. Do not revive the earlier downstream union-schema workaround; nullable list filter schemas remain standards-compliant as currently generated.

- [ ] **Step 8: Run MCP verification**

```bash
cargo fmt --all -- --check
cargo test -p note-mcp stdio::tests
cargo test -p note-mcp --test tools_test
cargo test -p note-mcp
```

Expected: all MCP tests pass and serialized note discovery/detail responses contain no attachment bytes.

- [ ] **Step 9: Commit**

```bash
git add crates/note-mcp/src/stdio.rs
git commit -m "feat(mcp): add standalone attachment tools"
```

### Task 8: Document and verify the complete scoped change

**Files:**

- Modify: `README.md`
- Verify: all files changed by Tasks 1-7

- [ ] **Step 1: Update the MCP README section**

Document:

- exact summary fields for `list_notes` and `semantic_search`;
- note content plus attachment metadata for `get_note`/`update_note`;
- no inline attachment input on MCP save/update;
- put/delete lifecycle by `attachment_id`;
- `get_note_attachment_content` as the only attachment byte read;
- UTF-8 versus Base64 response selection;
- same-id path immutability and rename as delete plus put.

Do not describe REST behavior as changed.

- [ ] **Step 2: Run formatting and static diff checks**

```bash
cargo fmt --all -- --check
git diff --check
rg -n "TODO|FIXME|unimplemented!|todo!" \
  crates/note-core \
  crates/note-storage \
  crates/note-storage-contract-tests \
  crates/note-storage-turso \
  crates/note-storage-pg \
  crates/note-attachments \
  crates/note-pipelines \
  crates/note-mcp \
  README.md
```

Expected: formatting and diff checks pass; no unfinished implementation markers remain.

- [ ] **Step 3: Run scoped backend tests**

```bash
cargo test -p note-attachments
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-pipelines
cargo test -p note-mcp
cargo test -p note-server --test notes_api_test
```

When MinIO and PostgreSQL are configured, also run:

```bash
NOTE_TEST_MINIO_ENDPOINT=http://127.0.0.1:9000 \
NOTE_TEST_MINIO_BUCKET=agent-note-tests \
cargo test -p note-attachments --test s3_minio_test

TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
cargo test -p note-storage-pg
```

- [ ] **Step 4: Run affected-crate checks**

```bash
cargo check -p note-core
cargo check -p note-storage
cargo check -p note-storage-turso
cargo check -p note-storage-pg
cargo check -p note-attachments
cargo check -p note-pipelines
cargo check -p note-mcp
cargo check -p note-server
```

- [ ] **Step 5: Perform a contract-focused review**

Inspect the final diff and confirm:

- no MCP list/search/detail path calls attachment hydration;
- no MCP note response includes attachment bytes;
- save/update input schemas omit inline attachments;
- field-only updates cannot overwrite attachment metadata;
- single-object put/delete cannot rewrite siblings;
- put/delete share the complete-set same-note lock;
- identity/path uniqueness is revalidated inside an immediate transaction;
- attachment-only changes do not increment revision or enqueue embedding work;
- REST/export complete-set workflows still call the original hydrated/full-set paths;
- no local workaround was added for the downstream nullable-schema client bug.

- [ ] **Step 6: Commit documentation**

```bash
git add README.md docs/superpowers/specs/2026-07-19-mcp-note-summary-attachment-management-design.md docs/superpowers/plans/2026-07-19-mcp-note-summary-attachment-management.md
git commit -m "docs(mcp): describe standalone attachment lifecycle"
```

- [ ] **Step 7: Report verification truthfully**

Report exact commands and outcomes. Distinguish passing tests from PostgreSQL/MinIO tests skipped because their external services were unavailable. Do not claim the original `list_notes` nullable-schema report was fixed in this repository; it was confirmed as a downstream client-validation issue and intentionally left unchanged.
