# Org Claims and Operational Views Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan task-by-task.
> Every task begins with a failing externally observable test. After its scoped
> tests pass, obtain an independent requirements review and an independent
> code-quality review before accepting the task.

**Goal:** Complete the fencing-dependent Workflow and Audit requirements left
open by Delivery Slice 3 and deliver Delivery Slice 4: exclusive execution and
review leases, permanent stale-owner fencing, deterministic expiry/recovery,
attempt and retry enforcement, active-lease guards on every source-changing
path, and server-computed workspace queues, agendas, and operational summaries
with identical Turso and PostgreSQL behavior.

**Architecture:** `note-org` remains pure and decides readiness, claim-policy,
transition, recovery, and attempt-limit rules. `note-storage` owns
driver-neutral lease records and set-based operational query contracts; Turso
and PostgreSQL implement those contracts over authoritative runtime data and
rebuildable projections. `note-pipelines::org` remains the only application
boundary and owns the injected clock, secure token generation and hashing,
idempotency, immediate transactions, source edits, projection replacement,
attempt lifecycle, lease validation, events, context hydration, cursor DTOs,
and typed errors. No transport or browser code is added.

**Tech Stack:** Rust 2021, `async-trait`, `chrono`, `serde`, `serde_json`,
`sha2`, `uuid`, Turso/libSQL, SQLx/PostgreSQL, shared backend contracts, Tokio
barriers/notifications, controllable Org clocks, and real PostgreSQL integration
tests through `TEST_DATABASE_URL`.

---

## Scope and Slice Boundary

This plan implements:

- schema v5 authoritative lease storage and queue-supporting indexes;
- exclusive execution and review leases with opaque, unguessable fencing
  tokens whose raw value is returned only to the successful claimant;
- normal claim, review claim, heartbeat, voluntary release, deterministic
  expiry bookkeeping, expired-running reclaim, failure retry, and workspace
  concurrency limits;
- attempt allocation and closure, positive monotonic attempt numbers, retry
  limits, and preservation of progress, results, review outcomes, links, and
  artifacts;
- fenced progress, result submission, state transition, review request,
  approval, rejection, failure, completion, cancellation, and blocking paths;
- active-lease enforcement for archive, raw import, document/item movement,
  reparenting, reassignment, unassignment, and other semantic source changes;
- ready, assigned, running, blocked, review, scheduled, upcoming-deadline,
  failed, expired-lease, and completed operational views;
- explicit one- or multi-workspace queue/agenda queries, deterministic opaque
  cursor pagination, workspace summary counts, and lease-aware recovery
  context; and
- complete Turso/real-PostgreSQL parity, deterministic concurrency tests,
  independent spec/quality review, and scoped regression gates.

This plan does **not** add MCP registrations, offline commands, REST handlers,
OpenAPI, frontend routes, authentication, authorization, sessions, proxy
identity handling, schedulers, automatic polling, or multi-replica
coordination. Those remain Slices 5–7 or explicitly deferred product work.
Fencing tokens must not appear in general read DTOs, logs, event metadata,
queue/agenda rows, summaries, or recovery context.

Slice 4 assumes the Slice 3 foundation plan is committed and green through its
Tasks 1–8. If any Slice 3 command or recovery-context module named below is not
yet present, finish that prerequisite under the Slice 3 plan first; do not
silently reimplement it in this slice.

## Requirement Coverage

| Requirement | Tasks |
| --- | --- |
| Pure claim/recovery/retry/readiness decisions | 1 |
| Schema v5 leases and migrations | 2 |
| Lease/attempt/fencing storage parity | 3 |
| Set-based queues, agendas, summaries, cursors | 4 |
| Claim, reclaim, heartbeat, release, concurrency | 5 |
| Fenced execution, transition, result, review, retry | 6 |
| Archive/import/move/reassign active-lease guards | 7 |
| Operational DTOs, queries, summaries, context hydration | 8 |
| Cross-backend high-risk acceptance and regression | 9–10 |

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

Expected: every Slice 3 scoped suite passes. PostgreSQL tests that emit a skip
because `TEST_DATABASE_URL` is absent provide no PostgreSQL evidence; a real
configured run is mandatory in Tasks 9–10. Preserve unrelated work and stop on
out-of-scope failures rather than fixing them.

## Public Semantics

### Lease record and fencing token

An authoritative lease contains:

```text
lease_id
workspace_id (current workspace ownership)
work_item_id
attempt_id
kind = execution | review
actor_id
fencing_token_hash
acquired_at
last_heartbeat_at
expires_at
ended_at
end_reason
expiry_event_id
```

There is at most one open lease row per stable work-item ID. An open row whose
`expires_at <= now` is logically inactive even before bookkeeping closes it.
The database stores only a SHA-256 hash of the raw token. The raw token is
generated from operating-system randomness by the claim pipeline, returned in
the claim/idempotency result, and subsequently hashed before comparisons. It is
never recoverable from a general lease read.

The successful claim result intentionally persists the raw token inside the
idempotent operation result so an identical network retry can replay the exact
original result. Operation results are internal application data and must not
be surfaced through read-only DTOs. Token-bearing request/result values must
never be logged.

Every lease-sensitive mutation performs one conditional storage write using
the lease ID, work-item ID, token hash, kind, actor, open state, and
`expires_at > now`. A raw token from an expired, released, superseded, moved-and-
reassigned, or otherwise closed lease can never authorize a later mutation.

### Attempt and lease lifecycle

The implementation uses the following observable matrix:

