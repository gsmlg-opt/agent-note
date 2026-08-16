# Bulk Note Label Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one atomic label-only operation that sets desired label values on every active note matching a selector, exposed through MCP and REST.

**Architecture:** Add narrow storage primitives for locked target selection, idempotent label upsert, and monotonic metadata timestamps in both Turso and PostgreSQL. A transport-neutral pipeline validates the complete request and owns one immediate transaction; thin MCP and REST adapters share its request semantics and bounded count result.

**Tech Stack:** Rust 2021, async traits, Turso SQL, PostgreSQL/sqlx, Axum, utoipa/OpenAPI, rmcp/schemars, Tokio tests.

---

## File Map

- `crates/note-storage/src/repositories.rs`: add the three backend-neutral repository operations.
- `crates/note-storage-contract-tests/src/bulk_labels.rs`: define shared storage behavior for both adapters.
- `crates/note-storage-contract-tests/src/lib.rs`: run the new shared contract.
- `crates/note-storage-turso/src/{notes,labels}.rs`: implement immediate-transaction selection, monotonic timestamp advance, and label upsert.
- `crates/note-storage-pg/src/{notes,labels}.rs`: implement row-locked selection, monotonic timestamp advance, and label upsert.
- `crates/note-storage-pg/tests/transactions_test.rs`: prove matching PostgreSQL rows remain locked until the bulk transaction completes.
- `crates/note-pipelines/src/bulk_update_note_labels.rs`: own validation, fixed target selection, catalog handling, transaction flow, and counts.
- `crates/note-pipelines/src/lib.rs`: export the new pipeline.
- `crates/note-pipelines/tests/bulk_update_note_labels_test.rs`: prove public behavior, atomicity, and absence of note-content side effects.
- `crates/note-pipelines/tests/support/events.rs`: forward and fail-inject the new repository operations.
- `crates/note-mcp/src/{tools,stdio,org}.rs`: add the wrapper, closed schemas, tool handler, error mapping, and inventory entry.
- `crates/note-mcp/tests/{tools_test,org_inventory_test}.rs`: cover delegation and exact aggregate inventory.
- `crates/note-server/src/{notes_api,openapi,main}.rs`: add REST/OpenAPI behavior and update exact inventory assertions.
- `crates/note-server/tests/bulk_label_transport_conformance_test.rs`: prove MCP and REST expose the same result and state transition.
- `README.md`: document both public surfaces and updated inventory counts.

## Task 1: Add Storage Contracts and Adapter Primitives

**Files:**
- Create: `crates/note-storage-contract-tests/src/bulk_labels.rs`
- Modify: `crates/note-storage-contract-tests/src/lib.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/notes.rs`
- Modify: `crates/note-storage-turso/src/labels.rs`
- Modify: `crates/note-storage-pg/src/notes.rs`
- Modify: `crates/note-storage-pg/src/labels.rs`
- Modify: `crates/note-storage-pg/tests/transactions_test.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] **Step 1: Write the failing shared storage contract**

Create `bulk_labels.rs` with a `run` function that seeds two active notes and one deleted note, then exercises the desired primitives. Use fixed unique keys prefixed with `contract-bulk-` so the shared fresh-database contract remains deterministic.

```rust
use note_core::{parse_label_selectors, LabelValueType};
use note_storage::{NewNote, StorageBackend, TransactionMode};
use std::sync::Arc;

