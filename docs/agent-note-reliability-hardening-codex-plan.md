# Agent Note Reliability Hardening — Codex Implementation Plan

**Status:** Ready for implementation  
**Repository:** `gsmlg-opt/agent-note`  
**Review baseline:** `main@d59bf02` (`2026-08-12`)  
**Prepared:** `2026-08-14`  
**Execution model:** One independently mergeable PR at a time

> **Owner decision — authentication is outside this application.**
>
> Do **not** add authentication, authorization, users, sessions, workspace ACLs, trusted proxy identity headers, TLS termination, or OpenAPI security schemes. Agent Note is intentionally deployed behind an external access-control layer. Authentication is not a missing feature and must not appear in implementation scope, review findings, acceptance criteria, or follow-up recommendations.

---

## 1. Purpose

Harden Agent Note for concurrent agent use, external attachment storage, reliable backup and restore, degraded search operation, and production deployment without rewriting its existing architecture.

The current architecture is fundamentally sound:

- `note-core` and `note-org` hold pure domain logic.
- `note-storage` defines backend-neutral repository contracts.
- Turso and PostgreSQL implement the same storage boundary.
- `note-pipelines` owns effectful workflows.
- REST and MCP call shared pipelines instead of duplicating business rules.
- The Org subsystem already demonstrates the desired reliability level through revisions, idempotency, leases, fencing, attempts, append-only events, and rebuildable projections.

The main objective is to bring ordinary Markdown notes, attachments, backup, and search closer to the reliability discipline already present in the Org subsystem.

This is an incremental hardening plan, not a rewrite.

---

## 2. Codex Execution Contract

### 2.1 Default execution rule

On the first invocation, implement **PR 1 only**. Do not start PR 2 in the same change.

On later invocations, implement the first incomplete PR named by the operator. Every PR must be independently reviewable, testable, and revertible.

Do not create one branch containing the entire roadmap.

### 2.2 Before changing code

1. Fetch the latest `main`.
2. Read the repository `AGENTS.md`, `README.md`, and `docs/design.md`.
3. Compare the current tree with the review baseline `d59bf02`.
4. If the baseline has moved, preserve the intent of this document but adapt paths and types to the current code.
5. Run the relevant existing tests before editing so pre-existing failures are distinguishable from regressions.

### 2.3 During implementation

- Preserve the pure-core/effectful-shell dependency direction.
- Keep transport handlers thin.
- Put reusable mutation and error semantics in pipelines and storage contracts, not Axum, MCP, or Yew components.
- Keep Turso and PostgreSQL behavior covered by shared contract tests.
- Use ordered migrations. Never squash or rewrite migrations already released.
- Avoid broad renames or unrelated cleanup.
- Do not weaken Org revision, event, lease, fencing, idempotency, or audit behavior.
- Do not add `unsafe` code unless an existing FFI boundary strictly requires it.
- Never log attachment bytes, database URLs, API keys, S3 credentials, or Org fencing tokens.
- Update OpenAPI, MCP schemas, README/design documentation, and examples whenever a public contract changes.

### 2.4 End-of-PR report

At the end of each PR, return:

1. Summary of behavior changed.
2. Files changed.
3. Migrations added.
4. Public API or MCP schema changes.
5. Tests added.
6. Exact validation commands run and their results.
7. Known limitations or deferred work.
8. Confirmation that the next PR was not started.

---

## 3. Scope

### 3.1 In scope

1. Optimistic concurrency for ordinary notes.
2. Durable attachment publication and cleanup.
3. Attachment reconciliation and operational diagnostics.
4. Complete, streaming, verifiable backup and restore.
5. Search degradation when embeddings are unavailable.
6. Lexical body retrieval and explicit cross-backend retrieval contracts.
7. Lazy attachment loading and independent attachment mutations in the frontend/API.
8. Startup validation, graceful shutdown, liveness/readiness, CI, and container hardening.
9. Optional Org event streaming after the correctness work is complete.

### 3.2 Explicit non-goals

- Authentication or authorization of any kind.
- Users, accounts, sessions, roles, or ACLs.
- TLS termination or trusted proxy identity processing.
- Replacing Turso or PostgreSQL.
- Replacing Yew, Axum, MCP, or the Rust workspace structure.
- Rewriting the Org subsystem.
- Adding ANN indexes before measurement demonstrates a need.
- Moving business rules into transport-specific code.
- Making S3 or an external embedding service mandatory.
- Guaranteeing strong ranking-score equality between different database FTS engines.

---

## 4. Architecture Invariants

All changes in this plan must preserve these invariants.

### 4.1 Pure domain, explicit effects

Validation, state transitions, conflict interpretation, rank fusion, and deterministic ordering should remain pure where possible. Database access, object storage, inference, clocks, and background execution remain explicit effects passed through existing context boundaries.

### 4.2 One policy boundary, multiple transports

REST and MCP must call the same pipeline operations. They may map transport metadata differently, but they must not implement different mutation rules, revision checks, or conflict semantics.

### 4.3 Backend-neutral contracts

`note-storage` defines required semantics. Turso and PostgreSQL may use different SQL internally, but they must expose equivalent outcomes:

- applied;
- not found;
- conflict;
- storage failure.

### 4.4 Atomic database state

A note mutation and its related labels, chunks, embedding jobs, attachment metadata, and cleanup records must commit or roll back together when they belong to one logical operation.

### 4.5 External object storage is not transactional

Filesystem and S3 operations cannot join a database transaction. Correctness must come from:

- immutable object generations;
- object-first publication;
- database CAS;
- durable cleanup operations;
- idempotent workers;
- reconciliation.

### 4.6 Compatibility must be explicit

Never silently accept a mutation without an expected revision by reading the latest revision server-side. That would preserve the overwrite hazard. A compatibility endpoint may be retained temporarily, but it must be clearly deprecated and must not pretend to provide concurrency safety.

