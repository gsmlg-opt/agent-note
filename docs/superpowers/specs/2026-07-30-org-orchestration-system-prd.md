# Product Requirements Document: Org Orchestration System

**Date:** 2026-07-30

**Status:** Approved; interface scope revised 2026-08-04

**Source design:** [`../../org-module-design.md`](../../org-module-design.md)

**Interface expansion design:**
[`2026-08-04-org-delivery-interfaces-design.md`](2026-08-04-org-delivery-interfaces-design.md)

## Problem Statement

Agent Note provides durable Markdown knowledge, but it does not provide a durable
execution model. Agents can preserve research, decisions, and results, yet they
cannot use Agent Note to reliably discover work, claim it, coordinate with other
agents, enforce workflow rules, recover interrupted execution, or audit how a
task reached its current state.

Using Markdown notes as an informal task queue leaves critical behavior
undefined:

- task identity changes when prose or document structure changes;
- workflow states and transitions are not validated;
- dependencies cannot reliably gate execution;
- two agents can start the same work;
- task ownership disappears when an agent crashes;
- concurrent human and agent edits can overwrite one another;
- schedules, deadlines, reviews, retries, and blockers are not queryable as
  operational data;
- current document text does not provide a complete execution history; and
- implementation tasks can become disconnected from the Markdown knowledge
  that motivated or resulted from them.

Agent Note needs a separate execution plane that remains human-readable and
portable while providing strong orchestration semantics to agents.

## Solution

Add a multi-workspace, Org-native orchestration subsystem to Agent Note.

Standard Org text will remain the canonical, exportable representation of
plans and work items. Agent Note will store that source together with a
monotonic document revision, derive queryable work-item projections, validate
workspace workflow policy, and record execution events. Agents and API clients
will interact through equivalent structured MCP and REST operations for
queues, claims, transitions, dependencies, reviews, and progress rather than
rewriting whole documents. A workspace-first Web operations console will
expose the resulting state to human operators. Raw Org source and workflow
actions remain Web-read-only; workspace administration and document-container
create/rename/archive/restore are the approved lifecycle exceptions.

The subsystem will establish two complementary product planes:

- Markdown notes store durable knowledge, context, reasoning, and results.
- Org workspaces store durable work, workflow state, dependencies, schedules,
  ownership, and execution coordination.

The two planes will be connected through stable links without merging their
data models.

The first release will support concurrent agents through one active Agent Note
server process, complete MCP and REST interfaces, offline import/export, and a
Web operations console with scoped workspace and document lifecycle controls.
Autonomous scheduling, raw-source editing, and browser workflow execution remain
deferred.

## Goals and Success Measures

### Goals

1. Preserve standard Org text as the portable representation of orchestration
   data.
2. Support multiple isolated workspaces with independently configured workflow
   policy.
3. Give every orchestrated work item a stable identity independent of its
   heading, position, document, or workspace location.
4. Make routine agent operations semantic, validated, revision-safe, and
   auditable.
5. Prevent duplicate execution with exclusive leases and fencing tokens.
6. Make task readiness depend on state, dependencies, schedule, policy, and
   lease status.
7. Preserve recovery context across agent failure, lease expiry, and retry.
8. Link work items to Markdown notes used as context or produced as results.
9. Keep existing Markdown note behavior and APIs unchanged.
10. Provide complete and equivalent MCP and REST access to every initial Org
    operation.
11. Provide a workspace-first Web operations console that keeps raw source and
    workflow actions read-only while exposing approved lifecycle administration.
12. Keep automatic dispatch, source editing, hard delete, and browser workflow
    execution out of this release.

### Success Measures

1. Org documents round-trip through storage and export without losing
   unsupported but valid Org content.
2. A stale document revision is rejected without changing source, projections,
   leases, or events.
3. Concurrent claims for one item yield exactly one active lease.
4. A stale lease holder cannot mutate a task after expiry and reassignment.
5. Ready queues never return work with unsatisfied dependencies, future
   schedules, an active lease, or a non-executable state.
6. Every state-changing operation records an ordered event with actor, time,
   reason, and before/after state where applicable.
7. The embedded and PostgreSQL adapters pass the same Org storage contract
   suite.
8. MCP over stdio and Streamable HTTP expose the same Org tools and behavior.
9. REST exposes an OpenAPI operation equivalent to every initial Org MCP tool,
   and cross-transport conformance tests produce equivalent normalized results
   and errors.
10. Operators can inspect workspaces, operational queues, agendas, work-item
    context, leases, and ordered event history through a Web UI that performs
    no workflow mutation.
11. MCP exposes 40 Org tools plus 12 Markdown-note tools (52 total), and OpenAPI
    exposes the 40 matching Org operation IDs while excluding `/mcp`.
12. Existing Markdown behavior remains unchanged and its tests continue to pass.
   Aggregate MCP inventory assertions are intentionally updated for the added
   Org tools.

## User Stories

1. As a workspace owner, I want to create an isolated Org workspace, so that
   unrelated projects do not share policies, queues, or documents.
2. As a workspace owner, I want each workspace to have a stable identifier and
   human-readable name, so that it remains addressable after renaming.
3. As a workspace owner, I want to define allowed work-item types, so that the
   workspace reflects its operating model.
4. As a workspace owner, I want to define workflow states and valid
   transitions, so that agents cannot invent lifecycle behavior.
5. As a workspace owner, I want to identify executable and terminal states, so
   that queue and completion behavior is deterministic.
6. As a workspace owner, I want to require review for selected work types, so
   that agents cannot bypass approval policy.
7. As a workspace owner, I want to configure lease duration and retry limits,
   so that recovery behavior matches the workspace.
8. As a workspace owner, I want to archive or export one workspace
   independently, so that workspace ownership remains portable.
9. As a human Org user, I want documents to remain valid Org files, so that I
   can read and edit them with Emacs.
10. As a human Org user, I want unknown Org syntax to be preserved, so that
    Agent Note does not destroy content it does not interpret.
11. As a human Org user, I want to export a document as raw Org text, so that I
    can edit it using normal Org tools.
12. As a human Org user, I want to import an edited document with the revision
    I started from, so that a concurrent update cannot be silently overwritten.
13. As a human Org user, I want a conflict response to include the current
    revision, so that I can reconcile my changes explicitly.
