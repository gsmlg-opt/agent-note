# Org Read-Only Web Operations Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development and frontend-design to implement this
> plan task-by-task. Use chrome-devtools-cli for browser acceptance. Every task
> follows RED → GREEN → scoped verification → independent requirements review
> → independent code-quality review. Fix every finding and rerun focused checks
> before accepting the task.

**Goal:** Deliver Org Delivery Slice 7 as a production-ready, read-only Yew
operations console at the three approved workspace-first routes, with URL-
backed operational state, REST-only data access, complete loading/empty/error/
refresh behavior, safe lease/note/event rendering, local and remote browser
verification, and no mutations or authentication implementation.

**Architecture:** `note-frontend::org` owns transport DTOs, structured REST
errors, validated URL state, and time/display helpers. Three Yew page modules
call only Slice 6 read endpoints. The workspace operations page selects server-
computed queue/agenda views and passes its complete typed URL state into item
links; the item page uses that state to return exactly to the originating list.
Pages issue one request per explicit load/URL change and refresh only on user
action. Existing Axum SPA fallback serves direct `/org/...` loads in packaged
deployments.

**Visual direction:** A restrained **operations ledger** using the existing
DuskMoon design system: dense but readable data tables, clear state/priority
markers, a sequence-led audit trail, strong workspace/time context, and precise
empty/error panels. Keep the existing app shell and typography coherent; do
not introduce a generic board, decorative dashboard cards, purple gradients,
external fonts, or unrelated visual-system changes.

**Tech Stack:** Rust 2021, Yew 0.23, Yew Router 0.20, `gloo-net`, `serde`,
`chrono`, `chrono-tz`, `js-sys`, DuskMoon CSS, Trunk, Wasm
`wasm32-unknown-unknown`, Axum REST, and `chrome-devtools` CLI.

---

## Scope and Dependency Gate

Start only after `2026-08-04-org-rest-openapi.md` is committed and green with
all 36 REST operations and cross-transport conformance. The UI consumes only
these read operations:

```text
org_list_workspaces
org_get_workspace
org_get_item_context
org_query_queue
org_query_agenda
org_list_events
```

If a required read DTO lacks operational counts, availability, safe lease
metadata, lineage-ordered events, workspace time fields, or pagination, fix the
owning Slice 4/6 contract first. Do not reconstruct readiness, policy, lineage,
or lease validity in Yew.

This plan implements:

- `Org` primary navigation;
- `/org` workspace directory;
- `/org/:workspace_id` with ready, assigned, running, blocked, review,
  scheduled, upcoming-deadline, failed, expired-lease, and completed tables;
- `/org/:workspace_id/items/:item_id` with complete work context and event
  history;
- URL-backed view, filters, cursor/page state, and typed return context;
- workspace-time-first display with browser-local time secondary;
- unavailable Markdown-note visibility, archived workspace states, sanitized
  leases, explicit loading/empty/structured-error/manual-refresh states;
- native logic tests, Wasm/Trunk checks, local browser black-box acceptance,
  deployed remote URL verification, and before/after screenshots.

This plan does **not** add create/edit/claim/release/heartbeat/progress/
transition/retry/review/archive/import/raw Org controls, any non-GET Org REST
call, MCP in the browser, fencing-token display/storage, auto polling, SSE/Web
Sockets, global cross-workspace operations, authentication, authorization,
sessions, proxy identity headers, or workspace ACLs.

## URL Contract

### Workspace directory

`/org` accepts and canonicalizes:

```text
include_archived=false|true
cursor=<opaque>
limit=10|25|50|100|200
```

Defaults are `false`, no cursor, and 50. Unknown fields are ignored when read
but omitted from the next canonical URL. Invalid cursors remain server errors;
invalid local enum/limit values normalize to defaults.

### Workspace operational view

`/org/:workspace_id` accepts:

```text
view=ready|assigned|running|blocked|review|scheduled|upcoming_deadline|failed|expired_lease|completed
item_type=<type>
state=<state>
priority=<A..Z|none>
tags=<comma-separated canonical tags>
assignee=<actor-id>
from=<RFC3339 UTC>
to=<RFC3339 UTC>
cursor=<opaque>
limit=10|25|50|100|200
```

Default view is `ready`, no filters/cursor, limit 50. Filter/view/limit changes
clear the cursor. Ready, assigned, running, blocked, review, failed,
expired-lease, and completed call `GET /api/org/queue`; scheduled and
upcoming-deadline call `GET /api/org/agenda`. Every request names exactly the
path workspace; the UI never makes a multi-workspace operational query.

### Typed item return context

The item route carries the originating operational query using `return_*`
fields (`return_view`, `return_item_type`, `return_state`, `return_priority`,
`return_tags`, `return_assignee`, `return_from`, `return_to`, `return_cursor`,
`return_limit`). `OrgReturnContext::validated(workspace_id)` rejects malformed
or cross-workspace return state and falls back to the workspace's default ready
view. The back link uses `push_with_query`/typed router data, never browser-
history guessing or an unvalidated raw return URL.

## Read-Only and Data-Safety Contract

- `org/api.rs` contains only `Request::get` for Org URLs. No function accepts
  actor ID, operation ID, expected revision for mutation, or fencing token.
- Read DTOs deliberately have no token/hash field. Do not deserialize then hide
  a token; make token exposure unrepresentable in the frontend model.
- Browser network acceptance rejects `/mcp`, non-GET `/api/org`, and unexpected
  polling requests. Manual refresh produces exactly the documented new GETs.
- Missing/deleted note links remain rows with purpose, note ID/description, and
  `Unavailable`; available notes link to the existing note show route.
- Lease panels show kind, actor, acquired/heartbeat/expiry/end status and
  recovery marker only. Events are ordered by sequence and show timestamp as
  descriptive data.
- No login, identity picker, actor header, session storage, credential form, or
  ACL behavior is added. Deployment access remains a front-proxy concern.

## Task 1: Add Org Read Models, Structured REST Client, URL State, and Time Helpers

**Files:**

- Modify: `crates/note-frontend/Cargo.toml`
- Modify: `crates/note-frontend/Cargo.lock`
- Modify: `crates/note-frontend/src/main.rs`
- Create: `crates/note-frontend/src/org/mod.rs`
- Create: `crates/note-frontend/src/org/model.rs`
- Create: `crates/note-frontend/src/org/api.rs`
- Create: `crates/note-frontend/src/org/url.rs`
- Create: `crates/note-frontend/src/org/time.rs`

- [ ] Write failing native tests that deserialize workspace summaries, all ten
  operational views, hierarchy/dependencies, attempts, safe lease metadata,
  recovery context, note availability, and ordered events from Slice 6 JSON.
- [ ] Add negative compile/serialization audits proving no read model contains
  `fencing_token`, token hash, actor-operation mutation envelope, or auth field.
- [ ] Write failing parse/canonicalize/round-trip tests for all three URL
  contracts, filter normalization, cursor clearing, reserved-character
  encoding, invalid/cross-workspace return fallback, and typed list restoration.
- [ ] Add failing structured error tests for `code/message/details/retryable`,
  unexpected non-JSON errors, and safe display copy without raw response dumps.
- [ ] Implement only GET client functions for list workspaces, get workspace,
  query queue/agenda, get item context, and list item events. Encode every query
  component and pass cursor/limit directly to REST.
- [ ] Add workspace timezone rendering from server time fields/`chrono-tz` and
  browser-local secondary rendering from UTC. Invalid timestamps render an
  explicit unavailable marker rather than panic or guess DST ambiguity.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml org::
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

## Task 2: Add Org Navigation and Workspace Directory Route

**Files:**