---

## 5. Baseline Findings

| Priority | Finding | Primary code areas | Required outcome |
|---|---|---|---|
| P0 | Ordinary note updates can lose concurrent writes because revision checking is not an atomic CAS | `note-pipelines/src/edit_note.rs`, `update_note.rs`; storage note repositories | Every existing-note mutation requires and atomically checks an expected revision |
| P0 | Attachment metadata can commit before filesystem/S3 publication succeeds | `note-pipelines/src/note_attachments.rs`, `save_note.rs`, `update_note.rs`; `note-attachments` | Database metadata never points to an object that was not durably written first |
| P0 | Current system backup is a notes-only in-memory archive | `note-pipelines/src/export.rs`, `note-server/src/system_api.rs` | Streaming, checksummed, versioned backup with complete durable application state |
| P1 | Embedding failure disables title FTS because embedding is executed first | `note-pipelines/src/search_notes.rs` | Hybrid search degrades to available channels |
| P1 | Body content has dense retrieval but no lexical channel | storage retrieval adapters and search pipeline | Add body/chunk lexical retrieval for exact identifiers and terms |
| P1 | Filtered title ranking semantics differ between Turso and PostgreSQL | adapter `retrieval.rs` files and retrieval contract tests | Define and test an explicit common eligibility/determinism contract |
| P1 | Frontend loads every attachment serially and re-sends all attachment bytes on note updates | `note-frontend/src/api.rs` and note state/pages | Metadata-first note loading and independent attachment operations |
| P1 | Missing packaged `index.html` can produce an empty successful response; shutdown does not drain requests | `note-server/src/main.rs` | Fail-fast startup and graceful shutdown |
| P2 | CI does not enforce the complete documented test/lint surface | `.github/workflows/ci.yml` | Add format, clippy, workspace, frontend, reliability, and multi-backend gates |
| P2 | Container build is effectively x86-64-specific and runs as root | `Dockerfile` | Pinned, non-root, amd64/arm64 image with healthcheck |

---

# PR 1 — Ordinary Note Optimistic Concurrency

## 6. Objective

Prevent silent lost updates when multiple agents or clients mutate the same note.

After this PR, every mutation of an existing note must be an atomic compare-and-swap operation using `note_revision` as the authoritative concurrency token.

The existing line-edit content tag remains useful for detecting content-specific drift, but it is not a substitute for the note revision.

## 7. Required semantics

### 7.1 Expose the revision

Add `revision: i64` to the ordinary note read model and all transport DTOs that clients use before mutation:

- note detail;
- note list item, unless list payload size or compatibility requires a separate decision;
- trash item;
- line-oriented note read result;
- MCP note detail and summary results;
- frontend note state.

At minimum, `GET /api/notes/{id}` and the MCP equivalent must return the revision.

### 7.2 Require an expected revision

The following operations must require `expected_revision`:

- full note update;
- fields-only update;
- line edit;
- label change;
- attachment add/replace;
- attachment delete;
- soft delete;
- restore;
- permanent delete.

A create operation does not use an expected revision.

A missing expected revision must return a caller error. Do not infer it from the current row.

### 7.3 Mutation result

Introduce a backend-neutral result with semantics equivalent to:

```rust
pub enum NoteMutationResult<T> {
    Applied {
        value: T,
        revision: i64,
    },
    NotFound,
    Conflict {
        expected_revision: i64,
        current_revision: i64,
    },
}
```

The exact type name may follow repository conventions. The semantic distinction is mandatory.

### 7.4 Atomic CAS

A storage adapter must perform a conditional mutation equivalent to:

```sql
UPDATE notes
SET
  title = $title,
  content = $content,
  note_revision = note_revision + 1,
  updated_at = $updated_at
WHERE id = $id
  AND note_revision = $expected_revision
  AND deleted_at IS NULL
RETURNING note_revision;
```

If no row is returned, distinguish `NotFound` from `Conflict` inside the same transaction by reading the row state. Do not perform the revision read before opening the transaction and then issue an unconditional update.

For a compound mutation, labels, chunks, embedding jobs, and attachment metadata must be part of the same database transaction. A failed CAS must roll back every related database write.

### 7.5 Revision rules

- Start new notes at the existing initial revision convention.
- Increment exactly once per successful logical mutation.
- Do not increment for a rejected or no-op request unless the existing product deliberately treats the operation as a write.
- Soft delete increments the revision.
- Restore increments the revision.
- Attachment metadata mutations increment the owning note revision.
- Rebuilding embeddings or derived search indexes does not increment the note revision.
- Reads never increment the revision.

### 7.6 Line edit rules

`read_note_lines` must return both:

- `revision` — authoritative note-wide concurrency token;
- `tag` — hash/tag of the currently read content.

`edit_note` must require both values and reject the operation when either is stale.

This catches:

- content changed after the line read;
- labels or attachments changed while content remained identical;
- another mutation incremented the note revision.

### 7.7 Structured conflict error

Use the existing Org-style error envelope for ordinary note errors rather than returning a plain string for the new conflict path:

```json
{
  "code": "stale_revision",
  "message": "the note changed after it was read",
  "details": {
    "note_id": "note-id",
    "expected_revision": 5,
    "current_revision": 6
  },
  "retryable": false
}
```

A client must refetch and deliberately merge or reapply its change. It must not blindly retry the stale payload.

REST mapping:

- missing `expected_revision`: `400 Bad Request` with `code = "expected_revision_required"`;
- stale revision: `409 Conflict` with `code = "stale_revision"`;
- missing note: `404 Not Found`;
- storage failure: `500 Internal Server Error` with a safe message.

MCP must expose equivalent structured fields. Protocol framing may differ, but conflict semantics must match REST.

### 7.8 Public API transition

Prefer an explicit `expected_revision` field in mutation input schemas because MCP also needs that field.

