# Atomic Bulk Note Label Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing atomic bulk note-label operation so MCP and REST callers can remove label keys from every matching active note without replacing full notes or losing unrelated state.

**Architecture:** Add one idempotent `remove_note_label` storage primitive to both adapters, then extend the existing transport-neutral pipeline with optional `set` and `remove` mutations over the same fixed, locked target IDs and immediate transaction. MCP and REST remain thin, closed-schema adapters over that pipeline; the public names, result counts, tool inventory, and REST operation inventory stay unchanged.

**Tech Stack:** Rust 2021, async traits, Turso SQL, PostgreSQL/sqlx, Axum, utoipa/OpenAPI, rmcp/schemars, Tokio tests, Docker/Podman deployment on `ms04`.

---

## File Map

- `crates/note-storage/src/repositories.rs`: add the backend-neutral idempotent remove primitive.
- `crates/note-storage-contract-tests/src/bulk_labels.rs`: extend the shared adapter contract.
- `crates/note-storage-{turso,pg}/src/labels.rs`: implement equivalent association deletion.
- `crates/note-pipelines/tests/support/events.rs`: forward and fail-inject the new repository method.
- `crates/note-mcp/src/stdio.rs` and `crates/note-server/src/notes_api.rs`: extend test-only forwarding repositories.
- `crates/note-pipelines/src/bulk_update_note_labels.rs`: validate and apply set/remove mutations atomically.
- `crates/note-pipelines/tests/bulk_update_note_labels_test.rs`: prove behavior and invariants.
- `crates/note-mcp/src/{tools,stdio}.rs`: extend the MCP adapter and closed schema.
- `crates/note-mcp/tests/{tools_test,org_inventory_test}.rs`: cover delegation and stable inventory.
- `crates/note-server/src/notes_api.rs`: extend REST/OpenAPI and handler adaptation.
- `crates/note-server/tests/bulk_label_transport_conformance_test.rs`: prove transport parity and cache behavior.
- `README.md` and `docs/design.md`: document set/replace/remove behavior.

## Task 1: Add the Idempotent Storage Removal Primitive

**Files:**
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-contract-tests/src/bulk_labels.rs`
- Modify: `crates/note-storage-turso/src/labels.rs`
- Modify: `crates/note-storage-pg/src/labels.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`
- Modify: `crates/note-mcp/src/stdio.rs` (test forwarding only)
- Modify: `crates/note-server/src/notes_api.rs` (test forwarding only)

- [ ] **Step 1: Write the failing shared storage contract**

Extend `bulk_labels::run` after the existing set assertions. Prove changed, replay, unknown-key,
catalog-preservation, and unrelated-label behavior:

```rust
assert!(session
    .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
    .await
    .unwrap());
assert!(!session
    .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
    .await
    .unwrap());
assert!(!session
    .remove_note_label("contract-bulk-active-a", "contract-bulk-unknown")
    .await
    .unwrap());

let labels = session.labels_for_note("contract-bulk-active-a").await.unwrap();
assert!(!labels.iter().any(|label| label.key == "contract-bulk-target"));
assert!(labels.iter().any(|label| {
    label.key == "contract-bulk-unrelated" && label.value == "preserved"
}));
assert!(session.list_label_keys().await.unwrap().iter().any(|key| {
    key.key == "contract-bulk-target"
}));
```

Restore the target, remove it and advance time inside an immediate transaction, roll back, then
assert both association and timestamp survive:

```rust
let setup = storage.begin(TransactionMode::Immediate).await.unwrap();
assert!(setup
    .set_note_label("contract-bulk-active-a", "contract-bulk-target", "replacement")
    .await
    .unwrap());
setup.commit().await.unwrap();

let rollback = storage.begin(TransactionMode::Immediate).await.unwrap();
assert!(rollback
    .remove_note_label("contract-bulk-active-a", "contract-bulk-target")
    .await
    .unwrap());
assert_eq!(rollback
    .advance_note_updated_at("contract-bulk-active-a", 800)
    .await
    .unwrap(), 1);
rollback.rollback().await.unwrap();

let observer = storage.session().await.unwrap();
assert!(observer
    .labels_for_note("contract-bulk-active-a")
    .await
    .unwrap()
    .iter()
    .any(|label| {
        label.key == "contract-bulk-target" && label.value == "replacement"
    }));
