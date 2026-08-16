# Bulk Note Label Updates Design

## Goal

Add one atomic, label-only operation that applies desired label values to every
active note selected by the existing label-selector grammar. Expose the same
operation through MCP and REST without adding frontend or CLI behavior.

The motivating request is:

```json
{
  "selector": "type=ietf-rfc",
  "set": [["project", "IETF-RFC"]]
}
```

Success means:

- every active note matching `type=ietf-rfc` has `project=IETF-RFC` afterward;
- a different existing `project` value is replaced and an exact value is left
  unchanged;
- unrelated labels and all note content, attachments, revisions, chunks, and
  embedding state remain unchanged;
- all matching-note changes commit together or all roll back;
- MCP and REST share one request, result, validation, and transaction contract.

## Public Contract

Add the MCP tool `bulk_update_note_labels` and the REST endpoint
`POST /api/notes/bulk-labels`. Both accept:

```json
{
  "selector": "type=ietf-rfc",
  "set": [
    ["project", "IETF-RFC"]
  ]
}
```

`selector` uses the existing label-filter grammar, including AND composition,
presence terms, typed comparisons, and case-insensitive string-match operators.
Only active notes participate; soft-deleted notes are excluded by the storage
selection path.

`set` is a nonempty array of `(key, value)` pairs. One request may set multiple
label keys. Target keys must be unique within the request. Setting a key means
desired-state replacement: an absent label is added, an exact key/value pair is
unchanged, and a different value for the key is replaced. Labels whose keys are
not present in `set` are preserved.

Both surfaces return:

```json
{
  "matched": 42,
  "updated": 40,
  "unchanged": 2
}
```

`updated` counts notes on which at least one requested label changed.
`unchanged` is `matched - updated`, not the number of unchanged label pairs.
The response is bounded and does not return note IDs.

## Validation and Catalog Behavior

Reject a blank selector so an omitted filter cannot mutate the whole active-note
collection. Reject malformed selector terms, an empty `set`, duplicate target
keys, invalid label keys, and invalid values for existing typed label keys before
performing any label write.

Existing label keys keep their cataloged value type. Values are validated with
the same type rules used by save and update. A missing target key is auto-created
as `text`, matching existing note-write behavior, but only after the operation has
found at least one matching note. A zero-match request therefore cannot change the
label-key catalog.

A syntactically valid selector that references a missing key, contains an invalid
regular expression, or otherwise matches no note is a successful zero-match
operation. It returns zero for all counts.

## Storage Primitives

Do not implement this feature through `update_note_fields`. That path replaces a
note's complete label vector, updates content revision state, and synchronizes
embedding jobs. Bulk label updates need narrow repository operations instead.

Extend the storage contract with operations that:

- select and lock active note IDs matching parsed selectors for a bulk mutation;
- set one note label with insert-or-update semantics and report whether its value
  changed;
- monotonically advance `updated_at` for a changed note without changing its
  content revision or other fields.

Turso uses its immediate-writer transaction to fix the selected target set during
the mutation. PostgreSQL selects matching note rows with update locks inside the
transaction. The design promises that the IDs returned by the target-selection
phase are fixed for this operation; it does not claim a snapshot earlier than
that selection phase.

The label set operation must be idempotent. It inserts a missing `(note, key)`
row, updates a different stored value, and reports no change for an exact stored
value. The existing `(note_id, label_key_id)` primary key continues enforcing one
value per label key per note. No schema migration or new index is required.

## Pipeline and Transaction Flow

Add a transport-neutral pipeline input and result for bulk label updates. The
pipeline runs in this order:

1. Trim and validate the selector and every target key, and reject duplicate
   assignment keys.
2. Begin an immediate write transaction.
3. Parse the selector through the existing shared selector semantics, select and
   lock the matching active-note IDs, and keep that ID set fixed.
4. If no IDs match, roll back the empty transaction and return zero counts without
   creating catalog keys.
5. Load the label-key catalog, determine missing target keys, and validate every
   value against its existing or default `text` type.
6. Create missing target keys within the transaction.
7. Set every requested label on every target note, recording whether each note had
   any actual change.
