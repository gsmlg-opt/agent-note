# Org Delivery Interfaces Expansion Design

**Date:** 2026-08-04

**Status:** Approved

**Revises:**
[`2026-07-30-org-orchestration-system-prd.md`](2026-07-30-org-orchestration-system-prd.md)

## Context

The approved Org orchestration PRD originally stopped after canonical storage,
workflow, leases, MCP, and offline import/export. It explicitly deferred Org
REST routes and any browser UI. The product direction now includes a complete
REST surface with MCP parity and a first read-only browser operations console.

This design records the approved boundary before implementation planning. It
does not combine all remaining work into one implementation milestone. Each
delivery slice remains independently planned, tested, reviewed, and accepted.

## Approved Product Boundary

- Org remains a separate execution domain from Markdown notes.
- `note-org` remains pure and owns loss-preserving Org semantics and domain
  rules.
- Canonical Org source remains database-backed and exportable as standard Org
  text.
- Turso and PostgreSQL implement the same Org storage contracts.
- `note-pipelines` is the only application boundary for Org operations.
- MCP and REST expose complete, equivalent Org query and mutation capabilities.
- The first Web UI is a read-only, workspace-first operations console.
- The Web UI calls REST and never implements MCP in the browser.
- Agent Note implements no authentication or authorization. A front proxy owns
  the deployment security boundary, and direct untrusted-network exposure is
  unsupported.
- `actor_id` is asserted audit metadata, not an authenticated identity.

## Architecture

Requests follow one of three paths:

```text
MCP client ------> MCP adapter -----+
                                     |
REST client -----> REST adapter ----+--> note-pipelines --> note-org
                                     |          |               |
Web UI ----------> REST adapter ----+          +--> note-storage+
                                                       |
                                              Turso / PostgreSQL
```

Transport adapters validate and map protocol DTOs, but they do not implement
workflow rules. Pipelines own transaction boundaries, revision checks,
idempotency, fencing-token validation, event creation, and composition with
Markdown note lookups. `note-org` owns pure source and policy behavior.

## Persistence and Data Ownership

### Canonical data

`org_workspaces` stores stable identity, slug, display metadata, IANA timezone,
policy, monotonic revision, timestamps, and archive state.

`org_documents` stores stable identity, workspace ownership, workspace-unique
path, byte-preserved Org source, monotonic revision, content hash, and
timestamps. Org source is the canonical representation of work-item semantics.

### Rebuildable projections

Work items, hierarchy, tags, dependencies, schedules, deadlines, priorities,
assignments, and Markdown note links are projections of canonical Org source.
They must be rebuildable without changing document source or identity.

### Authoritative runtime data

Events, attempts, progress records, leases, fencing tokens, expiry state, and
idempotency results are not Org projections. They are independently
authoritative runtime records and must survive projection rebuilds.

Slice 2 introduces the minimal append-only event table and per-workspace event
sequence required to commit document import and projection changes atomically.
Slice 3 adds the complete workflow event vocabulary and audit behavior. Slice
4 adds attempts and lease data.

`org_operations` stores the client `operation_id`, a canonical request
fingerprint, and the transport-neutral result of each successful mutation. A
retry with the same operation ID and fingerprint returns the original result.
Reusing an operation ID with a different fingerprint returns an
`idempotency_conflict` without side effects.

### Atomic mutation boundary

A source-changing operation performs the following work in one storage
transaction:

1. validate the operation ID, workspace and document revisions, and any
   required fencing token;
2. compute the minimal loss-preserving Org source edit through `note-org`;
3. validate the resulting complete document and workspace policy;
4. replace canonical source using storage-level compare-and-swap;
5. replace affected projections;
6. update runtime state where applicable;
7. allocate the next workspace event sequence and append events; and
8. persist the idempotency result.

Any failure rolls back every part of the operation. Multi-document and
cross-workspace document moves lock and validate every affected revision and
commit all source, projections, events, and operation results atomically.

## Workspace Time Model

Every workspace requires an IANA timezone such as `Asia/Shanghai`. Org
timestamps with an explicit zone or offset retain it. A timestamp without a
zone is interpreted in the workspace timezone. Pipelines preserve the original
Org text and persist the resolved UTC instant used by queue and agenda queries.

