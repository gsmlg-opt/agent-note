# Org Canonical Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver Org Delivery Slice 2 by persisting canonical workspaces and byte-preserved Org documents, rebuildable projections, weak Markdown note links, a minimal event ledger, and idempotency records with true revision compare-and-swap in both Turso and PostgreSQL.

**Architecture:** Extend the existing driver-neutral `note-storage` contract and both existing adapters; do not create an Org-specific database crate. `note-org` gains only the persistence-facing IDs, cross-workspace policy flag, and pure workspace-time resolution needed to form projections. Callers compose document source, projections, events, and operation results inside the existing storage transaction boundary; workflow mutations, leases, queues, MCP, REST, offline commands, and Web UI remain later slices.

**Tech Stack:** Rust 2021, `async-trait`, `chrono` 0.4.45, `chrono-tz` 0.10, `serde`/`serde_json`, `uuid`, Turso 0.7, SQLx 0.9/PostgreSQL, shared async storage contract tests.

---

## Scope Boundary

This plan implements only PRD Delivery Slice 2:

- stable workspace and document identity;
- required IANA workspace timezone and stored policy;
- canonical byte-preserved Org source and content hashes;
- storage-level workspace and document revision CAS;
- work-item, tag, dependency, schedule, deadline, assignment, and weak note-link
  projections;
- minimal per-workspace event sequencing;
- persisted operation fingerprints and results;
- Turso schema v2-to-v3 migration and PostgreSQL ordered migration; and
- one shared contract suite proving backend parity and transaction rollback.

Do not add pipeline workflows, semantic state mutation services, complete audit
event vocabulary, attempts, progress, leases, fencing tokens, queue/agenda
queries, MCP tools, CLI commands, REST routes, OpenAPI operations, or frontend
pages. Slice 2 exposes the storage primitives that those later slices compose.
Raw reads plus document/workspace listing provide the persistence foundation for
export; end-user offline import/export commands remain Slice 5.

## Requirement Coverage

| Slice 2 requirement | Implemented by |
| --- | --- |
| Stable workspace/document IDs and IANA time | Task 1 |
| Driver-neutral records and session contract | Task 2 |
| Turso v2-to-v3 and PostgreSQL ordered migration | Task 3 |
| Canonical source, path uniqueness, workspace/document CAS | Task 4 |
| Rebuildable projections, dependencies, tags, schedules, weak links | Task 5 |
| Minimal ordered events, operation records, atomic rollback | Task 6 |
| Scoped regression gates and operator documentation | Task 7 |

## Execution Prerequisites

Use an isolated worktree under the repository-local `.trees` directory. If this
plan is executed after its documentation branch is merged, create the
implementation worktree from the updated `main`:

```sh
git worktree add \
  .trees/org-canonical-persistence \
  -b codex/org-canonical-persistence
cd .trees/org-canonical-persistence
```

Establish a clean scoped baseline:

```sh
cargo fmt --all -- --check
cargo test -p note-org
cargo test -p note-storage
cargo test -p note-storage-turso
cargo test -p note-storage-pg
git status --short --branch
```

Expected: the tests pass; PostgreSQL integration cases may report their existing
configured skip when no test database is available; the worktree is clean. If
an existing out-of-scope failure occurs, record it and stop rather than fixing
it inside this slice.

## File Map

### Pure Org persistence vocabulary

- Modify `crates/note-org/Cargo.toml`
  - Add `chrono-tz` for IANA timezone resolution.
- Modify `crates/note-org/src/lib.rs`
  - Export the new time module.
- Modify `crates/note-org/src/types.rs`
  - Add typed `WorkspaceId` and `DocumentId` UUID wrappers.
- Modify `crates/note-org/src/policy.rs`
  - Add `allow_cross_workspace_agenda` to persisted policy.
- Create `crates/note-org/src/time.rs`
  - Resolve parsed local Org timestamps against a workspace IANA timezone.
- Create `crates/note-org/tests/time_test.rs`
  - Cover valid, invalid, ambiguous, and nonexistent local times.

### Driver-neutral storage contract

- Modify `crates/note-storage/Cargo.toml`
  - Depend on `note-org` and `serde_json`.
- Modify `crates/note-storage/src/records.rs`
  - Define workspace, document, projection, event, operation, and CAS records.
- Modify `crates/note-storage/src/repositories.rs`
  - Add `OrgRepository` and include it in `StorageSession`.
- Modify `crates/note-pipelines/tests/support/events.rs`
  - Forward the new repository methods through the transaction-observing test
    wrapper so existing pipeline tests continue to compile and preserve
    behavior.
- Modify `crates/note-pipelines/Cargo.toml`
  - Add `note-org` as a dev-dependency for the transaction test wrapper.
- Modify `Cargo.lock`
  - Record the new direct dependencies.

### Shared backend contracts

- Create `crates/note-storage-contract-tests/src/org.rs`
  - Prove workspace/document CAS, byte preservation, projection replacement,
    weak links, ordered events, operation replay records, and atomic rollback.
- Modify `crates/note-storage-contract-tests/src/lib.rs`
  - Run the Org contract against every backend.
- Modify `crates/note-storage-contract-tests/Cargo.toml`
  - Add direct `note-org` and `serde_json` test dependencies.

### Turso adapter and migration

- Modify `crates/note-storage-turso/schema.sql`
  - Add the schema-v3 Org tables for fresh databases.
- Create `crates/note-storage-turso/migrations/0002_to_0003_org.sql`
  - Add exactly those tables to a schema-v2 database.
- Create `crates/note-storage-turso/tests/fixtures/schema-v2.sql`
  - Preserve the current schema-v2 DDL for migration tests.
- Modify `crates/note-storage-turso/src/preflight.rs`
  - Admit marked versions 2 and 3 while rejecting every other version.
- Modify `crates/note-storage-turso/src/connection.rs`
  - Migrate v2 to v3 under `BEGIN IMMEDIATE` and update `user_version` last.