pub async fn run(storage: Arc<dyn StorageBackend>) {
    let session = storage.session().await.unwrap();
    session
        .insert_label_key_with_type(
            "contract-bulk-type",
            "Type",
            LabelValueType::Text,
        )
        .await
        .unwrap();
    session
        .insert_label_key("contract-bulk-project", "Project")
        .await
        .unwrap();
    for (id, updated_at) in [
        ("contract-bulk-a", 10),
        ("contract-bulk-b", 20),
        ("contract-bulk-deleted", 30),
    ] {
        session
            .insert_note(NewNote {
                id,
                title: id,
                content: "unchanged content",
                attachments: &[],
                created_at: 1,
                updated_at,
                note_revision: 7,
                deleted_at: None,
            })
            .await
            .unwrap();
        session
            .attach_label(id, "contract-bulk-type", "ietf-rfc")
            .await
            .unwrap();
    }
    session
        .attach_label("contract-bulk-a", "contract-bulk-project", "old")
        .await
        .unwrap();
    session
        .soft_delete_note("contract-bulk-deleted", 40)
        .await
        .unwrap();
    drop(session);

    let tx = storage.begin(TransactionMode::Immediate).await.unwrap();
    let ids = tx
        .matching_note_ids_for_update(&parse_label_selectors(
            "contract-bulk-type=ietf-rfc",
        ))
        .await
        .unwrap();
    assert_eq!(ids, ["contract-bulk-a", "contract-bulk-b"]);
    assert!(tx
        .set_note_label("contract-bulk-a", "contract-bulk-project", "IETF-RFC")
        .await
        .unwrap());
    assert!(!tx
        .set_note_label("contract-bulk-a", "contract-bulk-project", "IETF-RFC")
        .await
        .unwrap());
    assert!(tx
        .set_note_label("contract-bulk-b", "contract-bulk-project", "IETF-RFC")
        .await
        .unwrap());
    assert_eq!(tx.advance_note_updated_at("contract-bulk-a", 10).await.unwrap(), 1);
    tx.commit().await.unwrap();

    let observer = storage.session().await.unwrap();
    let a = observer.get_note("contract-bulk-a").await.unwrap().unwrap();
    assert_eq!(a.updated_at, 11);
    assert_eq!(a.content, "unchanged content");
    assert_eq!(a.labels.iter().find(|label| label.key == "contract-bulk-project").unwrap().value, "IETF-RFC");
    assert_eq!(observer.get_note_revision(&a.id).await.unwrap(), Some(7));

    let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
    rollback
        .insert_label_key("contract-bulk-rollback", "Rollback")
        .await
        .unwrap();
    rollback
        .set_note_label("contract-bulk-a", "contract-bulk-rollback", "temporary")
        .await
        .unwrap();
    rollback.advance_note_updated_at("contract-bulk-a", 100).await.unwrap();
    rollback.rollback().await.unwrap();
    let observer = storage.session().await.unwrap();
    assert!(!observer.list_label_keys().await.unwrap().iter().any(|key| key.key == "contract-bulk-rollback"));
    assert_eq!(observer.get_note("contract-bulk-a").await.unwrap().unwrap().updated_at, 11);
}
```

Register it in `lib.rs`:

```rust
mod bulk_labels;

pub async fn run_storage_contracts(storage: Arc<dyn StorageBackend>) {
    notes::run(storage.clone()).await;
    bulk_labels::run(storage.clone()).await;
    org::run(storage.clone()).await;
    // existing remaining suites stay in their current order
}
```

- [ ] **Step 2: Run the contract to verify RED**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts -- --exact
```

Expected: compilation fails because `matching_note_ids_for_update`, `set_note_label`, and `advance_note_updated_at` do not exist.

- [ ] **Step 3: Add exact repository signatures and event forwarding**

Add to `NotesRepository`:

```rust
/// Select active matching note ids for mutation and lock them until transaction end.
async fn matching_note_ids_for_update(
    &self,
    selectors: &[note_core::LabelSelector],
) -> StorageResult<Vec<String>>;

/// Advance metadata time without changing note revision or content fields.
async fn advance_note_updated_at(&self, id: &str, now: i64) -> StorageResult<u64>;
```

Add to `LabelRepository`:

```rust
/// Insert or replace one note label; false means the exact value already existed.
async fn set_note_label(
    &self,
    note_id: &str,
    key: &str,
    value: &str,
) -> StorageResult<bool>;
```

Extend the existing forwarding macro lists in `events.rs` exactly once:

```rust
fn matching_note_ids_for_update(selectors: &[LabelSelector]) -> Vec<String>;
fn advance_note_updated_at(id: &str, now: i64) -> u64;
fn set_note_label(note_id: &str, key: &str, value: &str) -> bool;
```

- [ ] **Step 4: Implement the Turso methods minimally**

In `notes.rs`, factor the existing matching-ID query into one unlocked helper used by both public methods. `matching_note_ids_for_update` uses the same query because `TransactionMode::Immediate` already owns the Turso writer.