Update the bundled frontend and all MCP schemas in the same PR. Do not preserve unsafe behavior by making the field optional.

If a legacy endpoint must temporarily remain for compatibility, document it as unsafe/deprecated and do not route new first-party clients through it.

## 8. Expected code areas

### `note-core`

- Add revision to ordinary note/read models.
- Add or reuse typed concurrency errors if they belong in the pure domain.
- Keep tag computation pure.

### `note-storage`

- Replace unconditional ordinary-note mutation contracts with CAS-aware contracts.
- Add the backend-neutral applied/not-found/conflict result.
- Ensure attachment metadata mutations can participate in note CAS.

### `note-storage-turso`

- Implement conditional note updates in `src/notes.rs`.
- Use an appropriate transaction mode, but rely on the SQL predicate rather than transaction mode alone.
- Preserve schema upgrade compatibility.

### `note-storage-pg`

- Implement conditional note updates in `src/notes.rs`.
- Use `UPDATE ... WHERE note_revision = ... RETURNING ...`.
- Keep all dependent writes in the same SQL transaction.

### `note-storage-contract-tests`

Add shared concurrency tests in `src/notes.rs` covering both adapters.

### `note-pipelines`

Expected changes include:

- `update_note.rs`;
- `edit_note.rs`;
- `note_attachments.rs`;
- `system.rs` for delete/restore/permanent-delete paths if those functions are located there;
- public exports and input/result DTOs.

Remove the read-revision-then-unconditional-write sequence.

### `note-server`

- Add revision to REST response DTOs.
- Add expected revision to mutation requests.
- Add structured ordinary-note error mapping.
- Update OpenAPI tests and examples.

### `note-mcp`

- Add revision to note outputs.
- Add expected revision to update, edit, attachment, delete, restore, and permanent-delete tool inputs where those tools exist.
- Preserve the single shared pipeline boundary.

### `note-frontend`

- Store revision with the loaded note.
- Send it on mutations.
- On `stale_revision`, preserve the local edit, show a conflict state, and offer refetch/reapply rather than silently discarding content.

## 9. PR 1 required tests

### Storage contract tests

1. Two transactions read revision `5`; first update succeeds to `6`; second returns conflict with current revision `6`.
2. A conflict does not change title, content, labels, chunks, jobs, or attachment metadata.
3. A missing ID returns `NotFound`, not `Conflict`.
4. Soft delete with the correct revision succeeds and increments once.
5. Restore with a stale revision fails without restoring.
6. Permanent delete with a stale revision fails without deleting.
7. Attachment metadata mutation with a stale note revision fails atomically.

### Pipeline tests

1. Full update returns the new revision.
2. Fields-only update returns the new revision.
3. Line edit rejects a stale revision even if the content tag still matches.
4. Line edit rejects a stale content tag.
5. Embedding jobs are not enqueued after a conflict.
6. Prepared attachment changes are rolled back or cleaned when CAS fails under the current attachment implementation.

### Transport tests

1. REST detail includes revision.
2. REST stale update returns JSON `409` with the common error shape.
3. MCP stale update exposes the same code and details.
4. OpenAPI marks `expected_revision` required.
5. Built-in frontend sends the loaded revision.

### Concurrency test

Run a real concurrent update test against Turso and PostgreSQL rather than only invoking operations serially. The final database state must contain exactly one of the two writes, and the loser must observe a conflict.

## 10. PR 1 acceptance criteria

- No existing-note write path can update an ordinary note without an expected revision.
- The revision check and database mutation are atomic.
- REST and MCP return equivalent conflict semantics.
- The frontend does not silently overwrite a newer revision.
- Turso and PostgreSQL pass the same storage contract tests.
- All existing Org tests continue to pass.
- No authentication-related code or documentation is added.

---

# PR 2 — Immutable Attachment Generations and Durable Cleanup Records

## 11. Objective

Eliminate the state in which database metadata is committed and active while the referenced filesystem or S3 object was never successfully published.

The design must prefer a safe orphaned object over a database reference to a missing object.

## 12. Required attachment model

### 12.1 Immutable object generations

Each attachment revision must use a new immutable object key. A suggested logical layout is:

```text
notes/{note_id}/{attachment_id}/{generation}-{checksum_prefix}
```

The exact format may differ, but it must satisfy these properties:

- generated by the application, not directly from a user path;
- immutable after successful write;
- unique per attachment generation;
- safe for filesystem and S3;
- original user-visible path remains metadata and is not trusted as an object-store key.

### 12.2 Attachment metadata

Extend attachment metadata with fields equivalent to:

```text
object_key
storage_generation
size_bytes
checksum_sha256
created_at / updated_at as appropriate
```

Expose at least `size_bytes`, `checksum_sha256`, and generation in internal DTOs. Public exposure may be added where useful for download verification and UI display.

### 12.3 Durable cleanup table

Add a backend-neutral cleanup/outbox record equivalent to:

```text
attachment_operations
  id
  kind                 # delete_object initially
  note_id
  attachment_id
  storage_generation
  object_key
  status               # pending | running | completed | dead
  attempts
  next_attempt_at
  lease_owner
  lease_expires_at
  last_error
  created_at
  updated_at
```

Names may follow repository conventions. Required semantics:

- inserted in the same transaction that removes/replaces the database reference;
- safe for multiple server instances;
- claimable with a lease;
- object-not-found on delete is success;
- idempotent after process restart;
- retry metadata survives restart.

## 13. Correct write ordering

### 13.1 Add or replace attachment

Use this order:

```text
1. Validate metadata and content.
2. Compute checksum, size, generation, and immutable object key.
3. Write the complete object to the immutable final key.
4. Verify the object write at the adapter boundary.
5. Start database transaction.
6. CAS the owning note revision.
7. Activate metadata pointing to the new object key.
8. Enqueue cleanup for the previous object, if any.
9. Commit database transaction.
10. Attempt immediate cleanup opportunistically; durable outbox remains authoritative.
```