assert_eq!(observer.get_note("contract-bulk-active-a").await.unwrap().unwrap().updated_at, 500);
```

- [ ] **Step 2: Run the contract to verify RED**

```bash
cargo test -p note-storage-turso --test contracts_test \
  turso_satisfies_shared_storage_contracts -- --exact
```

Expected: compilation fails because `LabelRepository::remove_note_label` does not exist.

- [ ] **Step 3: Add the trait and every forwarding signature**

Add beside `set_note_label`:

```rust
/// Removes one note-label association by key, returning whether a row changed.
/// Unknown catalog keys and absent associations are no-ops.
async fn remove_note_label(&self, note_id: &str, key: &str) -> StorageResult<bool>;
```

Add to the label-repository forwarding macro lists in pipeline event support and MCP/server test
wrappers:

```rust
fn remove_note_label(note_id: &str, key: &str) -> bool;
```

- [ ] **Step 4: Implement Turso removal**

```rust
async fn remove_note_label(&self, note_id: &str, key: &str) -> StorageResult<bool> {
    let _operation_guard = self.operation_guard().await;
    let affected = self.connection
        .execute(
            "DELETE FROM note_labels
             WHERE note_id = ?1
               AND label_key_id = (SELECT id FROM label_keys WHERE key = ?2)",
            turso::params![note_id, key],
        )
        .await
        .map_err(|error| map_turso_error("remove note label", error))?;
    Ok(affected > 0)
}
```

- [ ] **Step 5: Implement PostgreSQL removal**

```rust
async fn remove_note_label(&self, note_id: &str, key: &str) -> StorageResult<bool> {
    let mut connection = self.connection().await?;
    let result = sqlx::query(
        "DELETE FROM note_labels
         WHERE note_id = $1
           AND label_key_id = (SELECT id FROM label_keys WHERE key = $2)",
    )
    .bind(note_id)
    .bind(key)
    .execute(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("remove note label", error))?;
    Ok(result.rows_affected() > 0)
}
```

- [ ] **Step 6: Verify storage GREEN and commit**

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test
cargo check -p note-storage -p note-storage-turso -p note-storage-pg \
  -p note-pipelines -p note-mcp -p note-server --all-targets
```

Expected: Turso passes; PostgreSQL helper tests pass and live cases run or explicitly self-skip
without `TEST_DATABASE_URL`; all trait implementers compile.

```bash
git add crates/note-storage/src/repositories.rs \
  crates/note-storage-contract-tests/src/bulk_labels.rs \
  crates/note-storage-turso/src/labels.rs crates/note-storage-pg/src/labels.rs \
  crates/note-pipelines/tests/support/events.rs \
  crates/note-mcp/src/stdio.rs crates/note-server/src/notes_api.rs
git commit -m "feat(storage): remove individual note labels"
```

## Task 2: Extend the Atomic Pipeline Test-First

**Files:**
- Modify: `crates/note-pipelines/src/bulk_update_note_labels.rs`
- Modify: `crates/note-pipelines/tests/bulk_update_note_labels_test.rs`

- [ ] **Step 1: Make existing set-only fixtures explicit**

Add `remove: vec![]` to every existing `BulkUpdateNoteLabelsInput`; preserve all expectations.

- [ ] **Step 2: Write failing remove-only behavior**

Add `removes_selector_key_from_the_fixed_active_target_set`:

```rust
let result = bulk_update_note_labels(
    &ctx,
    BulkUpdateNoteLabelsInput {
        selector: "type=ietf-rfc".into(),
        set: vec![],
        remove: vec!["type".into()],
    },
)
.await
.unwrap();
assert_eq!(result, BulkUpdateNoteLabelsResult {
    matched: 3,
    updated: 3,
    unchanged: 0,
});
```

For every original active target, assert `type` is absent, `owner`/`project` remain, and the
preservation snapshot differs only in monotonic `updated_at`. The deleted note remains untouched.
The current snapshot helper excludes only `project`; make the mutation keys explicit so removal is
not mistaken for unrelated-label loss:

```rust
async fn preservation_snapshot(
    ctx: &Context,
    backend: &Arc<dyn StorageBackend>,
    id: &str,
    excluded_label_keys: &[&str],
) -> NotePreservationSnapshot {
    // existing fields stay unchanged
    let unrelated_labels = note.labels
        .into_iter()
        .filter(|label| !excluded_label_keys.contains(&label.key.as_str()))
        .collect();
    // build the existing snapshot with `unrelated_labels`
}
```

Pass `&["project"]` from existing set tests, `&["type"]` from remove-only tests, and
`&["project", "owner"]` from mixed tests.

