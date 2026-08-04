# Org Workflow and Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan task-by-task.
> Each task starts with a failing externally observable test and receives an
> independent requirements and code-quality review before the next task.

**Goal:** Deliver the unfenced foundation of Org Delivery Slice 3 with
transport-free, idempotent semantic workflow commands, complete append-only
audit records, durable attempt history foundation, and recovery-context reads
while preserving canonical Org source and transaction atomicity in both storage
backends. Slice 3 remains open until Slice 4 adds and verifies every required
fenced progress/review/transition/attempt command.

**Architecture:** `note-org` remains pure and performs loss-preserving source
edits plus policy/dependency validation. `note-storage` defines driver-neutral
workflow/audit records and queries, implemented identically by Turso and
PostgreSQL. `note-pipelines::org` is the only application boundary: it owns the
injectable clock, typed errors/DTOs, canonical request fingerprints, immediate
transactions, CAS, projection mapping, events, and operation-result replay.
No transport or frontend code is introduced in this slice.

**Tech Stack:** Rust 2021, `async-trait`, `chrono`, `chrono-tz`, `serde`,
`serde_json`, `sha2`, `uuid`, Turso/libSQL, SQLx/PostgreSQL, shared backend
contract tests, Tokio integration tests.

---

## Safety Boundary with Slice 4

The approved documents describe attempts/progress/review in Slice 3 while also
requiring their executable mutations to validate a current fencing token, which
is introduced in Slice 4. This plan preserves both requirements without adding
an unsafe temporary API:

- Slice 3 adds the attempt schema, records, preservation rules, event
  vocabulary, audit queries, and recovery-context representation.
- Slice 3 implements source mutations that do not require lease ownership:
  workspace/document operations, create/move/reparent, assignment, scheduling,
  dependencies, and note links. Pure state-edit and transition validation remain
  available, but the executable transition command is activated only with
  fencing in Slice 4 because the PRD requires every transition to validate the
  current token.
- Slice 4 activates attempt allocation/closure and every ownership-sensitive
  command: claim/reclaim, progress, submit result, request/approve/reject
  review, failure, completion, release, expiry, and retry.
- No unfenced progress, review, failure, or completion command is exposed.
- Slice 4 also activates workspace archive only after checking for active
  leases, and replaces the conservative existing-document import guard with
  per-item fencing-token validation.

This is an execution dependency only; the final product scope and the complete
Slice 3–7 objective are unchanged.

## Scope

This plan implements:

- missing minimal semantic edits for title, priority, deadline, and typed
  `agent-note:` links;
- workspace/document/item reads plus atomic document-ownership movement needed
  by transport-free commands and recovery context;
- schema v4 with durable `org_attempts` and richer append-only events;
- the complete event type vocabulary, per-workspace sequence ordering, optional
  attempt association, and before/after state snapshots;
- typed Org pipeline context, clock, DTOs, errors, projection mapper, and
  transaction executor;
- required actor and operation IDs, reserved `system` enforcement, canonical
  fingerprints, same-result replay, and divergent reuse conflicts;
- create/update workspace commands, read-only archived workspace behavior, and
  conservative revision-safe document put/import semantics;
- create/follow-up/move/reparent/assign/schedule/dependency/note-link commands
  plus pure transition validation for the fenced Slice 4 command;
- weak-note hydration and recovery-context assembly from source, projections,
  attempts, links, and ordered events; and
- rollback, archived-workspace, omission, dependency, review, revision, and
  idempotency verification.

Workspace/document application commands and cross-workspace ownership CAS are
required Slice 2 completion backfill: the deployed Slice 2 baseline persisted
their storage primitives but did not deliver the `note-pipelines` boundary that
later transports require. They are included here as explicit prerequisites,
not reclassified as new Slice 3 product scope.

This plan does **not** add leases, fencing tokens, claim/reclaim, attempt
allocation, ownership-sensitive progress/review/completion, retry, queues,
agendas, MCP, offline commands, REST/OpenAPI, or Web UI. Those remain Slices
4–7. Consequently, passing this plan is a Slice 3 foundation gate, not a claim
that the named Slice 3 is complete; the Slice 4 completion audit closes both
slices' interdependent workflow requirements.

## Requirement Coverage

| Requirement | Tasks |
| --- | --- |
| Minimal loss-preserving semantic source edits | 1 |
| Durable attempts and complete audit fields/vocabulary | 2–3 |
| Backend-parity point/relation/audit reads | 3 |
| Typed Org pipeline boundary and idempotent transaction runner | 4 |
| Projection mapping and workspace/document commands | 5 |
| Item semantic mutations and dependency/review gating | 6 |
| Recovery context, weak note availability, audit query | 7 |
| Cross-backend/scoped regression and documentation | 8 |