14. As a human Org user, I want headings to keep their identity after being
    renamed or moved, so that links and execution history remain valid.
15. As an agent, I want to list workspaces, so that I can discover the
    orchestration domains available to me.
16. As an agent, I want to inspect workspace policy before taking work, so that
    I follow its state, review, retry, and lease rules.
17. As an agent, I want to create an issue with tasks and subtasks, so that a
    plan can be decomposed into executable work.
18. As an agent, I want created work items to receive stable IDs, so that I can
    refer to them safely in later operations.
19. As an agent, I want to get one work item by ID, so that document movement
    does not break retrieval.
20. As an agent, I want a task context response to include its parent, policy,
    dependencies, history, and linked notes, so that I can begin with complete
    execution context.
21. As an agent, I want to add and remove dependencies by stable ID, so that
    execution order does not depend on heading text.
22. As an agent, I want dependency cycles rejected, so that the scheduler
    cannot create permanently unready work.
23. As an agent, I want only completed dependencies to satisfy a task, so that
    unfinished prerequisite work is not bypassed.
24. As an agent, I want scheduled work hidden until its scheduled time, so that
    I do not execute it early.
25. As an agent, I want deadlines visible in agenda responses, so that urgent
    work can be prioritized.
26. As an agent, I want a deterministic ready queue, so that repeated queries
    return a predictable ordering.
27. As an agent, I want to filter queues by workspace, type, state, priority,
    assignment, and time window, so that I can select appropriate work.
28. As an agent, I want cross-workspace agenda queries to be explicit, so that
    isolation is never bypassed accidentally.
29. As an agent, I want to atomically claim ready work, so that another agent
    cannot execute it at the same time.
30. As an agent, I want a successful claim to return an opaque fencing token
    and expiry time, so that later operations prove ownership.
31. As an agent, I want to heartbeat an active claim, so that long-running work
    is not reassigned while I remain healthy.
32. As an agent, I want to release a claim voluntarily, so that another agent
    can continue work I cannot perform.
33. As an agent, I want to report progress without changing lifecycle state, so
    that humans and replacement agents can observe partial work.
34. As an agent, I want to mark work blocked with a reason, so that it leaves
    the ready queue and exposes what must change.
35. As an agent, I want to link blocker tasks, so that unblocking can be derived
    from explicit work rather than prose alone.
36. As an agent, I want to transition work only along configured paths, so that
    workspace policy is enforced consistently.
37. As an agent, I want completion rejected when required review is missing, so
    that review policy cannot be skipped.
38. As an agent, I want to submit a result and request review, so that execution
    and evaluation are separate auditable stages.
39. As a reviewer, I want a review queue, so that I can discover work awaiting
    approval.
40. As a reviewer, I want to approve or reject work with a reason, so that the
    outcome and required follow-up are explicit.
41. As an agent, I want rejected work to return to a configured executable
    state, so that revision work can be claimed safely.
42. As an agent, I want a failed attempt to preserve its error and progress, so
    that a retry does not repeat discovery unnecessarily.
43. As an agent, I want an expired claim to become reclaimable, so that crashed
    agents cannot lock work permanently.
44. As a replacement agent, I want prior attempts, progress, failures, and
    artifacts in the task context, so that I can recover the work.
45. As a lease holder, I want stale ownership rejected after reassignment, so
    that my delayed response cannot overwrite the new owner's result.
46. As an agent, I want to create follow-up work from a result or review, so
    that newly discovered work remains tracked.
47. As an agent, I want to link a task to Markdown context notes, so that its
    rationale and research are available during execution.
48. As an agent, I want to link produced Markdown notes as results, so that the
    output remains durable and searchable.
49. As a note reader, I want to follow an explicit reference to the responsible
    task, so that knowledge can be traced back to execution.
50. As an operator, I want to list running tasks and their lease owners, so that
    current execution is observable.
51. As an operator, I want to list blocked, failed, expired, and review-pending
    work, so that stalled workflows are visible.
52. As an operator, I want to inspect an append-only event history, so that I
    can understand who changed a task and why.
53. As an operator, I want completion records to include attempt, actor, time,
    result summary, review outcome, notes, and artifacts, so that completed work
    remains auditable.
54. As an operator, I want workspace exports to contain canonical Org documents
    and workspace metadata, so that recovery does not depend on Agent Note.
55. As an API client, I want validation, not-found, revision-conflict,
    lease-conflict, and policy-conflict outcomes to be distinct, so that I can
    recover correctly.
56. As an MCP client, I want semantic Org tools separate from Markdown note
    tools, so that knowledge and execution operations cannot be confused.
57. As a storage operator, I want identical behavior from embedded and
    PostgreSQL storage, so that selecting a backend does not change workflow
    semantics.
58. As an existing Agent Note user, I want all current note behavior to remain
    unchanged, so that enabling Org orchestration does not disrupt my knowledge
    base.
59. As an API client, I want every Org MCP capability available through REST,
    so that non-MCP integrations do not need a separate business model.
60. As an API client, I want retries of one mutation operation to return the
    original result, so that network uncertainty cannot duplicate a claim,
    transition, or review.
61. As an operator, I want to enter Org through a workspace directory, so that
    policy, time, and queue boundaries remain explicit.
62. As an operator, I want workspace-scoped operational views in a browser, so
    that ready, assigned, running, blocked, review, scheduled, deadline, failed,
    expired-lease, and completed work are visible without MCP tooling.
63. As an operator, I want a work-item context page that preserves my source
    list filters when I return, so that investigation does not disrupt triage.
64. As an operator, I want the first browser console to be read-only, so that
    observing work does not accidentally mutate workflow state.
65. As an operator, I want to create an empty `.org` file with a stable UUID and
    portable path, so that source can be populated later through approved APIs.
66. As an operator, I want to rename active or archived files without changing
    source, projections, dependencies, or history.
67. As an operator, I want reversible document archive and restore with active
    lease protection, so that operational work can be withdrawn without deletion.
68. As an operator, I want active and archived file ledgers with revision-safe
    conflicts and no hard delete or raw-source editor.

## Functional Requirements

### Workspace and Policy

1. A workspace must have a stable ID, unique slug, display name, description,
   required IANA timezone, policy, revision, archive state, and timestamps.