8. Monotonically advance `updated_at` only for changed notes. Notes changed in the
   same batch may share the same timestamp, but each timestamp must be later than
   that note's stored value.
9. Commit once and return `{matched, updated, unchanged}`.

Any validation, repository, or commit failure rolls back target labels, created
catalog keys, and timestamps. The operation never partially succeeds and never
returns failed IDs. It does not run duplicate-note rules, touch attachments, bump
`note_revision`, rewrite chunks, or enqueue embedding jobs.

## MCP Surface

Add a closed MCP input schema with exactly `selector` and `set`, and a closed
output schema with exactly `matched`, `updated`, and `unchanged`. The tool
description must state that the operation:

- affects active matching notes only;
- replaces specified label values while preserving other labels;
- is atomic.

The stdio and Streamable HTTP transports continue sharing `NoteMcpServer`, so one
tool registration serves both. Update the exact tool inventory and schema tests.
Map request-validation failures to the existing MCP invalid-parameters error and
storage/transaction failures to the existing internal-error shape without
exposing backend details.

## REST Surface

Add `POST /api/notes/bulk-labels` to the notes router and generated OpenAPI
document. Its request and response fields mirror MCP exactly.

Return:

- `200 OK` with counts for success, including zero matches;
- `400 Bad Request` for blank or malformed selectors, empty or duplicate
  assignments, invalid label keys, and typed-value failures;
- `500 Internal Server Error` for storage or transaction failures.

Invalidate the dashboard cache only when `updated > 0`, because only then did note
metadata change. The endpoint does not accept pagination; selection is performed
by storage and must handle more than 1,000 matches.

## Testing

Implementation follows test-driven development.

### Shared Storage Contracts

Exercise both Turso and PostgreSQL adapters for:

- inserting a missing note label;
- replacing a different value while preserving unrelated labels;
- reporting an exact value as unchanged;
- preserving one row per note/key;
- selecting only active matching notes and fixing the selected ID set;
- monotonically touching only changed-note timestamps;
- rolling back set-label, catalog, and timestamp writes together.

Backend-focused tests cover Turso immediate-writer behavior and PostgreSQL row
locking. Live PostgreSQL cases remain conditional on `TEST_DATABASE_URL`, matching
the current adapter test convention.

### Pipeline

Add focused pipeline tests for:

- the motivating `type=ietf-rfc` to `project=IETF-RFC` update;
- add, replace, exact-value no-op, and mixed updated/unchanged counts;
- multiple assignments on one request;
- preserving unrelated labels, content, attachments, created timestamps,
  revisions, chunks, vectors, and embedding jobs;
- excluding soft-deleted matching notes;
- zero matches and no catalog creation;
- more than 1,000 matches without transport pagination;
- selectors that use a key also changed by the operation, proving the initially
  selected ID set remains fixed;
- blank/malformed selectors, empty assignments, duplicate keys, invalid target
  keys, and invalid values for each typed-label kind;
- an injected failure after writes begin, proving complete rollback;
- unchanged notes retaining `updated_at` and changed notes advancing it.

### MCP and REST

MCP tests verify the exact closed request/response schemas, tool inventory,
request/result delegation, zero-match success, count values, and error mapping.

REST tests verify handler behavior, status mapping, dashboard-cache invalidation,
route registration, and generated OpenAPI request/response contracts. One
transport-conformance test sends equivalent MCP and REST requests and asserts the
same observable result.

Run focused tests for `note-storage`, `note-storage-contract-tests`,
`note-storage-turso`, `note-storage-pg`, `note-pipelines`, `note-mcp`, and
`note-server`. Finish with native formatting, workspace checks, and the workspace
test suite. The frontend crate is outside this change.

## Scope

In scope:

- atomic desired-state setting of one or more label keys selected by the existing
  label grammar;
- storage support in both Turso and PostgreSQL;
- a shared pipeline;
- MCP and REST exposure;
- OpenAPI, README, and focused tests.

Out of scope:

- removing labels;
- conflict policies other than replace;
- best-effort or per-note failure results;
- selecting soft-deleted notes;
- a Notes UI bulk action;
- a CLI command;
- changing selector operator semantics;
- returning every matched or changed note ID;
- database schema migrations or new indexes.
