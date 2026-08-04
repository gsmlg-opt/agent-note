# Org REST and OpenAPI Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan task-by-task.
> Every task follows RED → GREEN → scoped verification → independent
> requirements review → independent code-quality review. Fix every finding and
> rerun the focused commands before accepting the task.

**Goal:** Deliver Org Delivery Slice 6 by exposing REST equivalents for all 36
approved MCP Org operations, documenting every operation in generated OpenAPI
with the exact `org_*` operation ID, and proving transport-neutral success,
idempotency, validation, and error behavior through cross-transport tests.

**Architecture:** `note-server::org_api` is a thin Axum/utoipa adapter around
the same `note-pipelines::org` functions used by MCP. The production composition
root creates one shared `AppState` containing the existing
`Arc<note_pipelines::Context>` for Markdown behavior and the exact same
`Arc<OrgContext>` already injected into `NoteMcpServer`; REST and MCP therefore
share storage, clock, token source, and Org application state. Axum `FromRef`
extraction lets existing note handlers keep using Note Context while Org
handlers receive Org Context. Handler modules own only HTTP path, query/body
extraction, response status/JSON serialization, and centralized safe error
mapping. They never implement workflow rules, retries, revisions, cursor
decoding, fencing, or clock reads. The Yew frontend is not changed in this
slice.

**Tech Stack:** Rust 2021, Axum 0.8, `utoipa` 5, `utoipa-axum`, `serde`,
`serde_json`, `tower::ServiceExt`, `note-pipelines::org`, existing MCP adapters,
Turso integration contexts, deterministic clocks/tokens, and generated OpenAPI
3.1.

---

## Scope and Dependency Gate

Begin only after `2026-08-04-org-mcp-offline.md` is committed and green with
the exact 36-tool inventory on stdio and Streamable HTTP. REST consumes the
same transport-neutral DTOs and pipelines; if a required operation or stable
error category is missing, finish Slice 5/Slice 4 first rather than implementing
business behavior in a REST handler.

This plan implements:

- exactly 36 REST operations under `/api/org` with one-to-one MCP parity;
- complete generated OpenAPI paths, request/response schemas, stable error
  schema, cursor constraints, examples, and exact MCP-name `operationId`s;
- structured JSON errors with stable HTTP mappings;
- complete read and mutation parity, including token-bearing claim responses,
  raw Org import/export, queues/agendas, reviews, relationships, and audit;
- one production `AppState` that preserves existing Note Context and shares one
  Org Context instance between REST and both MCP transports;
- representative cross-transport black-box workflows plus schema-level
  coverage for every operation; and
- documentation of no authentication and the required front-proxy boundary.

This plan does **not** add Yew routes or call REST from a browser, change MCP or
offline semantics, add authentication/authorization/session/ACL behavior, add
proxy-specific headers, expose fencing tokens in read DTOs, or implement Web
mutations. Slice 7 consumes only the read subset after this plan is complete.

## Exact Route and Operation Matrix

Every OpenAPI `operationId` in the final column is literal and unique.