2. Workspace policy must define:
   - allowed work-item types;
   - workflow states;
   - initial, running, executable, review, failed, cancelled, successful
     terminal, and other terminal states;
   - allowed state transitions;
   - which types or transitions require review;
   - release, review-rejection, and lease-expiry recovery states;
   - whether claims are open, assignment-restricted, or explicitly dispatched;
   - lease duration;
   - retry limit;
   - concurrency limit;
   - allowed or required Org tags by work-item type;
   - whether cross-workspace agenda queries are allowed.
3. Policy updates must be revision-safe and must reject a policy that would
   make existing work invalid.
4. The default engineering policy must use `BACKLOG`, `READY`, `RUNNING`,
   `BLOCKED`, `REVIEW`, `DONE`, `FAILED`, and `CANCELLED`.
5. In the default engineering policy, `DONE` is the successful terminal state;
   `READY` is the release, review-rejection, and lease-expiry recovery state.
6. A retry limit of `N` permits the initial attempt plus at most `N` additional
   attempts.
7. Workspace deletion is not required. Workspaces may be archived and remain
   queryable and exportable.
8. Archiving must require the expected workspace revision and must be rejected
   while the workspace has any active lease.
9. An archived workspace must reject imports, semantic source changes, claims,
   heartbeats, transitions, reviews, and all other mutations while remaining
   queryable, exportable, and auditable.
10. Archived workspaces must be excluded from normal lists, queues, and agendas
    unless the caller explicitly requests archived data.
11. Unarchiving is not required in the first release.

### Org Documents and Identity

1. Canonical Org source must be stored byte-for-byte with a monotonically
   increasing document revision.
2. Document paths must be unique within a workspace and may change without
   changing document or work-item IDs.
3. Moving a document between workspaces must preserve document and work-item
   IDs, require expected revisions for both workspaces, and validate every
   contained item against the target policy before committing.
4. Work-item identity must use the standard Org `:ID:` property with a UUID
   value.
5. A newly created orchestrated heading must receive an ID before it is
   committed.
6. Duplicate IDs within an import must be rejected. IDs already owned by a
   different stored item must produce a conflict.
7. Supported semantic metadata must use valid Org constructs:
   - headings and heading hierarchy;
   - TODO keywords for workflow state;
   - native priority cookies for priority;
   - native heading tags for workspace tag conventions;
   - `SCHEDULED` and `DEADLINE` timestamps;
   - `:ID:`, `:AGENT_NOTE_TYPE:`, `:ASSIGNEE:`, `:DEPENDS_ON:`, and
     `:REQUIRES_REVIEW:` properties for orchestration metadata; and
   - Org links for Markdown note references.
8. `:DEPENDS_ON:` stores a whitespace-separated set of work-item UUIDs;
   `:REQUIRES_REVIEW:` stores `true` or `false`; and `:ASSIGNEE:` stores one
   actor ID.
9. The initial work-item type vocabulary must include project, epic, issue,
   task, subtask, review, approval, incident, and milestone.
10. The interpreted Org subset is limited to headings and hierarchy, configured
   TODO keywords, priority cookies, heading tags, planning timestamps, property
   drawers used by Agent Note, and Org links used by Agent Note.
11. All other source spans are opaque. Raw reads and exports must reproduce
    them byte-for-byte.
12. A semantic mutation must update the smallest relevant source region and
   preserve unrelated source formatting and content.
13. If the parser cannot isolate a semantic mutation without rewriting an
    opaque span, the mutation must be rejected as unsupported.
14. Whole-document replacement must remain an administrative operation and
    require the expected document revision.
15. Import must parse and validate the complete candidate document before any
    stored source or projection changes.
16. Import must derive semantic differences and enforce the same transition,
    dependency, review, assignment, and lease rules as structured operations.
17. Import must reject ownership-sensitive changes to an actively leased item
    unless the current fencing token is supplied.
18. Omitting a previously stored work item from a document is valid only when
    the same atomic import batch moves that ID to another document. Otherwise
    the omission must be rejected; cancellation is a semantic transition, not
    implicit deletion.
19. Moving or reparenting a work item between documents must be one atomic
    semantic operation guarded by the expected revision of every affected
    document.
20. Source text, derived projections, links, and the corresponding execution
    event must commit atomically.
21. Every document must carry nullable `archived_at`; active documents have
    `null`, and archived documents have the archive timestamp in direct reads,
    lists, exports, and snapshots.
22. Document paths must be portable relative paths ending in lowercase `.org`.
    Leading/trailing whitespace, absolute or drive-prefixed paths, backslashes,
    repeated separators, and empty, `.` or `..` segments are invalid.
23. Empty create must store `source == ""`, revision 1, active state, and the
    caller-supplied stable document UUID.
24. Rename must accept active or archived documents, require the current
    document revision, increment that revision, and preserve source, content
    hash, stable IDs, projections, dependencies, direct history, and archive
    state.
25. Archive must require an active workspace, active document, current revision,
    and no active execution or review lease for any item in that document. It
    must preserve path and content, set `archived_at`, and increment revision.
26. Restore must require an active workspace, archived document, and current
    revision. It must clear `archived_at`, increment revision, and reactivate the
    same identities and projections.
27. A path remains reserved while its document is archived. Renaming an archived
    document releases the old path and reserves the new one.
28. Archived documents remain directly readable and exportable, but their items
    are excluded from every operational view and count. An unfinished dependency
    on an archived document's item remains unsatisfied and blocking.
29. Raw source and workflow mutations against archived documents must return
    `archived_document`. Document hard delete is not supported.

### Work Items, State, and Dependencies

1. The hierarchy in the Org document must define parent-child relationships.
2. State transitions must be validated against the active workspace policy.
3. A transition must reject:
   - an unknown state;
   - a disallowed transition;
   - completion with unmet dependencies;
   - completion without required review;
   - lease-bound mutation without the current fencing token; and
   - an update based on a stale document revision.
4. Dependency relationships must use stable work-item IDs.
5. The first release must support finish-to-start dependencies: a dependent
   item is ready only when every prerequisite is in a successful terminal
   state.
6. Dependency creation must reject self-dependencies and cycles.
7. Dependencies within one workspace are required. Cross-workspace
   dependencies are deferred.
8. Review and approval requirements must be explicit policy or relationship
   data rather than inferred from heading text.