- [ ] **Step 3: Write failing mixed/idempotent/no-op behavior**

Add `mixed_set_and_remove_counts_each_note_once_and_replays_as_noop`:

```rust
let input = BulkUpdateNoteLabelsInput {
    selector: "type=ietf-rfc".into(),
    set: vec![("project".into(), "bear_note".into())],
    remove: vec!["owner".into()],
};
assert_eq!(bulk_update_note_labels(&ctx, input.clone()).await.unwrap().updated, 3);
let replay = bulk_update_note_labels(&ctx, input).await.unwrap();
assert_eq!(replay.updated, 0);
assert_eq!(replay.unchanged, replay.matched);
```

Add `absent_and_unknown_remove_keys_are_noops_without_catalog_changes`: remove a known-absent key
and an unknown key; assert `updated == 0`, no catalog key creation, unchanged timestamps, and no
mutation notification.

- [ ] **Step 4: Write failing validation cases**

Add these exact cases and assert no `begin` event:

```rust
BulkUpdateNoteLabelsValidationError::EmptyMutations
// "at least one label set or remove mutation is required"

BulkUpdateNoteLabelsValidationError::DuplicateRemoveKey("type".into())
// "duplicate label removal key: type"

BulkUpdateNoteLabelsValidationError::ConflictingKey("type".into())
// "label key cannot be both set and removed: type"
```

Also reject invalid remove key `target$key` with the existing typed `LabelKeyValidationError`.

- [ ] **Step 5: Write failing rollback and notifier cases**

Inject a failure after a successful `remove_note_label` and before commit; assert the association
and timestamp roll back. Extend notifier coverage so changed removal notifies once after commit,
while absent removal, zero matches, repository failure, and commit failure notify zero times.

- [ ] **Step 6: Run pipeline tests to verify RED**

```bash
cargo test -p note-pipelines --test bulk_update_note_labels_test
```

Expected: compilation fails because `remove` and the new validation variants do not exist.

