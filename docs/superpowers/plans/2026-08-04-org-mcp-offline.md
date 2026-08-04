# Org MCP and Offline Interfaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan task-by-task.
> Every task follows RED → GREEN → scoped verification → independent
> requirements review → independent code-quality review. Fix every finding and
> rerun the focused commands before accepting the task.

**Goal:** Deliver Org Delivery Slice 5 by exposing all 36 approved `org_*`
operations through the existing MCP server over stdio and Streamable HTTP,
adding offline workspace and document import/export modes to `note-server`, and
proving conflict, idempotency, lease recovery, and existing-note compatibility
without adding REST or browser behavior.

**Architecture:** `note-pipelines::org` remains the only application boundary.
`note-mcp` owns closed protocol request/response schemas and a safe structured
error adapter, while the existing `NoteMcpServer` remains the single tool
registry instantiated by both transports. The production `note-server`
composition root constructs the existing Markdown-note `Context` and exactly
one production `Arc<OrgContext>` over the same storage backend, then injects
both contexts through `NoteMcpServer::new`, `run_stdio`, and `mcp_router`; stdio
and HTTP must not construct private Org contexts. Offline modes deliberately
construct a separate storage-only `OrgContext` before attachment or embedding
runtime startup. Offline files are a portable manifest plus raw `.org` bytes;
they use the same import/export pipelines and never read or mutate Markdown
note data.

**Tech Stack:** Rust 2021, `rmcp` 2, `schemars`, `serde`, `serde_json`, Axum
Streamable HTTP, Tokio process/I/O tests, `note-pipelines::org`, Turso test
databases, and the existing `note-server` configuration loader.

---

## Scope and Dependency Gate

This plan starts only after every task in
`2026-08-04-org-claims-operational-views.md` is committed and green. In
particular, all 36 operations must already exist as transport-neutral pipeline
functions with stable DTOs/errors, an injectable clock/token source, complete
fencing, queues/agendas, and sanitized read models. If a pipeline operation is
missing, finish Slice 4 first; do not implement workflow rules in MCP.

This plan implements:

- exactly the approved 36 `org_*` MCP tools in addition to the existing 11
  Markdown-note tools;
- closed JSON schemas, cursor bounds, mutation envelopes, expected revisions,
  token maps, and claim-only fencing-token responses;
- one safe structured MCP error shape shared by every Org tool;
- the same registry, schemas, tool calls, results, and errors over stdio and
  Streamable HTTP;
- one production Org clock/token source and one shared `OrgContext` instance
  per server process, while preserving the existing Note `Context` for all
  Markdown tools;
- offline workspace export/import and raw document export/import, including
  create and revision-safe update modes;
- portable export layout, path-escape protection, atomic writes, structured
  per-document conflict reports, and storage-only startup; and
- high-risk end-to-end conflict, idempotency, claim/recovery, stale-owner, and
  archived-workspace scenarios plus existing MCP/offline regression gates.

This plan does **not** add `/api/org` REST routes, OpenAPI operations, frontend
routes, authentication, authorization, sessions, proxy identity headers,
browser controls, automatic polling, or a filesystem watcher. Those belong to
Slices 6–7 or remain deferred.

## Exact MCP Inventory

The Org inventory is fixed and filtered by the `org_` prefix in tests:

```text
org_list_workspaces       org_create_workspace      org_get_workspace
org_update_workspace      org_archive_workspace     org_list_documents
org_get_document          org_put_document           org_move_document
org_move_item             org_import_workspace       org_export_workspace
org_create_item           org_get_item               org_get_item_context
org_create_follow_up      org_assign_item            org_schedule_item
org_query_queue           org_query_agenda           org_claim_item
org_heartbeat_claim       org_release_claim          org_report_progress
org_submit_result         org_transition_item        org_retry_item
org_request_review        org_approve_item            org_reject_item
org_add_dependency        org_remove_dependency      org_link_note
org_unlink_note           org_list_note_work_items   org_list_events
```

The aggregate server inventory becomes 47 tools. Existing note tool names and
schemas do not change.