```rust
async fn matching_note_ids_for_update(
    &self,
    selectors: &[LabelSelector],
) -> StorageResult<Vec<String>> {
    self.matching_note_ids(selectors).await
}

async fn advance_note_updated_at(&self, id: &str, now: i64) -> StorageResult<u64> {
    let _operation_guard = self.operation_guard().await;
    self.connection
        .execute(
            "UPDATE notes
             SET updated_at = CASE
                 WHEN updated_at >= ?2 THEN updated_at + 1
                 ELSE ?2
             END
             WHERE id = ?1 AND deleted_at IS NULL",
            turso::params![id, now],
        )
        .await
        .map_err(|error| map_turso_error("advance note updated_at", error))
}
```

In `labels.rs`, resolve the label-key ID exactly as `attach_label` does, then run:

```rust
let changed = self.connection
    .execute(
        "INSERT INTO note_labels (note_id, label_key_id, value)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(note_id, label_key_id) DO UPDATE
         SET value = excluded.value
         WHERE note_labels.value != excluded.value",
        turso::params![note_id, label_key_id, value],
    )
    .await
    .map_err(|error| map_turso_error("set note label", error))?;
Ok(changed > 0)
```

- [ ] **Step 5: Implement the PostgreSQL methods minimally**

Reuse the current resolved-selector predicate builder. The mutation selector differs only by the lock suffix:

```rust
builder.push(" ORDER BY n.id FOR UPDATE OF n");
```

Implement timestamp advancement without touching `note_revision`:

```rust
let result = sqlx::query(
    "UPDATE notes
     SET updated_at = GREATEST(updated_at + 1, $2)
     WHERE id = $1 AND deleted_at IS NULL",
)
.bind(id)
.bind(now)
.execute(&mut *connection)
.await
.map_err(|error| map_sqlx_error("advance note updated_at", error))?;
Ok(result.rows_affected())
```

Resolve the label-key ID first so an unknown key remains an operation error, then use:

```rust
let result = sqlx::query(
    "INSERT INTO note_labels (note_id, label_key_id, value)
     VALUES ($1, $2, $3)
     ON CONFLICT (note_id, label_key_id) DO UPDATE
     SET value = EXCLUDED.value
     WHERE note_labels.value IS DISTINCT FROM EXCLUDED.value",
)
.bind(note_id)
.bind(label_key_id)
.bind(value)
.execute(&mut *connection)
.await
.map_err(|error| map_sqlx_error("set note label", error))?;
Ok(result.rows_affected() > 0)
```

- [ ] **Step 6: Add the conditional PostgreSQL lock test**

In `transactions_test.rs`, provision through the existing `support::TestDatabase`. Start an immediate transaction, call `matching_note_ids_for_update`, then start a deferred transaction that calls `advance_note_updated_at` for the same note. Assert a short `tokio::time::timeout` expires before the first transaction commits and the second call completes afterward. This specifically proves the SQL row lock rather than the process-local immediate advisory gate.

```rust
let locked = backend.begin(TransactionMode::Immediate).await.unwrap();
locked
    .matching_note_ids_for_update(&parse_label_selectors("status=ready"))
    .await
    .unwrap();
let mut concurrent = tokio::spawn(async move {
    let tx = concurrent_backend.begin(TransactionMode::Deferred).await.unwrap();
    tx.advance_note_updated_at("locked-note", 20).await.unwrap();
    tx.commit().await.unwrap();
});
assert!(tokio::time::timeout(Duration::from_millis(100), &mut concurrent).await.is_err());
locked.commit().await.unwrap();
concurrent.await.unwrap();
```

- [ ] **Step 7: Run storage tests to verify GREEN**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test
cargo check -p note-storage -p note-storage-turso -p note-storage-pg -p note-pipelines --all-targets
```

Expected: all runnable tests pass. PostgreSQL live tests explicitly report skips when `TEST_DATABASE_URL` is unset.

- [ ] **Step 8: Commit the storage slice**

```bash
git add crates/note-storage crates/note-storage-contract-tests crates/note-storage-turso crates/note-storage-pg crates/note-pipelines/tests/support/events.rs
git commit -m "feat(storage): support atomic bulk label writes"
```

## Task 2: Build the Atomic Pipeline Test-First

**Files:**
- Create: `crates/note-pipelines/src/bulk_update_note_labels.rs`
- Create: `crates/note-pipelines/tests/bulk_update_note_labels_test.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the failing motivating behavior test**