| Action | Source/state result | Lease result | Attempt result | Required audit |
| --- | --- | --- | --- | --- |
| Claim executable work | executable → running | new execution lease | allocate next running attempt | `claim`, `start` |
| Claim review work | review unchanged | new review lease | attach to current submitted attempt; do not allocate an execution attempt | `claim` |
| Heartbeat | unchanged | same token, heartbeat and expiry become `now` and `now + policy duration` | unchanged | `heartbeat` |
| Report progress | unchanged | remains active | running attempt unchanged | `progress` |
| Submit result without review | running → successful terminal | close execution lease | running → completed with result/notes/artifacts | `completion` |
| Submit/request review | running → review | close execution lease | running → submitted with result/notes/artifacts | `review_request` |
| Approve | review → successful terminal | close review lease | submitted → completed with review outcome | `approval`, `completion` |
| Reject | review → rejection recovery state | close review lease | submitted → failed with review outcome | `rejection` |
| Fail | running → failed | close execution lease | running → failed with error/context | `failure` |
| Block | running → configured blocked state | close execution lease | running → cancelled while retaining progress/context | `block` |
| Cancel execution | valid source → cancelled | close execution lease if present | running → cancelled | `cancellation` |
| Cancel review | review → cancelled | close review lease if present | submitted → cancelled | `cancellation` |
| Release execution | running → requested valid target or configured release state | close execution lease | running → cancelled | `release` |
| Release review | review → requested valid target or remains review by default | close review lease | submitted remains submitted only in review; otherwise closes as described below | `release` |
| Expired execution reclaim | running → expiry-recovery → running atomically | close expired lease, create new execution lease/token | old running → expired, allocate next running attempt | `lease_expiry`, `claim`, `start` |
| Explicit failed retry | failed → running atomically | new execution lease/token | allocate next running attempt | `retry`, `claim`, `start` |

`OrgAttemptStatus::Submitted` is the only non-terminal post-execution status.
It remains `Submitted` across review claim, review heartbeat, reviewer release
that leaves the item in review, and reviewer-lease expiry. Approval closes it
as `Completed`; rejection closes it as `Failed` with the rejection outcome;
reviewer release to any allowed non-review, non-cancelled target closes it as
`Failed` with outcome `released`; item cancellation or reviewer release to the
cancelled state closes it as `Cancelled`. No other transition from `Submitted`
is accepted, and every terminal attempt closure is one-time. Attempt number
uniqueness is global to the stable work-item ID across document and workspace
moves. The initial claim is attempt 1; a retry limit of `N` permits at most
`N + 1` execution attempts. Review leases never consume the execution retry
budget.

Release requests carry an optional `target_state`. Execution release defaults
to `policy.release_state`; review release defaults to the unchanged review
state so another reviewer can claim it. A requested target must be a configured
state reachable by the active policy from the current state and must pass the
normal dependency/review/terminal checks. Release may not manufacture a direct
successful completion, enter the running state without a new claim, or bypass
required review. A different `target_state` is part of the versioned release
DTO and canonical idempotency fingerprint, so reuse of one operation ID with a
different target returns `idempotency_conflict`. The `release` event always
records the requested/defaulted target and has paired before/after state when
the state changes; unchanged review release has no state pair.

`org_submit_result` selects the successful terminal state only when policy and
dependencies allow completion. If review is required it produces the review
branch above. `org_request_review` is the explicit resultless/partial-result
form of the same transition. Policy-defined transitions without active
ownership are accepted only for non-running, non-review states and still
require expected document revisions; any transition from an actively leased,
running, or review-owned item requires the current token.

### Expiry, recovery, and concurrency

Time comes exclusively from `OrgContext::clock()`. Tests advance a fixed clock;
they never sleep. `expires_at <= now` is expired. An expired lease is excluded
from concurrency counts and treated as inactive by reads and claims.

Expiry bookkeeping is performed inside the next lease-mutating claim, retry,
release, archive, or guarded-source transaction. Read-only operational queries
observe expiry logically and never mutate while reading. Bookkeeping
conditionally closes the lease, closes an active execution attempt as
`Expired`, and appends one reserved-`system` `lease_expiry` event. The lease
row stores the expiry event ID so repeated processing is idempotent. There is
no background scheduler in this slice.

An expired review lease is closed with one system expiry event while the item
remains in review and its submitted execution attempt remains unchanged. A new
reviewer may then claim it with a new token.

Only a claim/retry transaction may recover a running item from an expired
execution lease. It verifies capacity after ignoring the expired lease, checks
the attempt limit, applies the configured recovery-state transition to source,
allocates a new attempt/token, returns to running, replaces projections, and
stores events plus the operation result atomically. If any step fails, the old
source, lease, attempt, events, and operation record remain unchanged.

Workspace concurrency counts all unexpired execution and review leases. A
claim at the configured limit returns `concurrency_limit`. Claim evaluation
also enforces workspace active state, dependency satisfaction, due schedule,
assignment/dispatch policy, correct queue kind, expected document revision,
and retry budget.

Concurrency is a workspace-wide atomic invariant, not merely a per-item unique
constraint. With `concurrency_limit = 1`, simultaneous claims for two different
eligible items yield exactly one successful lease and one
`concurrency_limit`, even though the item IDs differ. The same invariant holds
when one transaction moves an actively leased item into a target workspace
while another transaction claims an item already in that target: the target
may commit at most the configured number of active leases. The losing move or
claim changes no source, projection, workspace revision, lease, attempt, event,
or operation result. Both races require barrier-driven Turso and live
PostgreSQL evidence.

### Active-lease guard matrix

- Archiving is always rejected while any unexpired lease exists; a fencing
  token does not override this rule.
- Raw import/put requires a token map keyed by every actively leased item whose
  interpreted source fields change. Unrelated opaque edits do not require a
  token, but all existing omission, complete-parse, policy, and revision rules
  still apply.