Important consequences:

- If object write fails, no database change occurs.
- If CAS or database commit fails, the new object may be orphaned but is not referenced. Perform best-effort immediate deletion and let reconciliation handle any remainder.
- If old-object cleanup fails, the note still points to the valid new object and the durable cleanup operation remains retryable.

Do **not** commit active metadata first and publish the initial object afterward.

### 13.2 Create note with attachments

Generate the note ID before object publication, write immutable objects first, then insert the note and attachment metadata in one database transaction.

If the transaction fails, best-effort delete the unreferenced objects. Reconciliation remains the crash-recovery mechanism.

### 13.3 Delete attachment

In one database transaction:

1. CAS the note revision.
2. Remove or deactivate the attachment metadata.
3. Insert a cleanup operation containing the old immutable object key.
4. Commit.

Then attempt cleanup. A cleanup failure must not resurrect metadata or make the note mutation appear uncommitted.

### 13.4 Soft-delete and permanent-delete behavior

- Soft delete preserves attachments so restore remains possible.
- Permanent delete captures all referenced object keys in cleanup records inside the deletion transaction.
- Cleanup occurs after commit and is idempotent.

## 14. Adapter contract changes

Refactor `note-attachments` around object-level operations equivalent to:

```rust
async fn put_immutable(&self, request: PutObjectRequest) -> Result<StoredObject>;
async fn read_object(&self, object_key: &str) -> Result<Vec<u8>>;
async fn head_object(&self, object_key: &str) -> Result<Option<ObjectMetadata>>;
async fn delete_object(&self, object_key: &str) -> Result<DeleteObjectOutcome>;
```

A streaming read/write API is preferred for large attachments, but it may be introduced in PR 6 if changing all transport paths here would make this PR too large.

Filesystem publication must use an atomic temporary-file-to-final-key rename where the platform permits it. S3 publication writes directly to a unique immutable key.

## 15. Migration and compatibility

Do not require an all-at-once rewrite of every existing attachment object during startup.

Implement a compatible path such as:

- existing metadata is assigned a logical legacy object key;
- generation defaults to the initial generation;
- checksum and size can be backfilled lazily or by a maintenance command;
- reads understand legacy and generated keys during the transition;
- the next successful replacement writes the new generated format.

Add:

- the next ordered PostgreSQL migration after the current latest migration;
- the next Turso schema version and upgrade path;
- downgrade is not required, but startup must reject unsupported future schema versions clearly.

## 16. PR 2 required tests

For filesystem and S3-compatible test adapters:

1. Object-write failure leaves database metadata unchanged.
2. CAS conflict after object write leaves the old metadata active; the unreferenced new object is removed immediately or detectable as an orphan.
3. Database failure after object write never activates metadata.
4. Successful replacement points to the new object and creates cleanup for the old object.
5. Delete-object not found is treated as successful cleanup.
6. Replaying the same cleanup operation is harmless.
7. Permanent note deletion creates cleanup records for every referenced object.
8. User-controlled paths cannot escape the attachment root or become raw S3 object keys.
9. Existing legacy objects remain readable after migration.
10. Turso and PostgreSQL expose equivalent cleanup-record semantics.

## 17. PR 2 acceptance criteria

- Active metadata is written only after the referenced immutable object exists.
- Old object deletion is never required for the user-visible mutation to commit.
- Cleanup intent is durable and transactionally coupled to metadata removal.
- All attachment mutations use note CAS from PR 1.
- Existing attachments remain readable through migration.
- Filesystem and S3-compatible tests cover failure boundaries.

---

# PR 3 — Attachment Worker, Reconciliation, and Doctor Command

## 18. Objective

Make attachment cleanup and integrity observable and recoverable after process crashes, transient S3 failures, and partial historical states.

## 19. Cleanup worker

Add a background worker used by the HTTP server and reusable by maintenance commands.

Required behavior:

- atomically claim pending operations with an expiring lease;
- support multiple application instances without double-processing hazards;
- treat object deletion as idempotent;
- use bounded concurrency;
- apply exponential backoff with jitter;
- record sanitized errors;
- move repeatedly failing operations to `dead` after a configurable threshold;
- permit manual retry of dead operations;
- renew or safely expire long-running claims;
- stop claiming new work during graceful shutdown and finish or release current claims.

The MCP stdio path must remain correct even when no long-running HTTP worker exists. Attachment mutations should attempt immediate cleanup after commit, while the durable record guarantees later recovery when a server or maintenance command runs.

## 20. Reconciler

Implement a reconciliation service with dry-run as the safe default.

It must detect:

1. **Missing referenced object** — database metadata points to an object that cannot be found.
2. **Metadata mismatch** — object size or checksum differs from metadata.
3. **Orphan object** — immutable object is not referenced by active metadata or a protected backup.
4. **Stuck cleanup** — pending/running operation exceeded its expected age.
5. **Dead cleanup** — retry budget exhausted.
6. **Legacy object** — readable but not yet on the generation/checksum model.

Automatic policy:

- Never silently delete metadata for a missing referenced object.
- Never silently invent content.
- Orphan deletion requires an age grace period.
- Repair mode must be explicit.
- A dry-run report must make no changes.

For S3, paginate listings and avoid loading the complete bucket inventory into memory. Restrict scanning to the configured prefix.

## 21. Operator interface

Add a command compatible with the existing binary structure, for example:

```bash
agent-note doctor attachments
agent-note doctor attachments --dry-run
agent-note doctor attachments --repair
agent-note attachments cleanup --limit 100
agent-note attachments retry-dead --operation-id <id>
```

Exact command nesting may follow current CLI conventions.

The report should include counts and actionable identifiers without exposing credentials or attachment bytes.

## 22. Health and metrics hooks