Use the existing pipeline test context helpers. Seed an add case, replace case, exact no-op case, and deleted case, then assert counts and preserved state.

```rust
let result = bulk_update_note_labels(
    &ctx,
    BulkUpdateNoteLabelsInput {
        selector: "type=ietf-rfc".into(),
        set: vec![("project".into(), "IETF-RFC".into())],
    },
)
.await
.unwrap();

assert_eq!(
    result,
    BulkUpdateNoteLabelsResult {
        matched: 3,
        updated: 2,
        unchanged: 1,
    }
);
```

For each observed active note assert its `project` label, every unrelated label,
title, content, attachment metadata/content, `created_at`, and `note_revision`.
Assert the deleted note retains its prior labels. Snapshot chunks, vectors, and
pending jobs before the call and compare them afterward.

- [ ] **Step 2: Run the pipeline test to verify RED**

Run:

```bash
cargo test -p note-pipelines --test bulk_update_note_labels_test
```

Expected: compilation fails because the pipeline input, result, and function are absent.

- [ ] **Step 3: Add the pipeline API and validation error**

Create these exact public types:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateNoteLabelsInput {
    pub selector: String,
    pub set: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkUpdateNoteLabelsResult {
    pub matched: usize,
    pub updated: usize,
    pub unchanged: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BulkUpdateNoteLabelsValidationError {
    EmptySelector,
    MalformedSelector,
    EmptySet,
    DuplicateKey(String),
}
```

Implement `Display` with fixed caller-facing messages and `std::error::Error`. Add:

```rust
pub mod bulk_update_note_labels;
pub use bulk_update_note_labels::*;
```

to `lib.rs`.

Validation must run before `begin`:

```rust
let selector = input.selector.trim();
if selector.is_empty() {
    return Err(anyhow::Error::new(BulkUpdateNoteLabelsValidationError::EmptySelector));
}
if selector.split('&').any(|term| term.trim().is_empty()) {
    return Err(anyhow::Error::new(BulkUpdateNoteLabelsValidationError::MalformedSelector));
}
let selectors = parse_label_selectors(selector);
if selectors.is_empty() {
    return Err(anyhow::Error::new(BulkUpdateNoteLabelsValidationError::MalformedSelector));
}
for selector in &selectors {
    validate_label_key(&selector.key).map_err(anyhow::Error::new)?;
}
if input.set.is_empty() {
    return Err(anyhow::Error::new(BulkUpdateNoteLabelsValidationError::EmptySet));
}
let mut unique = HashSet::new();
for (key, _) in &input.set {
    validate_label_key(key).map_err(anyhow::Error::new)?;
    if !unique.insert(key.clone()) {
        return Err(anyhow::Error::new(
            BulkUpdateNoteLabelsValidationError::DuplicateKey(key.clone()),
        ));
    }
}
```

Invalid regex remains a valid no-match selector, preserving current selector semantics.

- [ ] **Step 4: Implement the minimal transaction flow**

```rust
let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
let transaction_result = async {
    let note_ids = transaction.matching_note_ids_for_update(&selectors).await?;
    if note_ids.is_empty() {
        return anyhow::Ok(None);
    }

    let existing = transaction.list_label_keys().await?;
    for (key, _) in &input.set {
        if !existing.iter().any(|label| &label.key == key) {
            transaction.insert_label_key_if_missing(key, "").await?;
        }
    }
    let value_types = transaction
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label| (label.key, label.value_type))
        .collect::<HashMap<_, _>>();
    for (key, value) in &input.set {
        let value_type = value_types[key];
        if !validate_label_value(value_type, value) {
            return Err(anyhow::Error::new(ValidationError::InvalidLabelValue {
                key: key.clone(),
                value: value.clone(),
                value_type,
            }));
        }
    }

    let now = chrono::Utc::now().timestamp();
    let mut updated = 0;
    for note_id in &note_ids {
        let mut changed = false;
        for (key, value) in &input.set {
            changed |= transaction.set_note_label(note_id, key, value).await?;
        }
        if changed {
            transaction.advance_note_updated_at(note_id, now).await?;
            updated += 1;
        }
    }
    anyhow::Ok(Some(BulkUpdateNoteLabelsResult {
        matched: note_ids.len(),
        updated,
        unchanged: note_ids.len() - updated,
    }))
}
.await;
```

For `Ok(None)`, explicitly roll back and return the all-zero result. For every other outcome, call `crate::save_note::finish_transaction`. This preserves rollback-error context and commit-failure behavior used by existing pipelines.

- [ ] **Step 5: Add validation, scale, fixed-target, and rollback tests**

Add separate tests with explicit assertions for:

```rust
assert_validation("", vec![("project", "x")], "selector must not be empty").await;
assert_validation("&&", vec![("project", "x")], "label selector is malformed").await;
assert_validation("type=x", vec![], "at least one label assignment is required").await;
assert_validation(
    "type=x",
    vec![("project", "a"), ("project", "b")],
    "duplicate label assignment key: project",
).await;
```

Also cover:

- invalid selector and target keys through `LabelKeyValidationError`;
- invalid number, version, date, datetime, and time through `ValidationError`;
- invalid regex and missing selector key returning zero without catalog creation;
- 1,001 matches, proving no list-limit code participates;
- selector `project=old` plus `set project=new`, proving all initially selected IDs change;
- multiple assignments with updated counted once per note;
- monotonic timestamps and exact no-op timestamps;
- `EventStorageBackend::fail_next_repository_call("advance_note_updated_at")` after one or more label writes, proving labels, new catalog keys, and timestamps roll back;
- `fail_next_commit`, proving the same rollback state and no partial result.

- [ ] **Step 6: Run pipeline tests to verify GREEN**

Run:

```bash
cargo test -p note-pipelines --test bulk_update_note_labels_test
cargo test -p note-pipelines
```

Expected: all tests pass with no attachment-store reads and no embedding notifier wake.

- [ ] **Step 7: Commit the pipeline slice**

```bash
git add crates/note-pipelines
git commit -m "feat(pipelines): update note labels atomically"
```

## Task 3: Expose the MCP Tool

**Files:**
- Modify: `crates/note-mcp/src/tools.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/src/org.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`
- Modify: `crates/note-mcp/tests/org_inventory_test.rs`

- [ ] **Step 1: Write failing wrapper, schema, behavior, and inventory tests**

Add a wrapper test that seeds matching notes and asserts:

```rust
let output = bulk_update_note_labels_tool(
    &ctx,
    BulkUpdateNoteLabelsToolInput {
        selector: "type=ietf-rfc".into(),
        set: vec![("project".into(), "IETF-RFC".into())],
    },
)
.await
.unwrap();
assert_eq!(output.matched, 2);
assert_eq!(output.updated, 2);
assert_eq!(output.unchanged, 0);
```

Extend stdio schema tests to require exact closed input fields `selector,set` and output fields `matched,unchanged,updated`. Change the aggregate inventory expectations from 47 tools/11 note tools to 48 tools/12 note tools and require the new name.

- [ ] **Step 2: Run MCP tests to verify RED**

Run:

```bash
cargo test -p note-mcp
```

Expected: compilation/schema/inventory failures because the tool is not registered.

- [ ] **Step 3: Add transport-independent wrapper types**

In `tools.rs`:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsToolInput {
    pub selector: String,
    pub set: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
pub struct BulkUpdateNoteLabelsToolOutput {
    pub matched: usize,
    pub updated: usize,
    pub unchanged: usize,
}

pub async fn bulk_update_note_labels_tool(
    ctx: &Context,
    input: BulkUpdateNoteLabelsToolInput,
) -> anyhow::Result<BulkUpdateNoteLabelsToolOutput> {
    let result = bulk_update_note_labels(
        ctx,
        BulkUpdateNoteLabelsInput {
            selector: input.selector,
            set: input.set,
        },
    )
    .await?;
    Ok(BulkUpdateNoteLabelsToolOutput {
        matched: result.matched,
        updated: result.updated,
        unchanged: result.unchanged,
    })
}
```

- [ ] **Step 4: Add closed MCP schemas, handler, and sanitized errors**

In `stdio.rs`:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsRequest {
    pub selector: String,
    #[schemars(length(min = 1))]
    pub set: Vec<(String, String)>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsResponse {
    pub matched: usize,
    pub updated: usize,
    pub unchanged: usize,
}
```

Add `From` conversions and register:

```rust
#[tool(
    name = "bulk_update_note_labels",
    description = "Atomically set or replace labels on active notes matching a label selector while preserving unrelated labels."
)]
pub async fn bulk_update_note_labels(
    &self,
    params: Parameters<BulkUpdateNoteLabelsRequest>,
) -> Result<Json<BulkUpdateNoteLabelsResponse>, ErrorData> {
    let output = bulk_update_note_labels_tool(&self.ctx, params.0.into())
        .await
        .map_err(to_bulk_update_note_labels_error)?;
    Ok(Json(output.into()))
}
```

Use a dedicated mapper so infrastructure text is not echoed:

```rust
fn to_bulk_update_note_labels_error(error: anyhow::Error) -> ErrorData {
    if error.downcast_ref::<BulkUpdateNoteLabelsValidationError>().is_some()
        || error.downcast_ref::<note_core::ValidationError>().is_some()
        || error.downcast_ref::<note_core::LabelKeyValidationError>().is_some()
    {
        ErrorData::invalid_params(error.to_string(), None)
    } else {
        ErrorData::internal_error("bulk note label update failed", None)
    }
}
```

Add the tool name to `NOTE_TOOL_NAMES`, change its array length to 12, and update all aggregate counts to 48.

- [ ] **Step 5: Verify MCP GREEN and commit**

Run:

```bash
cargo test -p note-mcp
```

Expected: all tool, exact schema, error-mapping, and 48-tool inventory tests pass.

```bash
git add crates/note-mcp
git commit -m "feat(mcp): add bulk note label updates"
```

## Task 4: Expose REST and OpenAPI Parity

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/main.rs`
- Create: `crates/note-server/tests/bulk_label_transport_conformance_test.rs`