| Method and path | Behavior | `operationId` |
| --- | --- | --- |
| `GET /api/org/workspaces` | list summaries | `org_list_workspaces` |
| `POST /api/org/workspaces` | create | `org_create_workspace` |
| `GET /api/org/workspaces/{workspace_id}` | get | `org_get_workspace` |
| `PATCH /api/org/workspaces/{workspace_id}` | update | `org_update_workspace` |
| `POST /api/org/workspaces/{workspace_id}/archive` | archive | `org_archive_workspace` |
| `GET /api/org/workspaces/{workspace_id}/documents` | list documents | `org_list_documents` |
| `GET /api/org/documents/{document_id}` | raw document read | `org_get_document` |
| `PUT /api/org/documents/{document_id}` | create/update raw document | `org_put_document` |
| `POST /api/org/documents/{document_id}/move` | move document | `org_move_document` |
| `POST /api/org/items/{item_id}/move` | move/reparent item | `org_move_item` |
| `POST /api/org/workspaces/{workspace_id}/import` | atomic workspace import | `org_import_workspace` |
| `GET /api/org/workspaces/{workspace_id}/export` | workspace manifest/documents | `org_export_workspace` |
| `POST /api/org/workspaces/{workspace_id}/items` | create item | `org_create_item` |
| `GET /api/org/items/{item_id}` | get item | `org_get_item` |
| `GET /api/org/items/{item_id}/context` | recovery context | `org_get_item_context` |
| `POST /api/org/items/{item_id}/follow-ups` | create follow-up | `org_create_follow_up` |
| `POST /api/org/items/{item_id}/assignment` | assign/unassign | `org_assign_item` |
| `POST /api/org/items/{item_id}/schedule` | schedule/deadline | `org_schedule_item` |
| `GET /api/org/queue` | operational queue | `org_query_queue` |
| `GET /api/org/agenda` | agenda | `org_query_agenda` |
| `POST /api/org/items/{item_id}/claim` | claim/reclaim | `org_claim_item` |
| `POST /api/org/items/{item_id}/claim/heartbeat` | heartbeat | `org_heartbeat_claim` |
| `POST /api/org/items/{item_id}/claim/release` | release | `org_release_claim` |
| `POST /api/org/items/{item_id}/progress` | report progress | `org_report_progress` |
| `POST /api/org/items/{item_id}/result` | submit result | `org_submit_result` |
| `POST /api/org/items/{item_id}/transition` | policy transition | `org_transition_item` |
| `POST /api/org/items/{item_id}/retry` | retry/reclaim failed work | `org_retry_item` |
| `POST /api/org/items/{item_id}/review/request` | request review | `org_request_review` |
| `POST /api/org/items/{item_id}/review/approve` | approve | `org_approve_item` |
| `POST /api/org/items/{item_id}/review/reject` | reject | `org_reject_item` |
| `POST /api/org/items/{item_id}/dependencies` | add dependency | `org_add_dependency` |
| `DELETE /api/org/items/{item_id}/dependencies/{dependency_item_id}` | remove dependency | `org_remove_dependency` |
| `POST /api/org/items/{item_id}/note-links` | link note | `org_link_note` |
| `DELETE /api/org/items/{item_id}/note-links` | unlink note identified by body | `org_unlink_note` |
| `GET /api/org/notes/{note_id}/work-items` | reverse lookup | `org_list_note_work_items` |
| `GET /api/org/workspaces/{workspace_id}/events` | ordered history | `org_list_events` |

Mutation endpoints, including `DELETE`, use JSON request bodies so
`actor_id`, `operation_id`, expected revisions, fencing tokens, and action data
share one envelope. Path IDs and duplicated body IDs must either match or be
rejected as `invalid_input`; prefer omitting duplicate IDs from bodies.

## HTTP and Error Semantics

- Success bodies serialize the same pipeline DTO as MCP. Mutation success is
  `200 OK`, except create workspace/item resources may use `201 Created` only
  if cross-transport normalization treats status as transport metadata and the
  response body remains identical. Use one consistent rule in all tests.
- Read/query inputs use path/query extraction only. `workspace_ids` is repeated
  or comma-normalized in one documented form, and all remaining filters/cursor/
  limit fields map directly to the pipeline query DTO.
- Raw document/workspace export remains JSON carrying byte-preserved UTF-8 Org
  strings and manifest metadata. File layout is an offline concern from Slice
  5, not a REST response concern.
- Claim/retry success may include a raw fencing token. No other read response,
  OpenAPI example, log, error, event, queue row, context, or export may expose a
  token/hash.
- Every error response uses content type `application/json` and:

```json
{
  "code": "active_lease",
  "message": "work item already has an active lease",
  "details": {},
  "retryable": false
}
```

Stable mappings are:

| HTTP status | Pipeline codes |
| --- | --- |
| `400` | `invalid_input`, `unsupported_semantic_edit` |
| `404` | `not_found`, `note_unavailable` when the requested resource itself is unavailable |
| `409` | `archived_workspace`, `stale_revision`, `idempotency_conflict`, `invalid_transition`, `dependency_blocked`, `review_required`, `active_lease`, `stale_lease`, `retry_limit` |
| `429` | `concurrency_limit` with `retryable: true` |
| `500` | `storage_failure` with a safe generic message |

Missing linked notes inside an otherwise valid context are successful data with
`available: false`, not a 404. Unexpected extractor rejections are normalized
into the same `invalid_input` envelope rather than Axum's default plain text.

## Task 1: Build the REST DTO, Structured Error, and Router Foundation

**Files:**

- Modify: `crates/note-server/src/lib.rs`
- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/labels_api.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Create: `crates/note-server/src/app_state.rs`
- Create: `crates/note-server/src/org_api/mod.rs`
- Create: `crates/note-server/src/org_api/dto.rs`
- Create: `crates/note-server/src/org_api/error.rs`
- Create: `crates/note-server/tests/org_api_inventory_test.rs`

- [ ] Write failing tests that require exactly 36 `/api/org` operations, unique
  exact `org_*` operation IDs, an `org` OpenAPI tag, and no missing/extra MCP
  mapping when compared with Slice 5's 36-name constant.
- [ ] Add schema tests for the common mutation envelope, cursor/default/max
  documentation, all stable error fields, safe examples, and the absence of
  auth/security schemes and read-only fencing-token fields.
- [ ] Define REST request wrappers and conversions around pipeline DTOs. Reuse
  transport-neutral result types where `ToSchema` permits; otherwise define
  mechanical REST schemas and prove field parity by serialization tests.
- [ ] Implement `OrgApiError`/`IntoResponse` and extractor rejection adapters
  with the exact mapping above. Never return `anyhow`/SQL text or token data.
- [ ] Define cloneable `AppState` with `note: Arc<Context>` and
  `org: Arc<OrgContext>` plus explicit Axum `FromRef<AppState>` implementations
  for both context types. It must not create contexts or own a second storage
  backend.
- [ ] Make existing notes/labels/system routers and the new Org router compose
  under `OpenApiRouter<AppState>`. Existing handlers continue extracting Note
  Context through `FromRef`; Org handlers extract the shared Org Context. Their
  behavior and public schemas remain unchanged.
- [ ] In `note-server` main, build `AppState` from the already-created Note
  Context and the same `Arc<OrgContext>` passed to `note_mcp::mcp_router`.
  Assert pointer identity at the composition seam so REST cannot silently use a
  separate clock/token source.
- [ ] Add `org_api::router()` to `openapi::rest_router()` and add only the `org`
  tag. Do not add auth middleware, request-local Org contexts, or duplicated
  application state.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_api_inventory_test
cargo test -p note-server openapi::tests
cargo test -p note-server app_state
cargo fmt --all -- --check
```

## Task 2: Add Workspace and Document REST Operations

**Files:**

- Modify: `crates/note-server/src/org_api/mod.rs`
- Modify: `crates/note-server/src/org_api/dto.rs`
- Create: `crates/note-server/src/org_api/workspaces.rs`
- Create: `crates/note-server/src/org_api/documents.rs`
- Create: `crates/note-server/tests/org_workspace_document_api_test.rs`

- [ ] Write failing route tests for the first twelve operations in the matrix,
  including all exact methods/paths and `Content-Type` behavior.
- [ ] Cover active/archived lists, create/get/update/archive, raw get/put,
  document/item moves, import/export, LF/CRLF and opaque source preservation,
  expected revision maps, token maps, and archive read/export behavior.
- [ ] Prove stale revision includes `current_revision`, identical operation
  replay returns the original result, divergent reuse returns 409, and every
  conflict leaves source/projections/events unchanged.
- [ ] Keep import/export payloads transport-neutral JSON. Do not reproduce the
  offline filesystem adapter in REST.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_workspace_document_api_test
cargo test -p note-server --test org_api_inventory_test
cargo fmt --all -- --check
```

