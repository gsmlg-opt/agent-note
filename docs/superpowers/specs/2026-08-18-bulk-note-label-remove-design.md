# Atomic Bulk Note Label Removal Design

## Status

Approved for implementation on 2026-08-18.

## Goal

Extend the existing `bulk_update_note_labels` pipeline, MCP tool, and
`POST /api/notes/bulk-labels` REST endpoint so callers can atomically remove label keys from every
active note matching a label selector.

The production operation that motivates this extension is:

```json
{
  "selector": "type=bear_note",
  "remove": ["type"]
}
```

It must remove `type=bear_note` from the transaction-local fixed target set while preserving
`project=bear_note`, every other unrelated label, note content, attachments, note revision,
chunks, embeddings, and derived state.

## Alternatives Considered

### 1. Extend the existing atomic bulk operation — selected

Add a `remove` list beside the existing `set` assignments. Reuse the current fixed-target,
immediate-transaction pipeline and note-level result counts. This keeps MCP and REST behavior
identical and preserves all-or-nothing behavior across more than 1,000 notes.

### 2. Rewrite each note through the ordinary update API — rejected

A client loop would require paging, hydration, revision retries, and complete label-vector
replacement for 1,781 notes. It cannot provide cross-note atomicity and risks changing content,
attachments, revisions, or derived state.

### 3. Delete the `type` label catalog key — rejected

`DELETE /api/labels/type` removes the key and all values from every note, not only
`type=bear_note`. That scope is destructive and does not match the request.

## Public Contract

The MCP tool name and REST path stay unchanged.

Requests contain:

- required nonblank `selector`;
- optional `set`, defaulting to an empty list;
- optional `remove`, defaulting to an empty list.

At least one mutation must be requested across `set` and `remove`.

Examples:

```json
{
  "selector": "type=ietf-rfc",
  "set": [["project", "ietf-rfc"]]
}
```

```json
{
  "selector": "type=bear_note",
  "remove": ["type"]
}
```

```json
{
  "selector": "status=stale",
  "set": [["status", "active"]],
  "remove": ["legacy-owner"]
}
```

Existing set-only clients remain compatible. Both transports retain closed request schemas and
the existing response:

```json
{
  "matched": 1781,
  "updated": 1781,
  "unchanged": 0
}
```

`updated` and `unchanged` are note counts, not operation counts. A note changed by multiple set or
remove operations contributes once to `updated`.

## Validation

Validation happens before opening a transaction:

- the selector follows the existing strict grammar and key validation;
- every set and remove key is a valid label key;
- set keys are unique;
- remove keys are unique;
- a key cannot appear in both `set` and `remove`;
- an empty mutation request is rejected.

The current `EmptySet` validation error becomes an empty-mutations error with a stable caller-safe
message. Existing malformed-selector, invalid-key, invalid-value, and duplicate-key behavior stays
caller-visible through MCP invalid parameters and REST HTTP 400.

## Storage Contract

Add a label repository method:

```rust
async fn remove_note_label(&self, note_id: &str, key: &str) -> StorageResult<bool>;
```

It deletes only the `(note_id, label_key_id)` association and returns `true` when a row was
deleted. An unknown catalog key or an absent association returns `false`; removal never creates or
deletes a catalog key.

Turso and PostgreSQL implement equivalent single-statement deletion behavior. Shared storage
contract tests cover changed, absent, unrelated-label preservation, and rollback cases.

## Transaction and Data Flow

1. Validate the complete request before opening storage.
2. Begin an immediate transaction.
3. Select and lock every matching active note ID once, with no pagination limit.
4. If no notes match, roll back and return zero counts without creating label keys.
5. Resolve/create and type-check set targets exactly as today. Remove targets do not require a
   catalog row and never create one.
6. For each fixed note ID:
   - remove every requested key;
   - apply every requested set assignment;
   - advance `updated_at` once only if any operation changed the note.
7. Commit all changes together or roll back all label, catalog, and timestamp changes.
8. Notify note mutation listeners once, after a successful commit, only when `updated > 0`.

Set and remove order is not observable because overlap is rejected.

Removing a selector key does not shrink the target set: target IDs are fixed before any mutation.

## Preserved Invariants

- Only active notes are targeted.
- Matching is not capped by list pagination.
- Unrelated labels remain unchanged.
- Note title, content, attachments, revision, chunks, embeddings, and jobs remain unchanged.
- `updated_at` advances monotonically only for changed notes.
- Exact replays are no-ops and return all matched notes as `unchanged`.
- Turso and PostgreSQL retain equivalent locking and rollback behavior.
- Dashboard cache invalidation remains transport-neutral and post-commit.

## Transport and Documentation Changes

- Add optional `remove` to the MCP request schema and tool adapter.
- Add optional `remove` to the REST/OpenAPI request schema and handler adapter.
- Update the tool description, README, and `docs/design.md` to say set, replace, or remove.
- Keep the note-tool inventory at 12 and the aggregate MCP inventory at 48.
- Keep the REST path and operation count unchanged.

## Verification

Test-driven coverage must prove:

- remove-only requests remove matching labels and preserve unrelated state;
- mixed set/remove requests count each note once;
- removing an absent or unknown key is an exact no-op;
- duplicate remove keys, set/remove overlap, and empty mutations fail before transaction begin;
- selector-key removal uses the original fixed target IDs;
- repository, commit, and rollback failures leave every label and timestamp unchanged;
- Turso and PostgreSQL storage contracts agree;
- MCP and REST schemas are closed and expose optional `set`/`remove` correctly;
- MCP and REST produce identical results and final labels;
- changed MCP operations invalidate the shared dashboard cache;
- existing set-only behavior remains unchanged.

Before integration, run focused storage, pipeline, MCP, REST, and conformance tests, formatting,
workspace all-target checks, and the serialized full workspace suite. PostgreSQL live cases remain
conditional on `TEST_DATABASE_URL` and must be reported if unavailable.

## Production Completion

After local integration and the standard `ms04` deployment workflow, execute:

```json
{
  "selector": "type=bear_note",
  "remove": ["type"]
}
```

Before the mutation, record `count(type=bear_note)` and `count(project=bear_note)` so verification
does not assume that every existing `project=bear_note` note also has the removed type.

Then verify:

- the operation reports the expected matched/updated/unchanged counts;
- `count(type=bear_note) == 0`;
- `count(project=bear_note)` is unchanged from its pre-mutation value;
- replaying the removal matches zero notes and makes no further changes;
- dashboard and public API readiness remain healthy.