9. Follow-up work must be linkable to the event or work item that created it.
10. Assignment must be a semantic operation that records an event. By default,
    an item assigned to one actor may be claimed only by that actor until it is
    reassigned or unassigned.
11. Retrying failed work must be explicit, must follow workspace policy, must
    preserve earlier attempts, and must reject attempts beyond the configured
    retry limit.
12. Source-changing item operations, including create, move, reparent,
    assignment, schedule, dependency, claim, and transition, must use the
    expected revision of every affected document. The first release does not
    define a separate item revision.
13. Event-only progress updates and lease-only heartbeats do not increment the
    document revision; they require the current fencing token and return the
    resulting event or lease state.

### Claims, Leases, and Recovery

1. Only items currently eligible for the requested execution or review queue
   may be claimed.
2. Claiming must be atomic and must create:
   - a unique lease ID;
   - an opaque, unguessable fencing token;
   - an actor ID;
   - an attempt number;
   - acquired, heartbeat, and expiry times; and
   - a claim event.
3. Claiming normal executable work must transition it to the workspace running
   state in the same transaction.
4. Claiming review work must create an exclusive reviewer lease while
   preserving the review state.
5. At most one unexpired lease may exist for a work item.
6. Heartbeat, release, progress, transition, review submission, approval,
   rejection, failure, and completion must validate the current fencing token.
7. Heartbeat must extend expiry according to workspace policy without changing
   the fencing token.
8. Voluntary release must end the lease and return the item to its configured
   releasable state unless a different valid state is explicitly requested.
9. Blocking, failure, review submission, approval, rejection, completion,
   cancellation, and lease expiry must end the active lease.
10. Expired leases must be treated as inactive during queue and claim
   evaluation, even before maintenance processing records the expiry event.
11. Lease-expiry event creation must be idempotent.
12. An item in the running state with an expired lease is a recovery candidate.
    Reclaiming it must atomically close the expired lease, record the expiry,
    transition through the configured lease-expiry recovery state, create a new
    attempt and fencing token, and return it to the running state.
13. Reclaiming expired or failed work must create a new attempt and fencing
    token while preserving prior attempts, progress, errors, links, and events.
14. Concurrency limits must be enforced per workspace during claim.

### Events and Audit

1. Execution events must be append-only.
2. Each event must include an event ID, workspace ID, subject kind, subject ID,
   actor ID, attempt where applicable, event type, timestamp, reason or
   summary, and structured metadata.
3. State-change events must include the previous and resulting state.
4. Required event types must include creation, assignment, claim, heartbeat,
   release, start, progress, block, unblock, review request, approval,
   rejection, completion, failure, retry, cancellation, lease expiry,
   dependency change, schedule change, document import, and note link change.
5. Each workspace must allocate a monotonically increasing event sequence in
   the same transaction as the mutation. History is ordered by that sequence;
   timestamps are descriptive rather than the causal ordering mechanism.
6. Event history must remain queryable after completion or workspace archive.
7. Events are associated audit data and must not replace the canonical Org
   source.
8. Attempts and completion events may reference produced artifacts by stable
   URI, media type, name, and description. Artifact bytes are not stored by the
   Org subsystem.
9. Automatic expiry and recovery events use the reserved `system` actor ID.
   Client operations require an asserted non-empty actor ID and may not assert
   the reserved value.

### Mutation Idempotency

1. Every client-initiated mutation over MCP or REST must require a non-empty,
   client-generated `operation_id` in addition to the asserted `actor_id`.
2. The service must persist the operation ID, a canonical request fingerprint,
   and the transport-neutral result in the same transaction as the mutation.
3. Retrying an operation ID with the same request fingerprint must return the
   original result without appending another event or repeating any mutation.
4. Reusing an operation ID with a different request fingerprint must return an
   `idempotency_conflict` without side effects.
5. Internal lease-expiry bookkeeping remains independently idempotent and uses
   the reserved `system` actor.

### Queues and Agendas

1. The ready queue must include only items whose:
   - state is executable, or whose running state has an expired lease and is
     therefore recoverable;
   - prerequisites are complete;
   - scheduled time is absent or due;
   - workspace is active;
   - concurrency policy permits another claim; and
   - current lease is absent or expired.
2. Workspace concurrency counts only unexpired execution and review leases.
3. Queue ordering must be deterministic: priority, earliest deadline, earliest
   scheduled time, creation time, then stable ID.
4. Recovery candidates must be identified explicitly in queue results.
5. Separate operational queries must support ready, assigned, running,
   blocked, review, scheduled, upcoming-deadline, failed, expired-lease, and
   completed views.
6. Queue and agenda queries must support filtering by work-item type, state,
   priority, tags, assignment, and time window.
7. Queue and agenda queries must use opaque cursor pagination with a default
   limit of 50, a maximum limit of 200, and stable-ID tie-breaking.
8. A query must be scoped to one workspace unless the caller explicitly
   supplies multiple workspace IDs.
9. Cross-workspace results must preserve workspace identity on every item.
10. An explicit multi-workspace query must be rejected unless every selected
    workspace policy permits cross-workspace agenda queries.
11. Queue clients must not need to parse Org documents or reimplement
    readiness rules.

### Markdown Knowledge Links

1. Work items must support typed links to Markdown notes for context, result,
   report, design, postmortem, and other purposes.
2. Canonical source must represent these references with
   `[[agent-note:<purpose>:<note-uuid>][<description>]]` Org links so purpose
   and target are both recoverable from source.
3. A linked note must exist when the link is created.
4. Deleting or soft-deleting a note must not silently remove the Org link;
   retrieval must expose that the target is unavailable.
5. Task context must hydrate available Markdown note metadata without copying
   note bodies into Org storage.
6. Reverse lookup must identify work items linked to a note.
7. Note links are weak references stored as note IDs without cascading foreign
   keys. Pipelines validate existence when a link is created and report missing
   targets when links are read.

### Import, Export, and Human Editing

1. A raw document read must return Org source, document ID, path, and revision.
2. A raw document update must require the expected revision and return the new
   revision.
3. A workspace export must include all active and archived canonical Org
   documents and a machine-readable workspace manifest containing policy,
   revisions, and each document's nullable `archived_at`.