## Task 3: Add Item, Context, Queue, and Agenda REST Operations

**Files:**

- Modify: `crates/note-server/src/org_api/mod.rs`
- Modify: `crates/note-server/src/org_api/dto.rs`
- Create: `crates/note-server/src/org_api/items.rs`
- Create: `crates/note-server/src/org_api/operational.rs`
- Create: `crates/note-server/tests/org_item_query_api_test.rs`

- [ ] Write failing tests for create/get/context/follow-up, assignment,
  schedule, queue, and agenda routes.
- [ ] Cover all ten operational views, every approved filter, active/archive
  selection, explicit multi-workspace policy validation, workspace identity,
  cursor round-trip/mismatch, default 50/max 200, and stable tie pagination.
- [ ] Assert REST context contains hierarchy, dependencies, attempts, safe lease
  metadata, recovery state, missing-note markers, revisions, and global event
  lineage order without token/hash fields.
- [ ] Verify raw/workspace/UTC time fields serialize identically to MCP and no
  REST handler reads current time separately from the pipeline clock.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_item_query_api_test
cargo test -p note-pipelines --test org_operational_views_test --test org_recovery_context_test
cargo fmt --all -- --check
```

## Task 4: Add Claim, Execution, and Review REST Operations

**Files:**

- Modify: `crates/note-server/src/org_api/mod.rs`
- Modify: `crates/note-server/src/org_api/dto.rs`
- Create: `crates/note-server/src/org_api/execution.rs`
- Create: `crates/note-server/src/org_api/review.rs`
- Create: `crates/note-server/tests/org_execution_review_api_test.rs`

- [ ] Write failing tests for claim/heartbeat/release/progress/result/transition/
  retry and request/approve/reject review routes.
- [ ] Cover normal and reviewer claims, current token success, exact token-
  bearing replay, active/concurrency conflicts, expiry/reclaim, stale-owner
  rejection, progress without document revision change, review-required result,
  approval/rejection, failure/retry, block/unblock, and cancellation.
- [ ] Assert token-bearing fields appear only in successful claim/retry
  responses and lease-bound request bodies, are marked sensitive in OpenAPI
  descriptions, and never enter errors or general schemas.
- [ ] Verify 409/429 and `retryable` mappings without branching on database
  implementation details.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_execution_review_api_test
cargo test -p note-pipelines --test org_claims_test --test org_fenced_workflow_test
cargo fmt --all -- --check
```

## Task 5: Add Relationship and Audit REST Operations

**Files:**

- Modify: `crates/note-server/src/org_api/mod.rs`
- Modify: `crates/note-server/src/org_api/dto.rs`
- Create: `crates/note-server/src/org_api/relationships.rs`
- Create: `crates/note-server/src/org_api/audit.rs`
- Create: `crates/note-server/tests/org_relationship_audit_api_test.rs`

- [ ] Write failing tests for dependency add/remove, note link/unlink, reverse
  note lookup, and sequence-ordered event history.
- [ ] Cover dependency self/cycle/policy errors, weak missing/deleted note
  visibility, typed link purposes, operation replay/conflict, subject filters,
  attempt association, cursor bounds, workspace moves, and archived history.
- [ ] Ensure DELETE bodies receive the same mutation envelope and extractor
  failures use the structured error shape.
- [ ] Prove event order is workspace sequence/lineage order, never timestamp or
  UUID order, and event metadata cannot contain raw fencing tokens.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_relationship_audit_api_test
cargo test -p note-server --test org_api_inventory_test
cargo fmt --all -- --check
```

## Task 6: Prove OpenAPI Completeness and MCP/REST Conformance

**Files:**

- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/main.rs`
- Modify: `crates/note-server/tests/org_api_inventory_test.rs`
- Create: `crates/note-server/tests/org_transport_conformance_test.rs`