- Modify: `crates/note-frontend/src/routes.rs`
- Modify: `crates/note-frontend/src/pages/mod.rs`
- Modify: `crates/note-frontend/src/components/app_bar.rs`
- Create: `crates/note-frontend/src/pages/org_workspaces.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] Write failing route and active-navigation tests for `Route::Org`, direct
  `/org` matching, canonical `include_archived` query state, and an `Org` nav
  destination distinct from Notes/System.
- [ ] Add pure view-state tests for loading, nonempty, empty, structured error,
  archived inclusion, refresh generations, and stale response suppression.
- [ ] Render an operations-ledger directory table with workspace name/slug,
  timezone, archive state, revision, and counts for all ten views. Each row links
  to that workspace's ready view.
- [ ] Provide an explicit `Include archived` control, cursor pagination where
  returned, and a native `Refresh` button. The page loads once per canonical URL
  or refresh generation; do not use timers.
- [ ] Add accessible table captions/headers, visible focus, `aria-current` nav,
  `aria-live` refresh/error status, and distinct archived treatment.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::org_workspaces
cargo test --manifest-path crates/note-frontend/Cargo.toml routes::tests
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

## Task 3: Build the Workspace Operational Tables with URL-Backed Triage

**Files:**

- Modify: `crates/note-frontend/src/routes.rs`
- Modify: `crates/note-frontend/src/pages/mod.rs`
- Create: `crates/note-frontend/src/components/org_table.rs`
- Modify: `crates/note-frontend/src/components/mod.rs`
- Create: `crates/note-frontend/src/pages/org_workspace.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] Write failing pure tests for all ten view selectors, queue-vs-agenda REST
  selection, every approved filter, cursor reset, next/previous URL state,
  workspace-only requests, and exact item return context generation.
- [ ] Add loading, empty, structured error, manual refresh, archived-read,
  cursor-end, recovery-candidate, review-leased, and unavailable timestamp
  render-state tests.
- [ ] Render a workspace header with name, timezone, revision/archive badge,
  ten count-bearing view tabs, filter controls, and a horizontally scrollable
  semantic table. Do not implement a board or draggable rows.
- [ ] Columns must expose stable ID/title, type/state/priority, assignment,
  schedule/deadline in workspace time, browser-local secondary time where
  useful, lease/recovery summary, and relevant attempt/dependency status.
- [ ] Item links carry the complete typed return context. Browser back and the
  explicit item-page back link must restore the exact view/filter/cursor/limit.
- [ ] Keep page requests generation-guarded so an older response cannot replace
  state after rapid URL/filter navigation. No automatic polling.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::org_workspace
cargo test --manifest-path crates/note-frontend/Cargo.toml components::org_table
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

## Task 4: Build the Work-Item Context and Sequence-Ordered Audit Route

**Files:**