- Same-workspace document/item movement and reparenting preserve an active
  lease only when its current token is supplied.
- Cross-workspace movement requires tokens for every active contained item,
  validates target policy and capacity, moves current lease ownership to the
  target workspace, and leaves attempt workspace-at-start/history unchanged.
- Reassignment or unassignment of actively leased work requires the current
  token, closes and permanently fences the lease, closes the attempt as
  cancelled, moves source to its configured release/rejection state, and then
  applies the assignment edit. A later claim uses a new token.
- A semantic mutation that does not touch an actively leased item is never
  forced to invent a fencing token. Supplying a stale token cannot grant extra
  authority.

A move never rotates or closes a valid lease: lease ID, raw-token validity,
token hash, actor, kind, heartbeat, and expiry remain unchanged. After a
cross-workspace move, the same current token authorizes lease-bound commands
against the item in the target workspace. Tokens from leases closed before the
move remain stale and cannot become valid through ownership transfer.
Reassignment or unassignment is deliberately different: it closes the current
lease and attempt as specified above, so that token becomes permanently stale
and a later claimant receives a new token.

Raw import is an alternate input form, not an alternate workflow engine. For
each actively leased item, an interpreted state difference must call the same
Task 6 lifecycle decision used by the corresponding semantic command:

- running → review closes the execution lease and changes the running attempt
  to `Submitted`, with a `review_request` event;
- running → failed, blocked, cancelled, or a successful terminal state applies
  the same lease closure, `Failed`/`Cancelled`/`Completed` attempt outcome,
  policy/dependency/review checks, and `failure`/`block`/`cancellation`/
  `completion` event as the fenced command;
- review → successful, rejection/rework, or cancelled applies the same
  reviewer-token validation and `Submitted -> Completed|Failed|Cancelled`
  decision plus `approval` and `completion`, `rejection`, or `cancellation`
  events; and
- every other policy-defined state change delegates to the same fenced
  transition validator and attempt/lease decision rather than inventing an
  import-only rule.

An active item's non-state interpreted changes, including title, priority,
schedule/deadline, dependency, and note links, still require its current token
but preserve the open lease and attempt unless their ordinary Task 6 semantic
command says otherwise. Raw import appends its batch-level `document_import`
event **and** every applicable typed semantic or lifecycle event; it may not
hide review, failure, block, cancellation, completion, schedule, link, title,
or other auditable meaning inside only the generic import event.

Every guard and source edit is in the same immediate transaction as document
CAS, projection replacement, lease/attempt changes, events, workspace revision
changes, and the idempotent result.

### Operational queries and opaque cursors

`org_query_queue` owns the state/ownership views `ready`, `assigned`, `running`,
`blocked`, `review`, `failed`, `expired_lease`, and `completed`.
`org_query_agenda` owns the time-oriented `scheduled` and
`upcoming_deadline` views. The predicates are exact:

| View | Membership at injected `now` | Primary order |
| --- | --- | --- |
| `ready` | Pure readiness is `Ready` for executable work, or `RecoveryCandidate` for running work with an expired execution lease; dependencies, due schedule, assignment/dispatch policy, retry budget, workspace activity, absence of another active lease, and workspace capacity must all permit claim | common order |
| `assigned` | `assignee IS NOT NULL` and state is not terminal; an optional assignee filter narrows the value | common order |
| `running` | state equals `policy.running_state`, whether its execution lease is active, absent, or expired | common order |
| `blocked` | state equals configured `BLOCKED` when that state exists; otherwise the view is empty | common order |
| `review` | state equals `policy.review_state`, with a marker for unleased, actively review-leased, or expired-review-lease | common order |
| `scheduled` | non-terminal item has `scheduled_utc`; past/due and future values are both members and the requested inclusive schedule window narrows them | `scheduled_utc`, then common order |
| `upcoming_deadline` | non-terminal item has `deadline_utc >= now`; the requested inclusive deadline window further narrows it | `deadline_utc`, then common order |
| `failed` | state equals `policy.failed_state`, regardless of remaining retry budget; the row reports whether retry is exhausted | common order |
| `expired_lease` | item has one open execution or review lease with `expires_at <= now`, before or after another read has observed it but before bookkeeping closes it | `expires_at`, then common order |
| `completed` | state belongs to `policy.successful_terminal_states`; failed and cancelled terminal items are not completed | common order |

The common order is priority (`A` highest through `Z`, absent last), earliest
deadline (absent last), earliest schedule (absent last), creation time,
workspace ID, and work-item ID. Every specialized order still ends with the
full common tuple, so all ties are total. Time comparisons are integral UTC
seconds: schedule is due at `scheduled_utc <= now`, lease is expired at
`expires_at <= now`, upcoming deadline begins at `deadline_utc >= now`, and
all explicit `from`/`to` bounds are inclusive. Queue filters may independently
supply inclusive scheduled and deadline bounds. Agenda `from`/`to` applies to
its primary scheduled/deadline value. The completed view's optional time
window uses the authoritative completion-event time while its ordering remains
the common order.

Views intentionally overlap. A scheduled assigned item may also be ready; a
running recovery candidate appears in `running`, `expired_lease`, and `ready`;
a review item with an expired reviewer lease appears in `review` and
`expired_lease`; an item with a future deadline may appear in any non-terminal
queue view and `upcoming_deadline`. `completed` cannot overlap ready, running,
blocked, review, failed, scheduled, or upcoming-deadline because those views
require a non-successful-terminal item. Expiry bookkeeping removes an item from
`expired_lease` only when a later mutation closes that lease; read-only pages
remain side-effect free.