- [ ] Extend the generated-document tests to compare the exact 36 MCP Org names
  to exact 36 REST operation IDs and assert every operation has success plus
  structured-error responses, schemas, tag, and documented cursor/revision/
  token constraints.
- [ ] Update existing aggregate OpenAPI path/operation counts intentionally;
  preserve all existing note/label/system operations and keep `/mcp` excluded
  from OpenAPI.
- [ ] Build table-driven black-box clients for actual Streamable HTTP MCP JSON-
  RPC and Axum REST against separate identically seeded Turso contexts with
  fixed clocks/tokens/IDs. Construct each side through `AppState`; assert its
  REST Org extraction and the Org Context injected into `mcp_router` are the
  same `Arc` before executing the scenario.
- [ ] Execute and normalize representative create, import, item/context, queue,
  claim, heartbeat, expiry/reclaim, transition, review, relationships, events,
  archived read/export, stale revision, divergent idempotency, dependency,
  active lease, stale lease, and concurrency-limit cases.
- [ ] Compare result fields and `code/message/details/retryable`; ignore only
  protocol framing and HTTP status. Never normalize away a semantic revision,
  attempt, lease expiry, event sequence, or availability difference.
- [ ] Add a static audit proving all 36 REST handlers call named pipeline
  operations and contain no storage-session access, raw SQL, policy parsing,
  token hashing, or clock access.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test -p note-server --test org_api_inventory_test --test org_transport_conformance_test
cargo test -p note-server generated_rest_openapi_is_complete_and_excludes_mcp
cargo test -p note-mcp --test org_transports_test
cargo fmt --all -- --check
```

## Task 7: Documentation, Regression, and Slice Gate

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`
- Modify: `crates/note-server/tests/org_transport_conformance_test.rs`

- [ ] Document the full route matrix, Swagger/OpenAPI locations, common
  mutation/error shapes, actor attribution, fencing-token sensitivity, and
  MCP/REST parity.
- [ ] State plainly that Agent Note implements no auth/session/ACL behavior,
  the front proxy owns TLS/auth/access control, and direct untrusted exposure is
  unsupported. Do not add an OpenAPI security scheme or auth configuration.
- [ ] Add a final high-risk restart/idempotency scenario and verify existing
  Markdown REST/MCP/OpenAPI behavior is unchanged except intentional aggregate
  inventory counts.
- [ ] Obtain a final independent Slice 6 requirements audit and fresh quality
  review; fix and rerun:

```sh
cargo test -p note-server --test org_api_inventory_test
cargo test -p note-server --test org_workspace_document_api_test
cargo test -p note-server --test org_item_query_api_test
cargo test -p note-server --test org_execution_review_api_test
cargo test -p note-server --test org_relationship_audit_api_test
cargo test -p note-server --test org_transport_conformance_test
cargo test -p note-server --test notes_api_test --test labels_api_test
cargo test -p note-mcp
cargo check -p note-server --all-targets
cargo fmt --all -- --check
git diff --check
```

Expected: generated OpenAPI contains exactly 36 Org operations with exact MCP
operation IDs, normalized cross-transport scenarios agree, existing REST/MCP
regressions pass, no frontend file changed, and no authentication feature was
introduced.

## Completion Checklist

- [ ] Slice 5 prerequisite is committed and green.
- [ ] All 36 route/operation pairs exist exactly once.
- [ ] All successes and errors originate from shared pipelines and DTOs.
- [ ] Structured HTTP errors and status mappings are complete and safe.
- [ ] OpenAPI covers all request/result/error schemas and no auth scheme.
- [ ] Cross-transport normalized outcomes match in high-risk scenarios.
- [ ] Existing note APIs, docs, and `/mcp` remain compatible.
- [ ] Every task has independent requirements and quality approval.
- [ ] Scoped checks and `git diff --check` pass.
- [ ] No Slice 7 Yew/Web work was introduced.