- Modify: `crates/note-frontend/src/routes.rs`
- Modify: `crates/note-frontend/src/pages/mod.rs`
- Create: `crates/note-frontend/src/components/org_event_table.rs`
- Modify: `crates/note-frontend/src/components/mod.rs`
- Create: `crates/note-frontend/src/pages/org_item.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] Write failing tests for typed return validation/restoration, item loading/
  missing/error states, hierarchy, dependencies, policy summary, revisions,
  attempts/progress/results/reviews/artifacts, safe lease fields, recovery
  context, note availability, and sequence-ordered event pages.
- [ ] Add tests that intentionally supply timestamp/UUID order different from
  event sequence and prove rendered order follows sequence across workspace
  move lineage.
- [ ] Render a strong context header plus parent/children, dependency,
  assignment/schedule, attempt/lease/recovery, linked-note, artifact, and policy
  sections. Available notes use the existing note detail route; missing/deleted
  targets stay visible as unavailable.
- [ ] Render event history as a table led by sequence, type, actor, attempt,
  workspace time, previous/resulting state, summary, and expandable structured
  metadata. Never render token-shaped metadata values.
- [ ] Paginate event history through `org_list_events` subject filters and
  refresh context/events only on page load, URL change, or manual refresh.
- [ ] Provide an explicit back link generated only from validated typed return
  state; fallback is the workspace ready route.
- [ ] Run and obtain requirements/quality review:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::org_item
cargo test --manifest-path crates/note-frontend/Cargo.toml components::org_event_table
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

## Task 5: Enforce Read-Only Boundaries and Complete Responsive UI Quality

**Files:**

- Modify: `crates/note-frontend/src/org/api.rs`
- Modify: `crates/note-frontend/src/pages/org_workspaces.rs`
- Modify: `crates/note-frontend/src/pages/org_workspace.rs`
- Modify: `crates/note-frontend/src/pages/org_item.rs`
- Modify: `crates/note-frontend/app.css`
- Create: `scripts/verify-org-console-browser.sh`

- [ ] Add a static failing test/audit that Org frontend production code contains
  only GET `/api/org` calls, no `/mcp`, no mutation verbs/functions, no fencing-
  token field, no timer/poll loop, and no login/auth/session/actor controls.
- [ ] Finish the operations-ledger styling with DuskMoon tokens: consistent
  state/priority indicators, numeric alignment, compact metadata, sticky table
  headers where appropriate, visible row focus, readable expanded JSON, and
  deliberate desktop/mobile information hierarchy.
- [ ] At narrow widths, retain semantic tables inside labeled horizontal scroll
  regions; do not hide stable IDs, state, or recovery markers. Controls wrap in
  source order and all targets remain keyboard accessible.
- [ ] Create a repeatable `chrome-devtools` black-box script accepting
  `ORG_CONSOLE_BASE_URL`, `ORG_CONSOLE_WORKSPACE_ID`, and
  `ORG_CONSOLE_ITEM_ID`. The IDs come from an ephemeral local fixture prepared
  through Slice 6 REST before the browser run; fixture mutation is test setup,
  never browser product behavior. The script must navigate all three routes,
  wait on explicit `data-testid` readiness markers, assert
  headings/tables/empty/error states, change a view/filter, open an item, use the
  typed back link, and verify exact URL restoration.
- [ ] Have the script inspect browser network/console state: zero console
  errors, no `/mcp`, no non-GET `/api/org`, no token text, no request repetition
  before refresh, and only the expected GETs after clicking Refresh.
- [ ] Add an explicit directory-only mode for deployments with no Org data. It
  verifies `/org` loading/empty/error/refresh behavior without pretending to
  cover workspace and item routes; the script and report label that outcome as
  partial remote evidence.
- [ ] Run desktop and mobile snapshots/screenshots via:

```sh
ORG_CONSOLE_BASE_URL=http://127.0.0.1:6221 \
ORG_CONSOLE_WORKSPACE_ID=<fixture-workspace-uuid> \
ORG_CONSOLE_ITEM_ID=<fixture-item-uuid> \
  scripts/verify-org-console-browser.sh
chrome-devtools resize_page 1440 900
chrome-devtools take_screenshot --fullPage true --filePath /tmp/org-console-desktop.png
chrome-devtools resize_page 390 844
chrome-devtools take_screenshot --fullPage true --filePath /tmp/org-console-mobile.png
chrome-devtools take_snapshot --verbose true
chrome-devtools list_console_messages --types error
```

- [ ] Review screenshots and accessibility snapshot for clipping, contrast,
  heading order, table names, control labels, focus, long IDs, empty cells, and
  mobile scroll affordance. Obtain requirements/quality review.

## Task 6: Local Build, Packaged Deep Links, Deployment, and Remote URL Gate

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`
- Modify: `scripts/verify-org-console-browser.sh`
- Verify: `crates/note-server/src/main.rs`
- Verify: `crates/note-frontend/Trunk.toml`

- [ ] Document the three routes, all ten views, URL state, manual refresh,
  workspace/browser time, unavailable notes, and read-only boundary. State that
  there is no application auth and a front proxy owns the security boundary.
- [ ] Prove Trunk `/api` proxying and packaged SPA fallback support direct loads
  and refreshes of `/org`, `/org/:workspace_id`, and the item route with HTTP
  200 and the frontend index, while `/api/org` remains claimed by REST.