Filters include workspace IDs, item type, state, priority, tags, assignee, and
inclusive UTC time bounds. One workspace is the default. A multi-workspace
request must name every workspace explicitly and is rejected unless every
selected policy permits cross-workspace agendas. Every row retains workspace
identity.

The default page size is 50 and the maximum is 200. Storage returns ordered
candidate batches; pipelines apply the final pure `note-org` readiness/policy
predicate at the same injected `now`. A page overfetches additional set-based
batches until it has `limit` eligible rows or storage is exhausted. The cursor
tracks the **last scanned** complete ordering tuple, including candidates
rejected by the pure post-filter, rather than merely the last returned row.
When the page fills in the middle of a batch, scanning stops at the row that
filled it so unscanned rows remain eligible for the next page.

A first page captures `evaluated_at` from the injected clock. A pipeline cursor
is a versioned opaque encoding of the normalized view/filter fingerprint, that
frozen `evaluated_at`, and the last-scanned tuple. Continuation pages reuse the
cursor's evaluation time even if the live clock advances; a new query without a
cursor captures a new time. Malformed cursors, cursor/filter reuse, an explicit
evaluation-time override, limit 0, and limit greater than 200 return
`invalid_input`. This overfetch/last-scanned contract prevents a page from
being underfilled by post-filtered rows and prevents filtered candidates from
reappearing forever, while unchanged data produces no duplicate or skipped
eligible rows across pages.

Workspace summaries contain counts for every operational view, timezone,
archive state, and workspace revision. They run the same candidate and pure
predicate definitions as pages at one injected `now`, not a looser raw SQL
count. Tests must prove that exhausting all unchanged pages for each view yields
exactly the corresponding summary count, including readiness post-filtered
fixtures and exact schedule/deadline/expiry boundaries. Both page and summary
DTOs expose their non-secret `evaluated_at` so callers/tests compare like with
like. Counts and read DTOs never include a fencing token.

### Stable errors

Extend the Slice 3 error model with:

```text
active_lease
stale_lease
concurrency_limit
retry_limit
```

`active_lease` means another unexpired owner prevents the operation.
`stale_lease` covers missing, expired, closed, wrong-actor, wrong-kind, or
token-mismatch ownership proof without revealing which token field failed.
`concurrency_limit` is retryable; `retry_limit` is not. Existing
`invalid_input`, `not_found`, `archived_workspace`, `stale_revision`,
`idempotency_conflict`, `invalid_transition`, `dependency_blocked`,
`review_required`, and `storage_failure` mappings remain stable.

---

## Task 1: Complete Pure Claim, Recovery, and Retry Rules

**Files:**

- Modify: `crates/note-org/src/readiness.rs`
- Modify: `crates/note-org/src/policy.rs`
- Modify: `crates/note-org/src/lib.rs`
- Create: `crates/note-org/src/execution.rs`
- Modify: `crates/note-org/tests/readiness_test.rs`
- Modify: `crates/note-org/tests/policy_test.rs`
- Create: `crates/note-org/tests/execution_test.rs`

- [ ] Add failing table tests for execution versus review eligibility, due
  schedule boundary, dependency state, open/expired lease, actor assignment,
  explicitly dispatched work, archived workspace, concurrency capacity, and
  expired-running recovery candidates.
- [ ] Add failing attempt-budget tests proving `retry_limit + 1`, review claims
  do not consume execution attempts, and expired/failed recovery is rejected
  after the budget is exhausted.
- [ ] Add pure lifecycle decision values for lease kind, claim/reclaim/retry,
  attempt closure, and transition target. Do not generate IDs/tokens or perform
  I/O in `note-org`.
- [ ] Validate that release, review rejection, and expiry recovery states are
  executable and that the policy contains the transitions required by each
  configured recovery path.
- [ ] Preserve the existing transition and dependency validators as the single
  source for state changes; do not duplicate policy rules in pipelines or SQL.
- [ ] Run:

```sh
cargo test -p note-org --test readiness_test --test policy_test --test execution_test
cargo test -p note-org
cargo fmt --all -- --check
```

## Task 2: Add Schema v5 Authoritative Leases

**Files:**

- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage/tests/org_workflow_records_test.rs`
- Modify: `crates/note-storage-turso/schema.sql`
- Create: `crates/note-storage-turso/migrations/0004_to_0005_org_claims.sql`
- Create: `crates/note-storage-turso/tests/fixtures/schema-v4.sql`
- Modify: `crates/note-storage-turso/src/preflight.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Modify: `crates/note-storage-turso/tests/schema_test.rs`
- Create: `crates/note-storage-pg/migrations/0004_org_claims_operational_views.sql`
- Modify: `crates/note-storage-pg/tests/lifecycle_test.rs`

- [ ] Add failing schema tests for fresh v5, v4-to-v5, chained v2/v3-to-v5,
  injected migration rollback with the original version readable, and ordered
  PostgreSQL migration registration.
- [ ] Define typed lease kind/end reason/record/new/update values in
  `note-storage`. Keep transport DTOs and raw token values out of storage read
  records.
- [ ] Add `org_leases` with the fields and checks in Public Semantics, no
  foreign key to rebuildable `org_work_items`, a foreign key to authoritative
  `org_attempts`, positive timestamps, paired end fields, and a partial unique
  index allowing only one open lease per work-item ID.
- [ ] Add indexes for workspace open/expiry lookup, item history, actor/kind,
  operational state/assignment/schedule/deadline ordering, dependencies, and
  stable pagination. Use equivalent indexes and constraints in both backends.
- [ ] Store only `fencing_token_hash`; add a schema assertion that no raw-token
  column exists.