- Modify `crates/note-storage-turso/src/lib.rs`
  - Register the Org repository module and run schema preparation on open.
- Create `crates/note-storage-turso/src/org.rs`
  - Implement the complete Org repository contract.
- Modify `crates/note-storage-turso/Cargo.toml`
  - Add the direct `note-org` dependency used when decoding typed IDs.
- Modify `crates/note-storage-turso/tests/schema_test.rs`
  - Prove fresh v3, successful v2 migration, rollback on migration failure, and
    rejection of unsupported versions.

### PostgreSQL adapter and migration

- Create `crates/note-storage-pg/migrations/0002_org_canonical.sql`
  - Add the Org tables using JSONB and ordered SQLx migration semantics.
- Modify `crates/note-storage-pg/src/lib.rs`
  - Register the Org repository module.
- Create `crates/note-storage-pg/src/org.rs`
  - Implement the complete Org repository contract.
- Modify `crates/note-storage-pg/Cargo.toml`
  - Add the direct `note-org` dependency used when decoding typed IDs.
- Modify `crates/note-storage-pg/tests/lifecycle_test.rs`
  - Prove the ordered migration is recorded and existing Markdown notes remain
    readable.

### Documentation

- Modify `README.md`
  - Mark Slice 2 as canonical database persistence and document schema v3
    compatibility without claiming workflow or transport availability.

## Public Contract to Implement

The storage record names and repository signatures below are authoritative for
this slice. Keep transport DTOs out of `note-storage`.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareAndSwap<T> {
    Applied(T),
    NotFound,
    Conflict { current_revision: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgWorkspace {
    pub id: note_org::WorkspaceId,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: note_org::WorkspacePolicy,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

pub struct NewOrgWorkspace<'a> {
    pub id: note_org::WorkspaceId,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: &'a str,
    pub timezone: &'a str,
    pub policy_schema_version: i64,
    pub policy: &'a note_org::WorkspacePolicy,
    pub now: i64,
}

pub struct OrgWorkspaceUpdate<'a> {
    pub id: note_org::WorkspaceId,
    pub expected_revision: i64,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: &'a str,
    pub timezone: &'a str,
    pub policy_schema_version: i64,
    pub policy: &'a note_org::WorkspacePolicy,
    pub archived_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDocument {
    pub id: note_org::DocumentId,
    pub workspace_id: note_org::WorkspaceId,
    pub path: String,
    pub source: String,
    pub content_hash: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewOrgDocument<'a> {
    pub id: note_org::DocumentId,
    pub workspace_id: note_org::WorkspaceId,
    pub path: &'a str,
    pub source: &'a str,
    pub content_hash: &'a str,
    pub now: i64,
}

pub struct OrgDocumentUpdate<'a> {
    pub id: note_org::DocumentId,
    pub expected_revision: i64,
    pub path: &'a str,
    pub source: &'a str,
    pub content_hash: &'a str,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOrgTimestamp {
    pub raw: String,
    pub local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgProjectedWorkItem {
    pub id: note_org::WorkItemId,
    pub workspace_id: note_org::WorkspaceId,
    pub document_id: note_org::DocumentId,
    pub parent_id: Option<note_org::WorkItemId>,
    pub source_order: i64,
    pub item_type: note_org::WorkItemType,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub scheduled: Option<StoredOrgTimestamp>,
    pub deadline: Option<StoredOrgTimestamp>,
    pub assignee: Option<String>,
    pub requires_review: bool,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub dependencies: Vec<note_org::WorkItemId>,
    pub note_links: Vec<note_org::NoteLink>,
}

pub struct NewOrgEvent<'a> {
    pub id: &'a str,
    pub workspace_id: note_org::WorkspaceId,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub actor_id: &'a str,
    pub event_type: &'a str,
    pub occurred_at: i64,
    pub summary: &'a str,
    pub metadata: &'a serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgEvent {
    pub id: String,
    pub workspace_id: note_org::WorkspaceId,
    pub sequence: i64,
    pub subject_kind: String,
    pub subject_id: String,
    pub actor_id: String,
    pub event_type: String,
    pub occurred_at: i64,
    pub summary: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOrgOperation {
    pub workspace_id: note_org::WorkspaceId,
    pub operation_id: String,
    pub request_fingerprint: String,
    pub result: serde_json::Value,
    pub created_at: i64,
}
```

`OrgRepository` must expose exactly these methods:

```rust
#[async_trait::async_trait]
pub trait OrgRepository: Send + Sync {
    async fn insert_org_workspace(&self, value: NewOrgWorkspace<'_>) -> StorageResult<()>;
    async fn get_org_workspace(
        &self,
        id: note_org::WorkspaceId,
    ) -> StorageResult<Option<OrgWorkspace>>;
    async fn get_org_workspace_by_slug(&self, slug: &str)
        -> StorageResult<Option<OrgWorkspace>>;
    async fn list_org_workspaces(&self, include_archived: bool)
        -> StorageResult<Vec<OrgWorkspace>>;
    async fn compare_and_swap_org_workspace(
        &self,
        update: OrgWorkspaceUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgWorkspace>>;

    async fn insert_org_document(&self, value: NewOrgDocument<'_>) -> StorageResult<()>;
    async fn get_org_document(
        &self,
        id: note_org::DocumentId,
    ) -> StorageResult<Option<OrgDocument>>;
    async fn list_org_documents(
        &self,
        workspace_id: note_org::WorkspaceId,
    ) -> StorageResult<Vec<OrgDocument>>;
    async fn compare_and_swap_org_document(
        &self,
        update: OrgDocumentUpdate<'_>,
    ) -> StorageResult<CompareAndSwap<OrgDocument>>;

    /// Must be called inside a storage transaction with the document write.
    async fn replace_org_document_projection(
        &self,
        document_id: note_org::DocumentId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()>;
    async fn list_org_document_projection(
        &self,
        document_id: note_org::DocumentId,
    ) -> StorageResult<Vec<OrgProjectedWorkItem>>;
    /// Rebuilds every projection row for a workspace from all canonical
    /// documents; events and operation records are not modified.
    async fn rebuild_org_workspace_projection(
        &self,
        workspace_id: note_org::WorkspaceId,
        items: &[OrgProjectedWorkItem],
    ) -> StorageResult<()>;

    /// Must be called inside the same transaction as the mutation it records.
    async fn append_org_event(&self, event: NewOrgEvent<'_>) -> StorageResult<OrgEvent>;
    async fn list_org_events(
        &self,
        workspace_id: note_org::WorkspaceId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> StorageResult<Vec<OrgEvent>>;

    /// Must be called inside the same transaction as the mutation result.
    async fn insert_org_operation(&self, operation: &StoredOrgOperation)
        -> StorageResult<()>;
    async fn get_org_operation(
        &self,
        workspace_id: note_org::WorkspaceId,
        operation_id: &str,
    ) -> StorageResult<Option<StoredOrgOperation>>;
}
```

Add `OrgRepository` to both the `StorageSession` supertraits and blanket
implementation. Default trait methods return `StorageErrorKind::UnsupportedSchema`
with message `Org persistence is not implemented by this storage session`; the
two production sessions override every method, and the shared contract prevents
an accidental default from shipping.

### Task 1: Add Stable Persistence IDs and Workspace Time Resolution

**Files:**

- Modify: `crates/note-org/Cargo.toml`
- Modify: `crates/note-org/src/lib.rs`
- Modify: `crates/note-org/src/types.rs`
- Modify: `crates/note-org/src/policy.rs`
- Create: `crates/note-org/src/time.rs`
- Create: `crates/note-org/tests/time_test.rs`

- [ ] **Step 1: Write the failing ID, policy, and timezone tests**

Create `crates/note-org/tests/time_test.rs`:

```rust
use chrono::NaiveDate;
use note_org::{
    resolve_org_timestamp, DocumentId, OrgTimestamp, TimeZoneError, WorkspaceId,
    WorkspacePolicy,
};
use std::str::FromStr;

fn timestamp(date: (i32, u32, u32), time: (u32, u32, u32)) -> OrgTimestamp {
    OrgTimestamp {
        raw: format!("<{:04}-{:02}-{:02} {:02}:{:02}>", date.0, date.1, date.2, time.0, time.1),
        local: NaiveDate::from_ymd_opt(date.0, date.1, date.2)
            .unwrap()
            .and_hms_opt(time.0, time.1, time.2)
            .unwrap(),
    }
}

#[test]
fn persistence_ids_roundtrip_as_uuid_strings() {
    let workspace = WorkspaceId::from_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let document = DocumentId::from_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    assert_eq!(workspace.to_string(), "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    assert_eq!(document.to_string(), "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
}

#[test]
fn default_policy_disallows_cross_workspace_agendas() {
    assert!(!WorkspacePolicy::engineering_default().allow_cross_workspace_agenda);
}

#[test]
fn valid_workspace_time_resolves_to_utc() {
    let resolved = resolve_org_timestamp(
        &timestamp((2026, 8, 4), (15, 30, 0)),
        "Asia/Shanghai",
    )
    .unwrap();
    assert_eq!(resolved.timezone, "Asia/Shanghai");
    assert_eq!(resolved.utc_timestamp, 1_785_828_600);
}

#[test]
fn invalid_and_daylight_saving_times_are_rejected() {
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 8, 4), (15, 30, 0)), "Mars/Olympus"),
        Err(TimeZoneError::InvalidZone(_))
    ));
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 11, 1), (1, 30, 0)), "America/New_York"),
        Err(TimeZoneError::Ambiguous { .. })
    ));
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 3, 8), (2, 30, 0)), "America/New_York"),
        Err(TimeZoneError::Nonexistent { .. })
    ));
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```sh
cargo test -p note-org --test time_test
```