- [ ] **Step 1: Write failing REST behavior and OpenAPI tests**

Extend `openapi_contains_all_note_operations` with:

```rust
("/api/notes/bulk-labels", &["post"][..]),
```

Add exact schema assertions for required `selector,set`, `additionalProperties: false`, `set.minItems == 1`, two-string tuple items, response fields `matched,updated,unchanged`, and documented `200/400/500` responses.

Add handler tests for replace/add/no-op counts, zero matches, invalid input as 400, storage failure as a fixed 500 body, and dashboard cache invalidation only when `updated > 0`.

- [ ] **Step 2: Run note API tests to verify RED**

Run:

```bash
cargo test -p note-server notes_api::tests
```

Expected: route and schema assertions fail because `/api/notes/bulk-labels` is missing.

- [ ] **Step 3: Add the nonempty label-pair schema helper**

In `openapi.rs`, wrap the existing tuple-array schema instead of duplicating it:

```rust
pub(crate) fn nonempty_label_pairs_schema() -> Array {
    ArrayBuilder::from(label_pairs_schema())
        .min_items(Some(1))
        .build()
}
```

This keeps the existing item `minItems=maxItems=2` contract unchanged while requiring at least one assignment.

- [ ] **Step 4: Add REST DTOs, handler, route, and error mapping**