Expose internal status fields needed later by readiness/metrics:

- pending cleanup count;
- oldest pending age;
- running count;
- dead count;
- last successful cleanup time;
- missing referenced object count from the latest reconciliation;
- last reconciliation time and result.

Do not introduce a heavy observability framework solely for this PR. Structured tracing plus status APIs is sufficient if that matches current dependencies.

## 23. PR 3 required tests

1. Worker resumes pending work after restart.
2. Expired claims are reclaimable.
3. Two workers do not produce an invalid state when racing for one operation.
4. Transient failure schedules retry with increasing delay.
5. Object-not-found completes the operation.
6. Dead-letter transition occurs at the configured attempt limit.
7. Reconciler detects a missing referenced object.
8. Reconciler detects and dry-runs an orphan.
9. Orphans younger than the grace period are retained.
10. Repair deletes only eligible orphan objects.
11. S3 listing is paginated in the MinIO test suite.
12. Shutdown releases or safely expires worker claims.

## 24. PR 3 acceptance criteria

- Cleanup survives process restart.
- Multi-instance execution is safe.
- Operators can distinguish missing objects, orphans, and retry backlog.
- Dry-run is non-destructive.
- No attachment content or credentials appear in logs or reports.

---

# PR 4 — Backup and Restore v3

## 25. Objective

Replace the misleading notes-only, all-in-memory backup behavior with a versioned, streaming, checksummed backup and a tested restore path.

Until v3 is implemented, documentation must call the existing archive a **notes export**, not a complete system backup.

## 26. Backup content

A v3 archive should use a manifest-driven layout similar to:

```text
manifest.json
labels.jsonl
notes.jsonl
org/workspaces.jsonl
org/documents.jsonl
org/work-items.jsonl
org/attempts.jsonl
org/events.jsonl
org/operation-results.jsonl
org/lease-audit.jsonl
attachment-operations.jsonl
attachments/{note_id}/{attachment_id}/{generation}
checksums.sha256
```

The exact split may be adjusted, but the manifest must declare every entry, count, checksum, schema version, and whether an omitted dataset is rebuildable.

### 26.1 Required durable state

Include:

- label-key catalog;
- ordinary notes and labels;
- ordinary-note revision and deletion state;
- attachment metadata and object bytes;
- canonical Org workspaces/documents;
- durable Org attempts and events;
- idempotency data after token sanitization rules below;
- durable attachment cleanup operations;
- safe logical application settings required to interpret data.

### 26.2 Rebuildable state

Embeddings, FTS indexes, and derived Org projections may be omitted if the manifest marks them as rebuildable and restore automatically rebuilds or queues them.

### 26.3 Never export secrets

Do not include:

- database URLs;
- API keys;
- S3 credentials;
- environment variables containing secrets;
- raw Org fencing tokens;
- token-bearing idempotent results that would re-enable an old ownership claim.

### 26.4 Lease and fencing policy

A restore must never reactivate an old fencing token.

Define and test this policy:

- preserve lease/attempt/event audit information without raw ownership proof;
- invalidate or expire active claims during restore;
- remove or transform token-bearing operation results so stale claim replay cannot return a usable token;
- append or otherwise record a restore-recovery event where the Org model requires audit continuity.

Do not weaken the current rule that fencing tokens are excluded from exports and general application state.

## 27. Manifest

`manifest.json` must include at least:

```text
format_name
format_version
created_at
application_version or source commit when available
source_storage_engine
schema_versions
entry list
record counts
per-entry checksum
attachment object count and total bytes
rebuildable datasets omitted
restore compatibility range
```

Use a deterministic encoding/order where practical so repeated backups are inspectable and testable.

## 28. Streaming and memory bounds

Do not construct the complete export, JSON document, compressed tarball, or attachment set in memory.

Required properties:

- stream records as JSON Lines or another incremental format;
- stream raw attachment entries into the archive;
- compute checksums while streaming;
- apply backpressure;
- keep memory bounded by configured buffers plus the largest unavoidable chunk, not total dataset size;
- cancellation releases database snapshots, backup leases, and temporary resources.

A `tokio::io::duplex`/channel plus blocking tar writer is acceptable if bounded and correctly cancelled. The exact implementation may use another established crate.

## 29. Consistency across database and object storage

A database snapshot alone is not enough because cleanup may remove an attachment generation after the snapshot identifies it but before backup copies it.

Implement a backup barrier or lease with these semantics:

1. Acquire a durable, expiring backup lease.
2. Attachment cleanup workers observe the lease and pause destructive object deletion.
3. Open a consistent database snapshot.
4. Enumerate the snapshot's referenced immutable attachment keys and durable records.
5. Stream those immutable objects while cleanup remains paused.
6. Finish archive and checksums.
7. Release the lease.
8. A crashed backup lease expires automatically.

New attachment writes may continue if safe; old-object cleanup must not delete a generation required by the active backup.

## 30. API and CLI

Provide operator-friendly commands, for example:

```bash
agent-note backup create --output backup.tar.gz
agent-note backup verify backup.tar.gz
agent-note restore --dry-run backup.tar.gz
agent-note restore backup.tar.gz
```

For HTTP, prefer a streaming endpoint. Preserve or deprecate the current path explicitly rather than silently changing a notes-only response into an incompatible full-system format without documentation.

A reasonable transition is:

- document the current endpoint as legacy notes export;
- add the v3 endpoint/format;
- retain the legacy route for a documented compatibility window;
- never label the legacy archive a complete backup.

## 31. Restore behavior

Restore must:

1. Verify manifest compatibility.
2. Verify every checksum before committing the corresponding logical dataset, or stage safely until verification completes.
3. Reject path traversal and duplicate archive entries.
4. Write attachment objects before activating their metadata.
5. Use database transactions for logical groups.
6. Preserve note IDs and revisions.
7. invalidate active Org claims and fencing tokens.
8. Queue embedding and projection rebuilds.
9. Support `--dry-run` without persistent changes.
10. Default to an empty target unless an explicit, well-tested replace/merge policy is selected.

