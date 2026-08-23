# Org Document Lifecycle Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add revision-safe create, rename, archive, and restore operations for canonical Org documents across storage, pipelines, REST/OpenAPI, MCP, and the read-only-source Web console.

**Architecture:** Add a shared pure path validator and nullable document archival state, then expose narrow lifecycle CAS writes that never rewrite Org source or projections. Build dedicated idempotent pipeline commands on that contract, exclude archived-document items from operational candidates while retaining them for history/dependency/policy reads, and add equivalent REST, MCP, snapshot, and Yew lifecycle surfaces.

**Tech Stack:** Rust 2021, async traits, Turso/libSQL-compatible SQL, PostgreSQL/sqlx, Axum, utoipa/OpenAPI, rmcp/schemars, serde/serde_json, Yew 0.23, gloo-net, Yew Router, Trunk/Wasm, Chrome DevTools CLI.

**Approved design:** [`../specs/2026-08-23-org-document-lifecycle-management-design.md`](../specs/2026-08-23-org-document-lifecycle-management-design.md)

---

## Scope and execution constraints

- Run this plan in an isolated worktree under `.trees/`, created at execution time with the
  `using-git-worktrees` skill.
- Preserve the unrelated untracked files currently visible in the main checkout:
  `docs/agent-note-reliability-hardening-codex-plan.md`, `docs/emacs-org-guide.md`, and
  `docs/superpowers/plans/2026-08-23-notes-batch-actions.md`.
- Use red-green-refactor. Every production change in a task follows a focused failing test.
- Run only the Org/storage/frontend tests named in this plan. If an unrelated test fails, record it
  and stop instead of repairing it.
- PostgreSQL tests require `TEST_DATABASE_URL`. A skipped PostgreSQL test is unverified, not passing.
- Do not push, release, deploy, or merge the worktree as part of this plan.

## File map

### Shared domain path rule

- Create `crates/note-org/src/path.rs`: one portable relative lowercase-`.org` path validator.
- Create `crates/note-org/tests/document_path_test.rs`: accepted/rejected path contract.
- Modify `crates/note-org/src/lib.rs`: export the path module.
- Modify `crates/note-pipelines/src/org/documents.rs`: replace the weaker import-path check.
- Modify `crates/note-server/src/org_offline.rs`: reuse the domain validator instead of a private copy.

### Storage model, migrations, adapters, and operational filtering

- Modify `crates/note-storage/src/records.rs`: document archival state, lifecycle update, and event kinds.
- Modify `crates/note-storage/src/repositories.rs`: narrow lifecycle CAS repository method.
- Modify `crates/note-storage/tests/org_workflow_records_test.rs`: event round trips.
- Modify `crates/note-storage-contract-tests/src/org.rs`: driver-neutral lifecycle and operational tests.
- Create `crates/note-storage-pg/migrations/0006_org_document_lifecycle.sql`: nullable document archive timestamp.
- Modify `crates/note-storage-pg/src/org.rs`: decode/write archival state, lifecycle CAS, and active-document operational joins.
- Modify `crates/note-storage-pg/tests/lifecycle_test.rs`: migration inventory and survivor state.
- Modify `crates/note-storage-pg/tests/contracts_test.rs`: continue invoking the shared Org contract.
- Create `crates/note-storage-turso/migrations/0006_to_0007_org_document_lifecycle.sql`: schema 6 to 7 migration.
- Create `crates/note-storage-turso/tests/fixtures/schema-v6.sql`: pre-lifecycle migration fixture.
- Modify `crates/note-storage-turso/schema.sql`: fresh schema archival column.
- Modify `crates/note-storage-turso/src/preflight.rs`: schema version 7 and accepted predecessor constants.
- Modify `crates/note-storage-turso/src/connection.rs`: ordered 6-to-7 migration and rollback-injection coverage.
- Modify `crates/note-storage-turso/src/org.rs`: decode/write archival state, lifecycle CAS, and active-document operational joins.
- Modify `crates/note-storage-turso/tests/schema_test.rs`: version-7 migration and survivor tests.
- Modify `crates/note-storage-turso/tests/contracts_test.rs`: continue invoking the shared Org contract.

### Pipeline lifecycle, reads, guards, and snapshots

- Create `crates/note-pipelines/src/org/document_lifecycle.rs`: four idempotent lifecycle commands and shared lifecycle result data.
- Modify `crates/note-pipelines/src/org/mod.rs`: register/export the lifecycle module.
- Modify `crates/note-pipelines/src/org/error.rs`: archived-document and path-conflict error codes.
- Modify `crates/note-pipelines/src/org/dto.rs`: document archival fields and document-specific status query.
- Modify `crates/note-pipelines/src/org/cursor.rs`: document status in read cursor fingerprints.
- Modify `crates/note-pipelines/src/org/operational.rs`: discriminator-aware generic read pagination.
- Modify `crates/note-pipelines/src/org/context_read.rs`: filter documents before pagination and map archival fields.
- Modify `crates/note-pipelines/src/org/documents.rs`: archived full-source/move guards and snapshot archival propagation.
- Modify `crates/note-pipelines/src/org/claims.rs`: reject mutations against archived documents in the shared loader.
- Modify `crates/note-pipelines/src/org/items.rs`: reject batch item-source mutations against archived documents.
- Modify `crates/note-pipelines/tests/org_workspace_document_test.rs`: lifecycle, idempotency, snapshot, and direct-read behavior.
- Modify `crates/note-pipelines/tests/org_active_lease_guards_test.rs`: execution/review lease blockers and rollback.
- Modify `crates/note-pipelines/tests/org_operational_views_test.rs`: queue/agenda/count exclusion, dependencies, and restoration.

### REST, MCP, and offline delivery

- Modify `crates/note-server/src/org_api/dto.rs`: lifecycle request bodies and document list query.
- Modify `crates/note-server/src/org_api/documents.rs`: four routes and status-aware listing.
- Modify `crates/note-server/src/org_api/error.rs`: map document lifecycle conflicts to HTTP 409.
- Modify `crates/note-server/src/openapi.rs`: typed lifecycle result/data schemas.
- Modify `crates/note-server/src/org_offline.rs`: manifest document archival state.
- Modify `crates/note-server/tests/org_workspace_document_api_test.rs`: route behavior and filters.
- Modify `crates/note-server/tests/org_api_inventory_test.rs`: 40-operation inventory/schema assertions.
- Modify `crates/note-server/tests/org_transport_conformance_test.rs`: REST/MCP equivalence.
- Modify `crates/note-server/tests/org_offline_test.rs`: archived snapshot round trips and validation.
- Modify `crates/note-mcp/src/org.rs`: register 40 Org tools and four handlers.
- Modify `crates/note-mcp/src/org_dto.rs`: lifecycle inputs/outputs and status-aware document listing.
- Modify `crates/note-mcp/src/stdio.rs`: aggregate tool-count assertions.
- Modify `crates/note-mcp/tests/org_inventory_test.rs`: exact 40-Org/52-total tool inventory.
- Modify `crates/note-mcp/tests/org_workspace_document_tools_test.rs`: lifecycle schemas/results/errors.
- Modify `crates/note-mcp/tests/org_transports_test.rs`: updated counts and transport parity.