## MCP Contract Rules

- MCP DTOs are protocol adapters around pipeline DTOs. They may derive
  `JsonSchema`, but must not duplicate policy, readiness, revision, lease, or
  idempotency decisions.
- Every client mutation schema requires trimmed non-empty `actor_id` and
  `operation_id`; the pipeline remains authoritative and rejects reserved
  actor `system`.
- Workspace mutations carry the expected workspace revision. Source mutations
  carry every affected document revision. `org_move_document` carries the
  document revision plus source and target workspace revisions. Raw imports
  carry a fencing-token map for each actively leased item they change.
- Lease-bound tools require a raw opaque fencing token. Only successful claim
  and retry/reclaim results may return it. General workspace, queue, agenda,
  item-context, lease-history, event, and export DTOs contain neither raw tokens
  nor token hashes.
- List inputs use an opaque cursor, default limit 50, and maximum 200. MCP does
  not decode cursors or implement ordering.
- Org errors use the structured data object below. The top-level MCP message is
  safe and contains no database detail or token material:

```json
{
  "code": "stale_revision",
  "message": "document revision is stale",
  "details": {"current_revision": 8},
  "retryable": false
}
```

Stable codes include all Slice 4 categories: `invalid_input`, `not_found`,
`archived_workspace`, `stale_revision`, `idempotency_conflict`,
`invalid_transition`, `dependency_blocked`, `review_required`,
`unsupported_semantic_edit`, `note_unavailable`, `active_lease`, `stale_lease`,
`concurrency_limit`, `retry_limit`, and `storage_failure`.

## Offline Command Contract

The server binary adds these exact modes while preserving existing
`--import`, `--export`, `--stdio`, and HTTP behavior:

```text
note-server org export-workspace --workspace-id <uuid> --output <directory>
note-server org import-workspace --input <directory> --mode create|update \
  --actor-id <id> --operation-id <id>
note-server org export-document --document-id <uuid> --output <file.org>
note-server org import-document --workspace-id <uuid> --document-id <uuid> \
  --path <org-path> --input <file.org> --mode create|update \
  --actor-id <id> --operation-id <id> [--expected-revision <n>]
```

`update` document import requires `--expected-revision`; `create` rejects an
existing document and rejects that flag. Workspace update import uses the
workspace/document revisions embedded in the exported manifest. All modes load
the normal database configuration but do not construct an attachment store,
resolve an embedding secret, start embedding workers/schedulers, or touch
Markdown notes.

A workspace export directory contains:

```text
manifest.json
documents/<document-uuid>.org
```

The manifest contains format version, workspace metadata/policy/revision, and
for every document its stable ID, canonical workspace path, revision, content
hash, and relative export filename. UUID filenames prevent a canonical Org path
from escaping the output directory. Import rejects absolute paths, `..`,
symlink escape, duplicate IDs/paths, missing or extra manifest files, content-
hash mismatch, unsupported format versions, and invalid UTF-8 before calling a
mutation pipeline. Export writes a sibling temporary file/directory and
atomically renames it only after every file succeeds; it never overwrites a
non-empty destination implicitly.

Success and failure write a machine-readable report to stdout. Domain
validation/conflict exits nonzero and includes document-level entries with
stable code, safe message/details, current revision where available, and
`applied: false`; it never partially applies a batch.

## Task 1: Establish Shared Org MCP Schemas, Error Mapping, and Inventory

**Files:**