4. A workspace import must support create and revision-safe update modes.
5. Import must report document-level validation or conflict errors without
   partially applying a document.
6. The supported first-release Emacs workflow is explicit export, edit, and
   revision-safe import.
7. Automatic filesystem watching, bidirectional live synchronization, and
   background conflict merging are not required.
8. Multi-document moves and imports must commit all affected document sources,
   projections, and events atomically or change nothing.
9. A missing `archived_at` in a legacy snapshot means active. Ordinary document
   import must not set archival state; only snapshot restore and lifecycle
   commands may do so.

### MCP and REST Interfaces

1. MCP must be the canonical agent interface.
2. Org tools must use an `org_` prefix and remain separate from Markdown note
   tools. REST must expose an equivalent operation for every initial Org MCP
   tool.
3. The initial 36-tool MCP family exposes these operations:
   - `org_list_workspaces`, `org_create_workspace`, `org_get_workspace`,
     `org_update_workspace`, and `org_archive_workspace`;
   - `org_list_documents`, `org_get_document`, `org_put_document`,
     `org_move_document`, `org_move_item`, `org_import_workspace`, and
     `org_export_workspace`;
   - `org_create_item`, `org_get_item`, `org_get_item_context`, and
     `org_create_follow_up`;
   - `org_assign_item` and `org_schedule_item`;
   - `org_query_queue` and `org_query_agenda`;
   - `org_claim_item`, `org_heartbeat_claim`, and `org_release_claim`;
   - `org_report_progress`, `org_submit_result`, `org_transition_item`, and
     `org_retry_item`;
   - `org_request_review`, `org_approve_item`, and `org_reject_item`;
   - `org_add_dependency` and `org_remove_dependency`;
   - `org_link_note`, `org_unlink_note`, and `org_list_note_work_items`; and
   - `org_list_events`.
4. The document lifecycle extension must add `org_create_document`,
   `org_rename_document`, `org_archive_document`, and `org_restore_document`,
   producing a current inventory of 40 Org tools plus 12 Markdown-note tools
   (52 MCP tools total).
5. The four matching REST operations are exactly:
   - `POST /api/org/workspaces/{workspace_id}/documents`;
   - `PATCH /api/org/documents/{document_id}/path`;
   - `POST /api/org/documents/{document_id}/archive`; and
   - `POST /api/org/documents/{document_id}/restore`.
6. `org_transition_item` must cover valid block, unblock, cancellation, and
   other workspace-defined transitions that do not have a more specific tool.
7. Routine tools must perform semantic operations and must not require clients
   to rewrite complete documents.
8. Every source-changing mutation must accept the expected revision of every
   affected document.
9. Workspace mutations must accept the expected workspace revision.
   `org_move_document` must accept the document revision plus source and target
   workspace revisions.
10. Every lease-bound operation must accept the opaque fencing token.
11. Every client mutation must accept a non-empty actor ID and operation ID.
12. Actor ID in this release is an asserted client identity, not an
   authenticated principal.
13. MCP stdio and Streamable HTTP must register the same schemas and call the
    same pipelines.
14. MCP and REST errors must distinguish invalid input, missing resource, stale
    revision, idempotency conflict, invalid transition, unmet dependency,
    active lease, stale lease, `archived_document`, `document_path_conflict`,
    concurrency limit, and storage failure.
15. REST must expose all 40 current Org operations under `/api/org` and include
    every operation in the generated OpenAPI document; `/mcp` is excluded.
16. Each Org REST OpenAPI `operationId` must equal the corresponding `org_*`
    MCP tool name.
17. REST and MCP must use the same pipeline operations, transport-neutral DTOs,
    validation rules, idempotency behavior, result semantics, and error codes.
18. REST must use structured JSON errors with stable `code`, `message`,
    `details`, and `retryable` fields. MCP structured errors must expose the
    same fields.
19. List operations must use opaque cursors, a default limit of 50, and a
    maximum limit of 200 unless an operation defines a stricter bound.
20. Document lists must accept `status=active|archived|all`, default to active,
    and retain `include_archived=false|true` as the legacy alias for active/all.
    Supplying both is invalid, and status must bind the opaque cursor.
21. Fencing tokens must be returned to a successful claiming client and must
    never be included in general read-only operational DTOs.

### Human Import and Export Interface

1. The existing server binary must provide offline Org workspace and document
   import/export commands that call the same pipelines as MCP.
2. Export must write raw `.org` documents and the workspace manifest without
   requiring an active HTTP server.
3. Import must require an explicit create or revision-safe update mode and
   report per-document validation and conflict results.
4. Offline commands must load the normal storage configuration and must not
   start embedding workers or mutate Markdown note data.

### Web Operations and Workspace Management Interface

1. The frontend navigation must add an `Org` destination.
2. `/org` must list workspace summaries and default to active workspaces, with
   an explicit option to include archived workspaces.
3. `/org/:workspace_id` must expose workspace-scoped ready, assigned, running,
   blocked, review, scheduled, upcoming-deadline, failed, expired-lease, and
   completed views using server-computed operational data.
4. `/org/:workspace_id/items/:item_id` must expose hierarchy, dependencies,
   Markdown note availability, attempts, lease metadata, recovery context, and
   sequence-ordered event history.
5. `/org/:workspace_id/files` must expose a semantic file ledger with File,
   Revision, Status, and Actions columns; Active and Archived URL-backed filters;
   opaque cursor pagination; and explicit loading, empty, error, and refresh
   states. It must never render or edit raw Org source.
6. Operational views must use tables rather than a board. Current view,
   filters, cursor or page state, and typed return context must be represented
   in the URL so returning from an item restores its originating list.
7. The UI must default to workspace time and may show browser-local time as
   secondary information.
8. Missing or deleted Markdown note targets must remain visible and be marked
   unavailable.
9. The UI must provide explicit loading, empty, structured error, and manual
   refresh states.
10. `/org/new` must create a workspace from a complete structured engineering
   default, and `/org/:workspace_id/settings` must edit every workspace policy
   field using the loaded revision.
11. Active workspace pages must expose edit and reversible archive controls.
    Archive requires exact-slug confirmation; archived workspaces remain
    readable and expose neither lifecycle control. Restore and hard delete are
    not part of this UI.