- [ ] Run the complete local scoped gate:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
(cd crates/note-frontend && trunk build)
cargo test -p note-server --test org_item_query_api_test --test org_api_inventory_test
cargo test -p note-server composed_transport_router_serves_docs_and_preserves_mcp
git diff --check
```

- [ ] Obtain a final independent Slice 7 requirements audit and fresh quality/
  accessibility review. Fix every finding and rerun the gate before deployment.
- [ ] Use the repository `deploy-93` skill to build/publish/deploy the accepted
  revision. Verify the running service reports/serves that exact revision before
  browser acceptance; a local Trunk result is not remote evidence.
- [ ] Perform a read-only remote data preflight before choosing browser IDs:
  list active and archived workspaces, then query the ten views of candidate
  workspaces until an existing item is found. Prefer those existing IDs and do
  not create data merely to make the gate pass.
- [ ] If no remote workspace/item exists, request explicit operator approval
  before any fixture mutation. The approval must name the target deployment,
  fixture records, and a proven cleanup mechanism. A fixture is cleanable only
  in an isolated verification database/deployment that can be discarded or in
  an operator-approved snapshot/restore workflow; because the product has no
  workspace/item delete, creating a permanent shared-database fixture is not a
  cleanup plan.
- [ ] Without that explicit approval, run only the `/org` directory empty-state
  check and mark workspace/item remote verification **incomplete**. Local
  fixture evidence still stands, but the task and full remote gate must not be
  declared complete.
- [ ] With existing IDs or an approved cleanable fixture, set the actual front-
  proxy URL, not localhost, and run:

```sh
export ORG_CONSOLE_BASE_URL='https://<deployed-agent-note-host>'
export ORG_CONSOLE_WORKSPACE_ID='<existing-workspace-uuid>'
export ORG_CONSOLE_ITEM_ID='<existing-item-uuid>'
curl --fail --show-error --silent "$ORG_CONSOLE_BASE_URL/api/org/workspaces?limit=1"
curl --fail --show-error --silent "$ORG_CONSOLE_BASE_URL/org" >/dev/null
curl --fail --show-error --silent \
  "$ORG_CONSOLE_BASE_URL/org/<workspace-id>/items/<item-id>?return_view=ready&return_limit=50" \
  >/dev/null
scripts/verify-org-console-browser.sh
chrome-devtools list_console_messages --types error
chrome-devtools list_network_requests --resourceTypes Fetch
chrome-devtools take_screenshot --fullPage true --filePath /tmp/org-console-remote.png
```

- [ ] Remote acceptance requires visible `Org` navigation, workspace data, all
  ten views, item context/events, exact return-state restoration, GET-only REST
  traffic, no fencing token/mutation/auth UI, no console error, and working
  direct-route refresh. Prefer pre-existing remote workspace/item IDs selected
  with read-only REST queries. If a fixture was explicitly approved, verify its
  cleanup and record that evidence before acceptance. Capture the actual
  verified URL and screenshot in the delivery report. An empty-directory-only
  result is useful partial evidence but is not the complete remote gate.

## Completion Checklist

- [ ] Slice 6 prerequisite is committed and green.
- [ ] All three routes work on local and packaged direct loads.
- [ ] All ten server-computed views and every URL filter/cursor are represented.
- [ ] Item links restore exact typed list context.
- [ ] Workspace time is primary and browser-local time is secondary.
- [ ] Missing notes, sanitized leases, attempts, recovery, and sequence events
  render correctly.
- [ ] Loading, empty, structured error, and manual refresh states are explicit.
- [ ] Browser performs only GET REST calls, never MCP or mutations.
- [ ] No token, auto polling, auth/session/ACL, or browser mutation exists.
- [ ] Native, Wasm, Trunk, local browser, deployment, and remote URL gates pass.
- [ ] Remote workspace/item data came from pre-existing records or an explicitly
  approved fixture whose cleanup was verified; an empty-only run is not marked
  complete.
- [ ] Every task has independent requirements and quality approval.
- [ ] The final report includes the exact deployed URL and remote screenshot.
