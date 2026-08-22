# Notes Batch Actions Design

**Date:** 2026-08-23

## Goal

Add current-page multi-selection to `/notes` so users can soft-delete selected notes or add,
update, and remove labels from exactly the selected notes. Also make the shared add/edit note
Markdown textarea grow with its content through the native CSS field-sizing behavior.

## Scope

This work includes:

- checkbox selection in both the normal Notes table and the scored search-results table;
- exact-ID, revision-aware REST and pipeline operations for atomic batch label mutation and
  atomic batch soft deletion;
- Add label, Update label, Remove label, and Delete selected controls and dialogs;
- result and error feedback, selection reconciliation, and refreshed page handling;
- OpenAPI and focused backend/frontend tests; and
- a scoped `field-sizing: content` rule for the shared note Markdown textarea.

This work does not include:

- selection that persists across pages, searches, filters, or page-size changes;
- selecting all notes matching the current query beyond the rendered page;
- permanent deletion or changes to the Trash page;
- changing the existing selector-based `POST /api/notes/bulk-labels` contract;
- new MCP tools;
- a JavaScript textarea-autosizing fallback; or
- deployment, release, or push work.

## Chosen Approach

Add two exact-ID batch REST contracts rather than reusing the selector-based bulk endpoint or
looping over full-note updates in the frontend:

- `POST /api/notes/batch-labels`
- `POST /api/notes/batch-delete`

Both call transport-neutral pipelines and execute in one immediate transaction. The existing
`POST /api/notes/bulk-labels` endpoint remains the mechanism for mutating every active note that
matches a label selector. Keeping the contracts separate prevents checkbox selection from
silently reaching hidden selector matches and keeps exact-ID result semantics clear.

A frontend-only fan-out is rejected for label changes because full-note PUT replaces the complete
note state and unnecessarily exercises content, attachment, chunk, and embedding behavior. It is
also rejected for deletion because partial completion is less predictable than the atomic batch
behavior available through a dedicated pipeline.

## REST Contracts

### Shared target

Each request carries between 1 and 1,000 unique targets:

```json
{
  "id": "note-id",
  "expected_revision": 3
}
```

Empty target lists, duplicate IDs, blank IDs, and requests above 1,000 targets are caller errors.
The maximum matches the largest rendered Notes page.

### Batch labels

The request uses a tagged action with one of three shapes:

```json
{
  "notes": [{ "id": "note-id", "expected_revision": 3 }],
  "action": {
    "type": "add",
    "key": "project",
    "value": "agent-note"
  }
}
```

```json
{
  "notes": [{ "id": "note-id", "expected_revision": 3 }],
  "action": {
    "type": "update",
    "from_key": "status",
    "key": "stage",
    "value": "review"
  }
}
```

```json
{
  "notes": [{ "id": "note-id", "expected_revision": 3 }],
  "action": {
    "type": "remove",
    "key": "owner"
  }
}
```

The response is:

```json
{
  "requested": 3,
  "updated": 2,
  "unchanged": 1
}
```

`requested` counts validated explicit targets. `updated` and `unchanged` are note-level counts,
so a note is counted once even when Update both removes and sets a key.

### Batch delete

The request is:

```json
{
  "notes": [{ "id": "note-id", "expected_revision": 3 }]
}
```

The response is:

```json
{
  "requested": 3,
  "deleted": 3
}
```

Deletion is soft deletion. Successfully deleted notes move to Trash and disappear from active
list and search results.

## Label Semantics

### Add

Add sets `key=value` only on selected notes that do not already contain `key`. A note that already
has the key is unchanged even when its existing value differs from the requested value.

### Update

Update applies only to selected notes containing `from_key`:

- when `from_key == key`, replace the value in place;
- when the keys differ, remove `from_key` and set `key=value`; and
- when the destination key already exists, replace its value with the requested value.

Notes missing `from_key` are unchanged. Key renaming and destination replacement occur within the
same note-level transaction step and count as one updated note.

### Remove

Remove deletes the selected key association from notes that contain it. Notes missing the key are
unchanged. The label catalog entry is never deleted.

### Label definitions and typed values

The UI offers catalog keys through a datalist and chooses the value input type from the catalog.
Custom destination keys remain allowed, matching the existing note editor and selector-based bulk
pipeline; they are created as text label definitions. Existing catalog types are validated before
any mutation begins. Invalid typed values roll back the whole request.

Label actions preserve note content, attachments, chunks, embeddings, revision, and unrelated
labels. They advance `updated_at` once for each changed note, matching the existing bulk label
contract. Label-only transactions serialize through the immediate writer transaction; because
label mutations do not advance note revision, concurrent label-only batches apply in commit order
and evaluate Add/Update/Remove applicability against the transaction's current label state.

## Pipeline and Transaction Design

Introduce transport-neutral batch-label and batch-delete inputs/results in `note-pipelines`.
REST handlers only deserialize, call the pipeline, map errors, and invalidate relevant cached or
published views after a changed commit.