## Execution Prerequisites

Execute in the existing isolated worktree:

```sh
cd /data/development/gsmlg-opt/agent-note/.trees/org-delivery-slices-3-7
git status --short --branch
cargo fmt --all -- --check
cargo test -p note-org
cargo test -p note-storage
cargo test -p note-storage-turso
cargo test -p note-storage-pg
cargo test -p note-pipelines
```

Expected: scoped tests pass and the worktree is clean except for this plan. A
failure outside the Slice 3 scope is reported and not fixed here.

## Public Semantics

### Mutation envelope

Every client mutation accepts a non-empty `operation_id` and non-empty
`actor_id`. Client actor `system` is rejected. The workspace ID is part of the
operation key; workspace creation therefore requires a caller-provided stable
workspace ID so retries can locate the original result.

The canonical request fingerprint is SHA-256 over a versioned serde DTO whose
unordered collections are represented by `BTreeMap`/`BTreeSet`. It covers the
operation kind, actor, workspace, expected revisions, and full payload.

Inside one immediate transaction:

1. Look up `(workspace_id, operation_id)`.
2. Replay the stored transport-neutral result for the same fingerprint.
3. Reject a different fingerprint with `IdempotencyConflict` and no side
   effects.
4. Reject an archived workspace.
5. Load canonical source and projections.
6. Apply the smallest pure `note-org` edit and reparse every affected document.
7. Validate policy, complete workspace dependency graph, note targets, semantic
   diff, and every expected revision.
8. Commit CAS, projections, audit events, and operation result atomically.

### Typed errors

Slice 3 provides stable pipeline categories:

```text
invalid_input
not_found
archived_workspace
stale_revision
idempotency_conflict
invalid_transition
dependency_blocked
review_required
unsupported_semantic_edit
note_unavailable
storage_failure
```

Errors carry a safe message, structured details, and retryability. Storage or
database internals are not exposed.

### Event and attempt records

Events are append-only and ordered by the existing per-workspace sequence. Each
event contains ID, workspace, subject kind/ID, actor, optional attempt,
event type, time, summary, metadata, and paired optional previous/resulting
state. The complete accepted vocabulary is:

```text
creation, assignment, claim, heartbeat, release, start, progress, block,
unblock, review_request, approval, rejection, completion, failure, retry,
cancellation, lease_expiry, dependency_change, schedule_change,
document_import, note_link_change, workspace_change, workspace_archive,
document_move, item_move, title_change, priority_change, deadline_change
```

The first 21 names are the PRD's mandatory vocabulary; the additional names
ensure every other Slice 3 state-changing operation has a specific audit type
instead of being hidden in generic metadata. Workspace/document/item creation
uses `creation`. Slice 4 emits the ownership-sensitive entries; Slice 3 makes
the vocabulary and storage format complete now.

Attempts are authoritative runtime history and survive projection rebuilds.
They have a stable ID, workspace-at-start/item identity, positive attempt number, actor,
status, start/end time, error/result/review outcome, note-reference snapshot,
artifact references, and structured metadata. Slice 4 allocates and closes
attempts together with leases. Attempt numbers are unique by stable work-item ID
across workspace moves; historical attempt rows retain the workspace where the
attempt began.

Cross-workspace moves never rewrite old audit rows. The move appends linked
provenance events in source and target workspaces. Recovery context returns an
item's global history as workspace sequence segments ordered by move lineage;
events inside each segment retain authoritative `(workspace_id, sequence)`
order. It does not invent a cross-workspace causal sequence from timestamps.

Progress events are the authoritative append-only progress records. There is
no second mutable progress table: the event ID, attempt ID, actor, workspace
sequence, timestamp, summary, and structured metadata provide the complete
durable record and survive projection rebuilds.

---

## Task 1: Complete Pure Semantic Edits

**Files:**

- Modify: `crates/note-org/src/types.rs`
- Modify: `crates/note-org/src/source.rs`
- Modify: `crates/note-org/src/lib.rs`
- Modify: `crates/note-org/tests/semantic_edits_test.rs`
- Modify: `crates/note-org/tests/link_projection_test.rs`

- [ ] Add failing tests for setting/clearing deadline, editing title and
  priority, adding/removing one typed `agent-note:` link, preserving unrelated
  bytes/line endings, and rejecting ambiguous/unsafe link edits.
- [ ] Extend the internal index only with the exact spans required for those
  edits. Property drawers and opaque regions remain opaque.