### Yew Web lifecycle console

- Create `crates/note-frontend/src/org/mutation.rs`: shared Web actor envelope, UUID submission identity, and command result.
- Create `crates/note-frontend/src/org/document_management.rs`: file filter/query state, request bodies, drafts, and validation.
- Modify `crates/note-frontend/src/org/workspace_management.rs`: reuse shared mutation primitives.
- Modify `crates/note-frontend/src/org/model.rs`: document archival state.
- Modify `crates/note-frontend/src/org/api.rs`: document reads/lifecycle clients and exact browser mutation allowlist.
- Modify `crates/note-frontend/src/org/url.rs`: file-route/filter cursor URLs.
- Modify `crates/note-frontend/src/org/mod.rs`: export new modules.
- Modify `crates/note-frontend/Cargo.toml`: depend on pure `note-org` path validation.
- Create `crates/note-frontend/src/components/org_document_table.rs`: accessible active/archived file table and actions.
- Modify `crates/note-frontend/src/components/mod.rs`: export the file table.
- Create `crates/note-frontend/src/pages/org_workspace_files.rs`: load, paginate, mutate, retry, and conflict UI.
- Modify `crates/note-frontend/src/pages/mod.rs`: export the files page.
- Modify `crates/note-frontend/src/pages/org_workspace.rs`: add the Files entry point.
- Modify `crates/note-frontend/src/routes.rs`: `/org/:workspace_id/files` route/title/switch.
- Modify `crates/note-frontend/app.css`: namespaced responsive file-management styling.

### Documentation and browser acceptance

- Modify `docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md`: 40 operations and approved browser lifecycle boundary.
- Modify `docs/design.md`: document state, routes, REST, MCP, and operational exclusion.
- Modify `README.md`: operator-facing files route/capability summary.
- Create `scripts/verify-org-document-lifecycle-browser.sh`: disposable full lifecycle acceptance.
- Modify `scripts/verify-org-workspace-management-browser.sh`: allow only the newly approved document lifecycle traffic in addition to workspace lifecycle traffic.
- Modify `scripts/verify-org-console-browser.sh`: keep source/workflow mutations forbidden while allowing document lifecycle.

### Task 1: Centralize the portable Org document path contract

**Files:**
- Create: `crates/note-org/src/path.rs`
- Create: `crates/note-org/tests/document_path_test.rs`
- Modify: `crates/note-org/src/lib.rs`
- Modify: `crates/note-pipelines/src/org/documents.rs:1988-2024`
- Modify: `crates/note-server/src/org_offline.rs:1079-1107`

- [ ] **Step 1: Write the failing path matrix test**

```rust
use note_org::validate_document_path;

#[test]
fn accepts_only_portable_relative_lowercase_org_paths() {
    for valid in ["roadmap.org", "projects/roadmap.org", "release notes.org"] {
        assert!(validate_document_path(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "", " roadmap.org", "roadmap.org ", "/roadmap.org", "../roadmap.org",
        "./roadmap.org", "projects//roadmap.org", "projects\\roadmap.org",
        "C:/roadmap.org", "roadmap.ORG", "roadmap.txt",
    ] {
        assert!(validate_document_path(invalid).is_err(), "{invalid}");
    }
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p note-org --test document_path_test`

Expected: compilation fails because `validate_document_path` is not defined.

- [ ] **Step 3: Implement the pure shared validator**

Create `path.rs` with the complete rule, without filesystem access or normalization:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Org document path must be a portable relative lowercase .org path")]
pub struct DocumentPathError;

pub fn validate_document_path(value: &str) -> Result<(), DocumentPathError> {
    let bytes = value.as_bytes();
    let drive_prefix = bytes.get(1) == Some(&b':')
        && bytes.first().is_some_and(u8::is_ascii_alphabetic);
    let invalid_segment = value
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."));

    if value.trim().is_empty()
        || value != value.trim()
        || value.starts_with('/')
        || value.contains('\\')
        || drive_prefix
        || invalid_segment
        || !value.ends_with(".org")
    {
        return Err(DocumentPathError);
    }
    Ok(())
}
```

Export it with `pub mod path; pub use path::*;` from `note-org/src/lib.rs`.

- [ ] **Step 4: Replace both existing validators and verify GREEN**

In the pipeline, map the domain error to structured invalid input:

```rust
note_org::validate_document_path(&document.path).map_err(|_| {
    OrgError::new(
        OrgErrorCode::InvalidInput,
        "Org document path must be a portable relative lowercase .org path",
        serde_json::json!({"field": "path", "path": document.path}),
        false,
    )
})?;
```

In `org_offline.rs`, call the same helper and preserve the existing CLI error context. Delete the
private `validate_portable_path` function.

Run:

```bash
cargo test -p note-org --test document_path_test
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-server --test org_offline_test
```

Expected: all three focused suites pass, including nested paths and lowercase `.org` enforcement.

- [ ] **Step 5: Commit**

```bash
git add crates/note-org/src/lib.rs crates/note-org/src/path.rs crates/note-org/tests/document_path_test.rs crates/note-pipelines/src/org/documents.rs crates/note-server/src/org_offline.rs
git commit -m "feat(org): validate portable document paths"
```

### Task 2: Add document archival state and ordered migrations

**Files:**
- Modify: `crates/note-storage/src/records.rs:235-263`
- Create: `crates/note-storage-pg/migrations/0006_org_document_lifecycle.sql`
- Modify: `crates/note-storage-pg/src/org.rs:21-350,3231-3255`
- Modify: `crates/note-storage-pg/tests/lifecycle_test.rs`
- Create: `crates/note-storage-turso/migrations/0006_to_0007_org_document_lifecycle.sql`
- Create: `crates/note-storage-turso/tests/fixtures/schema-v6.sql`
- Modify: `crates/note-storage-turso/schema.sql:114-124`
- Modify: `crates/note-storage-turso/src/preflight.rs:13-18`
- Modify: `crates/note-storage-turso/src/connection.rs:15-268,459-900`
- Modify: `crates/note-storage-turso/src/org.rs:21-335,3903-3932`
- Modify: `crates/note-storage-turso/tests/schema_test.rs`

- [ ] **Step 1: Write failing migration-survivor assertions**

Add a schema-v6 fixture containing an active Org document, then assert opening it upgrades to
version 7 without changing source or revision:

```rust
#[tokio::test]
async fn schema_v6_document_migrates_as_active() {
    let (_dir, path) = create_schema_v6_database().await;
    let storage = TursoStorage::open(&path).await.unwrap();
    let document = storage
        .session().await.unwrap()
        .get_org_document(document_id()).await.unwrap().unwrap();
    assert_eq!(document.archived_at, None);
    assert_eq!(document.revision, 4);
    assert_eq!(database_user_version(&path), 7);
}
```

Extend the PostgreSQL lifecycle test to assert migration version `6` exists and
`org_documents.archived_at` is nullable with existing rows `NULL`.

- [ ] **Step 2: Run migration tests and verify RED**

```bash
cargo test -p note-storage-turso --test schema_test schema_v6_document_migrates_as_active
cargo test -p note-storage-pg --test lifecycle_test connect_runs_migrations_with_all_expected_tables_and_indexes
```

Expected: Turso fails because schema version 7/migration is absent. PostgreSQL fails when
`TEST_DATABASE_URL` is configured because migration 6/column is absent; otherwise record the skip.

- [ ] **Step 3: Add the nullable field and SQL migrations**

Add the record field:

```rust
pub struct OrgDocument {
    pub id: note_org::DocumentId,
    pub workspace_id: note_org::WorkspaceId,
    pub path: String,
    pub source: String,
    pub content_hash: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}