12. Browser mutations are limited to the three workspace lifecycle endpoints
    and four document-container lifecycle endpoints specified above. Raw Org
    source, work items, claims, transitions, review, import/move, and hard delete
    remain unavailable.
13. Document create must make an empty file. Rename must work in active and
    archived ledgers. Archive requires exact-path confirmation; restore reuses
    the same ID, source, and projections. An archived workspace exposes the
    file lists read-only with no Add, Rename, Archive, or Restore controls.
14. A stale document submission must preserve the requested draft, disable
    automatic Retry and submit, require **Refresh files**, load the latest
    revision, then allow only a new deliberate submission with a new operation
    UUID.
15. Workspace and document mutation requests must use `actor_id: "web-ui"`, a generated
    idempotent operation UUID, and an expected revision for update/archive.
16. Dialogs must move focus inside, close with Escape or Cancel, label every
    input, name row actions with the exact path, and announce each success
    through a changing polite live region. Interactive targets must be at least
    44 CSS pixels in each dimension and the page must not overflow at 390px.
17. The UI must not display fencing tokens, call MCP directly, add polling, or
    implement authentication/session behavior.

## Implementation Decisions

### Product Boundary

- The Org subsystem is a separate execution domain. Existing Markdown note
  entities, search, embedding, labels, attachments, and APIs remain unchanged.
- This is a master PRD delivered in independently verifiable slices. Each slice
  will receive its own implementation plan before code changes begin.
- One active Agent Note server process coordinates concurrent agents in the
  first release. Multi-replica server coordination is not introduced.

### Module Boundaries

- Add one new pure `note-org` crate as the deep domain module. It owns parsing,
  loss-preserving rendering, stable identity, policy validation, state
  transitions, dependency-cycle detection, readiness calculation, and
  source-to-projection logic. It performs no I/O and should not depend on
  sibling crates.
- Extend the existing storage abstraction with cohesive Org repository
  contracts for workspaces, canonical documents, projections, dependencies,
  note links, leases, attempts, events, and idempotent operation results.
- Implement those contracts in both existing database adapters. Do not add a
  separate Org database adapter or storage crate.
- Extend the shared storage contract suite so both adapters prove identical Org
  behavior.
- Add transport-free orchestration workflows to `note-pipelines`. Pipelines own
  transaction boundaries and compose the pure Org domain with storage and
  Markdown note lookups.
- Add an Org-specific pipeline context containing shared storage and an
  injectable clock. The full application context may embed this context, while
  offline Org commands construct it without embedding or attachment services.
- Add the semantic `org_*` tool family to the existing MCP server and reuse it
  unchanged across stdio and Streamable HTTP.
- Extend the existing server binary with offline Org import/export command
  modes and a complete Org REST surface. REST handlers call the same pipelines
  as MCP, and every operation is included in OpenAPI.
- Extend the Yew frontend with a REST-backed Org operations console. Org source
  and workflow operations remain read-only; workspace create/structured
  update/reversible archive and document-container create/rename/archive/restore
  are the only browser mutation controls.
- Keep the embedding and attachment modules unchanged.

### Canonical Source and Projections

- Canonical Org source is stored in the selected database backend, not in a
  watched filesystem.
- A document row contains the original Org text, current monotonic revision,
  stable UUID, portable unique path, and nullable `archived_at`.
- Work-item, dependency, schedule, queue, and link records are projections
  derived from canonical source. They are never an independent source of
  truth.
- A semantic mutation computes updated source and projections through
  `note-org`, then commits source, projections, and events in one storage
  transaction guarded by compare-and-swap revision checks.
- Raw source is retained byte-for-byte. Uninterpreted spans remain opaque, and
  a semantic mutation is rejected when its target cannot be safely isolated.
  The supported semantic subset may grow without requiring document migration.
- True storage-level compare-and-swap is required. Existing note content-tag
  checks are not sufficient for Org concurrency.
- Projections must be rebuildable from canonical Org source. Events, attempts,
  leases, and operation results are authoritative runtime data and are not
  discarded during projection rebuilds.

### Persistence Model

- Canonical data consists of workspaces and Org documents.
- Derived data consists of work items, hierarchy, dependencies, schedules,
  deadlines, priorities, tags, assignments, and Markdown note links.
- Associated runtime data consists of leases, execution attempts, progress,
  append-only events, and idempotent operation results.
- Canonical persistence introduces a minimal event ledger and per-workspace
  event sequence in Slice 2 so import, source, projections, and their event can
  commit atomically. Slice 3 adds the complete workflow event vocabulary.
- Embedded storage must admit existing schema-version-2 databases during
  preflight, migrate them atomically to version 3 under an immediate
  transaction, and update the schema version only after every DDL and data
  migration succeeds. A failed migration must roll back and leave the version-2
  database readable by the previous release.
- PostgreSQL uses its existing ordered migration mechanism.
- Lease claims use atomic conditional writes. Every ownership-sensitive update
  additionally validates the current fencing token.

### Default Workflow

- New work begins in `BACKLOG` unless the caller requests another valid initial
  state.
- Moving work to `READY` requires satisfied dependencies.
- Claiming `READY` work atomically creates a lease and transitions it to
  `RUNNING`.
- `RUNNING` work may become `BLOCKED`, `REVIEW`, `DONE`, `FAILED`, or
  `CANCELLED` only where workspace policy permits.
- Review-required work transitions from `RUNNING` to `REVIEW`; it cannot
  transition directly to `DONE`.
- Approval transitions review work to `DONE`.
- Rejection returns work to the workspace-configured rework state, which
  defaults to `READY`.
- Approval and rejection end the exclusive reviewer lease.
- Reclaiming expired `RUNNING` work closes the old lease, records expiry,
  transitions through the configured recovery state, and creates the new
  running attempt in one transaction.
- Terminal states end any active lease but retain all source, projections,
  attempts, links, and events.

### Queue and Time Semantics

- Every workspace has a required IANA timezone. Org timestamps preserve their
  original text and explicit timezone or offset where present.
- Org timestamps without an explicit zone are interpreted in the workspace
  timezone and persisted with the resolved UTC instant used for queries.
- Ambiguous or nonexistent local times caused by daylight-saving transitions
  are rejected rather than silently shifted or guessed.