- [ ] Add `SemanticEdit` variants and minimal edit implementations. Do not
  reconstruct whole headings or bodies.
- [ ] Reparse edited output and prove all unrelated bytes are identical.
- [ ] Run:

```sh
cargo test -p note-org --test semantic_edits_test --test link_projection_test
cargo test -p note-org
cargo fmt --all -- --check
```

## Task 2: Add Schema v4 Workflow Audit Records

**Files:**

- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/schema.sql`
- Create: `crates/note-storage-turso/migrations/0003_to_0004_org_workflow_audit.sql`
- Create: `crates/note-storage-turso/tests/fixtures/schema-v3.sql`
- Modify: `crates/note-storage-turso/src/preflight.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Create: `crates/note-storage-pg/migrations/0003_org_workflow_audit.sql`
- Modify: `crates/note-storage-turso/tests/schema_test.rs`
- Modify: `crates/note-storage-pg/tests/lifecycle_test.rs`

- [ ] Add failing schema tests for fresh v4, v3-to-v4, chained v2-to-v4,
  rollback leaving the original version unchanged, and PostgreSQL migration
  registration.
- [ ] Define typed `OrgEventType`, attempt status/artifact/note-reference
  values, `OrgAttempt`, `NewOrgAttempt`, `OrgAttemptUpdate`, and enriched event
  records in `note-storage` without transport DTOs.
- [ ] Add `org_attempts`; do not foreign-key it to rebuildable
  `org_work_items`. Enforce positive attempt number, unique
  `(work_item_id, attempt_number)`, non-empty actor/status, and
  valid end time.
- [ ] Rebuild/alter `org_events` with optional attempt and paired state fields,
  non-empty checks, workspace/subject and attempt indexes, while preserving all
  existing events and sequence allocation.
- [ ] Make Turso accept schema 2/3/4 and apply all needed migrations under one
  immediate transaction with `user_version=4` last.
- [ ] Run:

```sh
cargo test -p note-storage-turso --test schema_test
cargo test -p note-storage-pg --test lifecycle_test
cargo fmt --all -- --check
```

## Task 3: Implement Backend-Parity Workflow/Audit Storage

**Files:**

- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Modify: `crates/note-storage-turso/src/embedding_jobs.rs`
- Modify: `crates/note-storage-turso/src/labels.rs`
- Modify: `crates/note-storage-turso/src/notes.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-turso/src/retrieval.rs`
- Modify: `crates/note-storage-turso/src/settings.rs`
- Modify: `crates/note-storage-turso/tests/transactions_test.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/lib.rs`
- Modify: `crates/note-storage-contract-tests/src/transactions.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] Add failing shared contracts for point work-item reads, workspace
  projection reads, children, reverse note links, workspace and global-subject
  event reads,
  attempt round trips/order/one-time closure, state snapshots, event attempt
  association, document ownership CAS across workspaces, and preservation
  across projection rebuild.
- [ ] Replace stringly event writes with validated typed event records while
  preserving the complete event vocabulary for later slices.
- [ ] Implement identical repository methods in Turso and PostgreSQL.
- [ ] Add an explicit atomic document-move contract that validates the document
  revision plus source and target workspace revisions, changes ownership, and
  advances both workspace revisions. A conflict changes nothing.
- [ ] Preserve attempt workspace-at-start and append linked source/target move
  provenance events; expose global work-item attempt/history reads without
  rewriting old rows or pretending separate workspace sequences are one causal
  counter.
- [ ] Ensure reserved `system` is rejected for client-originated event writes;
  expose an explicit internal-event path for Slice 4 expiry bookkeeping rather
  than trusting arbitrary callers.
- [ ] Prove immediate-transaction rollback covers attempt/event/operation
  changes.
- [ ] Run:

```sh
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test transactions_test
```

During iteration PostgreSQL tests may use their existing configured skip when
no test database is available; do not replace backend contracts with mocks. A
real configured PostgreSQL shared-contract run is mandatory before the Slice 3
foundation or the final delivery can be declared verified.

## Task 4: Build the Typed Org Pipeline Boundary

**Files:**

- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/note-pipelines/src/lib.rs`
- Create: `crates/note-pipelines/src/org/mod.rs`
- Create: `crates/note-pipelines/src/org/context.rs`
- Create: `crates/note-pipelines/src/org/error.rs`
- Create: `crates/note-pipelines/src/org/dto.rs`
- Create: `crates/note-pipelines/src/org/idempotency.rs`
- Create: `crates/note-pipelines/src/org/mutation.rs`
- Create: `crates/note-pipelines/tests/support/org.rs`
- Create: `crates/note-pipelines/tests/org_idempotency_audit_test.rs`