Do not invent merge semantics in the first implementation. Empty-target restore is acceptable and safer.

## 32. PR 4 required tests

1. Round-trip a dataset containing notes, labels, deleted notes, attachments, Org state, attempts, events, and pending attachment cleanup.
2. Restored note revisions equal source revisions.
3. Restored attachment bytes and checksums match.
4. Active Org tokens are not exported and cannot be replayed after restore.
5. Rebuildable indexes/embeddings are restored by rebuild, not copied as opaque state.
6. Corrupted entry checksum fails verification.
7. Missing archive entry fails verification.
8. Duplicate/path-traversal entry is rejected.
9. `--dry-run` makes no persistent change.
10. Backup memory remains bounded on a generated large dataset.
11. Cancelling a backup releases the backup lease.
12. Attachment cleanup does not delete a generation pinned by an active backup.
13. Both Turso and PostgreSQL round-trip tests pass.

## 33. PR 4 acceptance criteria

- Backup is streamed and memory-bounded.
- The manifest is versioned and checksummed.
- Durable application data is included or explicitly declared rebuildable.
- Secrets and raw fencing tokens are excluded.
- Restore is tested, not merely documented.
- The existing notes-only export is no longer presented as a complete backup.

---

# PR 5 — Search Resilience and Body Lexical Retrieval

## 34. Objective

Keep useful search available when the embedding backend is unavailable and improve exact-term retrieval from note bodies.

## 35. Search modes

Add a transport-independent mode equivalent to:

```rust
pub enum SearchMode {
    Hybrid,
    Lexical,
    Dense,
}
```

Behavior:

### `Hybrid`

- Run lexical and dense channels independently.
- If both succeed, fuse them.
- If one fails, return the successful channel with degraded diagnostics.
- If every channel fails, return a search error.

### `Lexical`

- Do not invoke the embedder.
- Search title and body lexical indexes.
- Fail only when lexical storage retrieval fails.

### `Dense`

- Invoke only the dense channel.
- An embedding or dense-storage failure is an error.

Default remains `Hybrid` unless product configuration currently specifies otherwise.

## 36. Retrieval channels

The target channel set is:

```text
title lexical
body/chunk lexical
dense body chunks
        ↓
weighted deterministic RRF
```

Keep title ranking strongly weighted unless evaluation demonstrates a better configuration. Centralize weights in one ranking policy rather than scattering constants across transports or adapters.

## 37. Body lexical index

Add lexical retrieval over note body chunks or normalized body text.

It must improve recall for:

- issue IDs;
- commit hashes;
- API paths;
- module/function names;
- error messages;
- configuration keys;
- URLs;
- uncommon abbreviations.

Use database-native facilities where appropriate, but expose a backend-neutral ranked result contract.

Do not claim high-quality CJK segmentation unless an actual tokenizer supports it. Add CJK evaluation cases to document current behavior and prevent crashes/regressions, but keep product claims accurate.

## 38. Failure isolation

The current search sequence must be restructured so title/body lexical retrieval is not blocked by an embedding call.

Use independent channel results equivalent to:

```rust
struct SearchChannelOutcome<T> {
    channel: SearchChannel,
    result: Result<T, SearchChannelError>,
}
```

Do not swallow failures silently. Return or expose diagnostics such as:

```json
{
  "channels_used": ["title_lexical", "body_lexical"],
  "degraded": true,
  "degraded_reason": "embedding_backend_unavailable"
}
```

## 39. Compatibility of search responses

The current REST search response is an array. Do not silently change a public array into an object without a compatibility plan.

Choose one explicit strategy:

1. Introduce a versioned search response/endpoint containing `{results, retrieval}` while retaining the legacy array endpoint; or
2. Preserve the array and expose degradation through documented response headers, while MCP returns equivalent structured metadata through its supported result mechanism.

Prefer a versioned structured response if the codebase can update all first-party clients cleanly. Update OpenAPI and MCP schemas accordingly.

## 40. Cross-backend retrieval contract

Do not require PostgreSQL and Turso to produce identical raw BM25 scores; their FTS engines differ.

Require these common semantics instead:

- identical eligibility and label-filter behavior;
- no result outside `allowed_note_ids`;
- deleted notes excluded;
- deterministic tie-breaking;
- stable result limits;
- exact identifier cases retrieve the expected note within the tested top-k;
- backend-specific scores never leak as if they were directly comparable;
- RRF consumes ranked positions, not assumed score equivalence.

Fix filtered Turso retrieval so filtering is applied safely before or as part of candidate ranking rather than producing a semantically unrelated fallback without an explicit contract.

If strict cross-backend top-k order is later required, centralize a common ranker and measure the cost before implementing it.

## 41. Search evaluation corpus

Add a small checked-in evaluation corpus containing:

- exact title match;
- exact body identifier;
- paraphrase suited to dense search;
- mixed title/body match;
- label-filtered candidates;
- punctuation and case variants;
- tied candidates;
- deleted notes;
- CJK samples with documented expectations.

Record at least:

- Recall@k;
- MRR or expected top-rank assertions;
- exact-token recall;
- label-filter correctness;
- p50/p95 timing in an optional benchmark, not a flaky CI assertion.

Do not add ANN in this PR. Consider ANN only after corpus size and measured latency justify it.

## 42. PR 5 required tests

1. Hybrid search returns lexical results when embedder fails.
2. Hybrid search returns dense results when lexical retrieval fails.
3. Hybrid search fails when all channels fail.
4. Lexical mode never calls the embedder.
5. Dense mode does not run lexical retrieval.
6. Exact body identifier is returned through body lexical retrieval.
7. Label filters exclude every disallowed note in both adapters.
8. Tie ordering is deterministic.
9. Deleted notes are excluded.
10. Diagnostics correctly identify used and failed channels.
11. Legacy response compatibility or version transition is tested.
12. Turso and PostgreSQL satisfy the shared retrieval contract.