- [ ] Make Turso accept versions 2–5 and advance `user_version` only after all
  DDL/data changes commit. Do not change Markdown tables or data.
- [ ] Run:

```sh
cargo test -p note-storage --test org_workflow_records_test
cargo test -p note-storage-turso --test schema_test
cargo test -p note-storage-pg --test lifecycle_test
cargo fmt --all -- --check
```

## Task 3: Implement Lease, Attempt, and Fencing Storage Parity

**Files:**

- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/transactions.rs`
- Modify: `crates/note-storage-contract-tests/src/lib.rs`
- Modify: `crates/note-storage-turso/tests/contracts_test.rs`
- Modify: `crates/note-storage-turso/tests/transactions_test.rs`
- Modify: `crates/note-storage-pg/tests/contracts_test.rs`
- Modify: `crates/note-storage-pg/tests/transactions_test.rs`

- [ ] Add failing shared contracts for lease insert/get/history, one-open-lease
  uniqueness, active versus expired reads at exact time boundaries, workspace
  active counts, review/execution kinds, token-hash conditional validation,
  heartbeat extension, one-time closure, expiry-event association, and lease
  preservation across projection rebuild.
- [ ] Add failing attempt contracts for atomic next-number allocation,
  `Running -> Submitted -> Completed|Failed|Cancelled`, one-time terminal closure,
  workspace-at-start preservation, and retry-budget reads by stable item ID.
- [ ] Implement driver-neutral methods that expose one open lease internally,
  list sanitized lease history, count unexpired workspace leases, conditionally
  heartbeat/close/move a lease, and allocate the next attempt under the active
  transaction.
- [ ] Return a single generic conditional conflict for wrong token hash,
  actor, kind, expiry, or closed state. Do not leak the expected hash.
- [ ] Prove lease/attempt/event/operation rollback together and prove a
  cancelled waiter or failed savepoint cannot leave an open lease or consume an
  attempt number.
- [ ] Use barriers/notifications to race two inserts for one item and prove one
  open-lease winner in each backend. Do not use sleeps.
- [ ] Add an atomic workspace-capacity contract, then race claims for two
  different items with `concurrency_limit = 1`; exactly one lease insert may
  commit. Prove the same contract guards a lease-ownership move into a target
  workspace against a simultaneous target-workspace lease insert.
- [ ] Run:

```sh
cargo test -p note-storage-turso --test contracts_test --test transactions_test
cargo test -p note-storage-pg --test contracts_test --test transactions_test
```

PostgreSQL skips are not acceptance evidence. Repeat these exact suites with a
real `TEST_DATABASE_URL` in Task 9.

## Task 4: Add Backend-Parity Operational Query Contracts

**Files:**

- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/lib.rs`
- Modify: `crates/note-storage-turso/tests/contracts_test.rs`
- Modify: `crates/note-storage-pg/tests/contracts_test.rs`

- [ ] Add failing shared contracts for all ten operational views, every
  required filter, exact inclusive time bounds, active/expired lease joins,
  dependency satisfaction, workspace archive filtering, current attempt
  counts, and workspace summary counts at an explicit `now`.
- [ ] Encode the exact queue-versus-agenda predicates, allowed overlaps,
  primary/common sort tuples, successful-only completed semantics, literal
  configured `BLOCKED` semantics, and `<=`/`>=` schedule, deadline, and expiry
  boundaries from Public Semantics. Turso and PostgreSQL must return identical
  ordered candidate streams before the pipeline post-filter.
- [ ] Add ordering fixtures that tie on priority, deadline, schedule, and
  creation time; prove stable workspace/item tie-breaking and strict resume
  after a structured storage cursor key.
- [ ] Add explicit recovery-candidate and review-lease markers to storage read
  rows, but never raw token/hash fields.
- [ ] Implement set-based SQL queries over projections plus runtime tables.
  Do not load all Org source or implement readiness by client-side document
  parsing. Pipeline/pure-domain validation remains the final claim authority.
- [ ] Treat expired leases as inactive in every query without requiring prior
  mutation, and exclude archived workspaces unless requested.
- [ ] Keep storage cursors structured and driver-neutral. Opaque encoding and
  normalized filter binding belong to pipelines in Task 8.
- [ ] Run:

```sh
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
cargo fmt --all -- --check
```

## Task 5: Implement Atomic Claim, Reclaim, Heartbeat, and Release

**Files:**

- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Modify: `crates/note-pipelines/src/org/error.rs`
- Modify: `crates/note-pipelines/src/org/context.rs`
- Create: `crates/note-pipelines/src/org/claims.rs`
- Modify: `crates/note-pipelines/src/org/projection.rs`
- Modify: `crates/note-pipelines/src/org/context_read.rs`
- Modify: `crates/note-pipelines/tests/support/org.rs`
- Create: `crates/note-pipelines/tests/org_claims_test.rs`

- [ ] Add failing tests for normal execution claim, review claim, complete
  claimed context, fixed-clock expiry, heartbeat boundary, release, actor and
  claim-policy checks, future schedule, dependency blocking, archived
  workspace, expected revision, concurrency limit, and idempotent replay with
  the exact original raw token.
- [ ] Add deterministic concurrent-claim tests using a barrier: exactly one
  caller receives a lease/context/token, the loser receives `active_lease` or
  `concurrency_limit` as appropriate, and only one attempt plus one claim/start
  event set exists.
- [ ] Separately race two different ready items in a workspace whose limit is
  one. The loser must receive `concurrency_limit`, not an item-level
  `active_lease`, and must leave no attempt, source edit, event, lease, or
  idempotency result.