- [ ] Move `note-org` from dev-dependencies to dependencies and define a small
  `OrgContext` with shared storage plus an injected `OrgClock`; do not require
  embedding or attachments.
- [ ] Add failing tests for empty actor/operation IDs, reserved `system`,
  deterministic fixed time, identical replay, divergent conflict, replay after
  restart, and rollback without duplicate events.
- [ ] Define versioned transport-neutral request/result DTOs and typed errors.
- [ ] Implement deterministic canonical fingerprints and an immediate
  transaction runner that stores a successful result in the same transaction.
- [ ] Map CAS `NotFound`/`Conflict` directly to typed Org errors and preserve
  the current revision in conflict details.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_idempotency_audit_test
cargo test -p note-pipelines --test save_note_transaction_test
cargo fmt --all -- --check
```

## Task 5: Projection Mapping and Workspace/Document Commands

**Files:**

- Create: `crates/note-pipelines/src/org/projection.rs`
- Create: `crates/note-pipelines/src/org/workspaces.rs`
- Create: `crates/note-pipelines/src/org/documents.rs`
- Create: `crates/note-pipelines/tests/org_workspace_document_test.rs`

- [ ] Add failing tests for workspace create/update, timezone/policy
  validation, document put/import, byte preservation, expected revisions,
  omitted-item rejection, cross-document atomic move, archived mutation
  rejection, and idempotent event/result replay.
- [ ] Add explicit timestamp tests for ambiguous and nonexistent DST local
  times and for preservation of raw local text, workspace timezone, and
  resolved UTC values on schedule/deadline projections.
- [ ] Prove a workspace policy update is rejected when the candidate policy
  invalidates any existing work-item type, state, transition role, tag rule, or
  required review rule; failed policy updates leave the workspace revision and
  audit history unchanged.
- [ ] Inject failures after source computation and during projection/event/
  operation writes; prove parse, policy, stale revision, omitted item,
  note-target, and storage failures leave canonical text, projections, events,
  workspace revisions, and operation records unchanged.
- [ ] Map parsed source to projections using workspace time, preserving stored
  `created_at` for existing IDs and using the injected clock only for new IDs.
- [ ] Validate the complete workspace dependency graph and every new note target
  before any write.
- [ ] For multi-document edits, require the complete expected-revision map and
  rebuild the workspace projection in the same transaction.
- [ ] Implement cross-workspace document movement through the explicit storage
  contract: validate both complete workspace projections and policies, prevent
  cross-workspace dependency edges, write an audit event in each affected
  workspace, and persist one replay result under the source workspace operation
  key in the same transaction.
- [ ] Carry an explicit fail-closed `LeaseGuard` seam in archive, document/item
  movement, assignment, and raw-import orchestration. Before Slice 4 it permits
  only states for which no lease can exist; Slice 4 must replace it with active
  execution/review lease lookup plus per-item fencing validation. Slice 4 tests
  must cover archive, movement, reassignment/unassignment, and raw changes with
  current, missing, and stale tokens.
- [ ] Derive semantic differences for raw document put/import and reject
  implicit omission/deletion or an unsupported semantic change. Until Slice 4
  can inspect leases, replacement of an existing document must reject every
  state change and reject candidate/original items in running or review states;
  new-document import remains allowed after full validation. Slice 4 replaces
  this conservative guard with fencing-token validation.
- [ ] Do not expose workspace archive from this pipeline yet. Slice 4 must add
  it with an active-lease query and tests proving active leases reject archive,
  no lease permits revision-guarded archive, and archived workspaces remain
  readable/exportable/auditable while every mutation is rejected.
- [ ] Never emulate ownership movement with delete/reinsert; stable document
  and work-item IDs, attempts, links, events, and operation history must remain
  intact.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-pipelines --test org_idempotency_audit_test
```

## Task 6: Item Workflow Commands

**Files:**