Nonexistent or ambiguous local times caused by daylight-saving transitions are
validation errors. The system never silently shifts or guesses them. API DTOs
return the original local representation, workspace timezone, and resolved UTC
instant where applicable. The Web UI defaults to workspace time and may show
browser-local time as secondary information.

## Workspace Archive Model

Archiving requires the expected workspace revision and is rejected while any
active lease exists. Once archived, a workspace rejects document imports,
semantic changes, claims, heartbeats, transitions, reviews, and other
mutations. It remains queryable and exportable, and its event history remains
available.

Archived workspaces are excluded from normal workspace lists, queues, agendas,
and the Web UI unless `include_archived` is explicit. The first release has no
unarchive operation.

## Shared Application Contract

Every client mutation supplies:

- a non-empty client-generated `operation_id`;
- a non-empty asserted `actor_id`, except reserved internal system operations;
- the expected workspace revision for workspace mutations;
- every affected document revision for source-changing mutations; and
- the current fencing token for lease-bound mutations.

List operations use an opaque cursor with a default limit of 50 and a maximum
limit of 200. Ordering always ends with a stable ID tie-breaker. Event history
is ordered by per-workspace sequence, never by timestamp.

Queue and agenda queries default to one workspace. An explicit multi-workspace
query is accepted only when every selected workspace policy permits it, and
every returned row preserves workspace identity. The first Web UI never makes
cross-workspace operational queries.

## MCP and REST Parity

MCP remains the canonical agent protocol. REST exposes the same 36 Org
operations with equivalent inputs, outcomes, validation rules, and errors.
OpenAPI `operationId` values use the corresponding `org_*` MCP tool names.

| Operation family | MCP operations | REST routes |
| --- | --- | --- |
| Workspaces | `org_list_workspaces`, `org_create_workspace`, `org_get_workspace`, `org_update_workspace`, `org_archive_workspace` | `GET/POST /api/org/workspaces`, `GET/PATCH /api/org/workspaces/{workspace_id}`, `POST /api/org/workspaces/{workspace_id}/archive` |
| Documents | `org_list_documents`, `org_get_document`, `org_put_document`, `org_move_document`, `org_move_item`, `org_import_workspace`, `org_export_workspace` | workspace document collection, document resource, explicit move, import, and export actions |
| Items | `org_create_item`, `org_get_item`, `org_get_item_context`, `org_create_follow_up`, `org_assign_item`, `org_schedule_item` | workspace item collection, item resource and context, follow-up, assignment, and schedule actions |
| Operational queries | `org_query_queue`, `org_query_agenda` | `GET /api/org/queue` and `/api/org/agenda` with explicit `workspace_ids` |
| Claims and execution | `org_claim_item`, `org_heartbeat_claim`, `org_release_claim`, `org_report_progress`, `org_submit_result`, `org_transition_item`, `org_retry_item` | item-scoped claim, heartbeat, release, progress, result, transition, and retry actions |
| Review | `org_request_review`, `org_approve_item`, `org_reject_item` | item-scoped review request, approval, and rejection actions |
| Relationships | `org_add_dependency`, `org_remove_dependency`, `org_link_note`, `org_unlink_note`, `org_list_note_work_items` | item dependency and note-link actions plus note reverse lookup |
| Audit | `org_list_events` | workspace event collection with subject filters |

Mutation actions use JSON request bodies so the common mutation envelope is
available consistently. REST resource naming may group related actions, but it
must not omit an MCP capability or introduce different workflow semantics.

MCP stdio and Streamable HTTP continue to register identical tools and schemas.
REST includes every Org operation in the generated OpenAPI document.

## DTO and Error Model

Shared pipeline DTOs include:

- `WorkspaceSummary` for the workspace directory and operational counts;
- `OperationalItemSummary` for queues and agendas;
- `WorkItemContext` for hierarchy, policy, dependencies, note metadata,
  attempts, lease metadata, revisions, and recent events;
- cursor-paginated workspace, document, item, and event pages; and
- structured mutation results containing resulting revisions, events, leases,
  or item context as appropriate.

Fencing tokens are returned only to the claiming client and accepted only by
lease-bound mutations. Read-only Web DTOs never expose fencing tokens.

Stable error codes include at least:

- `invalid_input`;
- `not_found`;
- `stale_revision`;
- `idempotency_conflict`;
- `invalid_transition`;
- `dependency_blocked`;
- `active_lease`;
- `stale_lease`;
- `concurrency_limit`; and
- `storage_failure`.