- [ ] **Step 7: Implement input validation and transaction flow**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateNoteLabelsInput {
    pub selector: String,
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BulkUpdateNoteLabelsValidationError {
    EmptySelector,
    MalformedSelector,
    EmptyMutations,
    DuplicateKey(String),
    DuplicateRemoveKey(String),
    ConflictingKey(String),
}
```

Validate selector, all set/remove keys, unique set keys, unique remove keys, and disjoint sets
before `begin`. Keep catalog creation/type checks scoped to set assignments. Apply mutations over the
fixed IDs:

```rust
for note_id in note_ids {
    let mut changed = false;
    for key in &input.remove {
        changed |= transaction.remove_note_label(&note_id, key).await?;
    }
    for (key, value) in &input.set {
        changed |= transaction.set_note_label(&note_id, key, value).await?;
    }
    if changed {
        transaction.advance_note_updated_at(&note_id, now).await?;
        updated += 1;
    }
}
```

Do not change target locking, zero-match rollback, result counts, commit handling, or post-commit
notifier ordering.

- [ ] **Step 8: Verify pipeline GREEN and commit**

```bash
cargo test -p note-pipelines --test bulk_update_note_labels_test
cargo test -p note-pipelines
git add crates/note-pipelines/src/bulk_update_note_labels.rs \
  crates/note-pipelines/tests/bulk_update_note_labels_test.rs
git commit -m "feat(pipelines): remove note labels atomically"
```

## Task 3: Extend the MCP Contract Without Changing Inventory

**Files:**
- Modify: `crates/note-mcp/src/tools.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`
- Modify: `crates/note-mcp/tests/org_inventory_test.rs`

- [ ] **Step 1: Write failing tool-adapter behavior**

Keep the existing set-only tool test with `remove: vec![]`. Add a remove-only call:

```rust
BulkUpdateNoteLabelsToolInput {
    selector: "type=ietf-rfc".into(),
    set: vec![],
    remove: vec!["type".into()],
}
```

Assert expected counts, absence of `type`, and preservation of unrelated labels.

- [ ] **Step 2: Write failing MCP schema and handler tests**

Update exact schema assertions so the input has exactly `selector`, `set`, `remove`; only
`selector` is required; `additionalProperties == false`; set remains an array of two-string arrays;
remove is an array of strings. Add remove-only handler success plus empty-mutations and overlap
caller-error cases. Preserve unknown-field and sanitized rollback/storage failure coverage.

Keep inventories at 12 note tools, 36 Org tools, 48 aggregate tools.

- [ ] **Step 3: Run MCP tests to verify RED**

```bash
cargo test -p note-mcp stdio::tests:: --lib
cargo test -p note-mcp --test tools_test
cargo test -p note-mcp --test org_inventory_test
```

Expected: request/schema assertions fail because `remove` is absent and `set` is required.

- [ ] **Step 4: Extend transport-neutral MCP DTOs**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsToolInput {
    pub selector: String,
    #[serde(default)]
    pub set: Vec<(String, String)>,
    #[serde(default)]
    pub remove: Vec<String>,
}
```

Pass both vectors into `PipelineBulkUpdateNoteLabelsInput`.

- [ ] **Step 5: Extend the closed MCP request schema**

```rust
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsRequest {
    /// Label selector used to choose active notes.
    pub selector: String,
    /// Label `(key, value)` pairs to set or replace.
    #[serde(default)]
    pub set: Vec<(String, String)>,
    /// Label keys to remove from every fixed matching note.
    #[serde(default)]
    pub remove: Vec<String>,
}
```

Pass `remove` through conversion. Change the tool description to:

```text
Atomically set, replace, or remove labels on active notes matching a label selector while preserving unrelated labels.
```

Reuse existing typed/sanitized error adaptation.

- [ ] **Step 6: Verify MCP GREEN and commit**

```bash
cargo test -p note-mcp
git add crates/note-mcp/src/tools.rs crates/note-mcp/src/stdio.rs \
  crates/note-mcp/tests/tools_test.rs crates/note-mcp/tests/org_inventory_test.rs
git commit -m "feat(mcp): remove labels in bulk updates"
```

Expected: behavior, closed schema, error sanitization, and exact inventory pass.

## Task 4: Extend REST/OpenAPI and Cross-Transport Parity

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/tests/bulk_label_transport_conformance_test.rs`

- [ ] **Step 1: Write failing REST behavior tests**

Send remove-only JSON using the existing `type=ietf-rfc` test fixtures:

```json
{"selector":"type=ietf-rfc","remove":["type"]}
```

Assert HTTP 200, note-level counts, removed `type`, preserved project/unrelated labels, unchanged
revision/content/attachments/derived state, and dashboard invalidation only when a note changes. Add
REST 400 cases for empty set/remove, duplicate remove, overlap, malformed JSON, and unknown fields.
Keep the malformed/unknown extraction body exactly `invalid bulk note label update request`.

- [ ] **Step 2: Write failing OpenAPI assertions**

Update `openapi_documents_exact_bulk_label_contract`:

```rust
assert_eq!(required, vec!["selector"]);
assert_eq!(properties, vec!["remove", "selector", "set"]);
assert_eq!(schema["additionalProperties"], false);
assert_eq!(schema["properties"]["remove"]["type"], "array");
assert_eq!(schema["properties"]["remove"]["items"]["type"], "string");
```

The path/operation inventory stays unchanged.

- [ ] **Step 3: Write failing MCP/REST conformance coverage**

Send the same mixed request to independently seeded REST and MCP routers:

```json
{
  "selector": "type=ietf-rfc",
  "set": [["project", "ietf-rfc"]],
  "remove": ["owner"]
}
```

Assert identical counts and final sorted label vectors. Add remove-only MCP mutation coverage to the
shared-dashboard test so REST reads refreshed counts after the post-commit notifier.

- [ ] **Step 4: Run REST tests to verify RED**

```bash
cargo test -p note-server notes_api::tests:: --lib
cargo test -p note-server --test bulk_label_transport_conformance_test
```

Expected: DTO/schema/conformance assertions fail because REST has no `remove` field.

- [ ] **Step 5: Extend the REST DTO and handler**

```rust
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BulkUpdateNoteLabelsRequest {
    #[schema(min_length = 1)]
    selector: String,
    #[serde(default)]
    #[schema(
        schema_with = crate::openapi::label_pairs_with_empty_default_schema,
        required = false
    )]
    set: Vec<(String, String)>,
    #[serde(default)]
    #[schema(default = json!([]), required = false)]
    remove: Vec<String>,
}
```

Pass both vectors:

```rust
BulkUpdateNoteLabelsInput {
    selector: req.selector,
    set: req.set,
    remove: req.remove,
}
```

Keep route, response, status mapping, notifier composition, and inventory unchanged.

- [ ] **Step 6: Verify REST GREEN and commit**

```bash
cargo test -p note-server notes_api::tests:: --lib
cargo test -p note-server --test bulk_label_transport_conformance_test
cargo test -p note-server --test org_api_inventory_test
git add crates/note-server/src/notes_api.rs \
  crates/note-server/tests/bulk_label_transport_conformance_test.rs