- Create: `crates/note-pipelines/src/org/items.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Create: `crates/note-pipelines/tests/org_item_workflow_test.rs`

- [ ] Add failing tests for create/follow-up, same/cross-document move,
  reparent, assignment, schedule/deadline, dependencies, and note links. Add
  tests proving transition inputs receive pure policy/dependency validation but
  the executable transition command is unavailable until Slice 4.
- [ ] Prove new work defaults to workspace `initial_state` (`BACKLOG` under the
  default policy), accepts another configured valid initial state, and rejects
  an unknown or policy-invalid initial state.
- [ ] Prove follow-up creation stores its originating event or work-item
  relationship in immutable creation-event metadata and hydrates that origin
  in recovery context.
- [ ] Inject parse/policy/revision/note-link and post-source-computation storage
  failures for item mutations and prove no partial source, projection, event, or
  operation result remains.
- [ ] Prove self/missing/cyclic/cross-workspace dependencies are rejected and
  dependency-gated transitions cannot enter executable or successful states.
- [ ] Prove only configured successful terminal prerequisite states satisfy a
  dependency; failed, cancelled, running, review, and unknown states do not.
- [ ] Prove the pure transition validator rejects required-review work moving
  directly to a successful terminal state.
- [ ] Prove note creation links require an active Markdown note while later
  deletion does not rewrite or delete the weak Org link.
- [ ] Prove assignment, reassignment, and unassignment each preserve unrelated
  source bytes and append one typed assignment event with before/after values.
- [ ] Require all affected expected document revisions, apply the smallest
  source edit, update projections, append exactly the typed semantic event(s),
  and persist the replay result atomically.
- [ ] Do not expose a pipeline state-transition mutation until Slice 4 supplies
  a required current lease/fencing token; do not classify transitions as
  "administrative" or accept an optional/dummy token.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_item_workflow_test
cargo test -p note-org --test policy_test --test dependencies_test
```

## Task 7: Recovery Context and Audit Reads

**Files:**

- Modify: `Cargo.lock`
- Modify: `crates/note-pipelines/Cargo.toml`
- Create: `crates/note-pipelines/src/org/audit.rs`
- Create: `crates/note-pipelines/src/org/context_read.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Create: `crates/note-pipelines/tests/org_recovery_context_test.rs`

- [ ] Add failing tests for an item context containing workspace policy,
  canonical revision, parent/children, dependencies and satisfaction,
  linked-note availability, prior attempts, progress/failure/review events,
  artifacts, and sequence-ordered recent history.
- [ ] Add a cross-workspace document-move test proving target context includes
  pre-move attempts and source history as lineage-linked workspace sequence
  segments, while every old event and attempt retains its original workspace.
- [ ] Hydrate note availability with active-note lookup; soft/permanent deletion
  produces `available=false` while retaining the link and audit history.
- [ ] Add subject-filtered, cursor-bounded event queries with deterministic
  ascending sequence order. The transport-neutral page contract uses an opaque
  cursor, defaults to 50, rejects limits above 200, and resumes strictly after
  the last returned workspace sequence.
- [ ] Represent lease as absent until Slice 4; never fabricate ownership state
  or expose fencing-token-shaped placeholders.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_recovery_context_test
cargo test -p note-pipelines --test org_item_workflow_test
```

## Task 8: Slice 3 Foundation Regression and Documentation Gate

**Files:**

- Modify: `README.md`

- [ ] Document schema v4, transport-free Org workflow availability, audit and
  recovery semantics, and the explicit Slice 4 fencing boundary. Do not claim
  MCP, REST, or UI availability.
- [ ] Confirm every Slice 3 requirement maps to an observable test and inspect
  the diff for accidental Slice 4–7 work.
- [ ] Run the scoped final gate:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p note-org
cargo test -p note-storage
cargo test -p note-storage-turso
cargo test -p note-storage-pg
cargo test -p note-pipelines
git diff --check
git status --short --branch
```

- [ ] Run the shared contract suite against a real configured PostgreSQL
  database; an unavailable database is missing parity evidence, not a passing
  completion result.
- [ ] Independently review requirements compliance and code quality. Resolve
  all Critical and Important findings, rerun the affected scoped tests, and
  proceed directly to the separately planned Slice 4 implementation. Do not
  mark Slice 3 complete until Slice 4's fenced-command matrix passes.

## Slice 3 Foundation Evidence

This plan's foundation gate passes only when current evidence proves:

1. canonical source edits are minimal and revision-safe;
2. every mutation is atomic and idempotent across restart;
3. same operation replay emits no duplicate event and divergent reuse changes
   nothing;
4. projections rebuild without losing attempts, events, or operations;
5. complete event fields, vocabulary, state snapshots, workspace ordering,
   cross-workspace lineage, and filters round-trip in both backends;
6. archived workspaces reject mutations but remain readable/auditable;
7. dependency and review rules reject invalid transitions;
8. weak note links preserve missing targets as unavailable;
9. recovery context returns canonical revision and all available historical
   information; and
10. no ownership-sensitive command is accepted without the Slice 4 fencing
    implementation.

The named Slice 3 becomes complete only after the Slice 4 plan additionally
proves claim-created attempts and fencing-protected progress, result,
transition, review, failure, completion, retry, raw import, and archive paths.