In `notes_api.rs`:

```rust
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BulkUpdateNoteLabelsRequest {
    #[schema(min_length = 1)]
    selector: String,
    #[schema(schema_with = crate::openapi::nonempty_label_pairs_schema)]
    set: Vec<(String, String)>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct BulkUpdateNoteLabelsResponse {
    matched: usize,
    updated: usize,
    unchanged: usize,
}
```

Add the handler:

```rust
#[utoipa::path(
    post,
    path = "/api/notes/bulk-labels",
    tag = "notes",
    request_body = BulkUpdateNoteLabelsRequest,
    responses(
        (status = 200, body = BulkUpdateNoteLabelsResponse),
        (status = 400, body = String, content_type = "text/plain"),
        (status = 500, body = String, content_type = "text/plain")
    )
)]
async fn bulk_update_note_labels_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<BulkUpdateNoteLabelsRequest>,
) -> Result<Json<BulkUpdateNoteLabelsResponse>, (StatusCode, String)> {
    let result = bulk_update_note_labels(
        &ctx,
        BulkUpdateNoteLabelsInput {
            selector: req.selector,
            set: req.set,
        },
    )
    .await
    .map_err(|error| {
        if is_bulk_update_validation_error(&error) {
            (StatusCode::BAD_REQUEST, error.to_string())
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, "bulk note label update failed".into())
        }
    })?;
    if result.updated > 0 {
        invalidate_dashboard_cache();
    }
    Ok(Json(BulkUpdateNoteLabelsResponse {
        matched: result.matched,
        updated: result.updated,
        unchanged: result.unchanged,
    }))
}
```