## 43. PR 5 acceptance criteria

- Embedding outage no longer disables lexical search.
- Body exact terms have a lexical retrieval path.
- Search degradation is observable.
- Backend ranking differences are explicitly bounded by contract tests.
- No ANN index is added without measurement.

---

# PR 6 — Frontend Attachment Isolation and Independent Mutations

## 44. Objective

Stop downloading every attachment when opening a note and stop re-uploading every attachment when editing ordinary note fields.

## 45. Read model

The note detail response remains metadata-first. Attachment metadata should contain:

```text
id
path
mime
description
size_bytes
checksum_sha256
storage_generation
```

Opening a note loads:

- title;
- content;
- labels;
- revision;
- attachment metadata.

It must not automatically download every attachment body.

## 46. Frontend state

Represent attachment state independently, for example:

```text
metadata
content_state = not_loaded | loading | loaded | failed
preview_state
mutation_state
```

A failed attachment read must not make the note title/content unavailable.

Load content only when:

- the user requests download;
- a supported small preview is opened;
- rendering requires a specific referenced attachment.

Use bounded concurrency for optional previews. Do not start unbounded parallel reads.

## 47. Independent REST mutations

Add REST operations equivalent to the existing MCP attachment capabilities:

- add/replace one attachment using `expected_revision`;
- delete one attachment using `expected_revision`;
- fetch one attachment by stable identity or path as required by rendering;
- return the new note revision after mutation.

The exact transport encoding may be JSON, multipart, or streaming binary, but it must satisfy:

- one attachment mutation does not resend unrelated attachment bytes;
- large bytes are not needlessly duplicated in memory;
- OpenAPI accurately describes the payload;
- metadata and content limits are validated;
- mutation uses the PR 1 CAS and PR 2 object-generation flow.

## 48. Ordinary note field update

Provide/use a fields-only mutation that sends:

```text
expected_revision
title
content
labels
```

It must preserve attachments unchanged.

Retain the old full-replacement endpoint only if compatibility requires it, and deprecate it clearly.

## 49. User-visible conflict behavior

When a note or attachment mutation returns `stale_revision`:

- keep unsaved local edits in memory;
- show that the server revision changed;
- allow the user to reload or reapply;
- never automatically overwrite the newer server state.

## 50. PR 6 required tests

1. Loading note detail performs no attachment-content requests.
2. Opening one preview requests only that attachment.
3. One failed attachment preview does not fail the note page.
4. Updating title/content sends no attachment bytes.
5. Replacing one attachment sends only that attachment.
6. Attachment mutation updates the stored note revision.
7. Stale attachment mutation shows conflict without losing local selection/content.
8. Large attachment response is streamed or bounded according to the chosen API.
9. OpenAPI documents independent attachment operations.
10. MCP and REST call the same attachment pipelines.

## 51. PR 6 acceptance criteria

- Note open cost is independent of total attachment bytes.
- Field edits do not re-upload attachments.
- Attachment failures are isolated.
- Independent attachment writes remain CAS-protected and durable.

---

# PR 7 — Runtime, CI, and Container Hardening

## 52. Objective

Make deployment failures visible at startup, drain requests safely, expose meaningful health state, enforce the full test surface, and support amd64/arm64 containers.

## 53. Startup validation

When configured, validate before declaring readiness:

- database connectivity and supported schema version;
- attachment backend configuration and required filesystem directory/bucket access;
- packaged static directory and readable non-empty `index.html`;
- embedding configuration syntax and local model path when explicitly configured;
- background worker configuration.

A missing packaged `index.html` must fail startup with a clear error. Do not return an empty SPA document with HTTP 200.

An unavailable optional remote embedder should be reported as degraded, not necessarily prevent startup, because PR 5 permits lexical-only operation.

## 54. Graceful shutdown

Use Axum graceful shutdown semantics:

1. receive termination signal;
2. mark readiness false;
3. stop accepting new requests;
4. stop claiming new background jobs;
5. drain in-flight requests within a configured deadline;
6. finish or release worker claims;
7. close storage and background resources;
8. exit with clear tracing.

Do not simply drop the server future when Ctrl-C wins a `select!`.

## 55. Health endpoints

Add structured endpoints:

```text
GET /health/live
GET /health/ready
```

Suggested semantics:

### Liveness

Returns success when the process/event loop is alive. It should not fail because a remote embedder is down.

### Readiness

Returns structured component status for:

```text
storage
schema
attachments
attachment_cleanup
backup_barrier
embedding
static_assets
```

Core storage or unsupported schema failure makes the service unready. Embedding failure may return `degraded` while readiness remains successful if lexical operation is available.

Health responses must not include credentials, paths containing secrets, database URLs, or raw errors from external services.

## 56. CI gates

Restructure `.github/workflows/ci.yml` into clear jobs.

