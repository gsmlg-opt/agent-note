# Notes Batch Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add atomic, exact-ID batch label and soft-delete actions to the rendered `/notes` rows, plus native content sizing for the shared note Markdown textarea.

**Architecture:** Add one exact-ID row-locking query to the storage contract, then build separate batch-label and batch-delete pipelines on a shared validated target helper. Expose thin REST/OpenAPI handlers and keep UI selection local to `NotesPage`; reuse the existing table, modal, typed-label, stale-revision, and URL-refresh patterns without changing the selector-based bulk endpoint.

**Tech Stack:** Rust 2021, async traits, PostgreSQL/sqlx, Turso/libSQL, Axum, utoipa/OpenAPI, Yew/Wasm, gloo-net, yew-duskmoon, CSS.

---

## File Map

### New files

- `crates/note-pipelines/src/batch_note_targets.rs` — shared explicit target type, bounds validation, deterministic ordering, row-lock preflight, and missing/stale detection.
- `crates/note-pipelines/src/batch_update_note_labels.rs` — Add/Update/Remove exact-ID label semantics and atomic transaction orchestration.
- `crates/note-pipelines/src/batch_delete_notes.rs` — atomic exact-ID soft deletion and search cleanup.
- `crates/note-pipelines/tests/batch_update_note_labels_test.rs` — label-action behavior, state-preservation, validation, and rollback coverage.
- `crates/note-pipelines/tests/batch_delete_notes_test.rs` — atomic delete success and rollback coverage.

### Modified files

- `crates/note-storage/src/repositories.rs` — exact-ID active-note revision locking contract.
- `crates/note-storage-pg/src/notes.rs` — PostgreSQL `FOR UPDATE` implementation.
- `crates/note-storage-turso/src/notes.rs` — immediate-transaction exact-ID implementation.
- `crates/note-storage-contract-tests/src/bulk_labels.rs` — shared adapter behavior for exact targets.
- `crates/note-storage-pg/tests/transactions_test.rs` — PostgreSQL row-lock concurrency proof.
- `crates/note-storage-turso/tests/transactions_test.rs` — Turso immediate-transaction requirement.
- `crates/note-pipelines/src/lib.rs` — export the new pipeline modules.
- `crates/note-pipelines/tests/support/events.rs` — forward and instrument the added repository method.
- `crates/note-server/src/notes_api.rs` — request/response DTOs, handlers, routes, structured error mapping, endpoint tests, and OpenAPI assertions.
- `crates/note-server/src/main.rs` — update generated REST path/operation inventory counts.
- `crates/note-frontend/src/api.rs` — batch target/action DTOs, request builders, network functions, and payload tests.
- `crates/note-frontend/src/pages/notes.rs` — selection reducer, table checkboxes, toolbar, dialogs, async actions, feedback, and page clamping.
- `crates/note-frontend/src/components/note_editor.rs` — CSS contract test only.
- `crates/note-frontend/app.css` — selection toolbar/dialog layout and scoped Markdown textarea field sizing.

The existing `crates/note-pipelines/src/bulk_update_note_labels.rs`, MCP tools, routes, Trash behavior, and vendored `crates/note-frontend/duskmoon-core.css` remain unchanged.

### Task 1: Lock exact active note IDs at the storage boundary

**Files:**
- Modify: `crates/note-storage/src/repositories.rs:328-343`
- Modify: `crates/note-storage-pg/src/notes.rs:83-112,703-713`
- Modify: `crates/note-storage-turso/src/notes.rs:226-263,1100-1117`
- Modify: `crates/note-storage-contract-tests/src/bulk_labels.rs:59-71`
- Modify: `crates/note-storage-pg/tests/transactions_test.rs:284-370`
- Modify: `crates/note-storage-turso/tests/transactions_test.rs:669-690`
- Modify: `crates/note-pipelines/tests/support/events.rs:274-312`
- Modify: `crates/note-server/src/notes_api.rs:1692-1731`

- [ ] **Step 1: Write the failing shared storage contract test**

After the existing `matching_note_ids_for_update` assertion in `bulk_labels.rs`, add:

```rust
let exact_selection = storage.begin(TransactionMode::Immediate).await.unwrap();
assert_eq!(
    exact_selection
        .active_note_revisions_for_update(&[
            "contract-bulk-deleted".to_string(),
            "contract-bulk-active-b".to_string(),
            "contract-bulk-missing".to_string(),
            "contract-bulk-active-a".to_string(),
        ])
        .await
        .unwrap(),
    vec![
        ("contract-bulk-active-a".to_string(), 7),
        ("contract-bulk-active-b".to_string(), 8),
    ]
);
exact_selection.rollback().await.unwrap();
```