- API DTOs return the original local representation, workspace timezone, and
  resolved UTC instant where applicable. The Web UI defaults to workspace time
  and may show browser-local time secondarily.
- A schedule is an earliest-start constraint. A deadline affects agenda and
  ordering but does not by itself make work executable.
- Lease expiry is evaluated transactionally at read/claim time so recovery does
  not depend on a scheduler process.
- All Org pipelines that evaluate current time use the injected Org clock and
  pass the resolved timestamp into storage operations.
- Queue order is stable and documented; clients must not implement their own
  readiness rules.

### API and Conflict Semantics

- Pipeline errors use stable domain categories that MCP, REST, and offline
  command modes map into their respective error responses.
- Every client mutation requires an operation ID. The mutation transaction
  stores its canonical request fingerprint and transport-neutral result;
  identical retries replay that result and divergent reuse returns an
  idempotency conflict.
- Source-changing mutations return the resulting item and new document revision.
  Lifecycle commands return `{document_id, path, archived_at}` plus exactly one
  document revision and no workspace revision. Event-only and lease-only
  mutations leave the document revision unchanged and return the resulting
  event or lease state.
- Multi-document operations accept an expected-revision map keyed by document
  ID. Raw imports that touch leased items additionally accept a fencing-token
  map keyed by work-item ID.
- A conflict never partially mutates source, projections, leases, events, or
  operation results.
- Document archive active-lease and lifecycle-state checks run in the same
  atomic transaction as the revision change; rejected commands reserve no
  operation result.
- Claim responses include complete execution context: item, parent, workspace
  policy, dependencies, linked notes, prior attempts, recent events, lease,
  and current revision.
- Actor IDs are recorded for audit but are not trusted authorization data in
  this release.
- Agent Note implements no authentication, authorization, session, or
  workspace ACL behavior. A front proxy owns the deployment security boundary,
  and direct exposure to an untrusted network is unsupported.

### Delivery Slices

1. **Org domain foundation**
   - pure parser and loss-preserving source model;
   - stable IDs and semantic metadata;
   - workspace policy and transition validation;
   - dependency graph and readiness calculation.
2. **Canonical persistence**
   - workspace and document storage;
   - derived projections and note links;
   - minimal event ledger, per-workspace event sequence, and operation records;
   - schema migrations and cross-backend contract tests;
   - revision-safe import, update, and export.
3. **Workflow and audit**
   - semantic item mutations;
   - state transitions and dependency gating;
   - complete append-only events, attempts, progress, review, idempotent
     command behavior, and recovery context.
4. **Claims and operational views**
   - fencing-token leases and heartbeats;
   - expiry, release, retry, and concurrency limits;
   - ready queues and agendas.
5. **MCP and offline interfaces**
   - all 36 MCP Org tools over both existing transports;
   - offline workspace/document import and export;
   - end-to-end conflict, recovery, and compatibility verification.
6. **REST and OpenAPI parity**
   - REST equivalents for all 36 initial Org MCP operations;
   - shared DTO, validation, idempotency, result, and error semantics;
   - generated OpenAPI coverage and cross-transport conformance tests.
7. **Web operations console and workspace management**
   - workspace directory, create, settings, operations, and work-item context routes;
   - URL-backed filters, pagination, and typed return context;
   - loading, empty, error, time, missing-note, and archived-workspace states;
   - browser create/update/archive for workspaces only; no Org source or workflow mutation.
8. **Document lifecycle management**
   - portable path contract, nullable archive state, and cross-backend migration;
   - empty create, rename, lease-guarded reversible archive, and restore with
     stable IDs, direct history, dependencies, source, and projections preserved;
   - four additional matching MCP/REST operations, making 40 Org and 52 total
     MCP tools, with `/mcp` excluded from OpenAPI; and
   - `/org/:workspace_id/files` with active/archived cursor ledgers and the four
     approved lifecycle controls, without raw-source/workflow/hard-delete UI.

## Testing Decisions

### Test Philosophy

- Test externally observable behavior and invariants, not private parser,
  renderer, SQL, or handler implementation details.
- Use the pure Org module for exhaustive domain cases, real temporary embedded
  databases for pipeline integration, and the shared contract suite for
  backend parity.
- Use a controllable clock for schedule, heartbeat, lease expiry, deadline, and
  retry tests through the Org pipeline context. Tests must not depend on
  real-time sleeps.
- Concurrency tests must coordinate with barriers or events rather than
  arbitrary delays.
- Fixtures must include realistic Emacs-produced Org documents and valid syntax
  outside the interpreted semantic subset.

### Modules to Test

- `note-org`
  - parse and loss-preserving round-trip;
  - stable IDs across rename, move, hierarchy, and document-path changes;
  - minimal semantic source updates;
  - policy validation and transition tables;
  - dependency-cycle detection and readiness;
  - schedules, deadlines, priorities, tags, properties, and note links.
- Storage contracts
  - workspace isolation;
  - canonical source and projection atomicity;
  - compare-and-swap revisions;
  - operation-ID replay and divergent-request conflict;
  - append-only per-workspace event sequencing and the reserved system actor;
  - atomic claim winner;
  - fencing-token rejection;
  - expiry and reclaim;
  - deterministic queue queries;
  - transactional embedded migration from schema version 2 to version 3,
    including rollback on injected failure.
- Pipelines
  - document import and semantic mutations;
  - atomic cross-document and cross-workspace moves;
  - import semantic-diff validation, active-lease conflict, and omitted-heading
    rejection;
  - rollback on parse, policy, revision, note-link, or storage failure;
  - dependency-gated readiness;
  - review-required completion;
  - claim, heartbeat, release, failure, retry, and completion;
  - recovery context hydration;
  - weak note-link behavior after soft and permanent note deletion;
  - explicit cross-workspace agenda behavior;
  - workspace-timezone resolution and daylight-saving rejection;
  - archived-workspace mutation rejection and continued read/export access;
  - idempotent original-result replay without duplicate events;
  - no partial events or projections after conflict.
- MCP
  - exact 40-tool Org inventory and schemas, plus 12 note tools for 52 total;
  - DTO mapping and stable error categories;
  - identical registration for stdio and Streamable HTTP;
  - complete claimed-task context.
- REST and OpenAPI
  - all 40 equivalent Org operation IDs with `/mcp` excluded;
  - shared DTO, validation, result, idempotency, and error semantics;
  - cursor bounds and structured error envelopes;
  - cross-transport conformance against MCP.