### Fast/static job

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/note-frontend && cargo test
cargo check --target wasm32-unknown-unknown --manifest-path crates/note-frontend/Cargo.toml
```

Adapt commands to workspace exclusions as needed.

### Integration jobs

- Turso storage contracts.
- PostgreSQL/pgvector storage contracts.
- MinIO filesystem/S3 attachment tests.
- REST and MCP parity tests.
- Backup/restore round trip.

### Reliability jobs

- concurrent note CAS race;
- attachment failure injection and restart recovery;
- cleanup lease race;
- backup cancellation/barrier;
- migration from supported prior schemas.

Do not make timing-sensitive benchmarks required CI assertions.

## 57. Toolchain pinning

Add a repository toolchain policy:

- pin Rust through `rust-toolchain.toml`;
- declare `rust-version` where appropriate;
- pin Trunk in container/CI installation;
- use one intentional version across jobs unless a compatibility matrix is deliberate.

## 58. Multi-architecture container

Update the Docker build to support:

```text
linux/amd64
linux/arm64
```

Requirements:

- map `TARGETARCH` to the correct ONNX Runtime distribution name;
- verify the upstream archive naming during implementation rather than guessing;
- run a startup smoke test for both image architectures;
- use configurable mirror build arguments instead of hardcoding one mirror as the only path;
- run the runtime image as a non-root UID/GID;
- create and own the data/static directories explicitly;
- add a `HEALTHCHECK` against `/health/ready`;
- pin build tools;
- avoid copying compilers or package caches into the runtime image.

## 59. PR 7 required tests

1. Missing configured `index.html` fails startup.
2. Liveness succeeds during embedding outage.
3. Readiness reports embedding as degraded.
4. Readiness fails for database/schema failure.
5. An in-flight request is drained on shutdown.
6. Cleanup worker stops claiming jobs during shutdown.
7. Format, clippy, workspace, frontend, and Wasm checks run in CI.
8. amd64 container starts and passes healthcheck.
9. arm64 container starts and passes healthcheck.
10. Runtime process is non-root.

## 60. PR 7 acceptance criteria

- Startup does not mask missing static assets.
- Shutdown drains requests and worker state.
- Health semantics distinguish unavailable from degraded.
- CI covers the documented test surface.
- Published container works on amd64 and arm64 as non-root.

---

# Optional PR 8 — Org Event Streaming

## 61. Objective

Replace manual-only refresh for operational Org views with an efficient event-driven refresh signal after all correctness work above is complete.

This PR is optional and must not delay PRs 1–7.

## 62. Suggested design

Expose an SSE endpoint similar to:

```text
GET /api/org/workspaces/{workspace_id}/events/stream?after_sequence=N
```

Properties:

- based on the existing append-only workspace event sequence;
- resumes after disconnect using the last observed sequence;
- sends bounded event metadata or invalidation signals, not raw fencing tokens;
- frontend refetches server-computed projections rather than reproducing workflow logic locally;
- heartbeat comments keep proxies from closing idle streams;
- backpressure and slow-client behavior are tested.

Do not add polling loops as a substitute unless SSE is unavailable in the deployment environment.

---

## 63. Cross-PR Migration Rules

1. PostgreSQL migrations are append-only and ordered after the current latest migration.
2. Turso receives a new schema version and explicit upgrade path from every currently supported version.
3. Every migration is tested from the oldest supported prior schema, not only from an empty database.
4. Migrations must be restart-safe according to the repository's existing migration mechanism.
5. External object migrations must be lazy or operator-driven; startup must not rewrite an entire S3 prefix.
6. Schema version reporting must appear in readiness and backup manifests.
7. A newer unsupported database schema must fail fast rather than being opened optimistically.

---

## 64. Error Contract

Ordinary note, attachment, backup, and search v2 errors should converge on the existing structured shape:

```json
{
  "code": "stable_machine_code",
  "message": "safe human-readable message",
  "details": {},
  "retryable": false
}
```

Recommended new codes:

```text
expected_revision_required
stale_revision
attachment_object_missing
attachment_checksum_mismatch
attachment_cleanup_failed
backup_in_progress
backup_format_unsupported
backup_checksum_mismatch
restore_target_not_empty
search_backend_unavailable
search_degraded
```

Rules:

- storage/internal errors must not expose credentials or raw SQL;
- attachment errors must not include bytes;
- Org errors retain their existing contracts;
- REST and MCP share code/details semantics;
- `retryable` must not encourage blind replay of stale mutations.

---

## 65. Required Validation Commands

Codex must adapt commands to the current tree, but the final full validation target is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd crates/note-frontend
cargo test
cargo check --target wasm32-unknown-unknown
cd ../..

TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg

cargo test -p note-storage-turso
cargo test -p note-storage-contract-tests
```

Run the repository's existing MinIO/S3 test commands for attachment PRs and add explicit backup/restore/reliability commands when those test targets exist.

A PR may run a targeted subset during development, but its final report must include all relevant tests for the changed boundary.

---

## 66. Definition of Done for the Full Program

The hardening program is complete when all of the following are true:

1. Concurrent ordinary-note writes cannot silently overwrite one another.
2. Every existing-note mutation is revision protected.
3. Active attachment metadata never references an object that was not durably written first.
4. Attachment cleanup survives process restart and transient object-store failure.
5. Operators can detect missing objects, orphans, and stuck cleanup.
6. Backup is streaming, versioned, checksummed, and restorable.
7. Backup excludes credentials and raw Org fencing tokens.
8. Embedding failure degrades search instead of disabling lexical retrieval.
9. Note bodies have a lexical exact-term retrieval channel.
10. Opening or editing a note does not transfer unrelated attachment bytes.
11. Startup fails clearly on invalid required resources.
12. Shutdown drains requests and worker claims.
13. CI enforces format, lint, full tests, storage contracts, and reliability tests.
14. Container images run as non-root on amd64 and arm64.
15. The pure-core/effectful-shell and shared-pipeline architecture remains intact.
16. No authentication, authorization, user, session, ACL, TLS, or proxy-identity subsystem has been added to Agent Note.

---

## 67. First Codex Task

Implement **PR 1 — Ordinary Note Optimistic Concurrency** only.

Start by tracing every ordinary-note mutation from REST/MCP/frontend through `note-pipelines` into `note-storage`, then list the complete mutation surface before editing. Use `note_revision` as the authoritative CAS token, add shared Turso/PostgreSQL contract tests, update first-party clients and schemas, run the required validation, and stop with the end-of-PR report defined in Section 2.4.

Do not begin attachment outbox, backup, search, runtime, authentication, or unrelated refactoring in the first task.