`is_bulk_update_validation_error` checks the pipeline validation error plus the two existing core validation errors, matching MCP. Register the static route before the `/{id}` route group:

```rust
.routes(routes!(bulk_update_note_labels_handler))
```

Update `main.rs` exact generated-REST inventory from 49 paths/59 operations to 50 paths/60 operations.

- [ ] **Step 5: Add cross-transport conformance**

Create two identically seeded Turso contexts because applying the operation twice to one database would make the second result idempotently unchanged. Send the REST request to one router and MCP `tools/call` over Streamable HTTP to the other. Assert both return:

```json
{"matched":2,"updated":2,"unchanged":0}
```

Then read both databases through storage and assert identical final label vectors. Follow the existing Streamable HTTP setup in `org_transport_conformance_test.rs`; do not create a second MCP server implementation.

- [ ] **Step 6: Verify REST GREEN and commit**

Run:

```bash
cargo test -p note-server notes_api::tests
cargo test -p note-server --test bulk_label_transport_conformance_test
cargo test -p note-server tests::generated_rest_openapi_is_complete_and_excludes_mcp
```

Expected: REST behavior, exact OpenAPI inventory, cache behavior, and cross-transport results pass.

```bash
git add crates/note-server
git commit -m "feat(api): add bulk note label updates"
```

## Task 5: Document and Verify the Complete Feature

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update README contracts and inventory counts**

Document the MCP tool and REST endpoint with the motivating request and bounded response:

```json
{
  "selector": "type=ietf-rfc",
  "set": [["project", "IETF-RFC"]]
}
```

```json
{
  "matched": 42,
  "updated": 40,
  "unchanged": 2
}
```

State that the operation is atomic, active-only, desired-state replacement, preserves unrelated labels, and rejects a blank selector. Update 11 note tools + 36 Org tools = 47 total to 12 + 36 = 48. Change “Label-key management is REST/UI-only” to “Label-key catalog management is REST/UI-only” so it does not contradict MCP assignment updates.

- [ ] **Step 2: Run formatting and static checks**

Run:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
git diff --check
```

Expected: all commands exit zero with no formatting diff or warnings introduced by this feature.

- [ ] **Step 3: Run scoped feature tests**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test
cargo test -p note-pipelines --test bulk_update_note_labels_test
cargo test -p note-mcp
cargo test -p note-server notes_api::tests
cargo test -p note-server --test bulk_label_transport_conformance_test
```

Expected: every runnable test passes; report PostgreSQL live-test skips if `TEST_DATABASE_URL` remains unset.

- [ ] **Step 4: Run the native workspace finish gate**

Run:

```bash
cargo test --workspace
git status --short --branch
git log --oneline --decorate -6
```

Expected: workspace tests pass, only the intended README change remains before the final documentation commit, and the branch contains the design plus focused storage/pipeline/MCP/REST commits.

- [ ] **Step 5: Commit documentation**

```bash
git add README.md
git commit -m "docs: document bulk note label updates"
```

- [ ] **Step 6: Final completion audit**

Verify each explicit spec invariant against source and test evidence:

- MCP and REST accept the same selector/set shape and return the same three counts.
- Blank/malformed selectors, empty/duplicate assignments, invalid keys, and invalid typed values are caller errors.
- Matching is active-only, unpaginated, and target IDs are fixed before mutation.
- Add/replace/no-op behavior preserves unrelated labels.
- One transaction covers catalog creation, label changes, and timestamps.
- Changed notes advance timestamps; unchanged notes do not.
- Content, attachments, revisions, chunks, vectors, and embedding jobs are untouched.
- Both adapters implement the storage contract; PostgreSQL row-lock evidence is either executed live or explicitly reported as skipped.
- README, OpenAPI, MCP inventory, and REST inventory match runtime registration.

Do not merge, push, deploy, or remove the worktree unless the user separately asks.
