# Org Document Lifecycle Management Design

**Status:** Approved

**Date:** 2026-08-23

## Problem

Agent Note models files in an Org workspace as canonical, database-backed Org documents. The
backend can already create or replace a document through `org_put_document`, and changing the
`path` in that full-source operation can act as a rename. Neither capability is exposed in the Web
UI, however, and the subsystem has no document removal lifecycle.

Using the full-source import command for a path-only rename is also a poor boundary. It requires a
caller to resend and revalidate the complete Org source and to supply lease proofs for projected
work items even though the source and projections do not change. Physical deletion would be worse:
it would discard canonical source or conflict with the stable identities, projections, leases,
dependencies, attempts, and audit history derived from that source.

The Org subsystem needs explicit file lifecycle management without turning the browser into an Org
source editor or weakening orchestration and audit guarantees.

## Product Decisions

- A **file** is one canonical Org document, identified by its stable document UUID.
- File lifecycle operations are available through pipelines, REST/OpenAPI, MCP, and the Web UI.
- The browser manages file lifecycle only. Raw Org source remains read-only there.
- Adding a file creates an empty valid Org document. The browser does not generate `#+TITLE:` or
  other starter content.
- File paths are portable, relative `.org` paths. Nested paths such as
  `projects/roadmap.org` are supported.
- Removing a file means reversible archival, not physical deletion.
- Archival preserves the document source, path, projections, work-item identities, revisions, and
  audit history.
- Archived files disappear from active document lists and operational views.
- A file with an active execution or review lease cannot be archived.
- Archived paths remain reserved. A new document cannot reuse the path of an archived document.
- An archived file may be renamed without restoring its work, allowing an operator to free its old
  reserved path deliberately.
- Restoring a file returns its preserved work items to operational views.

## Goals

1. Let users create, rename, archive, and restore Org files without editing their source.
2. Preserve document and work-item identity across every lifecycle change.
3. Keep lifecycle mutations revision-safe, idempotent, transactional, and auditable.
4. Maintain exact REST/MCP capability parity and OpenAPI inventory coverage.
5. Keep archived work out of queues, agendas, counts, and other operational candidate lists.
6. Preserve direct historical inspection and portable workspace snapshots.
7. Apply one portable path contract to online, offline, REST, MCP, and Web callers.

## Non-Goals

- Editing raw Org source in the Web UI.
- Physically deleting Org documents, work items, attempts, events, or relationships.
- Moving a document between workspaces; `org_move_document` remains responsible for that.
- Moving or reparenting work items between documents.
- Adding authentication, authorization, actor selection, or session behavior.
- Adding folders as independent stored entities. Nested path segments are part of the document path.
- Changing Markdown-note behavior.

## Path Contract

The existing portable offline-path validation becomes a shared Org-domain rule and gains the
`.org` extension requirement. A valid document path:

- is nonblank and already trimmed;
- is relative and uses `/` separators;
- may contain nested path segments;
- contains no empty, `.` or `..` segment;
- contains no backslash, absolute root, or Windows drive prefix; and
- ends in the exact lowercase extension `.org`.

Valid examples include `roadmap.org` and `projects/roadmap.org`. Invalid examples include
`/roadmap.org`, `../roadmap.org`, `projects//roadmap.org`, `projects\\roadmap.org`, `C:/roadmap.org`,
and `roadmap.txt`.

Valid paths are stored exactly as supplied. The service does not silently trim, normalize, change
case, or add an extension. The existing unique `(workspace_id, path)` constraint continues to cover
active and archived documents, thereby reserving archived paths.

## Domain and Storage Model

`OrgDocument` and both database schemas gain:

```text
archived_at: Option<i64>
```

The next PostgreSQL and Turso migrations add the nullable column without changing existing rows;
all existing documents remain active. Document read DTOs, source DTOs, workspace export manifests,
and offline snapshot metadata expose the archival state.

The storage contract gains transaction-safe, compare-and-swap operations for path-only rename,
archive, and restore. Creation continues to insert a new document, but goes through a dedicated
pipeline command. No repository delete method is introduced.

The canonical workspace projection continues to include archived-document items for dependency
evaluation, policy compatibility checks, history, and direct reads. Operational candidate queries
join or otherwise consult the owning document and exclude records whose document is archived.
This separation ensures:

- unfinished archived dependencies remain unsatisfied;
- workspace policy updates still account for preserved archived work;
- direct item context and event history remain readable; and
- restoration can reactivate the same projections without reconstruction or identity changes.

## Commands

All four commands use the existing mutation envelope: workspace ID, actor ID, operation ID, and
schema version. Replaying the same operation with the same fingerprint returns the original result;
reusing an operation ID with a different request is rejected.

### Create document

```text
org_create_document(document_id, path)
```

The client supplies a canonical document UUID. The pipeline validates that the workspace is active,
validates and reserves the path, parses the empty source, computes its content hash, inserts revision
1, installs an empty projection, and records a document creation event. It accepts no source field
and no expected revision.

### Rename document

```text
org_rename_document(document_id, new_path, expected_revision)
```

The pipeline verifies ownership, active workspace state, the exact document revision, and path
validity/uniqueness. The document itself may be active or archived. Rename changes only the stored
path and increments the document revision; it never changes archival state. Source, content hash,
projections, work-item identities, and leases remain untouched. Active leases do not block a
path-only rename.

### Archive document

```text
org_archive_document(document_id, expected_revision)
```

The pipeline verifies ownership, active workspace and document state, and the exact revision. It
rejects the request if any projected item has an active execution or review lease. Otherwise it sets
`archived_at`, increments the document revision, and records a document archive event. It does not
delete or rewrite source or projections.

### Restore document