- Modify: `crates/note-mcp/Cargo.toml`
- Modify: `crates/note-mcp/src/lib.rs`
- Modify: `crates/note-mcp/src/http.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Create: `crates/note-mcp/src/org.rs`
- Create: `crates/note-mcp/src/org_dto.rs`
- Create: `crates/note-mcp/src/org_error.rs`
- Create: `crates/note-mcp/tests/org_inventory_test.rs`
- Modify: `crates/note-server/src/main.rs`

- [ ] Write failing tests that filter exactly 36 `org_*` tools, update the
  aggregate inventory from 11 to 47, retain all original note tools, and reject
  duplicate names.
- [ ] Add schema tests for closed top-level objects, required mutation envelope
  fields, expected revisions, token maps, fencing tokens on lease-bound inputs,
  optional cursors, and 50/200 list semantics.
- [ ] Add negative schema assertions proving general read outputs have no
  `fencing_token`, `fencing_token_hash`, secret, or authentication field.
- [ ] Define MCP request/response adapters around pipeline DTOs. Prefer shared
  wrapper types and conversion implementations over hand-built JSON inside 36
  handlers; keep pipeline types free of `rmcp` and `schemars` dependencies.
- [ ] Export one ordered `ORG_TOOL_NAMES: [&str; 36]` contract from
  `note-mcp::org`; registration, HTTP/stdio inventory tests, and Slice 6 REST
  operation-ID conformance all consume this constant rather than maintaining
  separate name lists.
- [ ] Add one `OrgPipelineError -> ErrorData` adapter that preserves
  `code/message/details/retryable` in structured error data and never serializes
  a source error chain.
- [ ] Change `NoteMcpServer::new` to accept both the existing
  `Arc<note_pipelines::Context>` and an `Arc<OrgContext>`. Keep both fields so
  the eleven Markdown tools continue using Note Context and every `org_*` tool
  uses Org Context.
- [ ] Change `run_stdio` and `mcp_router` to require those same two contexts and
  pass their `Arc` clones into `NoteMcpServer::new`; neither adapter may create
  its own clock, token source, storage session, or Org context.
- [ ] In the production `note-server` composition root, construct Note Context
  as today, construct one production Org Context over the identical
  `Arc<dyn StorageBackend>` with `SystemOrgClock` and the Slice 4 secure token
  source, and inject the same `Arc<OrgContext>` into whichever MCP transport is
  selected. Unit-test context identity/clock-token ownership at the composition
  seam rather than relying only on matching values.
- [ ] Extend `ServerInfo.instructions` to describe Org orchestration and the
  asserted-audit-only actor ID. Do not describe actor IDs as authentication.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-mcp --test org_inventory_test
cargo test -p note-mcp stdio::tests::server_builds_and_lists
cargo fmt --all -- --check
```

## Task 2: Register Workspace and Document Tools

**Files:**

- Modify: `crates/note-mcp/src/org.rs`
- Modify: `crates/note-mcp/src/org_dto.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Create: `crates/note-mcp/tests/org_workspace_document_tools_test.rs`

- [ ] Write failing black-box handler tests for the twelve workspace/document
  tools: active/archived listing, create/get/update/archive, list/get/put,
  document move, item move, workspace import, and workspace export.
- [ ] Cover create and update import, raw byte preservation, expected workspace
  and document revisions, source/target revision maps, active-lease token maps,
  current-revision conflict details, omitted-item rejection, archive read/export
  access, and archived mutation rejection.
- [ ] Test same `operation_id` replay and divergent fingerprint conflict through
  the MCP adapter; assert no duplicate source revision or event.
- [ ] Delegate every handler directly to its named pipeline operation and map
  the transport-neutral result. Do not implement file I/O in MCP export/import.
- [ ] Confirm raw source is represented without rewriting line endings or
  unsupported Org syntax.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-mcp --test org_workspace_document_tools_test
cargo test -p note-pipelines --test org_workspace_document_test
cargo fmt --all -- --check
```

## Task 3: Register Item, Context, Queue, and Agenda Tools

**Files:**

- Modify: `crates/note-mcp/src/org.rs`
- Modify: `crates/note-mcp/src/org_dto.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Create: `crates/note-mcp/tests/org_item_query_tools_test.rs`

- [ ] Write failing tests for create/get/context/follow-up, assignment,
  scheduling, queue, and agenda tools.
- [ ] Cover all ten operational views; item type/state/priority/tag/assignee/time
  filters; workspace identity; explicit multi-workspace policy checks; malformed
  and filter-mismatched cursors; default 50/max 200; and stable next cursors.
- [ ] Assert context includes hierarchy, policy, dependencies, note
  availability, attempts, sanitized lease metadata, recovery status, revisions,
  and sequence-ordered events, but never a token/hash.
- [ ] Verify workspace-local/original/UTC timestamps survive DTO conversion and
  that no MCP adapter consults wall-clock time independently.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-mcp --test org_item_query_tools_test
cargo test -p note-pipelines --test org_operational_views_test --test org_recovery_context_test
cargo fmt --all -- --check
```