Expected: compilation fails because the IDs, policy field, and resolver do not
exist.

- [ ] **Step 3: Add the IDs and policy flag**

Add `WorkspaceId` and `DocumentId` to `types.rs` with the same derives,
transparent serde representation, trimmed `FromStr`, and `Display`
implementation as `WorkItemId`. Add this field to `WorkspacePolicy`:

```rust
#[serde(default)]
pub allow_cross_workspace_agenda: bool,
```

Set it to `false` in `WorkspacePolicy::engineering_default()`.

- [ ] **Step 4: Implement strict IANA resolution**

Add `chrono-tz = "0.10"` to `note-org`. Create `src/time.rs`:

```rust
use crate::OrgTimestamp;
use chrono::{LocalResult, TimeZone};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOrgTimestamp {
    pub raw: String,
    pub local: chrono::NaiveDateTime,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeZoneError {
    #[error("invalid IANA timezone {0}")]
    InvalidZone(String),
    #[error("local time {local} is ambiguous in {timezone}")]
    Ambiguous { local: String, timezone: String },
    #[error("local time {local} does not exist in {timezone}")]
    Nonexistent { local: String, timezone: String },
}

pub fn resolve_org_timestamp(
    value: &OrgTimestamp,
    timezone: &str,
) -> Result<ResolvedOrgTimestamp, TimeZoneError> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| TimeZoneError::InvalidZone(timezone.to_string()))?;
    let zoned = match zone.from_local_datetime(&value.local) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(_, _) => {
            return Err(TimeZoneError::Ambiguous {
                local: value.local.to_string(),
                timezone: timezone.to_string(),
            })
        }
        LocalResult::None => {
            return Err(TimeZoneError::Nonexistent {
                local: value.local.to_string(),
                timezone: timezone.to_string(),
            })
        }
    };
    Ok(ResolvedOrgTimestamp {
        raw: value.raw.clone(),
        local: value.local,
        timezone: timezone.to_string(),
        utc_timestamp: zoned.timestamp(),
    })
}
```