```text
org_restore_document(document_id, expected_revision)
```

The pipeline verifies ownership, active workspace and archived document state, the exact revision,
and the still-reserved path. It clears `archived_at`, increments the document revision, and records a
document restore event. Preserved work becomes operationally visible again.

### Existing command interaction

- `org_put_document` remains the full-source create/update command for non-Web clients. It rejects
  updates to an archived document until that document is restored.
- `org_move_document` remains a cross-workspace ownership move and rejects archived documents.
- Workspace import/export and offline snapshots carry `archived_at` for every document.
- Imports may preserve an archived document but may not bypass revision, workspace, active-lease,
  or exact-document-set protections.

## Audit Events and Results

Creation uses the existing document-subject creation event. Three explicit event kinds are added:

```text
document_rename
document_archive
document_restore
```

Rename metadata contains the previous and resulting paths. Archive and restore metadata contains
the document path and previous/resulting archival state. Events contain no raw Org source.

Commands return the existing `OrgCommandResult`, including workspace ID, operation ID, event IDs,
and the resulting document revision. Lifecycle operations do not advance the workspace revision
because they do not change workspace configuration or ownership.

## REST and MCP Interfaces

REST adds:

```text
POST  /api/org/workspaces/{workspace_id}/documents
PATCH /api/org/documents/{document_id}/path
POST  /api/org/documents/{document_id}/archive
POST  /api/org/documents/{document_id}/restore
```

The create body carries the mutation envelope, document UUID, and path. The other bodies carry the
mutation envelope, workspace ID, expected revision, and the new path where applicable.

MCP adds the four commands with the exact `org_*` names specified above. REST operation IDs match
those MCP names. The Org MCP and matching OpenAPI inventories therefore increase from 36 to 40.
Cross-transport tests normalize and compare successful results and typed errors.

`GET /api/org/workspaces/{workspace_id}/documents` accepts a dedicated lifecycle filter:

```text
status=active | archived | all
```

Omitting `status` returns active files. The existing `include_archived=true` form remains a
backward-compatible alias for `status=all`; combining it with `status` is rejected as ambiguous.
Filtering happens before cursor pagination, so an archived-only page cannot be shortened by active
rows. Direct document reads remain available for archived files when the caller supplies the owning
workspace ID.

## Web UI

The frontend adds a dedicated route:

```text
/org/:workspace_id/files
```

The workspace header links to **Files**. The page lists path, revision, and active/archived status,
with an Active/Archived filter, cursor pagination, manual refresh, and an accessible live-status
region. It never renders a source editor or sends full-source document mutations.

For an active workspace:

- **Add file** opens a dialog with one path field.
- **Rename** opens a path-only dialog prefilled with the current path and is available for active
  and archived files.
- **Archive** requires the user to type the exact path before submission.
- **Restore** appears only in the archived-file view.

Archived workspaces expose the file list without mutation controls.

Every dialog keeps its draft when a request fails. One operation UUID is generated when the user
submits and is retained for a retry of the same request. A changed request receives a new operation
UUID. Successful operations refresh the appropriate list and announce the result.

## Error Handling

The transports preserve the existing structured Org error envelope. The UI distinguishes:

- invalid or nonportable paths;
- a path already reserved by an active or archived document;
- missing workspace or document;
- stale document revision;
- active workspace/document state conflicts;
- active execution or review leases blocking archival;
- divergent reuse of an operation ID; and
- retryable storage or transport failures.

On a stale revision, the UI retains the requested path and asks the user to refresh before
deliberately reapplying it. It does not automatically retry with a newly fetched revision. An
active-lease error identifies the blocking work items without exposing fencing tokens.

All lifecycle writes are atomic. Validation, CAS conflicts, lease conflicts, event failures, and
storage failures leave the document and its projections unchanged.

## Verification

Testing is limited to the affected Org subsystem and frontend surfaces.

### Storage contracts

- both PostgreSQL and Turso migrate existing active documents successfully;
- active and archived documents reserve `(workspace_id, path)`;
- rename changes only path, revision, and update time;
- archive and restore use exact revisions and preserve all source/projection fields;
- failed CAS and transaction rollback leave every record unchanged.

### Pipeline behavior

- create produces an empty revision-1 Org document;
- every accepted and rejected portable-path example is covered;
- idempotent replay and divergent operation reuse behave consistently;
- stale revisions and invalid lifecycle transitions are rejected;
- active execution and review leases block archival;
- archived items disappear from operational views and counts;
- unfinished archived dependencies remain blockers;
- policy validation continues to include archived projections;
- restore reactivates the same work-item identities;
- full-source put and cross-workspace move reject archived documents; and
- workspace and offline snapshot round trips preserve archival state.

### REST, MCP, and OpenAPI

- all four commands have equivalent success and error behavior;
- request schemas require the correct IDs, revisions, and mutation envelope;
- OpenAPI operation IDs match MCP tool names;
- transport conformance and inventory assertions cover exactly 40 Org operations.

### Frontend and browser

- models, URLs, request bodies, filters, dialog state, retry state, and error mapping have focused
  Rust tests;
- source-boundary assertions prove the Web UI has no raw-source editor or full-source PUT;
- archived workspaces expose no file mutation controls;
- a browser acceptance flow creates, renames, archives, filters, and restores one file;
- the archived file is absent from active operational views and reappears after restoration; and
- keyboard focus, accessible names, live announcements, console output, and network methods are
  verified in a real browser.

## Delivery Boundary

This design authorizes only Org document lifecycle management and the directly required schema,
pipeline, transport, frontend, documentation, and scoped test changes. It does not authorize a raw
Org editor, physical deletion, unrelated workflow mutations, release publication, or deployment.