```

PostgreSQL migration:

```sql
ALTER TABLE org_documents ADD COLUMN archived_at bigint;
```

Turso migration:

```sql
ALTER TABLE org_documents ADD COLUMN archived_at INTEGER;
PRAGMA user_version = 7;
```

Add the same nullable column to the fresh Turso schema. Keep the existing unique
`(workspace_id, path)` constraint unchanged.

- [ ] **Step 4: Thread schema version 7 and decode state through both adapters**

Set Turso constants to:

```rust
pub(crate) const SCHEMA_V5_VERSION: u32 = 5;
pub(crate) const PREVIOUS_SCHEMA_VERSION: u32 = 6;
pub(crate) const SCHEMA_VERSION: u32 = 7;
```

Add `MIGRATION_6_TO_7`, accept versions 2 through 7, and pass the new migration through every
initialization and rollback-injection helper. Change future-version tests from 7 to 8.

Append `archived_at` to both adapters' document column lists/row structs and decode it as
`Option<i64>`. Inserts continue writing `NULL`; full-source CAS and ownership move return/preserve
the column. Add `archived_at: None` to direct `OrgDocument` test fixtures.

- [ ] **Step 5: Verify migrations and commit**

```bash
cargo test -p note-storage-turso --test schema_test
cargo test -p note-storage-turso connection::tests
cargo test -p note-storage-pg --test lifecycle_test
cargo test -p note-storage --test org_workflow_records_test
```

Expected: Turso fresh/open/migration/rollback tests pass. PostgreSQL passes when configured or is
reported unverified when skipped. Core storage record tests pass.

```bash
git add crates/note-storage/src/records.rs crates/note-storage-pg/migrations/0006_org_document_lifecycle.sql crates/note-storage-pg/src/org.rs crates/note-storage-pg/tests/lifecycle_test.rs crates/note-storage-turso/migrations/0006_to_0007_org_document_lifecycle.sql crates/note-storage-turso/schema.sql crates/note-storage-turso/src/preflight.rs crates/note-storage-turso/src/connection.rs crates/note-storage-turso/src/org.rs crates/note-storage-turso/tests/fixtures/schema-v6.sql crates/note-storage-turso/tests/schema_test.rs
git commit -m "feat(storage): migrate Org document archival state"
```

### Task 3: Implement narrow lifecycle CAS and operational exclusion in both adapters

**Files:**
- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs:583-636`
- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs:165-350,1124-1298`
- Modify: `crates/note-storage-turso/src/org.rs:191-335,1041-1215`
- Modify: `crates/note-storage-pg/tests/contracts_test.rs`
- Modify: `crates/note-storage-turso/tests/contracts_test.rs`

- [ ] **Step 1: Write failing driver-neutral lifecycle contract tests**

Add one shared test that creates a document/projection, renames, archives, and restores it:

```rust
let renamed = session.compare_and_swap_org_document_lifecycle(
    OrgDocumentLifecycleUpdate {
        id: document_id,
        expected_revision: 1,
        expected_archived_at: None,
        path: "archive/renamed.org",
        archived_at: None,
        updated_at: 20,
    },
).await.unwrap();
let CompareAndSwap::Applied(renamed) = renamed else { panic!("rename CAS"); };
assert_eq!(renamed.revision, 2);
assert_eq!(renamed.source, original_source);
assert_eq!(renamed.archived_at, None);
```

Continue with archive at revision 2 and restore at revision 3. Assert the source, hash, projection,
created time, and stable IDs never change; stale writes return `CompareAndSwap::Conflict`; and an
archived path still rejects another insert.

Add an operational contract fixture with active and archived documents. Assert all ten operational
views and the workspace summary exclude archived-document items while
`list_org_workspace_projection` and direct document/item reads retain them.

- [ ] **Step 2: Run both adapter contracts and verify RED**

```bash
cargo test -p note-storage-turso --test contracts_test org_document_lifecycle
cargo test -p note-storage-pg --test contracts_test org_document_lifecycle
```

Expected: compilation fails because lifecycle update/method do not exist; PostgreSQL may be skipped
without `TEST_DATABASE_URL`.

- [ ] **Step 3: Add the narrow storage contract**

```rust
pub struct OrgDocumentLifecycleUpdate<'a> {
    pub id: note_org::DocumentId,
    pub expected_revision: i64,
    pub expected_archived_at: Option<i64>,
    pub path: &'a str,
    pub archived_at: Option<i64>,
    pub updated_at: i64,
}
```

Add to `OrgRepository`:

```rust
async fn compare_and_swap_org_document_lifecycle(
    &self,
    update: OrgDocumentLifecycleUpdate<'_>,
) -> StorageResult<CompareAndSwap<OrgDocument>>;
```

The method changes only path, `archived_at`, `updated_at`, and `revision`. It never accepts source,
hash, workspace, or projection fields.

- [ ] **Step 4: Implement exact CAS SQL and operational joins**

PostgreSQL lifecycle SQL:

```sql
UPDATE org_documents
SET path = $2, archived_at = $3, updated_at = $4, revision = revision + 1
WHERE id = $1 AND revision = $5
  AND archived_at IS NOT DISTINCT FROM $6