- [ ] **Step 2: Run the Turso contract and verify RED**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
```

Expected: compilation fails because `active_note_revisions_for_update` does not exist.

- [ ] **Step 3: Add the repository contract and forwarding declarations**

Add to `NotesRepository`:

```rust
/// Returns sorted `(id, revision)` pairs for requested active notes while
/// locking those rows until the enclosing immediate transaction ends.
/// Missing and soft-deleted IDs are omitted.
async fn active_note_revisions_for_update(
    &self,
    ids: &[String],
) -> StorageResult<Vec<(String, i64)>>;
```

Add this exact signature to the `NotesRepository` forwarding lists in
`crates/note-pipelines/tests/support/events.rs` and `crates/note-server/src/notes_api.rs`:

```rust
fn active_note_revisions_for_update(ids: &[String]) -> Vec<(String, i64)>;
```

- [ ] **Step 4: Implement PostgreSQL exact-ID locking**

Add to `impl NotesRepository for PgSession`:

```rust
async fn active_note_revisions_for_update(
    &self,
    ids: &[String],
) -> StorageResult<Vec<(String, i64)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut connection = self.connection().await?;
    sqlx::query_as::<_, (String, i64)>(
        "SELECT id, note_revision
         FROM notes
         WHERE deleted_at IS NULL AND id = ANY($1)
         ORDER BY id
         FOR UPDATE",
    )
    .bind(ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("lock active note revisions", error))
}
```

- [ ] **Step 5: Implement Turso exact-ID selection**

Add to `impl NotesRepository for TursoSession`:

```rust
async fn active_note_revisions_for_update(
    &self,
    ids: &[String],
) -> StorageResult<Vec<(String, i64)>> {
    if self.transaction_mode != Some(note_storage::TransactionMode::Immediate) {
        return Err(StorageError::new(
            StorageErrorKind::Transaction,
            "locking active note revisions requires an immediate transaction",
        ));
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids_json = serde_json::to_string(ids).map_err(|error| {
        StorageError::with_source(
            StorageErrorKind::Operation,
            "serialize exact note ids",
            error,
        )
    })?;
    let _operation_guard = self.operation_guard().await;
    let mut rows = self
        .connection
        .query(
            "SELECT id, note_revision
             FROM notes
             WHERE deleted_at IS NULL
               AND id IN (SELECT value FROM json_each(?1))
             ORDER BY id",
            turso::params![ids_json],
        )
        .await
        .map_err(|error| map_turso_error("query active note revisions", error))?;
    let mut revisions = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read active note revisions", error))?
    {
        revisions.push((
            row.get(0)
                .map_err(|error| map_turso_error("decode active note id", error))?,
            row.get(1)
                .map_err(|error| map_turso_error("decode active note revision", error))?,
        ));
    }
    Ok(revisions)
}
```

- [ ] **Step 6: Add backend-specific transaction tests**

In the Turso transaction test, verify a deferred transaction returns `StorageErrorKind::Transaction`
and an immediate transaction accepts an empty ID list. In the PostgreSQL transaction test, mirror
`matching_note_ids_for_update_locks_selected_notes_until_commit`, but call:

```rust
first
    .active_note_revisions_for_update(&["exact-row-lock".to_string()])
    .await
    .unwrap()
```

and prove a concurrent `advance_note_updated_at("exact-row-lock", 10)` remains pending until the
first transaction commits.

- [ ] **Step 7: Run storage tests and verify GREEN**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
cargo test -p note-storage-turso --test transactions_test bulk_update_selection_requires_an_immediate_transaction
cargo test -p note-storage-pg --test contracts_test postgresql_satisfies_shared_storage_contracts
cargo test -p note-storage-pg --test transactions_test active_note_revisions_for_update_locks_selected_notes_until_commit
```

Expected: Turso tests pass. PostgreSQL tests pass when `TEST_DATABASE_URL` is configured; otherwise
their harness reports the existing skip path and returns successfully.

- [ ] **Step 8: Commit the storage contract**

```bash
git add crates/note-storage/src/repositories.rs crates/note-storage-pg/src/notes.rs crates/note-storage-turso/src/notes.rs crates/note-storage-contract-tests/src/bulk_labels.rs crates/note-storage-pg/tests/transactions_test.rs crates/note-storage-turso/tests/transactions_test.rs crates/note-pipelines/tests/support/events.rs crates/note-server/src/notes_api.rs
git commit -m "feat(storage): lock explicit note targets"
```

### Task 2: Validate exact targets and implement Add label

**Files:**
- Create: `crates/note-pipelines/src/batch_note_targets.rs`
- Create: `crates/note-pipelines/src/batch_update_note_labels.rs`
- Create: `crates/note-pipelines/tests/batch_update_note_labels_test.rs`
- Modify: `crates/note-pipelines/src/lib.rs:7-13`

- [ ] **Step 1: Write failing target-validation tests**

Create the test file with a Turso-backed context and these assertions:

```rust
mod support;

use note_pipelines::{
    batch_update_note_labels, BatchLabelAction, BatchNoteTarget,
    BatchNoteTargetsValidationError, BatchUpdateNoteLabelsInput,
};
use support::test_context;

fn target(id: &str, expected_revision: i64) -> BatchNoteTarget {
    BatchNoteTarget {
        id: id.to_string(),
        expected_revision,
    }
}

#[tokio::test]
async fn batch_targets_reject_empty_duplicate_blank_and_oversized_sets() {
    let (ctx, _backend, _dir) = test_context().await;
    let add = BatchLabelAction::Add {
        key: "project".into(),
        value: "agent-note".into(),
    };
    for (notes, expected) in [
        (vec![], BatchNoteTargetsValidationError::Empty),
        (
            vec![target("same", 1), target("same", 1)],
            BatchNoteTargetsValidationError::DuplicateId("same".into()),
        ),
        (
            vec![target("   ", 1)],
            BatchNoteTargetsValidationError::BlankId,
        ),
        (
            (0..=1_000)
                .map(|index| target(&format!("note-{index}"), 1))
                .collect(),
            BatchNoteTargetsValidationError::TooMany { maximum: 1_000 },
        ),
    ] {
        let error = batch_update_note_labels(
            &ctx,
            BatchUpdateNoteLabelsInput {
                notes,
                action: add.clone(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<BatchNoteTargetsValidationError>(),
            Some(&expected)
        );
    }
}
```

- [ ] **Step 2: Run the validation test and verify RED**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test batch_targets_reject_empty_duplicate_blank_and_oversized_sets
```

Expected: compilation fails because the batch modules and types do not exist.

- [ ] **Step 3: Implement target validation and preflight**

Create `batch_note_targets.rs` with these public types and helpers:

```rust
use note_storage::StorageTransaction;
use std::collections::{HashMap, HashSet};

pub const MAX_BATCH_NOTE_TARGETS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchNoteTarget {
    pub id: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchNoteTargetsValidationError {
    Empty,
    BlankId,
    DuplicateId(String),
    TooMany { maximum: usize },
}

pub(crate) fn validate_batch_note_targets(
    targets: Vec<BatchNoteTarget>,
) -> anyhow::Result<Vec<BatchNoteTarget>> {
    if targets.is_empty() {
        return Err(BatchNoteTargetsValidationError::Empty.into());
    }
    if targets.len() > MAX_BATCH_NOTE_TARGETS {
        return Err(BatchNoteTargetsValidationError::TooMany {
            maximum: MAX_BATCH_NOTE_TARGETS,
        }
        .into());
    }
    let mut seen = HashSet::with_capacity(targets.len());
    for target in &targets {
        if target.id.trim().is_empty() {
            return Err(BatchNoteTargetsValidationError::BlankId.into());
        }
        if !seen.insert(target.id.clone()) {
            return Err(BatchNoteTargetsValidationError::DuplicateId(target.id.clone()).into());
        }
    }
    let mut targets = targets;
    targets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(targets)
}

pub(crate) async fn preflight_batch_note_targets(
    transaction: &dyn StorageTransaction,
    targets: &[BatchNoteTarget],
) -> anyhow::Result<()> {
    let ids = targets.iter().map(|target| target.id.clone()).collect::<Vec<_>>();
    let revisions = transaction.active_note_revisions_for_update(&ids).await?;
    let revisions = revisions.into_iter().collect::<HashMap<_, _>>();
    for target in targets {
        let Some(current_revision) = revisions.get(&target.id).copied() else {
            return Err(crate::NoteMutationError::NotFound(target.id.clone()).into());
        };
        if current_revision != target.expected_revision {
            return Err(crate::NoteMutationError::StaleRevision {
                note_id: target.id.clone(),
                expected_revision: target.expected_revision,
                current_revision,
            }
            .into());
        }
    }
    Ok(())
}
```

Implement `Display` and `Error` for all validation variants with stable messages used by REST:

```text
at least one note target is required
note target id must not be blank
duplicate note target id: <id>
note target count exceeds maximum of 1000
```

- [ ] **Step 4: Write the failing Add behavior test**

Seed two revision-1 notes, attach `project=existing` only to the first, then assert:

```rust
let result = batch_update_note_labels(
    &ctx,
    BatchUpdateNoteLabelsInput {
        notes: vec![target("first", 1), target("second", 1)],
        action: BatchLabelAction::Add {
            key: "project".into(),
            value: "agent-note".into(),
        },
    },
)
.await
.unwrap();
assert_eq!(
    result,
    BatchUpdateNoteLabelsResult {
        requested: 2,
        updated: 1,
        unchanged: 1,
    }
);
assert!(
    labels_for(&backend, "first")
        .await
        .contains(&("project".into(), "existing".into()))
);
assert!(
    labels_for(&backend, "second")
        .await
        .contains(&("project".into(), "agent-note".into()))
);
```

Use these local helpers so every test declares its exact starting label state:

```rust
async fn seed_note(
    backend: &Arc<dyn StorageBackend>,
    id: &str,
    labels: &[(&str, &str)],
) {
    let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
    transaction
        .insert_note(NewNote {
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
    for (key, value) in labels {
        transaction
            .insert_label_key_if_missing(key, "")
            .await
            .unwrap();
        transaction.attach_label(id, key, value).await.unwrap();
    }
    transaction.commit().await.unwrap();
}

async fn labels_for(
    backend: &Arc<dyn StorageBackend>,
    id: &str,
) -> Vec<(String, String)> {
    backend
        .session()
        .await
        .unwrap()
        .labels_for_note(id)
        .await
        .unwrap()
        .into_iter()
        .map(|label| (label.key, label.value))
        .collect()
}
```

- [ ] **Step 5: Run the Add test and verify RED**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test add_only_changes_notes_missing_the_key
```

Expected: compilation fails because `batch_update_note_labels` is not implemented.

- [ ] **Step 6: Implement atomic Add**

Create `batch_update_note_labels.rs` with:

```rust
use crate::{
    preflight_batch_note_targets, validate_batch_note_targets, BatchNoteTarget, Context,
};
use chrono::Utc;
use note_core::{validate_label_key, validate_label_value, LabelValueType, ValidationError};
use note_storage::TransactionMode;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchLabelAction {
    Add { key: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchUpdateNoteLabelsInput {
    pub notes: Vec<BatchNoteTarget>,
    pub action: BatchLabelAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchUpdateNoteLabelsResult {
    pub requested: usize,
    pub updated: usize,
    pub unchanged: usize,
}
```

The function must validate the key before `begin`, create a missing destination catalog key as
text inside the transaction, validate the value against the final catalog type, preflight every
target, and apply Add only when `labels_for_note` lacks the key:

```rust
if !labels.iter().any(|label| label.key == key) {
    transaction.set_note_label(&target.id, key, value).await?;
    transaction.advance_note_updated_at(&target.id, now).await?;
    updated += 1;
}
```

Finish through `crate::save_note::finish_transaction`; call `ctx.notify_note_mutated()` only when
`updated > 0`.

- [ ] **Step 7: Export modules and run the focused test file**

Add to `lib.rs`:

```rust
mod batch_note_targets;
pub use batch_note_targets::*;

pub mod batch_update_note_labels;
pub use batch_update_note_labels::*;
```

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test
```

Expected: validation and Add tests pass.

- [ ] **Step 8: Commit target validation and Add**

```bash
git add crates/note-pipelines/src/batch_note_targets.rs crates/note-pipelines/src/batch_update_note_labels.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/tests/batch_update_note_labels_test.rs
git commit -m "feat(notes): add labels to selected notes"
```

### Task 3: Implement Update and Remove label semantics

**Files:**
- Modify: `crates/note-pipelines/src/batch_update_note_labels.rs`
- Modify: `crates/note-pipelines/tests/batch_update_note_labels_test.rs`

- [ ] **Step 1: Write failing Update tests**

Add separate tests for same-key replacement and key rename with a destination collision. The
rename assertion must cover all three selected states:

```rust
let result = batch_update_note_labels(
    &ctx,
    BatchUpdateNoteLabelsInput {
        notes: vec![target("rename", 1), target("collision", 1), target("missing", 1)],
        action: BatchLabelAction::Update {
            from_key: "status".into(),
            key: "stage".into(),
            value: "review".into(),
        },
    },
)
.await
.unwrap();
assert_eq!(
    result,
    BatchUpdateNoteLabelsResult {
        requested: 3,
        updated: 2,
        unchanged: 1,
    }
);
for id in ["rename", "collision"] {
    let labels = labels_for(&backend, id).await;
    assert!(!labels.iter().any(|(key, _)| key == "status"));
    assert!(labels.contains(&("stage".into(), "review".into())));
}
```

Seed `collision` with both `status=draft` and `stage=old`; seed `missing` without `status`.
Also run an Update against a note missing `status` with a previously unknown destination key and
assert the no-op does not create that destination in `list_label_keys()`.

- [ ] **Step 2: Run Update tests and verify RED**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test update_
```

Expected: compilation fails because `BatchLabelAction::Update` does not exist.

- [ ] **Step 3: Add Update to the action enum and implementation**

Extend the enum:

```rust
Update {
    from_key: String,
    key: String,
    value: String,
},
```

Validate both keys before `begin`. After target preflight, read every target's labels once and
identify eligible notes. If no note contains `from_key`, roll back and return every target as
unchanged without creating the destination catalog key. Otherwise create and type-check only the
destination key, then implement per eligible target:

```rust
let source_present = labels.iter().any(|label| label.key == *from_key);
if source_present {
    let mut changed = false;
    if from_key != key {
        changed |= transaction.remove_note_label(&target.id, from_key).await?;
    }
    changed |= transaction.set_note_label(&target.id, key, value).await?;
    if changed {
        transaction.advance_note_updated_at(&target.id, now).await?;
        updated += 1;
    }
}
```

- [ ] **Step 4: Run Update tests and verify GREEN**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test update_
```

Expected: same-key replacement, rename, collision replacement, and missing-source no-op pass.

- [ ] **Step 5: Write the failing Remove test**

Seed one note with `owner=gao` and one without it, then add:

```rust
let result = batch_update_note_labels(
    &ctx,
    BatchUpdateNoteLabelsInput {
        notes: vec![target("owned", 1), target("unowned", 1)],
        action: BatchLabelAction::Remove {
            key: "owner".into(),
        },
    },
)
.await
.unwrap();
assert_eq!(
    result,
    BatchUpdateNoteLabelsResult {
        requested: 2,
        updated: 1,
        unchanged: 1,
    }
);
assert!(!labels_for(&backend, "owned")
    .await
    .iter()
    .any(|(key, _)| key == "owner"));
assert!(backend
    .session()
    .await
    .unwrap()
    .list_label_keys()
    .await
    .unwrap()
    .iter()
    .any(|key| key.key == "owner"));
```

- [ ] **Step 6: Run Remove test and verify RED**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test remove_only_changes_notes_with_the_key
```

Expected: compilation fails because `BatchLabelAction::Remove` does not exist.

- [ ] **Step 7: Implement Remove**

Extend the enum:

```rust
Remove { key: String },
```

Validate the key before `begin`. Do not create or delete catalog keys. Per target:

```rust
if transaction.remove_note_label(&target.id, key).await? {
    transaction.advance_note_updated_at(&target.id, now).await?;
    updated += 1;
}
```

- [ ] **Step 8: Add rollback, typed-value, stale, and preservation regression tests**

Add focused tests that prove:

```rust
assert_eq!(error.downcast_ref::<NoteMutationError>(), Some(&expected_stale));
assert_eq!(labels_after_failure, labels_before_failure);
assert_eq!(note_after.revision, note_before.revision);
assert_eq!(note_after.content, note_before.content);
assert_eq!(note_after.attachments, note_before.attachments);
assert_eq!(chunks_after, chunks_before);
```

Use the existing `EventStorageBackend` failure injection to fail `advance_note_updated_at` after a
label write and verify the transaction rolls back the label, created catalog key, and timestamp.
Add one typed number label and assert `value="urgent"` returns
`ValidationError::InvalidLabelValue` before changing any target.

- [ ] **Step 9: Run the complete batch-label pipeline suite**

Run:

```bash
cargo test -p note-pipelines --test batch_update_note_labels_test
cargo test -p note-pipelines --test bulk_update_note_labels_test
```

Expected: both exact-ID and existing selector-based suites pass.

- [ ] **Step 10: Commit Update and Remove**

```bash
git add crates/note-pipelines/src/batch_update_note_labels.rs crates/note-pipelines/tests/batch_update_note_labels_test.rs
git commit -m "feat(notes): update selected note labels"
```

### Task 4: Implement atomic batch soft deletion

**Files:**
- Create: `crates/note-pipelines/src/batch_delete_notes.rs`
- Create: `crates/note-pipelines/tests/batch_delete_notes_test.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the failing successful-delete test**

Seed two active notes with revision 1, index their search content, and assert:

```rust
let result = batch_delete_notes(
    &ctx,
    BatchDeleteNotesInput {
        notes: vec![target("first", 1), target("second", 1)],
    },
)
.await
.unwrap();
assert_eq!(
    result,
    BatchDeleteNotesResult {
        requested: 2,
        deleted: 2,
    }
);
assert!(get_note(&ctx, "first").await.unwrap().is_none());
assert!(get_note(&ctx, "second").await.unwrap().is_none());
let deleted = list_deleted_note_summaries(&ctx).await.unwrap();
assert!(deleted.iter().any(|note| note.id == "first" && note.revision == 2));
assert!(deleted.iter().any(|note| note.id == "second" && note.revision == 2));
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p note-pipelines --test batch_delete_notes_test deletes_every_selected_note_atomically
```

Expected: compilation fails because the batch-delete types and function do not exist.

- [ ] **Step 3: Implement the batch-delete pipeline**

Create:

```rust
use crate::{
    mutation_error::mutation_revision, preflight_batch_note_targets,
    validate_batch_note_targets, BatchNoteTarget, Context,
};
use note_storage::TransactionMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteNotesInput {
    pub notes: Vec<BatchNoteTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchDeleteNotesResult {
    pub requested: usize,
    pub deleted: usize,
}

pub async fn batch_delete_notes(
    ctx: &Context,
    input: BatchDeleteNotesInput,
) -> anyhow::Result<BatchDeleteNotesResult> {
    let targets = validate_batch_note_targets(input.notes)?;
    let requested = targets.len();
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        preflight_batch_note_targets(transaction.as_ref(), &targets).await?;
        let deleted_at = chrono::Utc::now().timestamp();
        for target in &targets {
            mutation_revision(
                &target.id,
                transaction
                    .soft_delete_note(&target.id, target.expected_revision, deleted_at)
                    .await?,
            )?;
            transaction.clear_note_search_data(&target.id).await?;
        }
        anyhow::Ok(BatchDeleteNotesResult {
            requested,
            deleted: requested,
        })
    }
    .await;
    crate::save_note::finish_transaction(transaction, transaction_result).await
}
```

Export the module from `lib.rs`.

- [ ] **Step 4: Add stale, missing, duplicate, and injected-failure rollback tests**

For stale and missing tests, include one valid target after the invalid target and assert neither
note enters Trash. For the injected failure, call
`fail_next_repository_call("clear_note_search_data")`; the first note is soft-deleted immediately
before that controlled failure, so assert the outer rollback restores it. Assert
duplicate/empty/oversized input returns the shared
`BatchNoteTargetsValidationError` without beginning a transaction.

- [ ] **Step 5: Run batch-delete and existing lifecycle tests**

Run:

```bash
cargo test -p note-pipelines --test batch_delete_notes_test
cargo test -p note-pipelines --test save_note_test delete
```

Expected: new atomic tests and existing single-note deletion tests pass.

- [ ] **Step 6: Commit the delete pipeline**

```bash
git add crates/note-pipelines/src/batch_delete_notes.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/tests/batch_delete_notes_test.rs
git commit -m "feat(notes): delete selected notes atomically"
```

### Task 5: Expose exact-ID batch label REST and OpenAPI

**Files:**
- Modify: `crates/note-server/src/notes_api.rs:13-21,331-357,944-1006,1565-1598,1947-2073,2508-2968`

- [ ] **Step 1: Write failing REST behavior tests**

Add an endpoint test that creates two notes, gives only the first a `project` label, and sends:

```rust
post(
    "/api/notes/batch-labels",
    &serde_json::json!({
        "notes": [
            {"id": first_id, "expected_revision": 1},
            {"id": second_id, "expected_revision": 1}
        ],
        "action": {"type": "add", "key": "project", "value": "agent-note"}
    })
    .to_string(),
)
```

Assert status 200 and body:

```rust
serde_json::json!({"requested": 2, "updated": 1, "unchanged": 1})
```

Add request cases for Update rename, Remove, duplicate IDs (400), missing ID (404), stale revision
(409 with structured `NoteMutationApiError`), unknown action fields (400), and a failing backend
(sanitized 500).

- [ ] **Step 2: Run one endpoint test and verify RED**

Run:

```bash
cargo test -p note-server batch_label_add_targets_only_explicit_note_ids
```

Expected: 404 because the route is absent.

- [ ] **Step 3: Add DTOs and conversions**

Add:

```rust
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BatchNoteTargetRequest {
    id: String,
    expected_revision: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BatchLabelActionRequest {
    Add { key: String, value: String },
    Update {
        from_key: String,
        key: String,
        value: String,
    },
    Remove { key: String },
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BatchUpdateNoteLabelsRequest {
    #[schema(min_items = 1, max_items = 1000)]
    notes: Vec<BatchNoteTargetRequest>,
    action: BatchLabelActionRequest,
}

#[derive(Serialize, utoipa::ToSchema)]
struct BatchUpdateNoteLabelsResponse {
    requested: usize,
    updated: usize,
    unchanged: usize,
}
```

Implement `From<BatchNoteTargetRequest> for BatchNoteTarget` and
`From<BatchLabelActionRequest> for BatchLabelAction` as exhaustive matches.

- [ ] **Step 4: Add structured batch validation error mapping**

Extend `NoteMutationApiError::from_anyhow` before the generic validation check:

```rust
if let Some(error) = error.downcast_ref::<BatchNoteTargetsValidationError>() {
    return Self::invalid_input(error.to_string());
}
```

Keep `NoteMutationError` mapping first so missing and stale targets retain 404/409 details. Do not
return raw storage errors.

- [ ] **Step 5: Implement and route the handler**

Add a `#[utoipa::path]` documenting 200, 400, 404, 409, and 500 structured responses. The handler
body is:

```rust
let result = batch_update_note_labels(
    &ctx,
    BatchUpdateNoteLabelsInput {
        notes: req.notes.into_iter().map(Into::into).collect(),
        action: req.action.into(),
    },
)
.await
.map_err(NoteMutationApiError::from_anyhow)?;
if result.updated > 0 {
    invalidate_dashboard_cache();
}
Ok(Json(BatchUpdateNoteLabelsResponse {
    requested: result.requested,
    updated: result.updated,
    unchanged: result.unchanged,
}))
```

Register `.routes(routes!(batch_update_note_labels_handler))` beside the existing bulk-label route.

- [ ] **Step 6: Add OpenAPI assertions**

Add `("/api/notes/batch-labels", &["post"])` to the expected paths. Assert request and 200 schema
refs, structured `NoteMutationApiError` for 400/404/409/500, `notes.minItems == 1`,
`notes.maxItems == 1000`, and an action discriminator with Add/Update/Remove schemas.

- [ ] **Step 7: Run focused server tests**

Run:

```bash
cargo test -p note-server batch_label_
cargo test -p note-server openapi_contains_all_note_operations
```

Expected: behavior and OpenAPI tests pass.

- [ ] **Step 8: Commit the label endpoint**

```bash
git add crates/note-server/src/notes_api.rs
git commit -m "feat(server): expose selected-note label actions"
```

### Task 6: Expose atomic batch delete REST and update inventory

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/main.rs:748-769`

- [ ] **Step 1: Write failing batch-delete endpoint tests**

Post two exact targets to `/api/notes/batch-delete`; assert 200 with
`{"requested":2,"deleted":2}`, both GETs return 404, and both notes appear in Trash at revision 2.
Add stale, missing, duplicate, malformed, and sanitized storage-failure cases; stale/missing must
leave every requested note active.

- [ ] **Step 2: Run one delete endpoint test and verify RED**

Run:

```bash
cargo test -p note-server batch_delete_moves_every_target_to_trash
```

Expected: 404 because the route is absent.

- [ ] **Step 3: Add DTOs, handler, and route**

Reuse `BatchNoteTargetRequest` and add:

```rust
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BatchDeleteNotesRequest {
    #[schema(min_items = 1, max_items = 1000)]
    notes: Vec<BatchNoteTargetRequest>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct BatchDeleteNotesResponse {
    requested: usize,
    deleted: usize,
}
```

The handler calls `batch_delete_notes`, maps through `NoteMutationApiError::from_anyhow`, always
invalidates the dashboard after a successful non-empty result, and returns the response DTO.
Document 200/400/404/409/500 and register the route.

- [ ] **Step 4: Update OpenAPI and global REST inventory tests**

Add `/api/notes/batch-delete` to the note path list and assert the same target bounds and structured
error schemas as batch labels. In `main.rs`, change:

```rust
assert_eq!(paths.len(), 52);
assert_eq!(operation_count, 62);
```

These are the current 50 paths/60 operations plus the two new POST paths.

- [ ] **Step 5: Run focused endpoint and inventory tests**

Run:

```bash
cargo test -p note-server batch_delete_
cargo test -p note-server openapi_contains_all_note_operations
cargo test -p note-server generated_rest_openapi_is_complete_and_excludes_mcp
```

Expected: all pass; MCP inventory remains unchanged.

- [ ] **Step 6: Commit the delete endpoint**

```bash
git add crates/note-server/src/notes_api.rs crates/note-server/src/main.rs
git commit -m "feat(server): expose selected-note deletion"
```

### Task 7: Add frontend batch request contracts

**Files:**
- Modify: `crates/note-frontend/src/api.rs:120-180,527-657,722-850`

- [ ] **Step 1: Write failing payload tests**

Add tests for all four bodies:

```rust
let targets = vec![
    BatchNoteTarget::new("first", 3),
    BatchNoteTarget::new("second", 5),
];
assert_eq!(
    batch_label_body(
        &targets,
        &BatchLabelAction::Update {
            from_key: "status".into(),
            key: "stage".into(),
            value: "review".into(),
        },
    ),
    serde_json::json!({
        "notes": [
            {"id": "first", "expected_revision": 3},
            {"id": "second", "expected_revision": 5}
        ],
        "action": {
            "type": "update",
            "from_key": "status",
            "key": "stage",
            "value": "review"
        }
    })
);
assert_eq!(
    batch_delete_body(&targets),
    serde_json::json!({
        "notes": [
            {"id": "first", "expected_revision": 3},
            {"id": "second", "expected_revision": 5}
        ]
    })
);
```

Add equivalent exact action assertions for Add and Remove.

- [ ] **Step 2: Run API tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml api::tests::batch_
```

Expected: compilation fails because the frontend batch DTOs do not exist.

- [ ] **Step 3: Implement serializable request and response types**

Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BatchNoteTarget {
    id: String,
    expected_revision: i64,
}

impl BatchNoteTarget {
    pub fn new(id: impl Into<String>, expected_revision: i64) -> Self {
        Self {
            id: id.into(),
            expected_revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BatchLabelAction {
    Add { key: String, value: String },
    Update {
        from_key: String,
        key: String,
        value: String,
    },
    Remove { key: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BatchLabelResult {
    pub requested: usize,
    pub updated: usize,
    pub unchanged: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BatchDeleteResult {
    pub requested: usize,
    pub deleted: usize,
}
```

Use private `#[derive(Serialize)]` request wrappers rather than manually constructing JSON; keep
`batch_label_body` and `batch_delete_body` as helpers that call
`serde_json::to_value(request).expect("serialize selected-note request")` so unit tests cover the
exact transport shape.

- [ ] **Step 4: Implement network functions**

Add:

```rust
pub async fn batch_update_note_labels(
    targets: &[BatchNoteTarget],
    action: &BatchLabelAction,
) -> Result<BatchLabelResult, NoteMutationApiError>
```

POST JSON to `/api/notes/batch-labels`, pass the response through `ok_or_mutation_error`, and decode
`BatchLabelResult`, mapping a malformed 200 response to `NoteMutationApiError::unexpected(200)`.
Add the equivalent:

```rust
pub async fn batch_delete_notes(
    targets: &[BatchNoteTarget],
) -> Result<BatchDeleteResult, NoteMutationApiError>
```

for `/api/notes/batch-delete`.

- [ ] **Step 5: Run frontend API tests and Wasm check**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml api::tests
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: API tests and Wasm compilation pass.

- [ ] **Step 6: Commit frontend API contracts**

```bash
git add crates/note-frontend/src/api.rs
git commit -m "feat(frontend): call selected-note APIs"
```

### Task 8: Add current-page selection to both Notes tables

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs:1-17,228-373,626-673,797-825,902-1197,1199-1490`
- Modify: `crates/note-frontend/app.css:540-675,960-990`

- [ ] **Step 1: Write failing reducer and target-order tests**

Add pure tests for:

```rust
let selected = SelectionState(HashSet::from(["first".into(), "hidden".into()]));
let reduced = Rc::new(selected).reduce(SelectionAction::RetainVisible(HashSet::from([
    "first".into(),
    "second".into(),
])));
assert_eq!(reduced.0, HashSet::from(["first".into()]));

let targets = selected_note_targets(
    &[
        NoteSelectionTarget::new("second", 5),
        NoteSelectionTarget::new("first", 3),
    ],
    &HashSet::from(["first".into(), "second".into()]),
);
assert_eq!(
    targets,
    vec![
        api::BatchNoteTarget::new("second", 5),
        api::BatchNoteTarget::new("first", 3),
    ]
);
```

The target helper preserves visible table order, not `HashSet` order.

- [ ] **Step 2: Run selection tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::selection_
```

Expected: compilation fails because selection types do not exist.

- [ ] **Step 3: Implement local selection state**

Add `HashSet`, `Deref`, and `Rc` imports plus:

```rust
#[derive(Clone, Default, PartialEq)]
struct SelectionState(HashSet<String>);

impl Deref for SelectionState {
    type Target = HashSet<String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

enum SelectionAction {
    Replace(HashSet<String>),
    RetainVisible(HashSet<String>),
    Toggle { id: String, selected: bool },
    Clear,
}
```

Implement `Reducible` exactly as the Trash reducer does for these four actions. Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct NoteSelectionTarget {
    id: String,
    revision: i64,
}
```

and visible-order conversion helpers for `NoteSummary` and `SearchResultSummary`.

- [ ] **Step 4: Write failing table structure tests**

Update the existing scored-table expected headers to include an empty selection header before
Score and assert the first body cell has class `col-select` with a checkbox whose
`aria-label == "Select Matched note"`. Add the equivalent normal-table assertion. Verify the
header checkbox has `aria-label="Select all visible notes"`.

- [ ] **Step 5: Run table tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::search_result_table_renders_score_first_with_normal_note_row_content
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::note_table_renders_selection_column
```

Expected: header/cell-count assertions fail because the checkbox column is absent.

- [ ] **Step 6: Thread selection through shared table rendering**

Add `selected: &HashSet<String>`, `selection_disabled: bool`, `select_all_ref: NodeRef`,
`on_select_all: Callback<Event>`, and `on_select_note: Callback<(String, bool)>` arguments through
`list_view`, `note_table`, `search_results_view`, `search_result_table`, `note_table_head`, and
`note_row`.

The row cell is:

```rust
<td class="col-select">
    <input
        class="checkbox checkbox-primary"
        type="checkbox"
        checked={selected}
        disabled={selection_disabled}
        aria-label={format!("Select {title}")}
        onchange={on_change}
    />
</td>
```

The head checkbox uses `select_all_ref`, selects only the visible targets, and has an effect that
sets `HtmlInputElement::set_indeterminate(selected_count > 0 && selected_count < visible_count)`.

- [ ] **Step 7: Reconcile selection on visible-row changes**

Compute the current rendered target list before rendering. For search results, use only
`hits[search_page.start..search_page.end]`. Dispatch `RetainVisible` from an effect keyed by the
visible IDs. Page, page size, query, and filter changes therefore remove every hidden selection.

- [ ] **Step 8: Add minimal checkbox/table CSS**

Reuse the existing `.col-select` width and checkbox classes. Add only Notes-specific alignment if
the normal table lacks it; update `.note-table` and `.search-result-table` minimum widths just
enough for the new checkbox column. Extend the existing mobile toolbar rule rather than adding a
second breakpoint.

- [ ] **Step 9: Run Notes tests and Wasm check**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: selection reducer and both table structures pass.

- [ ] **Step 10: Commit table selection**

```bash
git add crates/note-frontend/src/pages/notes.rs crates/note-frontend/app.css
git commit -m "feat(frontend): select visible notes"
```

### Task 9: Add label action toolbar and dialogs

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing action-model tests**

Define the desired dialog model through tests:

```rust
assert_eq!(
    label_action_from_draft(
        BatchLabelMode::Update,
        "status",
        "stage",
        "review",
    ),
    Some(api::BatchLabelAction::Update {
        from_key: "status".into(),
        key: "stage".into(),
        value: "review".into(),
    })
);
assert_eq!(
    batch_label_result_message(&api::BatchLabelResult {
        requested: 3,
        updated: 2,
        unchanged: 1,
    }),
    "Updated 2 of 3 selected notes; 1 unchanged."
);
```

Add separate Add and Remove payload assertions and singular result wording.

- [ ] **Step 2: Run helper tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::batch_label_
```

Expected: compilation fails because the mode/draft helpers do not exist.

- [ ] **Step 3: Implement action state and pure helpers**

Add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchLabelMode {
    Add,
    Update,
    Remove,
}
```

Add state handles for open mode, source key, destination key, value, mutation-in-flight, and
success summary. `label_action_from_draft` trims keys, preserves the entered value, and returns
`None` when a required key is blank.

- [ ] **Step 4: Write failing toolbar/dialog VNode tests**

Render the pure toolbar helper with zero and two selected notes. Assert it contains Add label,
Update label, Remove label, and Delete selected; all buttons are disabled at zero and enabled at
two. Render each label modal and assert its title, required inputs, typed destination input, Cancel,
and counted submit label.

- [ ] **Step 5: Run VNode tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::batch_toolbar_
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::batch_label_modal_
```

Expected: compilation fails because the render helpers do not exist.

- [ ] **Step 6: Render the toolbar and accessible label dialogs**

Place the toolbar immediately above either table and below errors/loading. Use native
`type="button"` controls so no surrounding form submits accidentally. Use catalog-backed datalists
for source/destination keys and the existing `label_value_input_type` for destination values.
The toolbar summary uses `aria-live="polite"`; mutation errors use `Alert variant="error"`.

- [ ] **Step 7: Wire the async label request**

On submit:

1. build targets from the current visible rows and selected IDs;
2. return early when the action helper returns `None` or targets are empty;
3. set `batch_mutating=true` before spawning;
4. call `api::batch_update_note_labels` once;
5. on success, close/reset the dialog, clear selection, store the result message, and call `reload`;
6. on stale error, dispatch `StaleRevisionGateAction::Conflict` and keep selection/draft;
7. on other errors, store the structured error string and keep selection/draft; and
8. set `batch_mutating=false` in every completion branch.

All table checkboxes, toolbar buttons, per-row Remove buttons, and dialog submits are disabled and
callback-guarded while `batch_mutating` is true.

- [ ] **Step 8: Add responsive toolbar/dialog CSS**

Add `.notes-batch-toolbar`, `.notes-batch-actions`, and `.notes-batch-label-form` rules using the
existing surface tokens, flex wrapping, and label-picker spacing. Do not introduce new fonts,
colors, shadows, or animations; this is an extension of the current dense utility UI.

- [ ] **Step 9: Run Notes tests and frontend suite**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: helper/VNode tests and the full frontend suite pass.

- [ ] **Step 10: Commit label actions**

```bash
git add crates/note-frontend/src/pages/notes.rs crates/note-frontend/app.css
git commit -m "feat(frontend): edit selected note labels"
```

### Task 10: Add atomic Delete selected UI and page clamping

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Write failing delete copy and page-target tests**

Add:

```rust
assert_eq!(
    batch_delete_modal_copy(1),
    (
        "Delete selected note".to_string(),
        "Move 1 selected note to Trash?".to_string(),
        "Delete 1 note".to_string(),
    )
);
assert_eq!(
    page_after_batch_delete(4, 1, 1, 5),
    3,
    "an emptied final page moves back once"
);
assert_eq!(page_after_batch_delete(1, 5, 2, 12), 1);
```

Parameters are zero-based current page, page size, deleted rows, and total before delete. Implement
the helper as:

```rust
fn page_after_batch_delete(
    current: usize,
    page_size: usize,
    deleted: usize,
    total_before: usize,
) -> usize {
    let total_after = total_before.saturating_sub(deleted);
    let pages_after = total_after.div_ceil(page_size.max(1));
    current.min(pages_after.saturating_sub(1))
}
```

- [ ] **Step 2: Run delete helper tests and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::batch_delete_
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests::page_after_batch_delete
```

Expected: compilation fails because helpers do not exist.

- [ ] **Step 3: Implement delete confirmation and async request**

The Delete selected button opens the existing `Modal` with counted copy and destructive styling.
On confirm, snapshot visible-order targets, set `batch_mutating`, and call
`api::batch_delete_notes` once. On success:

```rust
selected.dispatch(SelectionAction::Clear);
batch_success.set(Some(format!(
    "Moved {} selected {} to Trash.",
    result.deleted,
    if result.deleted == 1 { "note" } else { "notes" }
)));
```

If all visible rows were deleted and the pre-delete total proves the current page was the final
page, replace the Notes URL with `current - 1`; otherwise reload the current URL. Apply the same
nearest-valid-page replacement after scored-result refresh clamps a now-out-of-range page.
On error, retain selection, keep the dialog open, and use the same stale/error path as label
actions.

- [ ] **Step 4: Update VNode tests for mutation locking**

Assert Delete selected is disabled during mutation, its confirm button is callback-guarded, row
Remove controls are disabled, and checkboxes cannot change while the request is in flight.

- [ ] **Step 5: Run Notes and frontend tests**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::notes::tests
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: all frontend tests and Wasm check pass.

- [ ] **Step 6: Commit Delete selected**

```bash
git add crates/note-frontend/src/pages/notes.rs
git commit -m "feat(frontend): delete selected notes"
```

### Task 11: Enable native content sizing for the note Markdown textarea

**Files:**
- Modify: `crates/note-frontend/src/components/note_editor.rs:761-end`
- Modify: `crates/note-frontend/app.css:1455-1470`

- [ ] **Step 1: Write the failing CSS contract test**

Add to `note_editor.rs` tests:

```rust
#[test]
fn note_markdown_textarea_uses_native_content_sizing() {
    let app_css = include_str!("../../app.css");
    assert!(app_css.contains(
        ".note-content-input .markdown-input-field {\n    field-sizing: content;\n}"
    ));
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml components::note_editor::tests::note_markdown_textarea_uses_native_content_sizing
```

Expected: FAIL because the scoped rule is absent.

- [ ] **Step 3: Add the scoped CSS rule**

Immediately after the existing `.markdown-input-field` rule, add:

```css
.note-content-input .markdown-input-field {
    field-sizing: content;
}
```

Do not change `min-height: 18rem`, `width: 100%`, `resize: vertical`, the preview rule, or
`duskmoon-core.css`.

- [ ] **Step 4: Run the focused test and Wasm check**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml components::note_editor::tests::note_markdown_textarea_uses_native_content_sizing
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: test and check pass.

- [ ] **Step 5: Commit textarea sizing**

```bash
git add crates/note-frontend/src/components/note_editor.rs crates/note-frontend/app.css
git commit -m "style(frontend): size note editor to content"
```

### Task 12: Run scoped verification and browser acceptance

**Files:**
- Verify only; do not modify out-of-scope failures.

- [ ] **Step 1: Run focused backend verification**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test bulk_update_selection_requires_an_immediate_transaction
cargo test -p note-pipelines --test batch_update_note_labels_test
cargo test -p note-pipelines --test batch_delete_notes_test
cargo test -p note-pipelines --test bulk_update_note_labels_test
cargo test -p note-server batch_label_
cargo test -p note-server batch_delete_
cargo test -p note-server openapi_contains_all_note_operations
cargo test -p note-server generated_rest_openapi_is_complete_and_excludes_mcp
```

Expected: all pass.

- [ ] **Step 2: Run PostgreSQL verification when configured**

Run:

```bash
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test active_note_revisions_for_update_locks_selected_notes_until_commit
```

Expected: pass with a configured `TEST_DATABASE_URL`. If the harness skips because the variable is
unset, record PostgreSQL live coverage as skipped, not passed.

- [ ] **Step 3: Run frontend verification**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: all frontend tests and Wasm compilation pass.

- [ ] **Step 4: Run formatting and diff checks**

Run:

```bash
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
git status --short
```

Expected: formatting and diff checks pass. Status contains only the feature changes plus the two
pre-existing untracked documents `docs/agent-note-reliability-hardening-codex-plan.md` and
`docs/emacs-org-guide.md`.

- [ ] **Step 5: Start the local app and perform browser acceptance**

Use the Chrome DevTools workflow. Verify:

1. Normal `/notes` and scored search results both show accessible row/select-all checkboxes.
2. Select all affects only the rendered page; changing page/search/filter clears hidden selection.
3. Add skips an existing key and reports updated/unchanged counts.
4. Update handles same-key replacement, rename, and destination replacement.
5. Remove skips missing keys and preserves catalog settings.
6. Delete selected confirms the count and moves all targets to Trash.
7. Keyboard focus enters each modal, Escape/cancel closes it, and focus/error feedback is usable.
8. A forced stale revision keeps selection and shows the reload conflict path.
9. On `/new` and `/notes/{id}/edit`, `CSS.supports("field-sizing", "content")` is true in the test
   browser, computed `fieldSizing` is `content`, multiline text grows the textarea, deleting text
   shrinks it back to the 18rem minimum, and Preview → Write remounts at the correct height.

- [ ] **Step 6: Review the final diff against the approved spec**

Confirm every changed line maps to
`docs/superpowers/specs/2026-08-23-notes-batch-actions-design.md`. Confirm no MCP, Trash, deployment,
release, vendored CSS, or unrelated documentation changes entered the diff.

- [ ] **Step 7: Request code review**

Use the requesting-code-review workflow on the complete branch. Resolve only findings within this
spec. Re-run the focused verification after any review fix.