Export `time` from `lib.rs`.

- [ ] **Step 5: Run the Org crate tests and verify GREEN**

Run:

```sh
cargo test -p note-org
cargo fmt --all -- --check
```

Expected: all `note-org` tests pass and formatting is clean.

- [ ] **Step 6: Commit**

```sh
git add Cargo.lock crates/note-org
git commit -m "feat(org): add persistence identities and workspace time"
```

### Task 2: Define the Driver-Neutral Org Storage Contract

**Files:**

- Modify: `crates/note-storage/Cargo.toml`
- Modify: `crates/note-storage-contract-tests/Cargo.toml`
- Modify: `crates/note-storage-turso/Cargo.toml`
- Modify: `crates/note-storage-pg/Cargo.toml`
- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Create: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-turso/src/lib.rs`
- Create: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-storage-pg/src/lib.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`
- Modify: `crates/note-storage/tests/contracts_test.rs`

- [ ] **Step 1: Write the compile-time contract test**

Extend `crates/note-storage/tests/contracts_test.rs`:

```rust
use note_storage::{OrgRepository, StorageSession};

fn accepts_org_repository(_: &dyn OrgRepository) {}
fn accepts_org_storage_session(_: Box<dyn StorageSession>) {}

#[test]
fn storage_sessions_include_org_persistence() {
    let _ = accepts_org_repository;
    let _ = accepts_org_storage_session;
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```sh
cargo test -p note-storage --test contracts_test
```

Expected: compilation fails because `OrgRepository` does not exist.

- [ ] **Step 3: Add the records and repository interface**

Add `note-org` and `serde_json` to `note-storage/Cargo.toml`. Add `note-org`
to both adapter manifests, and add `note-org` plus `serde_json` to the shared
contract-test manifest. Add every
record and every method from **Public Contract to Implement** to `records.rs`
and `repositories.rs`. Add `OrgRepository` to `StorageSession` and its blanket
implementation.

Add `note-org` to the `note-pipelines` dev-dependencies so its test-only
transaction forwarder can name `WorkspaceId` and `DocumentId` directly.

For each default method, return this exact error:

```rust
Err(StorageError::new(
    StorageErrorKind::UnsupportedSchema,
    "Org persistence is not implemented by this storage session",
))
```

- [ ] **Step 4: Register production implementations and the test forwarder**

Create `org.rs` in each adapter with a temporary empty override:

```rust
#[async_trait::async_trait]
impl note_storage::OrgRepository for crate::TursoSession {}
```

and:

```rust
#[async_trait::async_trait]
impl note_storage::OrgRepository for crate::PgSession {}
```

Register each module from its `lib.rs`. In
`note-pipelines/tests/support/events.rs`, import the Org record types and add
this forwarding block so wrapped transactions delegate rather than return the
default error:

```rust
impl_forward_repository! {
    OrgRepository {
        fn insert_org_workspace(value: NewOrgWorkspace<'_>) -> ();
        fn get_org_workspace(id: note_org::WorkspaceId) -> Option<OrgWorkspace>;
        fn get_org_workspace_by_slug(slug: &str) -> Option<OrgWorkspace>;
        fn list_org_workspaces(include_archived: bool) -> Vec<OrgWorkspace>;
        fn compare_and_swap_org_workspace(
            update: OrgWorkspaceUpdate<'_>,
        ) -> CompareAndSwap<OrgWorkspace>;
        fn insert_org_document(value: NewOrgDocument<'_>) -> ();
        fn get_org_document(id: note_org::DocumentId) -> Option<OrgDocument>;
        fn list_org_documents(
            workspace_id: note_org::WorkspaceId,
        ) -> Vec<OrgDocument>;
        fn compare_and_swap_org_document(
            update: OrgDocumentUpdate<'_>,
        ) -> CompareAndSwap<OrgDocument>;
        fn replace_org_document_projection(
            document_id: note_org::DocumentId,
            items: &[OrgProjectedWorkItem],
        ) -> ();
        fn list_org_document_projection(
            document_id: note_org::DocumentId,
        ) -> Vec<OrgProjectedWorkItem>;
        fn rebuild_org_workspace_projection(
            workspace_id: note_org::WorkspaceId,
            items: &[OrgProjectedWorkItem],
        ) -> ();
        fn append_org_event(event: NewOrgEvent<'_>) -> OrgEvent;
        fn list_org_events(
            workspace_id: note_org::WorkspaceId,
            after_sequence: Option<i64>,
            limit: usize,
        ) -> Vec<OrgEvent>;
        fn insert_org_operation(operation: &StoredOrgOperation) -> ();
        fn get_org_operation(
            workspace_id: note_org::WorkspaceId,
            operation_id: &str,
        ) -> Option<StoredOrgOperation>;
    }
}
```

- [ ] **Step 5: Run compile and existing transaction checks**

Run:

```sh
cargo test -p note-storage --test contracts_test
cargo test -p note-pipelines --test attachment_management_test
cargo check -p note-storage-turso --all-targets
cargo check -p note-storage-pg --all-targets
```

Expected: all commands pass; no existing storage session loses a supertrait.

- [ ] **Step 6: Commit**

```sh
git add Cargo.lock crates/note-storage crates/note-storage-contract-tests/Cargo.toml \
  crates/note-storage-turso/Cargo.toml crates/note-storage-turso/src \
  crates/note-storage-pg/Cargo.toml crates/note-storage-pg/src \
  crates/note-pipelines/Cargo.toml \
  crates/note-pipelines/tests/support/events.rs
git commit -m "feat(storage): define Org persistence contracts"
```

### Task 3: Add Schema Version 3 and Ordered Migrations

**Files:**

- Modify: `crates/note-storage-turso/schema.sql`
- Create: `crates/note-storage-turso/migrations/0002_to_0003_org.sql`
- Create: `crates/note-storage-turso/tests/fixtures/schema-v2.sql`
- Modify: `crates/note-storage-turso/src/preflight.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Modify: `crates/note-storage-turso/src/lib.rs`
- Modify: `crates/note-storage-turso/tests/schema_test.rs`
- Create: `crates/note-storage-pg/migrations/0002_org_canonical.sql`
- Modify: `crates/note-storage-pg/tests/lifecycle_test.rs`

- [ ] **Step 1: Freeze the v2 fixture and write failing migration tests**

Copy the pre-change contents of `note-storage-turso/schema.sql` into
`tests/fixtures/schema-v2.sql`. In `schema_test.rs`, create a marked v2 database
from that fixture, insert note `migration-survivor`, reopen through
`TursoStorage`, and assert:

```rust
assert_eq!(user_version(&path).await, 3);
assert_eq!(
    reopened
        .connect()
        .await
        .unwrap()
        .get_note_content("migration-survivor")
        .await
        .unwrap()
        .as_deref(),
    Some("preserved")
);
for table in [
    "org_workspaces",
    "org_documents",
    "org_work_items",
    "org_work_item_tags",
    "org_dependencies",
    "org_note_links",
    "org_events",
    "org_operations",
] {
    assert!(table_exists(&path, table).await, "missing {table}");
}
```

Change the existing future-version test to version 4 and retain the v1
rejection test.

- [ ] **Step 2: Run the Turso migration tests and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test schema_test
```

Expected: the v2 migration case fails with `UnsupportedSchema`.

- [ ] **Step 3: Add the canonical Org DDL**

Add these tables and indexes to both the fresh Turso schema and
`0002_to_0003_org.sql`; the migration file omits `IF NOT EXISTS` so a partial or
incorrect schema cannot be silently accepted:

```sql
CREATE TABLE org_workspaces (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    description TEXT NOT NULL,
    timezone TEXT NOT NULL,
    policy_schema_version INTEGER NOT NULL CHECK (policy_schema_version = 1),
    policy TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    last_event_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_event_sequence >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER
);

CREATE TABLE org_documents (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    path TEXT NOT NULL,
    source TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (workspace_id, path)
);

CREATE TABLE org_work_items (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    document_id TEXT NOT NULL REFERENCES org_documents(id) ON DELETE RESTRICT,
    parent_id TEXT REFERENCES org_work_items(id) ON DELETE RESTRICT,
    source_order INTEGER NOT NULL CHECK (source_order >= 0),
    item_type TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT,
    priority TEXT CHECK (priority IS NULL OR length(priority) = 1),
    scheduled_raw TEXT,
    scheduled_local TEXT,
    scheduled_timezone TEXT,
    scheduled_utc INTEGER,
    deadline_raw TEXT,
    deadline_local TEXT,
    deadline_timezone TEXT,
    deadline_utc INTEGER,
    assignee TEXT,
    requires_review INTEGER NOT NULL CHECK (requires_review IN (0, 1)),
    created_at INTEGER NOT NULL,
    UNIQUE (document_id, source_order)
);

CREATE INDEX idx_org_work_items_workspace_document
    ON org_work_items(workspace_id, document_id, source_order);

CREATE TABLE org_work_item_tags (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    PRIMARY KEY (work_item_id, tag)
);

CREATE TABLE org_dependencies (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    depends_on_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE RESTRICT,
    PRIMARY KEY (work_item_id, depends_on_id),
    CHECK (work_item_id <> depends_on_id)
);

CREATE TABLE org_note_links (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    purpose TEXT NOT NULL,
    note_id TEXT NOT NULL,
    description TEXT NOT NULL,
    PRIMARY KEY (work_item_id, ordinal)
);

CREATE INDEX idx_org_note_links_note_id ON org_note_links(note_id, work_item_id);

CREATE TABLE org_events (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    summary TEXT NOT NULL,
    metadata TEXT NOT NULL,
    UNIQUE (workspace_id, sequence)
);

CREATE INDEX idx_org_events_subject
    ON org_events(workspace_id, subject_kind, subject_id, sequence);

CREATE TABLE org_operations (
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    operation_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
```

Create `0002_org_canonical.sql` with the explicit PostgreSQL representation:

```sql
CREATE TABLE org_workspaces (
    id text PRIMARY KEY,
    slug text NOT NULL UNIQUE,
    display_name text NOT NULL,
    description text NOT NULL,
    timezone text NOT NULL,
    policy_schema_version bigint NOT NULL CHECK (policy_schema_version = 1),
    policy jsonb NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    last_event_sequence bigint NOT NULL DEFAULT 0 CHECK (last_event_sequence >= 0),
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    archived_at bigint
);

CREATE TABLE org_documents (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    path text NOT NULL,
    source text NOT NULL,
    content_hash text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    UNIQUE (workspace_id, path)
);

CREATE TABLE org_work_items (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    document_id text NOT NULL REFERENCES org_documents(id) ON DELETE RESTRICT,
    parent_id text REFERENCES org_work_items(id) ON DELETE RESTRICT,
    source_order bigint NOT NULL CHECK (source_order >= 0),
    item_type text NOT NULL,
    title text NOT NULL,
    state text,
    priority char(1),
    scheduled_raw text,
    scheduled_local text,
    scheduled_timezone text,
    scheduled_utc bigint,
    deadline_raw text,
    deadline_local text,
    deadline_timezone text,
    deadline_utc bigint,
    assignee text,
    requires_review boolean NOT NULL,
    created_at bigint NOT NULL,
    UNIQUE (document_id, source_order)
);

CREATE INDEX idx_org_work_items_workspace_document
    ON org_work_items(workspace_id, document_id, source_order);

CREATE TABLE org_work_item_tags (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    tag text NOT NULL,
    PRIMARY KEY (work_item_id, tag)
);

CREATE TABLE org_dependencies (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    depends_on_id text NOT NULL REFERENCES org_work_items(id) ON DELETE RESTRICT,
    PRIMARY KEY (work_item_id, depends_on_id),
    CHECK (work_item_id <> depends_on_id)
);

CREATE TABLE org_note_links (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    purpose text NOT NULL,
    note_id text NOT NULL,
    description text NOT NULL,
    PRIMARY KEY (work_item_id, ordinal)
);

CREATE INDEX idx_org_note_links_note_id ON org_note_links(note_id, work_item_id);

CREATE TABLE org_events (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    sequence bigint NOT NULL CHECK (sequence >= 1),
    subject_kind text NOT NULL,
    subject_id text NOT NULL,
    actor_id text NOT NULL,
    event_type text NOT NULL,
    occurred_at bigint NOT NULL,
    summary text NOT NULL,
    metadata jsonb NOT NULL,
    UNIQUE (workspace_id, sequence)
);

CREATE INDEX idx_org_events_subject
    ON org_events(workspace_id, subject_kind, subject_id, sequence);

CREATE TABLE org_operations (
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    operation_id text NOT NULL,
    request_fingerprint text NOT NULL,
    result jsonb NOT NULL,
    created_at bigint NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
```

Keep note links weak: do not add a foreign key from `org_note_links.note_id` to
`notes`.

- [ ] **Step 4: Implement atomic Turso v2-to-v3 preparation**

Set `SCHEMA_VERSION` to 3 and add `PREVIOUS_SCHEMA_VERSION: u32 = 2`.
Represent marked files as `Preflight::Existing { version }` and accept only
versions 2 and 3 in both header and opened-state checks. Refactor initialization
to execute this state machine under `BEGIN IMMEDIATE`:

```rust
match opened_state {
    OpenedState::Empty => {
        connection.execute_batch(SCHEMA).await?;
        connection.execute("PRAGMA application_id = 1095651156", ()).await?;
        connection.execute("PRAGMA user_version = 3", ()).await?;
    }
    OpenedState::Existing { version: 2 } => {
        connection.execute_batch(MIGRATION_2_TO_3).await?;
        connection.execute("PRAGMA user_version = 3", ()).await?;
    }
    OpenedState::Existing { version: 3 } => {}
    OpenedState::Existing { version } => return Err(unsupported_schema(version)),
}
connection.execute("COMMIT", ()).await?;
```

On any error, execute `ROLLBACK`; set `user_version` only after every DDL
statement succeeds; then flush and checkpoint exactly as the existing fresh
initialization path does.

- [ ] **Step 5: Prove migration rollback with an injected bad tail**

Extract the transaction body into a private helper accepting the migration SQL.
Add a unit test in `connection.rs` that passes:

```sql
CREATE TABLE migration_probe (id INTEGER PRIMARY KEY);
INSERT INTO missing_migration_table(id) VALUES (1);
```

After the expected error, assert `PRAGMA user_version = 2` and
`migration_probe` does not exist. This proves both DDL and version rollback.

- [ ] **Step 6: Prove the PostgreSQL ordered migration**

In `lifecycle_test.rs`, provision PostgreSQL, connect `PgStorage`, insert a
Markdown note, reconnect, and query `_sqlx_migrations`:

```rust
let versions: Vec<i64> = sqlx::query_scalar(
    "SELECT version FROM _sqlx_migrations ORDER BY version",
)
.fetch_all(&inspection_pool)
.await
.unwrap();
assert_eq!(versions, vec![1, 2]);
assert_eq!(
    storage
        .connect_session()
        .await
        .unwrap()
        .get_note_content("migration-survivor")
        .await
        .unwrap()
        .as_deref(),
    Some("preserved")
);
```

- [ ] **Step 7: Run migration tests and verify GREEN**

Run:

```sh
cargo test -p note-storage-turso --test schema_test
cargo test -p note-storage-turso connection::tests
cargo test -p note-storage-pg --test lifecycle_test
```

Expected: Turso v2 migrates atomically to v3, injected failure remains v2,
versions 1 and 4 remain rejected, PostgreSQL records migrations 1 and 2, and
existing notes remain readable. PostgreSQL may use its established configured
skip when no test server is present.

- [ ] **Step 8: Commit**

```sh
git add crates/note-storage-turso crates/note-storage-pg
git commit -m "feat(storage): migrate databases for Org persistence"
```

### Task 4: Persist Workspaces and Canonical Documents with True CAS

**Files:**

- Create: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/lib.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`

- [ ] **Step 1: Write the shared workspace/document contract**

Add `org::run(storage.clone()).await` to `run_storage_contracts`. The first
contract creates two workspaces and a document containing CRLF, Unicode, an
opaque source block, and no final newline. Assert exact source equality,
workspace slug ordering, active/archive filtering, and document path ordering.

Exercise CAS with these exact assertions:

```rust
assert!(matches!(
    session.compare_and_swap_org_document(stale_update).await.unwrap(),
    CompareAndSwap::Conflict { current_revision: 2 }
));
assert_eq!(
    session.get_org_document(document_id).await.unwrap().unwrap().source,
    revision_two_source
);
assert!(matches!(
    session.compare_and_swap_org_document(missing_update).await.unwrap(),
    CompareAndSwap::NotFound
));
```

Repeat the same applied/conflict/not-found sequence for workspace revision.
Assert duplicate workspace slug and duplicate `(workspace_id, path)` produce
`StorageErrorKind::Constraint`.

- [ ] **Step 2: Run the Turso contract and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
```

Expected: failure with the temporary `UnsupportedSchema` default.

- [ ] **Step 3: Implement Turso workspace/document storage**

Replace the default methods in Turso `org.rs`. Serialize policy with
`serde_json::to_string`. For document CAS, use the revision in the predicate and
increment only in SQL:

```sql
UPDATE org_documents
SET path = ?2,
    source = ?3,
    content_hash = ?4,
    updated_at = ?5,
    revision = revision + 1
WHERE id = ?1 AND revision = ?6
RETURNING id, workspace_id, path, source, content_hash,
          revision, created_at, updated_at
```

If `RETURNING` yields no row, query the current revision by ID and return
`NotFound` or `Conflict`. Workspace CAS uses:

```sql
UPDATE org_workspaces
SET slug = ?2,
    display_name = ?3,
    description = ?4,
    timezone = ?5,
    policy_schema_version = ?6,
    policy = ?7,
    archived_at = ?8,
    updated_at = ?9,
    revision = revision + 1
WHERE id = ?1 AND revision = ?10
RETURNING id, slug, display_name, description, timezone,
          policy_schema_version, policy, revision,
          created_at, updated_at, archived_at
```

Decode typed
IDs with `FromStr`, policy JSON with `serde_json::from_str`, and map malformed
stored values to `StorageErrorKind::Corrupt`.

- [ ] **Step 4: Run the Turso contract and verify GREEN**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
```

Expected: the new workspace/document contract and all prior contracts pass.

- [ ] **Step 5: Run the PostgreSQL contract and verify RED**

Run:

```sh
cargo test -p note-storage-pg --test contracts_test
```

Expected when PostgreSQL is configured: failure with `UnsupportedSchema`.

- [ ] **Step 6: Implement PostgreSQL workspace/document storage**

Use JSONB policy binding through `serde_json::to_value`. Use the same CAS
predicate and `RETURNING` columns with `$1` through `$10` parameters, preserving
the exact column order shown in the Turso statements. When no row is returned,
query the current revision on the same `PgSession` connection and return the
same `CompareAndSwap` variant as Turso. Order workspace lists by `(slug, id)`
and document lists by `(path, id)` in both adapters.

- [ ] **Step 7: Run both contracts and verify parity**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: both adapters pass the same assertions; PostgreSQL may use the
existing configured skip.

- [ ] **Step 8: Commit**

```sh
git add crates/note-storage-contract-tests crates/note-storage-turso/src/org.rs \
  crates/note-storage-pg/src/org.rs
git commit -m "feat(storage): persist Org workspaces and documents"
```

### Task 5: Persist Rebuildable Work-Item Projections and Weak Note Links

**Files:**

- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`

- [ ] **Step 1: Add the failing projection replacement contract**

Inside one immediate transaction, replace a two-item document projection with:

- parent item source order 0;
- child item source order 1 and parent ID;
- sorted tags `backend`, `release`;
- a finish-to-start dependency on the parent;
- a scheduled timestamp resolved in `Asia/Shanghai`;
- one available-looking and one unavailable-looking note UUID, both stored as
  weak links without a note foreign key.

Commit and assert full structural equality from
`list_org_document_projection`. Replace the same projection with changed title,
tags, dependency set, and link descriptions; assert old owned relations are
gone and incoming stable IDs remain. Then attempt a duplicate source order and
assert `StorageErrorKind::Constraint` plus transaction rollback.

Call `rebuild_org_workspace_projection` with the projections formed from every
stored document. Assert a deliberately omitted stale projection row disappears
and the supplied projection is restored exactly.

- [ ] **Step 2: Run the Turso contract and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
```

Expected: projection replacement returns `UnsupportedSchema`.

- [ ] **Step 3: Implement Turso projection replacement**

Require every supplied item to match the target document ID and its stored
workspace ID, require every parent and dependency target to resolve inside that
workspace, and reject mismatches with `StorageErrorKind::Constraint`. Upsert items
in `(level implied by parent, source_order, id)` parent-before-child order. For
the IDs owned by the target document, delete and reinsert only their tags,
outgoing dependency rows, and note links. Do not delete work-item rows omitted
from the candidate; omission validation belongs to the later import pipeline.

Store item types using their snake-case serde names, priorities as one-character
text, local timestamps as `%Y-%m-%dT%H:%M:%S`, and booleans as `0` or `1`.
Serialize links in their input ordinal order. List projections ordered by
`(source_order, id)` and relations by tag, dependency ID, or ordinal.

For `rebuild_org_workspace_projection`, validate that every supplied item and
document belongs to the requested workspace, then perform these statements in
the caller's transaction before using the same insertion helper:

```sql
DELETE FROM org_dependencies
WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1)
   OR depends_on_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1);
DELETE FROM org_work_item_tags
WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1);
DELETE FROM org_note_links
WHERE work_item_id IN (SELECT id FROM org_work_items WHERE workspace_id = ?1);
UPDATE org_work_items SET parent_id = NULL WHERE workspace_id = ?1;
DELETE FROM org_work_items WHERE workspace_id = ?1;
```

Do not delete from `org_events`, `org_operations`, `org_documents`, or
`org_workspaces`.

- [ ] **Step 4: Run the Turso contract and verify GREEN**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
```

Expected: projection, replacement, weak-link, constraint, and rollback cases
pass.

- [ ] **Step 5: Implement the same PostgreSQL behavior**

Use one acquired `PgSession` connection for the whole method. Use SQLx
`QueryBuilder<Postgres>` for bounded bulk inserts, but preserve the same sort
orders and validation as Turso. Use `ON CONFLICT (id) DO UPDATE` for work items,
then delete/reinsert owned tags, outgoing dependencies, and note links.
Implement workspace rebuild with the same dependency/tag/link/parent/item
deletion order and the same prohibition on touching canonical or runtime rows.

- [ ] **Step 6: Run backend parity checks**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: identical projection records and error kinds from both adapters.

- [ ] **Step 7: Commit**

```sh
git add crates/note-storage-contract-tests/src/org.rs \
  crates/note-storage-turso/src/org.rs crates/note-storage-pg/src/org.rs
git commit -m "feat(storage): persist Org work item projections"
```

### Task 6: Commit Events and Idempotency Records Atomically

**Files:**

- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/transactions.rs`

- [ ] **Step 1: Add failing event and operation contracts**

Append two events in one workspace and one in another. Assert sequences are
`[1, 2]` and `[1]`, timestamps do not affect ordering, `after_sequence` is
exclusive, and `limit` is honored. Insert an operation record containing a
nested JSON result and assert exact retrieval.

Attempt another operation with the same `(workspace_id, operation_id)` and
assert `StorageErrorKind::Constraint`; fingerprint comparison and replay
behavior remain Slice 3 pipeline logic.

Call `rebuild_org_workspace_projection` after those records exist and assert
the event sequence, event JSON, and operation result are byte-for-byte
unchanged. This proves administrative projection recovery cannot erase
authoritative runtime data.

- [ ] **Step 2: Add the atomic rollback contract**

Start an immediate transaction and perform, in order:

1. successful document CAS from revision 2 to 3;
2. projection replacement;
3. append event sequence 3;
4. insert an operation result; and
5. insert a second operation with the same primary key to force a constraint.

Roll back and assert the document is still revision 2, the prior projection is
unchanged, the workspace's next successful event receives sequence 3, and no
operation row exists. This proves source, projections, sequence allocation,
event, and operation result share one transaction boundary.

- [ ] **Step 3: Run Turso contracts and verify RED**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
```

Expected: event or operation calls return `UnsupportedSchema`.

- [ ] **Step 4: Implement Turso sequence allocation and records**

`append_org_event` must update and read the workspace counter before inserting:

```sql
UPDATE org_workspaces
SET last_event_sequence = last_event_sequence + 1
WHERE id = ?1
RETURNING last_event_sequence
```

If no workspace row is returned, return `StorageErrorKind::Constraint`. Insert
the event with that sequence and serialized metadata. `list_org_events` uses:

```sql
WHERE workspace_id = ?1 AND sequence > ?2
ORDER BY sequence
LIMIT ?3
```

Normalize `after_sequence` to zero and reject limits outside `1..=200` with
`StorageErrorKind::Operation`. Store operation result JSON as text and use the
composite primary key unchanged.

- [ ] **Step 5: Implement PostgreSQL records with one-statement allocation**

Use this CTE so sequence allocation and insert are atomic even before the outer
transaction commits:

```sql
WITH next_sequence AS (
    UPDATE org_workspaces
    SET last_event_sequence = last_event_sequence + 1
    WHERE id = $1
    RETURNING last_event_sequence
)
INSERT INTO org_events (
    id, workspace_id, sequence, subject_kind, subject_id,
    actor_id, event_type, occurred_at, summary, metadata
)
SELECT $2, $1, last_event_sequence, $3, $4, $5, $6, $7, $8, $9
FROM next_sequence
RETURNING id, workspace_id, sequence, subject_kind, subject_id,
          actor_id, event_type, occurred_at, summary, metadata
```

If no row is returned, map it to `StorageErrorKind::Constraint`. Bind metadata
and operation results as JSONB. Enforce the same `1..=200` limit and ordering as
Turso.

- [ ] **Step 6: Run both shared contracts and transaction suites**

Run:

```sh
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test
```

Expected: event sequences, JSON round trips, duplicate-operation constraints,
and forced rollback pass identically.

- [ ] **Step 7: Commit**

```sh
git add crates/note-storage-contract-tests crates/note-storage-turso/src/org.rs \
  crates/note-storage-pg/src/org.rs
git commit -m "feat(storage): add Org event and operation records"
```

### Task 7: Document the Slice and Run Scoped Completion Gates

**Files:**

- Modify: `README.md`

- [ ] **Step 1: Update architecture and compatibility documentation**

Document these exact facts without advertising later slices:

- `note-org` supplies pure Org domain and workspace-time behavior;
- schema v3 stores canonical Org workspaces/documents and derived projections;
- Turso upgrades marked v2 databases atomically to v3;
- PostgreSQL uses ordered migration `0002`;
- Markdown notes remain unchanged;
- no Org pipeline service, leases, MCP, REST, CLI, or Web UI is available in
  Slice 2; and
- directly deleting projections is recoverable, while events and operation
  records are authoritative runtime data.

- [ ] **Step 2: Run formatting and scoped tests**

Run:

```sh
cargo fmt --all -- --check
cargo test -p note-org
cargo test -p note-storage
cargo test -p note-storage-contract-tests
cargo test -p note-storage-turso
cargo test -p note-storage-pg
cargo check --workspace --all-targets
git diff --check
```

Expected: all in-scope tests and workspace compilation pass. PostgreSQL tests
may use their existing configured skip only when the test database is absent.
Do not repair an unrelated out-of-scope failure; report it and stop.

- [ ] **Step 3: Verify the migration and contract inventory explicitly**

Run:

```sh
test "$(rg -o 'org_[a-z_]+' crates/note-storage-turso/schema.sql | sort -u | wc -l | tr -d ' ')" -ge 8
test -f crates/note-storage-turso/migrations/0002_to_0003_org.sql
test -f crates/note-storage-pg/migrations/0002_org_canonical.sql
rg -n "run_storage_contracts|org::run" crates/note-storage-contract-tests/src/lib.rs
rg -n "SCHEMA_VERSION: u32 = 3" crates/note-storage-turso/src/preflight.rs
git status --short
```

Expected: both migrations exist, the shared Org contract is registered, schema
version is 3, and only the intended README change remains uncommitted.

- [ ] **Step 4: Commit documentation**

```sh
git add README.md
git commit -m "docs(storage): describe Org canonical persistence"
```

- [ ] **Step 5: Verify the final slice history and cleanliness**

Run:

```sh
git status --short --branch
git log --oneline --decorate -7
```

Expected: the implementation worktree is clean and the Slice 2 commits are
present in task order. Stop here; do not begin Slice 3 in this plan.