## Task 4: Register Claims, Execution, Review, Relationship, and Audit Tools

**Files:**

- Modify: `crates/note-mcp/src/org.rs`
- Modify: `crates/note-mcp/src/org_dto.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Create: `crates/note-mcp/tests/org_workflow_tools_test.rs`

- [ ] Write a failing table-driven test for claim, heartbeat, release,
  progress, result, transition, retry, review request/approval/rejection,
  dependency add/remove, note link/unlink/reverse lookup, and event listing.
- [ ] Cover successful execution and review claims, exact replay of a token-
  bearing claim result, heartbeat without document revision change, release,
  failure/retry, review-required completion, approval/rejection, block/unblock,
  cancellation, note-unavailable reads, and sequence pagination.
- [ ] Prove missing/wrong/expired/released/superseded tokens all normalize to the
  same safe `stale_lease` data and never disclose comparison details.
- [ ] Prove Agent A expiry → Agent B reclaim → delayed Agent A progress,
  transition, and completion is permanently rejected without a second event.
- [ ] Ensure typed link purposes and event metadata remain structured; do not
  make clients parse summaries.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-mcp --test org_workflow_tools_test
cargo test -p note-pipelines --test org_claims_test --test org_fenced_workflow_test
cargo fmt --all -- --check
```

## Task 5: Prove Stdio and Streamable HTTP Isomorphism

**Files:**

- Modify: `crates/note-mcp/Cargo.toml`
- Modify: `crates/note-mcp/src/http.rs`
- Modify: `crates/note-mcp/src/stdio.rs`
- Create: `crates/note-mcp/tests/org_transports_test.rs`

- [ ] Factor a transport-parameterized stdio serving helper used unchanged by
  production `run_stdio(stdio())` and by tests with `tokio::io::duplex`. The
  test client must exchange real MCP JSON-RPC messages, including `initialize`,
  `notifications/initialized`, `tools/list`, and `tools/call`; directly calling
  Rust handler methods or inspecting `tool_router` is not stdio acceptance.
- [ ] Exercise the actual stateless `mcp_router` through Axum requests using
  Streamable HTTP JSON-RPC `tools/list` and `tools/call`. Direct handler calls
  are allowed only as lower-level unit tests, not as transport-isomorphism
  evidence.
- [ ] For immutable inventory/schema comparison, build both real transports
  from one seeded composition bundle and inject the same Note Context plus the
  same `Arc<OrgContext>`. Normalize and compare all 36 names, descriptions,
  input schemas, and output schemas returned by `tools/list`.
- [ ] Call representative read and mutation tools using real JSON-RPC
  `tools/call` through each transport:
  create workspace/item, queue, claim, heartbeat, transition/review, event
  history, stale revision, idempotency conflict, active lease, and stale lease.
  Use two separate, identically seeded composition bundles for mutating
  scenario comparison so one transport does not consume the other's
  idempotency/lease state. Inside each bundle, Note Context and Org Context use
  the same storage backend, and the Org Context passed to the tested transport
  is the exact composition-root instance. Compare normalized result/error JSON.
- [ ] Assert both transports return the same structured
  `code/message/details/retryable` error data, not merely matching top-level MCP
  error codes or Rust error variants.
- [ ] Prove the HTTP factory creates `NoteMcpServer` directly and that neither
  transport has a transport-specific Org registry or workflow branch.
- [ ] Assert Streamable HTTP remains stateless JSON mode at `/mcp`; do not add
  legacy HTTP+SSE endpoints, sessions, Host allowlists, or auth middleware.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-mcp --test org_transports_test