RETURNING id, workspace_id, path, source, content_hash, revision,
          created_at, updated_at, archived_at
```

Turso uses the same fields with `archived_at IS ?6`. If no row is returned, use the existing current
revision lookup to return `NotFound` or `Conflict`.

In both `query_org_operational` implementations and workspace-summary count queries, join
`org_documents document ON document.id = item.document_id` and unconditionally require
`document.archived_at IS NULL`. Keep `workspace.archived_at` controlled by the existing
workspace-level `include_archived` option. Do not change complete workspace projection or dependency
target queries.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: Turso contracts pass; PostgreSQL contracts pass when configured or are reported unverified.

```bash
git add crates/note-storage/src/records.rs crates/note-storage/src/repositories.rs crates/note-storage-contract-tests/src/org.rs crates/note-storage-pg/src/org.rs crates/note-storage-pg/tests/contracts_test.rs crates/note-storage-turso/src/org.rs crates/note-storage-turso/tests/contracts_test.rs
git commit -m "feat(storage): persist Org document lifecycle"
```

### Task 4: Add idempotent lifecycle commands and archived-document mutation guards

**Files:**
- Create: `crates/note-pipelines/src/org/document_lifecycle.rs`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Modify: `crates/note-pipelines/src/org/error.rs`
- Modify: `crates/note-storage/src/records.rs:294-420`
- Modify: `crates/note-storage/tests/org_workflow_records_test.rs`
- Modify: `crates/note-pipelines/src/org/claims.rs:944-989`
- Modify: `crates/note-pipelines/src/org/items.rs:959-978`
- Modify: `crates/note-pipelines/src/org/documents.rs:1189-1245,824-865`
- Modify: `crates/note-pipelines/tests/org_workspace_document_test.rs`
- Modify: `crates/note-pipelines/tests/org_active_lease_guards_test.rs`

- [ ] **Step 1: Write failing lifecycle and guard tests**

Cover these exact cases:

```rust
let created = create_document(
    &context,
    &envelope(workspace, "create-file"),
    &CreateDocumentRequest { document_id, path: "plans/main.org".into() },
).await.unwrap();
assert_eq!(created.document_revisions[&document_id.to_string()], 1);
let stored = session.get_org_document(document_id).await.unwrap().unwrap();
assert_eq!(stored.source, "");
assert_eq!(stored.archived_at, None);
```

Then assert rename preserves source/hash/projection and works while archived; renaming an archived
file frees its old reserved path for a new document; archive preserves data and blocks both execution
and review leases with `ActiveLease` details containing only work-item IDs; restore reactivates the
same ID; stale revisions and wrong lifecycle states leave no event/operation; same-operation replay
returns the original result; divergent reuse returns `IdempotencyConflict`; archived documents
reject put, move, claim, execution, review, and semantic item edits.

- [ ] **Step 2: Run focused pipeline tests and verify RED**

```bash
cargo test -p note-pipelines --test org_workspace_document_test document_lifecycle
cargo test -p note-pipelines --test org_active_lease_guards_test archive_document
```

Expected: compilation fails because lifecycle commands and archival guards are absent.

- [ ] **Step 3: Define request/result/error/event contracts**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDocumentRequest { pub document_id: DocumentId, pub path: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameDocumentRequest {
    pub document_id: DocumentId,
    pub new_path: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRevisionRequest {
    pub document_id: DocumentId,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLifecycleData {
    pub document_id: DocumentId,
    pub path: String,
    pub archived_at: Option<i64>,
}
```

Add `ArchivedDocument` and `DocumentPathConflict` to `OrgErrorCode`. Add
`DocumentRename`, `DocumentArchive`, and `DocumentRestore` to `OrgEventType`, increase `KNOWN` from
30 to 33, and map them to `document_rename`, `document_archive`, and `document_restore`.

- [ ] **Step 4: Implement the four commands and central guards**

Use these command kinds:

```rust
const CREATE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("create_document", 1);
const RENAME_DOCUMENT: OrgCommandKind = OrgCommandKind::new("rename_document", 1);
const ARCHIVE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("archive_document", 1);
const RESTORE_DOCUMENT: OrgCommandKind = OrgCommandKind::new("restore_document", 1);
```

`create_document` validates the shared path, verifies ID/path absence, parses `""`, hashes it,
inserts one active revision-1 document, installs an empty projection, emits `Creation`, and returns
`DocumentLifecycleData` through `OrgCommandResult.data`.

Implement rename/archive/restore through one private decision table before lifecycle CAS:

| Action | Required current state | Target path | Target `archived_at` | Lease check |
|---|---|---|---|---|
| Rename | active or archived | `new_path` | unchanged | none |
| Archive | active | unchanged | `Some(now)` | every projected item |
| Restore | archived | unchanged | `None` | none |

For archive, collect `get_active_org_lease(item.id, now)` results and return:

```rust
OrgError::new(
    OrgErrorCode::ActiveLease,
    "Org document has active leases",
    serde_json::json!({"document_id": document.id, "work_item_ids": blocking_ids}),
    false,
)
```

Never include lease IDs or fencing material. Map lifecycle CAS constraint errors to
`DocumentPathConflict`; map revision conflicts through `resolve_cas`. Emit one document-subject event
with previous/resulting path or archival state and never raw Org source. Do not change workspace
revision.

Expose the lease scan from `document_lifecycle.rs` as:

```rust
pub(crate) async fn blocking_document_lease_ids(
    transaction: &dyn StorageTransaction,
    document_id: DocumentId,
    now: i64,
) -> Result<Vec<WorkItemId>, OrgError> {
    let items = transaction
        .list_org_document_projection(document_id)
        .await
        .map_err(OrgError::storage)?;
    let mut blocking = Vec::new();
    for item in items {
        if transaction
            .get_active_org_lease(item.id, now)
            .await
            .map_err(OrgError::storage)?
            .is_some()
        {
            blocking.push(item.id);
        }
    }
    blocking.sort();
    Ok(blocking)
}
```

Snapshot import in Task 6 uses this exact helper, ensuring one definition of active
execution/review blockers.