- [ ] Add a token source to `OrgContext` that produces cryptographically random
  raw tokens in production and deterministic distinct tokens in tests. Hash
  before storage. Bind a one-way token digest into the canonical request
  fingerprint so different-token operation reuse conflicts, but never persist
  the raw token outside the token-bearing replay result or include it in
  errors, events, debug output, or general context.
- [ ] Implement normal claim as one idempotent immediate transaction: validate
  full readiness through `note-org`, apply the minimal running-state source
  edit with document CAS, replace projections, allocate attempt, create lease,
  append claim/start events, hydrate context, and store the token-bearing
  result.
- [ ] Implement review claim without changing source state or allocating a new
  execution attempt; bind the review lease to the submitted attempt.
- [ ] Implement heartbeat as lease/event-only with unchanged token and document
  revision. Implement release using the lifecycle matrix and the smallest
  source edit where a state changes.
- [ ] Add a versioned release request DTO with optional `target_state`. Test
  default execution/review behavior, every valid requested target class,
  invalid/disallowed/running/successful targets, dependency and review gates,
  unchanged-review versus changed-state event snapshots, and same-operation
  reuse with a different target returning `idempotency_conflict`.
- [ ] Implement expired execution reclaim atomically with idempotent internal
  expiry bookkeeping, recovery-state validation, retry-budget/capacity checks,
  a new attempt/token, and no intermediate committed state.
- [ ] Inject failures after source edit, attempt allocation, lease insert,
  events, context hydration, and operation-result write; prove full rollback
  and a safe same-operation retry.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_claims_test
cargo test -p note-pipelines org::mutation_test
cargo test -p note-pipelines --test org_recovery_context_test
```

## Task 6: Activate Every Fenced Execution and Review Command

**Files:**

- Modify: `crates/note-storage/src/records.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Modify: `crates/note-pipelines/src/org/items.rs`
- Create: `crates/note-pipelines/src/org/execution.rs`
- Create: `crates/note-pipelines/src/org/reviews.rs`
- Modify: `crates/note-pipelines/src/org/context_read.rs`
- Create: `crates/note-pipelines/tests/org_fenced_workflow_test.rs`
- Modify: `crates/note-pipelines/tests/org_recovery_context_test.rs`

- [ ] Add failing matrix tests for progress, submit-result direct completion,
  review-required submit, explicit request-review, reviewer claim, approval,
  rejection, failure, block, unblock, cancellation, and policy-defined
  transitions.
- [ ] Add one complete `Submitted` attempt matrix: claim/heartbeat/reviewer
  release/reviewer expiry keep it submitted; approval completes it; rejection
  or explicit release to another allowed non-review state fails it with the
  correct outcome; review cancellation or release to cancelled cancels it.
  Prove terminal closure cannot occur twice and review leases never allocate or
  consume a new execution attempt number.
- [ ] For every active-ownership command, test current token success plus
  missing, malformed, wrong actor, wrong kind, expired, released, and
  superseded token rejection with the same safe `stale_lease` shape.
- [ ] Add Agent A expiry → Agent B reclaim → delayed Agent A progress,
  completion, and transition tests. All delayed mutations must be rejected
  permanently without source, attempt, event, lease, or operation changes.
- [ ] Add explicit failed retry tests for attempt 2..N+1, over-limit rejection,
  assignment/capacity revalidation, new token each time, and preservation of
  previous progress/error/result/note/artifact history.
- [ ] Add/activate any missing typed event names required to distinguish result
  submission from ordinary progress; do not encode lifecycle meaning only in
  free-form metadata.
- [ ] Implement commands through stable `OrgCommandKind` values so the same
  operation ID and identical-shaped payload used by another command returns
  `idempotency_conflict` rather than replaying the wrong result.
- [ ] Update source, projections, lease, attempt, events, and idempotency result
  in one immediate transaction. Event-only progress leaves document revision
  unchanged; all source state changes require/return the document revision.
- [ ] Hydrate recovery context with sanitized active/expired lease metadata,
  all attempts, progress/results/review outcomes, and ordered events without a
  token or token hash.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_fenced_workflow_test