git commit -m "feat(api): remove labels in bulk updates"
```

Expected: behavior, exact OpenAPI schema, parity, cache refresh, and inventory pass.

## Task 5: Document and Verify the Integrated Feature

**Files:**
- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Update public documentation**

Document both shapes:

```json
{"selector":"type=ietf-rfc","set":[["project","ietf-rfc"]]}
```

```json
{"selector":"type=bear_note","remove":["type"]}
```

State that set/remove are optional individually but at least one mutation is required; keys must be
unique, overlap is rejected, targets are fixed before mutation, unrelated labels and note
revision/derived state are preserved, and exact replays are no-ops. Keep inventory at 12 note + 36
Org = 48 MCP tools and keep the REST operation count unchanged.

- [ ] **Step 2: Run focused formatting and contract checks**

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test transactions_test
cargo test -p note-pipelines --test bulk_update_note_labels_test
cargo test -p note-mcp
cargo test -p note-server
git diff --check
```

Expected: available tests pass. Report PostgreSQL live self-skips if `TEST_DATABASE_URL` is absent.

- [ ] **Step 3: Commit documentation**

```bash
git add README.md docs/design.md
git commit -m "docs: document bulk label removal"
```

- [ ] **Step 4: Run authoritative serialized verification**

```bash
cargo test --workspace -j 1
cargo fmt --all -- --check
cargo check --workspace --all-targets
git diff --check
git status --short --branch
```

Expected: all workspace/doc tests exit 0 and the worktree is clean. The frontend crate is not
changed; Docker release build during deployment remains the frontend production gate.

## Task 6: Review, Integrate, Deploy, and Execute Production Removal

**Files:**
- No source files; this task performs review, integration, deployment, and the approved data change.

- [ ] **Step 1: Request final code review**

Review the complete base-to-HEAD diff for fixed target stability, set/remove validation, rollback,
adapter parity, schema optionality, cache invalidation, and inventory stability. Fix every
Critical/Important finding with focused tests and re-run affected suites.

- [ ] **Step 2: Merge locally without touching unrelated worktrees**

Inspect the root checkout and registered worktrees. Merge only `codex/bulk-note-label-remove` into
current local `main`, preserve concurrent changes when resolving conflicts, rerun focused integration
tests, then remove this worktree and delete this branch. Do not push Git or modify other active
worktrees unless separately requested.

- [ ] **Step 3: Deploy through the approved ms04 workflow**

Follow `.agents/skills/deploy/SKILL.md` exactly:

```text
local docker buildx --load
-> inspect local image
-> docker save | gzip | ssh ms04 podman load
-> push the same tag to GHCR and inspect manifest
-> restart only podman-agent-note.service
-> compare wanted/running image IDs
-> bounded host/public readiness
```

If proxy probes fail but direct access succeeds, clear build proxy arguments for the retry. Never
transfer an image from a failed build. Preserve `pullPolicy = "always"` and the Nix-managed unit.

- [ ] **Step 4: Snapshot production counts**

```bash
curl -fsS -G https://agent-note.gsmlg.net/api/notes/count \
  --data-urlencode 'label=type=bear_note'
curl -fsS -G https://agent-note.gsmlg.net/api/notes/count \
  --data-urlencode 'label=project=bear_note'
```

The first is expected `matched`; the second must remain unchanged after removal.

- [ ] **Step 5: Execute the approved mutation once**

```bash
curl -fsS -X POST https://agent-note.gsmlg.net/api/notes/bulk-labels \
  -H 'Content-Type: application/json' \
  --data '{"selector":"type=bear_note","remove":["type"]}'
```

Require `matched == updated + unchanged`. On any error, stop; never fall back to a per-note loop or
delete the `type` catalog key.

- [ ] **Step 6: Verify production state and idempotency**

Assert:

```text
count(type=bear_note) == 0
count(project=bear_note) == pre-mutation project count
dashboard HTTP status == 200
```

Replay the same removal. Since the selector key is gone from every fixed target, require:

```json
{"matched":0,"updated":0,"unchanged":0}
```

Finally confirm OpenAPI exposes optional `remove`, MCP `tools/list` retains
`bulk_update_note_labels`, the service is active, running image matches the published tag, and
public HTTP returns 200.