Make `claims::load_document` reject `archived_at.is_some()` with `ArchivedDocument`; use that shared
loader from claim/execution/review flows. Add the same check to `items::load_documents`, full-source
import preparation, and cross-workspace document move.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-storage --test org_workflow_records_test
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-pipelines --test org_active_lease_guards_test
```

Expected: lifecycle, event, archived-mutation, active-lease, atomicity, and idempotency tests pass.

```bash
git add crates/note-pipelines/src/org/document_lifecycle.rs crates/note-pipelines/src/org/mod.rs crates/note-pipelines/src/org/error.rs crates/note-pipelines/src/org/claims.rs crates/note-pipelines/src/org/items.rs crates/note-pipelines/src/org/documents.rs crates/note-pipelines/tests/org_workspace_document_test.rs crates/note-pipelines/tests/org_active_lease_guards_test.rs crates/note-storage/src/records.rs crates/note-storage/tests/org_workflow_records_test.rs
git commit -m "feat(org): manage document lifecycle"
```

### Task 5: Add status-aware reads, stable cursors, and operational behavior

**Files:**
- Modify: `crates/note-pipelines/src/org/dto.rs:63-68,278-292`
- Modify: `crates/note-pipelines/src/org/cursor.rs:145-198`
- Modify: `crates/note-pipelines/src/org/operational.rs:689-724`
- Modify: `crates/note-pipelines/src/org/context_read.rs:18-114,649-665`
- Modify: `crates/note-pipelines/tests/org_workspace_document_test.rs`
- Modify: `crates/note-pipelines/tests/org_operational_views_test.rs`

- [ ] **Step 1: Write failing read/filter/operational tests**

Create active and archived documents whose IDs interleave. Assert filtering occurs before pagination:

```rust
let query = OrgDocumentReadQuery {
    cursor: None,
    limit: Some(1),
    status: DocumentStatus::Archived,
};
let page = list_documents(&context, workspace, &query).await.unwrap();
assert_eq!(page.items.len(), 1);
assert!(page.items[0].archived_at.is_some());
```

Assert a cursor from `Active` is rejected for `Archived`; archived workspace documents remain
listable read-only; direct document/item reads include archival state; every queue/agenda/count omits
archived items; an unfinished archived dependency still blocks an active dependent; restore returns
the same work-item ID to its prior operational view.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test -p note-pipelines --test org_workspace_document_test document_status
cargo test -p note-pipelines --test org_operational_views_test archived_document
```

Expected: compilation or assertions fail because document status and archival DTO fields are absent.

- [ ] **Step 3: Define the document-only read query and DTO fields**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus { Active, Archived, All }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgDocumentReadQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub status: DocumentStatus,
}
```

Add `archived_at: Option<i64>` to `OrgDocumentView` and `OrgDocumentSourceView`. Do not reuse or
change `OrgReadQuery.include_archived`, which continues to mean archived workspace inclusion for
unrelated reads.

- [ ] **Step 4: Filter before pagination and bind status to cursors**

Resolve the workspace without suppressing archived workspaces. Load the complete document list,
filter it by `DocumentStatus`, then paginate. Extend the read fingerprint helper to accept a stable
discriminator:

```rust
pub(crate) fn read_fingerprint(
    family: &str,
    scope: Option<&str>,
    discriminator: &serde_json::Value,
) -> String {
    let value = serde_json::json!({
        "family": family, "scope": scope, "filter": discriminator,
    });
    format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
}
```

Add `paginate_read_with_discriminator` in `operational.rs`; keep `paginate_read` as the compatibility
wrapper that passes the existing `include_archived` boolean. Document listing calls the new helper
with `{"status": query.status}`. Map `archived_at` in list/source DTOs and workspace export.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-pipelines --test org_operational_views_test
```

Expected: status pagination/cursor tests, direct reads, all operational views/counts, dependency
blocking, and restoration identity pass.

```bash
git add crates/note-pipelines/src/org/dto.rs crates/note-pipelines/src/org/cursor.rs crates/note-pipelines/src/org/operational.rs crates/note-pipelines/src/org/context_read.rs crates/note-pipelines/tests/org_workspace_document_test.rs crates/note-pipelines/tests/org_operational_views_test.rs
git commit -m "feat(org): filter archived document work"
```

### Task 6: Preserve document lifecycle in workspace and offline snapshots

**Files:**
- Modify: `crates/note-pipelines/src/org/documents.rs:42-80,371-790,1988-2055`
- Modify: `crates/note-pipelines/tests/org_workspace_document_test.rs`
- Modify: `crates/note-pipelines/tests/org_active_lease_guards_test.rs`
- Modify: `crates/note-server/src/org_api/dto.rs:1330-1365`
- Modify: `crates/note-server/src/org_offline.rs:77-106,475-511,719-797`
- Modify: `crates/note-server/tests/org_offline_test.rs`
- Modify: `crates/note-mcp/src/org_dto.rs:571-625`

- [ ] **Step 1: Write failing snapshot round-trip tests**

Export a workspace with one active and one archived document, import it under a new workspace ID,
and assert exact source, path, revision, archival timestamp, projection IDs, and operational
visibility. Add rejection tests for nonpositive archive timestamps, an archived snapshot document at
revision 1, and snapshot archival of a document with an active lease.

```rust
assert_eq!(restored_archived.archived_at, original_archived.archived_at);
assert_eq!(restored_archived.revision, original_archived.revision);
assert_eq!(restored_archived.source, original_archived.source);
assert_eq!(restored_items, original_items);
```

- [ ] **Step 2: Run snapshot tests and verify RED**

```bash
cargo test -p note-pipelines --test org_workspace_document_test archived_snapshot
cargo test -p note-server --test org_offline_test archived_document
```

Expected: archive state is missing from the pipeline export/import and offline manifest.

- [ ] **Step 3: Carry archival state without opening a lifecycle bypass**

Add `archived_at: Option<i64>` to `DocumentImport` and include it in import/snapshot fingerprints.
`PutDocumentRequest` always adapts to `DocumentImport { document_id, path, source,
archived_at: None }`. Add explicit
`archived_at` mapping to every `DocumentImport` literal/conversion in pipeline tests, REST DTOs,
offline code, and MCP DTOs so each commit remains compilable.

Use an explicit `allow_archived_state` flag in `ApplyDocumentImportOptions`:

```rust
struct ApplyDocumentImportOptions<'a> {
    event_ids: Vec<String>,
    created_target_revisions: Option<&'a BTreeMap<DocumentId, i64>>,
    import_mode: DocumentImportMode,
    allow_archived_state: bool,
}
```

Normal put/batch import passes `false`: new documents must be active and existing archived documents
remain read-only. Workspace snapshot restore passes `true`: it validates timestamps, exact document
sets/revisions, and active leases before applying the requested state.