cargo test -p note-pipelines --test org_claims_test
cargo test -p note-pipelines --test org_recovery_context_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test transactions_test
```

## Task 7: Close Archive, Raw Import, Move, and Assignment Fencing Seams

**Files:**

- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/org.rs`
- Modify: `crates/note-storage-pg/src/org.rs`
- Modify: `crates/note-pipelines/src/org/workspaces.rs`
- Modify: `crates/note-pipelines/src/org/documents.rs`
- Modify: `crates/note-pipelines/src/org/items.rs`
- Modify: `crates/note-pipelines/src/org/projection.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Create: `crates/note-pipelines/tests/org_active_lease_guards_test.rs`
- Modify: `crates/note-pipelines/tests/org_workspace_document_test.rs`
- Modify: `crates/note-pipelines/tests/org_item_workflow_test.rs`

- [ ] Replace every Slice 3 conservative `LeaseGuard` seam with a real active
  lease lookup and add failing current/missing/stale-token tests for raw
  document put/import, document/item move, reparent, assignment,
  reassignment, unassignment, schedule/deadline, dependency, note-link, title,
  and priority changes that touch an active item.
- [ ] Add raw-import diff tests where only opaque bytes change, semantic fields
  change with the correct token map, one of multiple tokens is missing, an
  item is omitted, or a state/assignment change requires closing ownership.
  Validate the whole candidate before any write and commit all documents or
  none.
- [ ] For active execution leases, test raw state diffs from running to review,
  failed, blocked, cancelled, and successful terminal. Assert the exact same
  Task 6 policy decision, lease closure, `Submitted`/`Failed`/`Cancelled`/
  `Completed` attempt outcome, source state, and typed lifecycle event(s) as
  the corresponding fenced semantic command.
- [ ] For active review leases, test raw review-to-approval,
  review-to-rejection/rework, and review-to-cancellation diffs. Assert current
  reviewer-token validation and the exact Task 6
  `Submitted -> Completed|Failed|Cancelled` attempt and approval/completion,
  rejection, or cancellation event matrix.
- [ ] Test active-lease non-state raw diffs for title, priority,
  schedule/deadline, dependency, and note-link changes. The current token is
  required, but lease ID/token validity/actor/kind/heartbeat/expiry and attempt
  status remain unchanged. Assert both the batch `document_import` event and
  every applicable typed semantic event; a generic import event alone fails
  the test.
- [ ] Add archive tests: active execution or review lease always rejects;
  expired leases are idempotently closed first; no active lease plus expected
  workspace revision archives successfully; archived reads/export/events
  remain available while every mutation and claim is rejected.
- [ ] Add same-workspace move tests proving current token preserves lease and
  attempt identity. Add cross-workspace move tests for source/target revisions,
  target policy and capacity, token maps for all contained active items,
  current lease ownership transfer, unchanged attempt workspace-at-start, and
  linked source/target events.
- [ ] After same- and cross-workspace moves, prove the pre-move current token
  remains valid with unchanged lease ID/hash/actor/kind/expiry, while every
  token from a lease closed before the move remains stale. Contrast this with
  reassignment/unassignment, which must close the current lease and make its
  token stale.
- [ ] Barrier-race an active-lease cross-workspace move against a claim in the
  target workspace at `concurrency_limit = 1`. Exactly one commits; the loser
  observes a capacity conflict and neither backend exposes partial ownership,
  workspace revision, source, projection, attempt, event, or operation state.
- [ ] Add active reassignment/unassignment tests proving correct token closes
  and fences ownership, closes the attempt, applies the recovery state and
  assignment atomically, and lets only a later fresh claim create a new token.
- [ ] Inject failure at each CAS/projection/lease/event/operation boundary and
  prove neither workspace observes a partial move, archive, import, or
  reassignment.
- [ ] For every raw-import lifecycle branch above, inject failure after the
  source CAS, projection replacement, lease decision, attempt decision, each
  specialized event, the batch import event, and operation-result write. Prove
  source, projection, lease/token validity, attempt, event sequence, workspace
  revision, and idempotency record all roll back together.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_active_lease_guards_test
cargo test -p note-pipelines --test org_workspace_document_test
cargo test -p note-pipelines --test org_item_workflow_test
cargo test -p note-storage-turso --test transactions_test
cargo test -p note-storage-pg --test transactions_test
```

## Task 8: Build Queue, Agenda, Summary, and Context Pipelines

**Files:**

- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/note-pipelines/src/org/mod.rs`
- Modify: `crates/note-pipelines/src/org/dto.rs`
- Create: `crates/note-pipelines/src/org/cursor.rs`
- Create: `crates/note-pipelines/src/org/operational.rs`
- Modify: `crates/note-pipelines/src/org/context_read.rs`
- Modify: `crates/note-pipelines/src/org/workspaces.rs`
- Create: `crates/note-pipelines/tests/org_operational_views_test.rs`
- Modify: `crates/note-pipelines/tests/org_recovery_context_test.rs`

- [ ] Add failing DTO/query tests for all ten views, filters, workspace
  identity, workspace-time/raw/UTC timestamp fields, recovery markers,
  sanitized lease metadata, attempt counts, and all summary counts.
- [ ] Add pagination tests for defaults 50/max 200, two or more pages with all
  sort-field ties, malformed cursor, cursor used with changed view/filter,
  stable-ID resumption, explicit archived inclusion, and no duplicates or
  omissions.
- [ ] Add readiness post-filter fixtures where rejected candidates precede and
  separate eligible rows. Prove batched overfetch fills the requested page when
  enough eligible rows exist, cursors resume after the last scanned rather than
  last returned candidate, an unchanged full traversal returns each eligible
  row exactly once, and continuation pages reuse the cursor's frozen
  `evaluated_at` after the live test clock advances.
- [ ] Add multi-workspace tests proving explicit IDs are required, every policy
  must allow cross-workspace agenda access, results preserve workspace IDs,
  and one denied workspace rejects the entire query without a partial page.
- [ ] Encode versioned opaque cursors in pipelines and bind them to a canonical
  normalized query fingerprint. Do not expose SQL offsets or backend row IDs.
- [ ] Compose storage candidates with final pure `note-org` readiness checks at
  the single injected `now`. Queue consumers never parse Org source or
  reimplement policy.
- [ ] Produce `WorkspaceSummary`, `OperationalItemSummary`, page, filter, and
  sanitized lease/attempt DTOs needed by later MCP/REST/Web slices without
  adding any transport serialization wrapper or handler.
- [ ] Calculate summary counts through the same candidate plus pure predicate
  path as pages. For every view, exhaust all unchanged pages and assert the row
  count equals the summary, including post-filtered readiness and exact
  schedule/deadline/expiry boundary fixtures.
- [ ] Extend item context with current operational classification, attempt
  budget/recovery status, and lease metadata excluding token/hash. Preserve
  global lineage-segment event ordering across workspace moves.
- [ ] Add query-count or trace assertions proving a page/summary uses bounded
  set-based reads rather than one database query per item.
- [ ] Run:

```sh
cargo test -p note-pipelines --test org_operational_views_test
cargo test -p note-pipelines --test org_recovery_context_test
cargo test -p note-pipelines --test org_claims_test
cargo fmt --all -- --check
```

## Task 9: Prove Turso and Real PostgreSQL High-Risk Parity

**Files:**

- Modify: `crates/note-pipelines/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/note-storage-contract-tests/src/org.rs`
- Modify: `crates/note-storage-contract-tests/src/transactions.rs`
- Modify: `crates/note-storage-turso/tests/contracts_test.rs`
- Modify: `crates/note-storage-turso/tests/transactions_test.rs`
- Modify: `crates/note-storage-pg/tests/contracts_test.rs`
- Modify: `crates/note-storage-pg/tests/transactions_test.rs`
- Modify: `crates/note-pipelines/tests/support/org.rs`
- Create: `crates/note-pipelines/tests/org_claims_acceptance_test.rs`

- [ ] Build one reusable high-risk scenario covering two-agent claim race,
  heartbeat boundary, expiry and reclaim, stale-token rejection, failure and
  retry, reviewer exclusivity, archive blocking, and deterministic ready/
  expired/completed views.
- [ ] Run it against Turso and through the PostgreSQL storage-backed pipeline
  context with the same fixed clock, IDs, source, policy, and normalized
  outcomes.
- [ ] Add barrier-driven concurrency at the backend/pipeline boundary; do not
  weaken PostgreSQL to an in-memory fake or accept timing sleeps.
- [ ] Run both capacity races in Turso and live PostgreSQL: two different-item
  claims at limit one, and active-lease move-to-target versus target claim at
  limit one. Normalize the winner-independent result and prove one commit, one
  capacity loser, exact target active count one, and no loser side effects.
- [ ] Prove operation replay after reopening storage returns the original lease
  result without a duplicate attempt, event, state change, or new token.
- [ ] Run Turso locally:

```sh
cargo test -p note-storage-turso --test contracts_test --test transactions_test
cargo test -p note-pipelines --test org_claims_acceptance_test
```

- [ ] Run the real PostgreSQL gate:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test lifecycle_test --test contracts_test --test transactions_test
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-pipelines --test org_claims_acceptance_test
```