For both operations, the pipeline:

1. validates request shape, target count, uniqueness, keys, and typed values before mutation;
2. sorts targets by ID for deterministic locking;
3. begins one immediate transaction;
4. resolves and locks every active target;
5. verifies every `expected_revision` before applying any mutation;
6. applies the operation to the fixed target set; and
7. commits once, or rolls back the complete request on any failure.

Missing or soft-deleted targets fail the request without changing any target. A stale revision also
fails the entire request. Inapplicable label actions are successful no-ops and contribute to the
`unchanged` count.

Batch deletion reuses the single-note soft-delete invariants: increment revision, set `deleted_at`,
and clear active search data for each target. No target is deleted unless every target passes the
preflight.

## Error Handling

REST errors follow the existing Notes mutation conventions:

- `400 Bad Request` for malformed input, invalid keys or typed values, empty/duplicate/oversized
  target sets, or unknown action fields;
- `404 Not Found` when any requested note is missing or already deleted;
- `409 Conflict` when any expected revision is stale; and
- a sanitized `500 Internal Server Error` for storage or transaction failures.

Responses do not report partial success because mutations are atomic. The frontend keeps the
selection and dialog input after an error, shows the error in an accessible alert, and offers the
existing reload path for stale revisions.

## Frontend Interaction

`NotesPage` owns an ephemeral selection reducer keyed by note ID. Both normal and scored tables
render the same selection column:

- each row has an accessible checkbox;
- the header checkbox selects only the currently rendered rows;
- the header checkbox becomes indeterminate for a partial selection; and
- selection is reconciled with the exact visible IDs after refresh or URL-backed page, page-size,
  search, or label-filter changes.

A compact toolbar above the table shows the selected count and four actions:

- Add label
- Update label
- Remove label
- Delete selected

Actions are disabled when no rows are selected and all selection/mutation controls are disabled
while a request is in flight. This freezes the exact UI target set and avoids stale async
completion overwriting newer selection state.

Add, Update, and Remove open the existing accessible `Modal` with focused label controls. Add uses
key and value inputs. Update uses source key, destination key, and destination value inputs. Remove
uses a key input. Delete opens a counted destructive confirmation dialog. The existing per-row
View, Edit, and Remove actions remain unchanged.

On success, the page shows an accessible result summary, clears selection, and refreshes the
current URL-backed view. If deletion leaves the requested page beyond the available result range,
the URL is replaced with the nearest valid page. On failure, the selected rows remain selected.

## Markdown Textarea Sizing

Both New Note and Edit Note render the shared `NoteEditor`, whose `MarkdownInput` wrapper already
has the `note-content-input` class. Add this scoped rule to `crates/note-frontend/app.css`:

```css
.note-content-input .markdown-input-field {
    field-sizing: content;
}
```

`field-sizing` is the valid CSS property; `content-sizing` is not. The existing `min-height: 18rem`,
`width: 100%`, and `resize: vertical` rules remain. Browsers without `field-sizing` support ignore
the declaration and retain the existing usable textarea. No maximum height or JavaScript fallback
is added.

## Testing and Verification

Implementation follows red-green-refactor. Each production behavior is preceded by a focused test
that fails for the missing behavior.

Backend coverage includes:

- request validation for empty, duplicate, blank, and oversized target lists;
- Add, same-key Update, rename Update, destination collision, Remove, and mixed applicable/no-op
  selections;
- typed-value validation and missing text-key creation;
- missing/deleted target and stale-revision rollback;
- deterministic target handling and full rollback on injected storage failures;
- preservation of content, attachments, chunks, embeddings, revision, and unrelated labels;
- one timestamp change per changed note;
- atomic soft deletion and search cleanup;
- PostgreSQL and Turso storage/transaction contracts; and
- REST status mapping, sanitized failures, response shapes, OpenAPI, and route inventory.

Frontend coverage includes:

- selection reducer behavior and visible-row reconciliation;
- select-all, partial-selection, and accessible checkbox markup;
- selection columns in normal and scored tables;
- action enablement, dialog copy, payload construction, and result summaries;
- successful clear/refresh and error selection retention;
- page clamping after deletion; and
- a CSS contract test for the scoped `field-sizing: content` rule.

Verification is limited to affected crates and surfaces:

- focused `note-pipelines`, storage-adapter, and `note-server` tests;
- `cargo test --manifest-path crates/note-frontend/Cargo.toml`;
- `cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown`;
- targeted `rustfmt`, `cargo fmt --all -- --check` where the affected native files are clean, and
  `git diff --check`; and
- browser verification of keyboard selection, the four dialogs, success/error focus and feedback,
  list/search refresh behavior, and Markdown textarea growth on both New Note and Edit Note.

If `TEST_DATABASE_URL` is unavailable, PostgreSQL live cases are reported as skipped rather than
passed. Pre-existing or out-of-scope failures are reported without modification.
