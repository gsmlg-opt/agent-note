# Org Workspace Management Web UI Design

**Status:** Approved

**Date:** 2026-08-11

## Problem

The first Org Web console interpreted “read-only” too broadly. Org documents and operational
workflow state must remain read-only in the browser, but workspace configuration and lifecycle
management must be available to human operators. The backend already exposes create, update, and
archive operations through REST, OpenAPI, and MCP; the missing capability is the Yew frontend.

## Product Boundary

The browser may create, edit, and archive Org workspaces. It must not mutate Org documents, raw Org
source, work items, claims, transitions, reviews, imports, or any other operational workflow state.

Workspace removal means archival. This slice adds no hard-delete or restore operation. Archived
workspaces remain readable and auditable, but they cannot be edited or archived again.

The application adds no authentication, session, actor picker, or authorization behavior. The
front proxy remains responsible for the deployment security boundary.

## Routes and Entry Points

The frontend adds two routes:

```text
/org/new
/org/:workspace_id/settings
```

The workspace directory at `/org` adds a primary **Create workspace** action. An active workspace
page adds **Edit workspace** and **Archive workspace** actions. Archived workspace pages expose
neither action.

Successful creation navigates to the new workspace operations page. Successful editing returns to
the workspace operations page with fresh data. Successful archival returns to
`/org?include_archived=true` so the archived workspace remains discoverable.

## Form Architecture

Creation and editing share one workspace form component. Creation generates the workspace UUID,
fixes `policy_schema_version` at `1`, and initializes every policy field from the existing
`WorkspacePolicy::engineering_default()` values. Editing loads the complete stored workspace and
policy without normalizing or discarding fields.

The form contains these sections:

1. **Identity** — slug, display name, description, and IANA timezone.
2. **Work types** — checkboxes for Project, Epic, Issue, Task, Subtask, Review, Approval, Incident,
   and Milestone.
3. **Workflow states** — add and remove state names.
4. **Transitions** — structured From/To rows whose options come from configured states.
5. **State roles** — initial, running, review, failed, cancelled, release, review-rejection, and
   lease-expiry-recovery selectors.
6. **State groups** — executable, successful-terminal, and terminal state selections.
7. **Review policy** — work types that require review.
8. **Dispatch policy** — claim policy and cross-workspace agenda permission.
9. **Limits** — lease duration, retry limit, and concurrency limit.
10. **Tag rules** — allowed and required tag lists per work type.

The UI uses structured controls rather than a raw JSON policy editor. Client validation catches
missing required values, invalid numeric limits, duplicate states or transitions, transition
endpoints absent from the state catalog, role states absent from the catalog, and required tags
not present in the corresponding allowed set. The server remains authoritative for complete policy
validation and compatibility with existing work items.

## REST Mutation Contract

The frontend uses the existing endpoints:

```text
POST  /api/org/workspaces
PATCH /api/org/workspaces/:workspace_id
POST  /api/org/workspaces/:workspace_id/archive
```

Every mutation sends `schema_version: 1`, `actor_id: "web-ui"`, and an automatically generated UUID
`operation_id`. Identity fields are not displayed to the operator.

Editing and archival submit the revision returned by the workspace read as `expected_revision`.
Creation sends the generated workspace UUID and the complete engineering-default-derived policy.

One form submission owns one operation ID. A transport-level retry reuses that ID. Changing the
form after a definitive response creates a new submission and a new operation ID. Submission
controls remain disabled while a request is in flight.

## State and Error Handling

The form has explicit loading, ready, submitting, success, and failed states. Entered values remain
intact after validation or server failure.

Structured errors are handled as follows:

- validation and policy errors appear near the form and preserve all input;
- a stale workspace revision offers **Reload latest** and never overwrites newer state;
- active-lease and policy-compatibility conflicts explain why the operation was rejected;
- retryable transport or server failures offer **Retry** with the same operation ID.

Archival uses a destructive confirmation dialog. The operator must enter the exact workspace slug
before the action is enabled. Server rejection, including active-lease rejection, leaves the
workspace active and displays the structured error.

## Presentation and Accessibility

The management UI extends the existing industrial operations-console visual language instead of
introducing a separate design system. The large policy editor uses clear sections and responsive
layouts, with native labels, field descriptions, keyboard-accessible controls, visible focus,
announced request status, and error summaries linked to affected fields. Destructive controls are
visually distinct from primary save actions.

## Documentation Correction

The master Org PRD and `docs/design.md` must be revised during implementation. “Read-only Web
console” means that Org content and operational workflow actions are read-only; workspace
configuration and lifecycle management are writable. Statements that prohibit all browser
mutations or restrict the Org frontend to GET requests must be replaced with this narrower boundary.

## Test Strategy

Implementation follows test-driven development. Tests must fail before the corresponding
production behavior is added.

Frontend unit tests cover:

- engineering-default policy initialization;
- lossless model and form round trips;
- client validation for states, transitions, roles, limits, and tag rules;
- create, update, and archive request bodies and URLs;
- fixed `web-ui` actor identity and operation-ID lifecycle;
- stale revision, validation, retry, and success reducer states;
- action suppression for archived workspaces;
- routing for `/org/new` and `/org/:workspace_id/settings`;
- continued absence of document, item, claim, transition, review, import, and raw-source mutations.

Browser acceptance uses a disposable local database and verifies the create → edit → archive flow,
including navigation, confirmation, structured errors, and archived-workspace visibility.

Scoped completion checks are:

```text
cd crates/note-frontend && cargo test
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cd crates/note-frontend && trunk build --release
scripts/verify-org-console-browser.sh
```

## Acceptance Criteria

1. An operator can create a valid workspace from `/org/new` without manually constructing policy
   JSON or supplying IDs.
2. An operator can edit every workspace metadata and policy field from the settings route.
3. A concurrent update returns a visible conflict and cannot silently overwrite the newer revision.
4. An operator can archive an eligible workspace only after confirming its slug.
5. Archived workspaces remain readable and expose no edit or archive controls.
6. Mutation retries are idempotent and duplicate submissions are prevented.
7. The Org document and operational workflow surfaces remain read-only in the browser.
8. No application authentication, actor-selection, session, or hard-delete behavior is introduced.