If no local pgvector PostgreSQL is available, obtain the same successful test
evidence from the repository CI service before declaring the slice complete.
A visible local skip is not success.

## Task 10: Slice 4 Documentation, Review, and Scoped Completion Gate

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] Document schema v5, claim/heartbeat/release/recovery behavior, asserted
  actor IDs, retry and concurrency semantics, operational pipeline views, and
  the absence of MCP/REST/UI exposure until later slices.
- [ ] State explicitly that Agent Note implements no authentication or
  authorization; a front proxy owns the deployment security boundary and
  direct untrusted-network exposure is unsupported. Do not implement or
  document a proxy-specific header protocol.
- [ ] Audit every ownership-sensitive mutation and prove there is no optional,
  dummy, or admin bypass token path for an active item. Search result/log/error
  DTOs for raw token or token-hash leakage.
- [ ] Audit the diff against this plan and reject accidental Slice 5–7 MCP,
  offline, REST/OpenAPI, or frontend work.
- [ ] Request an independent requirements review against the approved PRD,
  interface design, Slice 3 safety boundary, and this plan. Resolve every
  Critical or Important gap and rerun affected tests.
- [ ] Request a fresh independent code-quality review focused on transaction
  atomicity, cancellation safety, SQL parity, token-hash comparison and
  non-disclosure, cursor stability, bounded query shape, and secret-safe errors.
  Resolve every Critical or Important finding.
- [ ] Run the final scoped gate:

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

- [ ] Attach successful real PostgreSQL lifecycle, contract, transaction, and
  acceptance output. Do not mark Slice 3 or Slice 4 complete without it.

## Slice 4 Completion Evidence

Slice 4 is complete only when current evidence proves:

1. same-item claims and different-item claims at workspace limit one each
   produce exactly one lease/attempt/token winner, and move-to-target versus
   target-claim capacity races produce exactly one commit;
2. raw tokens are unguessable, stored only as hashes outside replay results,
   and absent from general reads, errors, events, logs, queues, and context;
3. a legal same- or cross-workspace move preserves the current lease and keeps
   its current token valid in the resulting workspace; tokens already stale
   before the move never regain validity; reclaim, reassignment, unassignment,
   and release each invalidate the old token permanently while every
   ownership-sensitive mutation accepts only the resulting current unexpired
   token;
4. heartbeat, default/explicit-target release, expiry bookkeeping, reclaim,
   failure retry, review cancellation, and every `Submitted` attempt closure
   follow the documented matrix at exact clock boundaries;
5. retry and workspace concurrency limits are enforced atomically;
6. archive, raw import, move/reparent, and reassign/unassign paths cannot bypass
   active lease rules and never partially commit; active-lease raw state diffs
   reuse the exact Task 6 attempt/lifecycle decisions and typed events, while
   non-state interpreted diffs preserve ownership and emit both import and
   specialized audit events;
7. ready queues exclude incomplete dependencies, future schedules, archived
   workspaces, active leases, disallowed assignments, exhausted retries, and
   capacity-blocked work while explicitly marking eligible recovery items;
8. all ten exact operational predicates, overlaps, filters, summaries, time
   boundaries, and overfetch/last-scanned opaque cursor pages are deterministic,
   summary-consistent, and backend-equivalent;
9. identical operation retries replay the exact original result without extra
   effects, while cross-command or divergent reuse conflicts; and
10. Turso and a real PostgreSQL database pass the same scoped contracts and
    high-risk acceptance scenario, with no Slice 5–7 implementation present.

Passing this gate closes the fencing-dependent remainder of Slice 3 and all of
Slice 4. The next work is a separately planned Slice 5 MCP/offline delivery;
this plan does not authorize starting it.