cargo test -p note-mcp
cargo fmt --all -- --check
```

## Task 6: Add Storage-Only Offline Workspace and Document Commands

**Files:**

- Modify: `crates/note-server/Cargo.toml`
- Modify: `crates/note-server/src/main.rs`
- Create: `crates/note-server/src/org_offline.rs`
- Create: `crates/note-server/tests/org_offline_test.rs`
- Modify: `crates/note-server/tests/embedding_selection_test.rs`

- [ ] Write failing parser/validation tests for the four exact command modes,
  required arguments, mutually exclusive create/update rules, nonzero domain
  error exits, and backward compatibility with existing flags.
- [ ] Write failing filesystem tests for deterministic manifest/document
  layout, byte-exact LF/CRLF and opaque syntax, stable IDs/revisions, content
  hashes, atomic destination replacement, and refusal to overwrite non-empty
  output.
- [ ] Add traversal, absolute path, symlink escape, duplicate manifest entry,
  missing/extra file, hash mismatch, malformed manifest, unsupported version,
  stale revision, active lease without token, and all-or-nothing batch tests.
- [ ] Implement a small typed argument parser and file adapter in
  `org_offline.rs`; call only workspace/document import/export pipelines. Keep
  canonical paths in the manifest and use stable document-ID filenames.
- [ ] Dispatch Org offline mode after database configuration/storage creation
  but before attachment-store construction, embedding-secret resolution,
  worker/scheduler startup, HTTP bind, or MCP setup.
- [ ] Extend the existing remote-embedding offline test to prove all four Org
  modes neither resolve/contact the embedder nor create/read attachment or
  Markdown note data.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_offline_test --test embedding_selection_test
cargo test -p note-pipelines --test org_workspace_document_test
cargo fmt --all -- --check
```

## Task 7: High-Risk Acceptance, Documentation, and Slice Gate

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`
- Modify: `crates/note-mcp/tests/org_transports_test.rs`
- Modify: `crates/note-server/tests/org_offline_test.rs`

- [ ] Add one end-to-end Turso scenario that crosses MCP create/import, ready
  queue, two-agent claim race, heartbeat boundary, expiry/reclaim, stale-owner
  rejection, review, event order, export, restart/reopen, and idempotent replay.
- [ ] Export, edit from revision N, mutate to N+1 through MCP, and prove offline
  revision-N import reports the current revision and changes no source,
  projection, lease, event, or operation record.
- [ ] Reopen storage and prove MCP/offline reads preserve workspace/item IDs,
  attempts, links, event sequence, and raw source. Confirm existing Markdown
  note MCP tools and legacy note import/export remain unchanged.
- [ ] Document all 36 tools, the four offline modes/layout, asserted actor IDs,
  no-auth boundary, front-proxy responsibility, token sensitivity, and the
  absence of REST/Web in this slice.
- [ ] Obtain a final independent Slice 5 requirements audit and a fresh code-
  quality review; fix and rerun all scoped gates:

```sh
cargo test -p note-mcp
cargo test -p note-server --test org_offline_test --test embedding_selection_test
cargo test -p note-server --test notes_api_test --test labels_api_test
cargo test -p note-pipelines --test org_claims_acceptance_test
cargo check -p note-mcp --all-targets
cargo check -p note-server --all-targets
cargo fmt --all -- --check
git diff --check
```

Expected: exact 36-tool Org inventory on both MCP transports, all offline and
high-risk scenarios pass, no REST `/api/org` route or frontend file changed,
and no authentication/session/proxy-identity implementation appears.

## Completion Checklist

- [ ] Slice 4 prerequisite is committed and green.
- [ ] All 36 Org tools and only those names are registered on both transports.
- [ ] Existing 11 note tools remain compatible.
- [ ] Structured errors and token exposure rules are proven.
- [ ] Workspace/document offline create/update flows are portable and atomic.
- [ ] Conflict, recovery, restart, and stale-owner acceptance scenarios pass.
- [ ] Every task has independent requirements and quality approval.
- [ ] Scoped checks and `git diff --check` pass.
- [ ] No Slice 6 REST/OpenAPI or Slice 7 frontend work was introduced.