For snapshot create, insert active revision 1. If the target is archived, require target revision at
least 2 and make the final revision increment the lifecycle CAS that sets `archived_at`; preceding
increments use full-source CAS. For snapshot update, use lifecycle CAS only when archival state
changes and call `blocking_document_lease_ids` before archival. Emit `DocumentArchive` or
`DocumentRestore` when snapshot import changes state; emit `DocumentImport` only when lifecycle state
does not change. Event metadata never contains source.

- [ ] **Step 4: Extend the offline manifest and remove duplicate path logic**

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestDocument {
    pub id: DocumentId,
    pub path: String,
    pub revision: i64,
    pub content_hash: String,
    pub file: String,
    #[serde(default)]
    pub archived_at: Option<i64>,
}
```

Write/read the field, reject timestamps `<= 0`, and map it into `DocumentImport`. Missing fields in
older manifests deserialize as active. Keep canonical snapshot filenames based on UUID, not path.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-server --test org_offline_test
```

Expected: active/archived snapshots round-trip, invalid timestamps/revisions/leases roll back, and
legacy active manifests still import.

```bash
git add crates/note-pipelines/src/org/documents.rs crates/note-pipelines/tests/org_workspace_document_test.rs crates/note-pipelines/tests/org_active_lease_guards_test.rs crates/note-server/src/org_api/dto.rs crates/note-server/src/org_offline.rs crates/note-server/tests/org_offline_test.rs crates/note-mcp/src/org_dto.rs
git commit -m "feat(org): preserve document lifecycle in snapshots"
```

### Task 7: Expose lifecycle through REST and OpenAPI