REST uses a structured JSON envelope with `code`, `message`, `details`, and
`retryable`, mapped to stable HTTP statuses. MCP returns the same fields as
structured tool error data. Internal database errors and secrets are never
returned directly.

## Read-Only Web Operations Console

The first Web UI adds three routes:

- `/org` lists active workspaces, operational counts, timezone, and an explicit
  `include_archived` filter.
- `/org/:workspace_id` shows workspace-scoped ready, assigned, running,
  blocked, review, scheduled, upcoming-deadline, failed, expired-lease, and
  completed views.
- `/org/:workspace_id/items/:item_id` shows item hierarchy, dependencies,
  linked note availability, attempts, lease metadata, recovery context, and
  ordered event history.

The navigation label is `Org`. Operational views use tables rather than a
board. Current view, filters, cursor/page state, and typed return context are
represented in the URL so returning from an item restores the originating
list.

The UI provides explicit loading, empty, error, and refresh states. It defaults
to workspace time and may show browser-local time secondarily. Event history is
shown as a sequence-ordered table with expandable structured metadata. Missing
Markdown note targets remain visible and are marked unavailable.

The first UI contains no create, edit, claim, release, transition, review,
archive, import, or raw Org editing controls. It does not display fencing
tokens. It performs no automatic polling; users refresh explicitly. Mutation
UI is a later design and delivery slice even though REST already exposes the
underlying mutation operations.

## Security Boundary

This design adds no authentication, session, credential, or workspace ACL
implementation. The front proxy is responsible for TLS, authentication,
authorization, and restricting access to the server. Agent Note does not trust
`actor_id` as an authorization claim.

README, configuration guidance, OpenAPI descriptions, and the Web deployment
documentation must state that directly exposing Agent Note to an untrusted
network is unsupported. No proxy-specific header or identity protocol is part
of the product contract in these slices.

## Delivery Slices

1. **Org domain foundation** — already delivered.
2. **Canonical persistence** — canonical workspaces/documents, rebuildable
   projections, note links, minimal event ledger, operation records,
   migrations, backend parity, and revision-safe import/export.
3. **Workflow and audit** — semantic mutations, transitions, dependencies,
   complete events, attempts, progress, review, idempotent command behavior,
   and recovery context.
4. **Claims and operational views** — leases, fencing tokens, expiry, retry,
   concurrency enforcement, queues, agendas, and operational summaries.
5. **MCP and offline interfaces** — all 36 MCP tools on both transports,
   offline import/export, and end-to-end conflict and recovery verification.
6. **REST and OpenAPI parity** — all 36 REST-equivalent operations, generated
   OpenAPI, shared DTO/error mapping, and cross-transport conformance tests.
7. **Read-only Web operations console** — the three workspace-first routes,
   URL-backed filters and return context, responsive states, and browser
   verification.

Every slice receives its own scoped implementation plan and verification
commands. Implementation does not start on a later slice merely to make an
earlier slice appear complete.

## Verification Strategy

- The shared storage contract suite runs against Turso and PostgreSQL.
- Embedded schema-version-2 migration to version 3 is atomic and rolls back on
  injected failure.
- Pipeline tests use an injected clock and synchronization barriers rather
  than sleeps.
- Concurrent claim tests prove one winner; delayed stale-token mutations prove
  permanent fencing.
- Idempotency tests prove original-result replay and different-request
  conflicts.
- MCP inventory and schema tests prove identical stdio and Streamable HTTP
  registration.
- REST OpenAPI tests prove all 36 operation IDs and stable error schemas.
- Cross-transport black-box scenarios execute the same workflows through MCP
  and REST and compare normalized results and error codes.
- Frontend unit tests cover URL state, return context, DTO rendering, time
  display, unavailable notes, and loading/error states.
- Wasm checks and browser black-box tests cover the three read-only routes.
- Existing Markdown note storage, pipeline, REST, MCP, import/export, and Web UI
  tests remain regression gates.

## Deferred Work

- Any Web UI mutation or administration controls.
- A global cross-workspace operations homepage.
- Built-in authentication, authorization, sessions, or workspace ACLs.
- Automatic polling, push updates, or live event streaming.
- Browser-based raw Org editing.
- Unarchiving workspaces.
- Multiple active Agent Note server replicas.
- Autonomous dispatch, recurring expansion, notifications, and calendar or
  email integration.