- Frontend
  - workspace directory, file lifecycle ledger, operational table, and
    work-item context routes;
  - URL-backed views, filters, pagination, and return context;
  - workspace and browser-local time rendering;
  - archived workspaces, unavailable notes, lease metadata without fencing
    tokens, event sequence, loading, empty, and error states;
  - accessible dialog focus, exact-path action names, live announcements,
    44px targets, desktop/mobile overflow, and browser history across three
    cursor pages; and
  - absence of raw Org editing, workflow, hard-delete, MCP, polling, and auth controls.
- Offline command modes
  - raw document and workspace export;
  - revision-safe import and conflict reporting;
  - configuration loading without starting unrelated runtime workers.
- Regression
  - all existing note storage contract, pipeline, MCP, REST, import/export, and
    frontend suites appropriate to unchanged behavior, with only aggregate MCP
    inventory and OpenAPI expectations intentionally expanded.

### High-Risk Acceptance Scenarios

1. Two agents claim the same ready item concurrently; one succeeds and one
   receives an active-lease conflict.
2. Agent A's lease expires, Agent B reclaims the item, and Agent A's delayed
   completion is rejected by fencing token.
3. A human exports revision 7, an agent commits revision 8, and the human's
   revision-7 import is rejected without side effects.
4. A heading is renamed and moved to another document while retaining its
   `:ID:`; dependencies, links, events, and retrieval continue to resolve.
5. A document containing unsupported valid Org syntax receives a semantic state
   transition; unrelated syntax and formatting remain unchanged.
6. A task with an incomplete dependency, future schedule, or required review
   never appears in the normal executable queue.
7. A transaction fails after source computation; canonical text, projections,
   lease state, and events all remain at the previous revision.
8. An existing embedded database migrates successfully and all existing
   Markdown notes remain readable and writable.
9. A current-revision raw import tries to change an actively leased item's
   state without its fencing token and is rejected without side effects.
10. One atomic move removes an item from the source document and inserts it in
    the target document while preserving its ID, history, dependencies, and
    links.
11. An import omits a stored heading without moving it elsewhere in the same
    batch and is rejected rather than deleting its execution history.
12. A successful mutation is retried with the same operation ID and returns
    the original result without a second event; different request data with the
    same operation ID is rejected without side effects.
13. The same representative create, claim, transition, review, and conflict
    scenarios produce equivalent normalized outcomes through MCP and REST.
14. An ambiguous or nonexistent local Org timestamp is rejected, while a valid
    timestamp is resolved through the workspace IANA timezone and displayed in
    workspace time by the Web UI.
15. Archiving fails while a lease is active; after release, archive succeeds,
    all mutations are rejected, and queries, export, and event history remain
    available.
16. A browser operator opens a filtered workspace queue, inspects one item, and
    returns to the same view, filters, and pagination context without any
    mutation control or exposed fencing token.
17. The browser creates an empty `projects/roadmap.org`, then direct read proves
    source `""`, revision 1, active state, and the same UUID.
18. A concurrent rename makes a browser draft stale; `stale_revision` preserves
    the draft and forbids retry until explicit refresh, after which a new
    deliberate rename succeeds with a new operation UUID.
19. Archive requires exact-path confirmation, omits the document from active
    and operational views, keeps direct history and unfinished dependencies,
    and rejects while any contained item has an active execution or review lease.
20. An archived document can be renamed; its new path remains reserved, its old
    path becomes reusable, and restore returns the same UUID, source, hash,
    projections, dependencies, and history to active operation.
21. Snapshot round-trip preserves `archived_at`, while a legacy snapshot without
    the field restores the document as active.

## Out of Scope

- Autonomous scheduling, agent dispatch, or continuous task execution.
- Agent capability matching, resource scoring, or performance-based assignment.
- Browser raw Org editing, document source replacement/import/move/hard delete,
  work-item create/edit, claim, release, transition, review, or workspace
  restore/delete controls.
- A browser raw Org editor, board view, or global cross-workspace operations
  homepage.
- Automatic Web UI polling, push updates, or live event streaming.
- Multiple concurrently active Agent Note server replicas.
- Live filesystem-canonical workspaces, file watching, bidirectional sync,
  FUSE, or automatic merge of Emacs changes.
- Full implementation of every Org Mode feature or extension.
- Cross-workspace dependencies in the first release.
- Recurring-task expansion, notifications, calendar synchronization, or email
  integration.
- Authentication, authorization, sessions, proxy identity protocols, and
  enforcement of workspace access-control lists. A front proxy owns this
  deployment boundary.
- Trusting asserted actor IDs as authenticated identities.
- Unarchiving workspaces in the first release.
- Automatic archival or deletion of completed work.
- Physical deletion of work items or execution history.
- Distributed transactions with external artifact stores.
- Replacing Markdown notes, embedding Org content into semantic note search, or
  converting existing notes into tasks.
- Importing task state from third-party project-management systems.
- A generic project-management product independent of agent execution.

## Further Notes

- The governing product principle is: **Org manages what agents do; Markdown
  stores what agents know.**
- Database-canonical Org source is selected because it allows source,
  projections, events, and revision checks to commit atomically across both
  supported storage backends. A filesystem-canonical design would require a
  watcher, conflict journal, crash recovery, and reconciliation subsystem that
  is not justified for the first release.
- The first-release Emacs integration is deliberately explicit:
  export, edit, and revision-safe import. A richer Emacs client can be designed
  later against the same document APIs.
- The asserted actor-ID limitation must be visible in API documentation. It
  provides attribution, not security.
- README, OpenAPI, configuration, and Web deployment documentation must state
  that Agent Note implements no authentication or authorization and must not be
  exposed directly to an untrusted network; a front proxy owns that boundary.
- MCP and REST are complete peers at the application contract. The Web UI
  consumes reads plus the three workspace and four document-container lifecycle
  operations; all raw-source and workflow mutations remain transport-only and
  are not exposed as browser controls.
- Each delivery slice must receive a scoped implementation plan and scoped test
  commands before implementation begins.
- The repository currently has no configured GitHub issue tracker or
  `ready-for-agent` label. This PRD remains repository-local until a target
  tracker is identified.