**Files:**
- Modify: `crates/note-server/src/org_api/dto.rs:65-100,1228-1290`
- Modify: `crates/note-server/src/org_api/documents.rs:19-222`
- Modify: `crates/note-server/src/org_api/error.rs:135-153`
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/tests/org_workspace_document_api_test.rs`
- Modify: `crates/note-server/tests/org_api_inventory_test.rs`
- Modify: `crates/note-server/tests/org_transport_conformance_test.rs`

- [ ] **Step 1: Write failing REST route, status, and schema tests**

Add requests for:

```text
POST  /api/org/workspaces/{workspace_id}/documents
PATCH /api/org/documents/{document_id}/path
POST  /api/org/documents/{document_id}/archive
POST  /api/org/documents/{document_id}/restore
```

Assert create returns revision 1 and empty source, rename/archive/restore return typed lifecycle data,
replay is stable, stale/path/active-lease/archived errors retain their codes, `status=archived` filters
before pagination, legacy `include_archived=true` means `status=all`, and combining both parameters is
`invalid_input`. Update the expected REST operation set from 36 to 40.

- [ ] **Step 2: Run REST tests and verify RED**

```bash
cargo test -p note-server --test org_workspace_document_api_test document_lifecycle
cargo test -p note-server --test org_api_inventory_test
```

Expected: four routes/operation IDs and lifecycle schemas are absent.

- [ ] **Step 3: Add exact request/query DTOs**

```rust
#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDocumentBody {
    #[serde(flatten)] pub command: OrgMutationEnvelope,
    pub document_id: String,
    pub path: String,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameDocumentBody {
    #[serde(flatten)] pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub new_path: String,
    pub expected_revision: i64,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevisionBody {
    #[serde(flatten)] pub command: OrgMutationEnvelope,
    pub workspace_id: String,
    pub expected_revision: i64,
}
```

Add `DocumentListQuery { cursor, limit, status: Option<DocumentStatusBody>,
include_archived: Option<bool> }`. Convert omitted parameters to `Active`, legacy true to `All`,
legacy false to `Active`, and reject any request containing both fields.

- [ ] **Step 4: Add routes, typed schemas, and conformance mappings**

Delegate each route directly to its pipeline command. Use `POST` create on the existing workspace
documents collection, `PATCH` path rename, and `POST` archive/restore actions. Register every route
in `documents::router()` with operation IDs equal to the MCP names.

Map `ArchivedDocument` and `DocumentPathConflict` to HTTP 409 in `status_for`; retain their stable
snake-case codes and sanitized details.

Define OpenAPI `OrgDocumentLifecycleData` and `OrgDocumentLifecycleCommandResult` rather than using
an untyped `serde_json::Value`; include `archived_at` in document list/source result schemas. Extend
success-schema and expected-revision maps for all four operation IDs.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-server --test org_workspace_document_api_test
cargo test -p note-server --test org_api_inventory_test
cargo test -p note-server --test org_transport_conformance_test
cargo test -p note-server openapi::tests
```

Expected: all four routes, typed schemas, status alias rules, replay/errors, inventory, and current
REST/OpenAPI behavior pass.

```bash
git add crates/note-server/src/org_api/dto.rs crates/note-server/src/org_api/documents.rs crates/note-server/src/org_api/error.rs crates/note-server/src/openapi.rs crates/note-server/tests/org_workspace_document_api_test.rs crates/note-server/tests/org_api_inventory_test.rs crates/note-server/tests/org_transport_conformance_test.rs
git commit -m "feat(server): expose Org document lifecycle"
```

### Task 8: Add equivalent MCP lifecycle tools

**Files:**
- Modify: `crates/note-mcp/src/org.rs:13-230,426-470`
- Modify: `crates/note-mcp/src/org_dto.rs:9-108,488-565`
- Modify: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/tests/org_inventory_test.rs`
- Modify: `crates/note-mcp/tests/org_workspace_document_tools_test.rs`
- Modify: `crates/note-mcp/tests/org_transports_test.rs`
- Modify: `crates/note-server/tests/org_transport_conformance_test.rs`

- [ ] **Step 1: Write failing tool inventory/schema/behavior tests**

Require the ordered document slice:

```rust
[
    "org_list_documents", "org_get_document", "org_put_document",
    "org_create_document", "org_rename_document", "org_archive_document",
    "org_restore_document", "org_move_document",
]
```

Assert exactly 40 Org tools and 52 total tools, exact required input fields, no source/lease-proof
fields on lifecycle tools, typed lifecycle output, status/legacy-alias behavior, replay, and normalized
REST/MCP errors.

- [ ] **Step 2: Run MCP tests and verify RED**

```bash
cargo test -p note-mcp --test org_inventory_test
cargo test -p note-mcp --test org_workspace_document_tools_test document_lifecycle
```

Expected: inventory remains 36 Org/48 total and lifecycle tools are absent.

- [ ] **Step 3: Add MCP DTOs matching REST**

```rust
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub(crate) struct CreateDocumentInput {
    pub schema_version: u32,
    pub workspace_id: String,
    pub actor_id: String,
    pub operation_id: String,
    pub document_id: String,
    pub path: String,
}
```

Add corresponding rename and revision inputs with exact expected revisions. Add
`DocumentLifecycleDataOutput`; include archival state in document list/source outputs. Replace
document listing's generic `WorkspaceListInput` with a document-specific input resolving
`status`/legacy `include_archived` exactly like REST.

- [ ] **Step 4: Register handlers and update every count assertion**

Increase `ORG_TOOL_NAMES` to 40 and place the four names beside current document tools. Each handler
adapts its DTO, invokes one pipeline function, and converts `OrgCommandResult` through the typed
lifecycle output. Update stdio and transport assertions to 40 Org and 52 aggregate tools.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p note-mcp --test org_inventory_test
cargo test -p note-mcp --test org_workspace_document_tools_test
cargo test -p note-mcp --test org_transports_test
cargo test -p note-server --test org_transport_conformance_test
```

Expected: exact 40/52 inventories, schemas, lifecycle behavior, and REST/MCP conformance pass.

```bash
git add crates/note-mcp/src/org.rs crates/note-mcp/src/org_dto.rs crates/note-mcp/src/stdio.rs crates/note-mcp/tests/org_inventory_test.rs crates/note-mcp/tests/org_workspace_document_tools_test.rs crates/note-mcp/tests/org_transports_test.rs crates/note-server/tests/org_transport_conformance_test.rs
git commit -m "feat(mcp): add Org document lifecycle tools"
```

### Task 9: Add typed frontend file contracts and REST clients

**Files:**
- Create: `crates/note-frontend/src/org/mutation.rs`
- Create: `crates/note-frontend/src/org/document_management.rs`
- Modify: `crates/note-frontend/src/org/workspace_management.rs:338-473`
- Modify: `crates/note-frontend/src/org/model.rs:1-68`
- Modify: `crates/note-frontend/src/org/api.rs:1-180,360-540`
- Modify: `crates/note-frontend/src/org/url.rs`
- Modify: `crates/note-frontend/src/org/mod.rs`
- Modify: `crates/note-frontend/Cargo.toml`

- [ ] **Step 1: Write failing model/query/body/boundary tests**

Test document deserialization with `archived_at`, canonical URLs for active/archived filters and
cursors, shared operation ID retention, exact JSON for all four mutations, frontend reuse of the
domain path validator, and typed error decoding. Replace the browser mutation boundary assertion so
it allows only workspace lifecycle plus these four document lifecycle paths while still forbidding
full-source PUT, item/workflow mutations, MCP, auth/session state, and fencing tokens.

```rust
assert_eq!(
    document_list_url("workspace-a", &DocumentListState::archived()),
    "/api/org/workspaces/workspace-a/documents?status=archived&limit=50"
);
```

- [ ] **Step 2: Run focused frontend tests and verify RED**

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml document_management
cargo test --manifest-path crates/note-frontend/Cargo.toml org::api
```

Expected: document management module, fields, bodies, and clients are absent.

- [ ] **Step 3: Extract shared mutation identity and define file state**

Move the existing Web actor envelope, UUID submission object, generated-ID helper, and generic
command result from `workspace_management.rs` into `org/mutation.rs`; update workspace callers
without changing their JSON.

In `document_management.rs`, define:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus { #[default] Active, Archived }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentListState {
    pub status: DocumentStatus,
    pub cursor: Option<String>,
    pub limit: u16,
}
```

Add create, rename, archive, and restore bodies using the shared mutation envelope. Create generates
one document UUID and one operation UUID; retries retain both. Rename/archive/restore retain the
operation UUID only while their complete body is unchanged.

Add `note-org = { path = "../note-org" }` and use `note_org::validate_document_path` for form
validation. Confirm it compiles for `wasm32-unknown-unknown`.

- [ ] **Step 4: Implement exact REST clients and URL encoding**

Add:

```rust
pub async fn list_documents(workspace_id: &str, state: &DocumentListState)
    -> Result<Page<Document>, OrgApiError>;
pub async fn create_document(workspace_id: &str, body: &CreateDocumentBody)
    -> Result<OrgMutationResult, OrgApiError>;
pub async fn rename_document(document_id: &str, body: &RenameDocumentBody)
    -> Result<OrgMutationResult, OrgApiError>;
pub async fn archive_document(document_id: &str, body: &DocumentRevisionBody)
    -> Result<OrgMutationResult, OrgApiError>;
pub async fn restore_document(document_id: &str, body: &DocumentRevisionBody)
    -> Result<OrgMutationResult, OrgApiError>;
```

Use POST/PATCH/POST/POST and the existing decoder. URL-encode all path IDs. Do not add full-source
PUT, DELETE, polling, MCP calls, actor choice, authentication, or fencing fields.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml document_management
cargo test --manifest-path crates/note-frontend/Cargo.toml org::api
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: request/query/model/boundary tests pass and the shared path validator builds for Wasm.

```bash
git add crates/note-frontend/Cargo.toml crates/note-frontend/Cargo.lock crates/note-frontend/src/org/api.rs crates/note-frontend/src/org/document_management.rs crates/note-frontend/src/org/model.rs crates/note-frontend/src/org/mod.rs crates/note-frontend/src/org/mutation.rs crates/note-frontend/src/org/url.rs crates/note-frontend/src/org/workspace_management.rs
git commit -m "feat(frontend): model Org file lifecycle"
```

### Task 10: Build the accessible workspace files page

**Files:**
- Create: `crates/note-frontend/src/components/org_document_table.rs`
- Modify: `crates/note-frontend/src/components/mod.rs`
- Create: `crates/note-frontend/src/pages/org_workspace_files.rs`
- Modify: `crates/note-frontend/src/pages/mod.rs`
- Modify: `crates/note-frontend/src/pages/org_workspace.rs:792-833`
- Modify: `crates/note-frontend/src/routes.rs:28-180`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing route, reducer, table, and source-boundary tests**

Assert `/org/workspace-a/files` recognition/title/switch, active/archived filters resetting cursors,
previous/next cursor history, active workspace action visibility, archived workspace read-only state,
exact-path archive confirmation, restore-only archived action, rename on either state, retry retaining
operation ID, stale conflict retaining draft but requiring refresh, accessible dialog/live-region
source, and complete absence of raw source controls.

- [ ] **Step 2: Run focused frontend tests and verify RED**

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml routes::tests::recognizes_org_workspace_files_route
cargo test --manifest-path crates/note-frontend/Cargo.toml org_workspace_files
cargo test --manifest-path crates/note-frontend/Cargo.toml org_document_table
```

Expected: files route/page/table do not exist.

- [ ] **Step 3: Implement the route and table component**

Add:

```rust
#[at("/org/:workspace_id/files")]
OrgWorkspaceFiles { workspace_id: String },
```

Map it to `OrgWorkspaceFilesPage` and title `Org files {workspace_id} | agent-note`. Add **Files** to
the active and archived workspace header. Declare the static `/files` route before the terminal
`/org/:workspace_id` route so route recognition cannot consume `files` as part of the workspace ID.

Render `OrgDocumentTable` with columns File, Revision, Status, and Actions. Use real `<button>`
controls with unique accessible names containing the path. Active rows expose Rename/Archive;
archived rows expose Rename/Restore. The component emits typed callbacks only and performs no I/O.

- [ ] **Step 4: Implement page state and four dialogs**

Load the workspace and one status-filtered document page. Preserve cursor history and manual refresh;
never poll. For active workspaces, mount `Modal` dialogs as follows:

- Add: one path field; create empty document via the dedicated POST.
- Rename: prefilled path field; submit exact loaded revision.
- Archive: require exact path confirmation; submit exact loaded revision.
- Restore: show preserved path/revision and submit exact loaded revision.

Store the complete pending body so transient Retry reuses the same operation UUID. On
`stale_revision`, keep the draft, disable Retry, and show **Refresh files**; refreshing clears the
stale pending body, loads the current revision, and requires a deliberate new submission/operation
ID. On success refresh the current filter and announce `Created`, `Renamed`, `Archived`, or
`Restored` in `aria-live="polite"`. Archived workspaces render no mutation controls.

Add namespaced `.org-files-*` styles: ledger table, status filters, 44px targets, visible focus,
mobile stacked rows/actions, danger confirmation, and no 390px overflow.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml routes
cargo test --manifest-path crates/note-frontend/Cargo.toml org_workspace_files
cargo test --manifest-path crates/note-frontend/Cargo.toml org_document_table
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: routing, state, actions, retry/conflict, source boundary, accessibility source assertions,
and Wasm compilation pass.

```bash
git add crates/note-frontend/app.css crates/note-frontend/src/components/mod.rs crates/note-frontend/src/components/org_document_table.rs crates/note-frontend/src/pages/mod.rs crates/note-frontend/src/pages/org_workspace.rs crates/note-frontend/src/pages/org_workspace_files.rs crates/note-frontend/src/routes.rs
git commit -m "feat(frontend): manage Org workspace files"
```

### Task 11: Update canonical docs, add browser acceptance, and run scoped completion gates

**Files:**
- Modify: `docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md`
- Modify: `docs/design.md`
- Modify: `README.md`
- Create: `scripts/verify-org-document-lifecycle-browser.sh`
- Modify: `scripts/verify-org-workspace-management-browser.sh`
- Modify: `scripts/verify-org-console-browser.sh`

- [ ] **Step 1: Write the browser gate before final acceptance**

Create a disposable fixture script using the established Chrome DevTools CLI helpers. It must:

1. create an active workspace fixture through REST;
2. open `/org/{workspace_id}/files` and verify focus/accessible names;
3. create `projects/roadmap.org` through the dialog;
4. GET the document and assert `source == ""`, revision 1, and active state;
5. rename it to `archive/roadmap.org`;
6. create an external revision bump and verify stale rename preserves the draft until Refresh;
7. archive with exact-path confirmation and verify the active filter omits it;
8. switch to Archived, rename it while archived, and restore it;
9. verify the same UUID/source returns to Active;
10. verify an archived workspace shows the files page without lifecycle controls;
11. capture desktop and 390px screenshots plus an accessibility snapshot;
12. assert no console errors and allow only GET, workspace lifecycle, and the exact four document
    lifecycle routes; reject full-source PUT, DELETE, `/mcp`, and workflow mutation traffic.

- [ ] **Step 2: Update the canonical documentation**

Change the master PRD and architecture document to state:

- browser raw Org source and workflow actions remain read-only;
- document create/rename/archive/restore are the only new document lifecycle controls;
- document archive is reversible and active-lease guarded;
- archived work remains historical/dependency-visible but operationally excluded;
- the route is `/org/:workspace_id/files`;
- REST and MCP expose exactly 40 matching Org operations, with 52 total MCP tools.

Update README with the operator route and no-delete/no-editor boundary. Do not modify the unrelated
untracked reliability plan, Emacs guide, or notes batch-actions plan named in the scope constraints.

- [ ] **Step 3: Run focused backend and transport gates**

```bash
cargo test -p note-org --test document_path_test
cargo test -p note-storage --test org_workflow_records_test
cargo test -p note-storage-turso --test schema_test --test contracts_test
cargo test -p note-storage-pg --test lifecycle_test --test contracts_test
cargo test -p note-pipelines --test org_workspace_document_test --test org_active_lease_guards_test --test org_operational_views_test
cargo test -p note-server --test org_workspace_document_api_test --test org_api_inventory_test --test org_transport_conformance_test --test org_offline_test
cargo test -p note-mcp --test org_inventory_test --test org_workspace_document_tools_test --test org_transports_test
```

Expected: every named suite passes. If PostgreSQL self-skips because `TEST_DATABASE_URL` is absent,
record it as unverified and do not report PostgreSQL as passing.

- [ ] **Step 4: Run frontend, browser, formatting, and diff gates**

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo fmt --all -- --check
cd crates/note-frontend && trunk build --release
scripts/verify-org-document-lifecycle-browser.sh
git diff --check
```

Expected: frontend tests, Wasm check, both formatting checks, Trunk release build, browser lifecycle,
console/network/a11y checks, and whitespace check pass. Use a disposable local service URL via
`ORG_CONSOLE_BASE_URL`; do not point the destructive fixture at production.

- [ ] **Step 5: Review scope and commit**

Confirm the diff contains only the files named by this plan, no raw source editor, no physical
delete, no unrelated fixes, and no changes to the three user-owned untracked documents.

```bash
git add README.md docs/design.md docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md scripts/verify-org-console-browser.sh scripts/verify-org-document-lifecycle-browser.sh scripts/verify-org-workspace-management-browser.sh
git commit -m "docs(org): document file lifecycle management"
```

After this commit, report the exact scoped test evidence, PostgreSQL verification/skip status,
browser artifacts, branch/worktree status, and commit list. Stop for an explicit merge, PR, keep, or
discard choice; do not integrate or push automatically.
