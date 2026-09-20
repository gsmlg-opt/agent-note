# MCP Replace and Patch Note Contract

## Goal

Align the Markdown-note MCP mutation surface with the reference implementation in
`/tmp/ant-design/agent-note` while preserving this repository's HTTP-only transport,
split Note/Org endpoints, storage abstractions, and Agent Note-specific tools.

The `/mcp` endpoint will replace `update_note` with `replace_note`, replace the
line-number/tag-based `edit_note` tool with `patch_note`, and keep `read_note_lines`
as an optional inspection tool. `/org/mcp`, MCP 2026-07-28 behavior, the bulk-label
tool, and the standalone attachment tools remain unchanged. The Note inventory
therefore remains twelve tools, with two names and contracts replaced.

The repository's `skills/agent-note` package will move to the same mutation model.

## Scope

This change includes:

- strict `replace_note` and `patch_note` MCP request and response contracts;
- transport-neutral note mutation logic in `note-pipelines`;
- strict contextual Markdown patching in `note-core`;
- integration with the current storage and attachment abstractions;
- stable structured MCP mutation errors;
- Note MCP inventory, schema, behavior, and HTTP transport tests;
- README and architecture documentation for the changed Note MCP surface; and
- the complete `skills/agent-note` package material affected by mutation guidance.

This change does not alter:

- `/org/mcp`, its forty tools, or Org behavior;
- the HTTP-only, stateless, JSON, POST-only MCP transport;
- MCP 2026-07-28 discovery, routing headers, cache metadata, or Origin policy;
- REST routes or frontend behavior;
- the internal line-edit pipeline where REST or frontend code still uses it;
- standalone attachment tools or bulk note-label updates; or
- authentication, authorization, label taxonomy, or project-label policy.

## Approaches Considered

### Native contract port into the current architecture

Add the reference mutation semantics to `note-core` and `note-pipelines`, then expose
them through the existing MCP server. Use the current storage, attachment-store, and
embedding abstractions rather than copying SQLite/filesystem-specific code. This is
the selected approach because it provides the intended atomic contract without
regressing the current architecture.

### Rename-only compatibility

Rename `update_note` and `edit_note` without changing their inputs or behavior. This
would advertise false compatibility: current `update_note` preserves attachments,
and current `edit_note` uses line numbers and a content tag rather than a contextual
patch. This approach is rejected.

### Compose existing MCP operations

Implement the new tools by chaining current update and standalone attachment calls.
This would expose intermediate states, increment revisions more than once, and lose
atomic no-op and failure behavior. This approach is rejected.

## MCP Inventory

After the change, `/mcp` exposes exactly these twelve tools:

1. `bulk_update_note_labels`
2. `save_note`
3. `get_note`
4. `read_note_lines`
5. `replace_note`
6. `patch_note`
7. `delete_note`
8. `list_notes`
9. `semantic_search`
10. `put_note_attachment`
11. `get_note_attachment_content`
12. `delete_note_attachment`

`update_note` and `edit_note` are removed from the MCP registry without aliases.
Keeping aliases would leave overlapping tools and would not match the requested
client contract. `read_note_lines` continues to return numbered content and a tag
for inspection, but neither value is a write precondition for the new mutations.

## Request Contracts

### `replace_note`

`replace_note` is a closed-schema full replacement with these required fields:

- `id`: note ID;
- `expected_revision`: positive revision from the latest `get_note`;
- `title`: complete title;
- `content`: complete Markdown body;
- `attachments`: complete attachment set; `[]` clears it; and
- `labels`: complete label-pair set; `[]` clears it.

Unknown fields are rejected. Missing collections are errors rather than preservation
signals. Existing validation rules still apply to the complete desired note.

### `patch_note`

`patch_note` is a closed-schema partial replacement. It requires `id`, a positive
`expected_revision`, and at least one of `title`, `content`, `attachments`, or
`labels`. Omitted writable fields are preserved. Explicit `null` is invalid for all
writable fields. Supplied arrays replace their complete collections; `[]` clears.

`content` accepts either:

- a string that replaces the complete Markdown body; or
- `{"apply_patch": "..."}`, a strict contextual content patch.

The patch body contains one or more plain `@@` hunks. Lines begin with a space for
exact context, `-` for deletion, or `+` for insertion. Each hunk must include existing
context or deletion lines that match exactly one place in the original content.
Hunks refer to the original content, must be ordered, and must not overlap. File
directives, envelopes, fuzzy matching, line-number headers, EOF markers, and
unanchored insertion are rejected. Untouched bytes, CRLF style, and the presence or
absence of a final newline are preserved. A complete string replacement handles
changes that cannot be expressed by the restricted patch format.

## Response and Error Contracts

Both mutation tools return only:

```json
{"id":"note-id","revision":13,"changed":true}
```

A semantic no-op returns `changed: false` with the existing revision and does not
change `updated_at`, attachment objects, labels, search data, or embedding work.
Label ordering alone is not a change. Revision validation still runs for no-ops.

Malformed arguments, including unknown fields and explicit nulls, use JSON-RPC
invalid parameters. Operational mutation failures are MCP tool errors with
`isError: true` and structured content containing `code`, `message`, `details`, and
`retryable`. Stable codes are:

- `not_found` for a missing or inactive note;
- `revision_conflict` with expected and current revisions;
- `patch_failed` for invalid or non-matching contextual patches;
- `invalid_request` for a nonpositive revision or an empty patch request;
- `validation_error` for invalid note, label, or attachment data; and
- `internal_error` for storage or attachment failures without leaking internals.

On conflict, clients must re-read and recompute the intended mutation. Replacing only
the revision token on a stale request is not safe.

## Pipeline and Storage Flow

The MCP layer performs schema conversion only. A shared pipeline mutation will:

1. validate the request-level invariants;
2. start the appropriate current storage transaction and read the active note for
   revision-guarded mutation;
3. reject a stale revision before performing attachment or embedding work;
4. compute the complete desired title, content, labels, and attachment set;
5. apply and validate any contextual content patch;
6. validate the complete desired note and typed label values;
7. detect a semantic no-op;
8. prepare changed attachment content through the current attachment store and
   durable attachment-operation model;
9. atomically commit note fields, label rows, attachment references, revision, and
   search/embedding job state in one storage transaction; and
10. notify embedding/dashboard consumers only after a changed commit.

The implementation must not chain public MCP tools. A changed request increments the
note revision exactly once. Failures before the database commit leave the persisted
note unchanged. Prepared but unreferenced external objects follow the existing
attachment-operation recovery/cleanup policy; they must never replace or destroy the
last committed attachment generation.

The existing REST/frontend line-edit path may retain its internal pipeline APIs.
Only MCP registration and MCP-facing DTOs remove the line/tag edit contract.

## Server Integration

The Note-only router continues to expose twelve tools, and the Org-only router
continues to expose forty. The aggregate server uniqueness checks and cross-path
rejection behavior remain intact. MCP server instructions will direct clients to
read `get_note.revision`, distinguish full replacement from partial patching, and
re-read after conflicts.

The custom MCP call handling must distinguish malformed tool arguments from
well-formed operational failures so that only the former become JSON-RPC errors.
Successes retain `structuredContent` and the MCP 2026 `resultType` discriminator.

## Agent Note Skill Update

The maintained repository source is `skills/agent-note`. Its project resolution,
project-label preservation, authorization, duplicate checking, verification, and
curation safeguards remain unchanged.

Mutation guidance changes as follows:

- `get_note.revision` is the write guard for both mutation tools;
- `patch_note` is the default for focused title, body, attachment-set, or label-set
  changes because omitted fields are preserved;
- body patches use unique contextual hunks, not line numbers or tags;
- `read_note_lines` is optional inspection and no longer supplies write tokens;
- `replace_note` is used only when every writable field is intentionally supplied;
- full replacement preserves intended labels and attachments by explicitly carrying
  them forward when they are not meant to change; and
- conflicts trigger one fresh read and semantic recomputation, never token swapping.

Update `SKILL.md`, `references/mcp-contract.md`, `references/examples.md`, relevant
evaluation cases, `README.md`, `SOURCES.md`, `VALIDATION.md`, and skill metadata/version
where appropriate. Remove stale normative guidance that recommends `edit_note` or
`update_note`; historical provenance may mention those names only when clearly marked
as superseded. The installed copy outside the repository is not an independent
source-of-truth edit in this task.

## Test Strategy

Implementation follows red-green-refactor. Each behavior begins with a focused test
that fails for the expected missing-contract reason.

### Core

- accept replacement and strict contextual patch content forms;
- apply one and multiple ordered non-overlapping hunks;
- preserve Unicode, CRLF, mixed untouched line endings, and unterminated EOF;
- reject missing, ambiguous, overlapping, reversed, unanchored, or file-level patches.

### Pipeline and storage

- replace all writable fields with one revision increment;
- patch each field independently and in combination;
- preserve omitted fields and clear supplied empty collections;
- reject stale/nonpositive revisions and empty patch requests;
- detect no-ops without writes, notifications, or queued embedding work;
- preserve the committed note on validation, patch, attachment, storage, or commit
  failures; and
- exercise current attachment metadata/content and storage backends without importing
  reference-repository filesystem assumptions.

### MCP

- assert the exact twelve-tool Note inventory and absence of `update_note`/`edit_note`;
- assert closed input schemas, required replacement fields, optional non-null patch
  fields, and exact output schemas;
- assert success, no-op, malformed-input, structured-error, and attachment behavior;
- call both tools over MCP 2026-07-28 HTTP and retain result/cache discriminators; and
- retain split endpoint, cross-path rejection, POST-only, Origin, and forty-tool Org
  inventory coverage.

### Skill and documentation

- run the repository's skill static validator;
- validate YAML, links, UTF-8/LF, evaluation-case integrity, and package metadata;
- scan active guidance for stale `update_note`/`edit_note` recommendations; and
- record checks actually run without converting fixtures into claimed results.

Only affected, focused crate and skill checks run during this scoped change. Failures
outside the scope are reported without unrelated repair.

## Completion Criteria

The work is complete when:

- `/mcp` advertises and executes `replace_note` and `patch_note` with the contracts
  above and no longer advertises `update_note` or `edit_note`;
- the Note and Org endpoint inventories remain twelve and forty respectively;
- mutation state, revision, attachment, label, search, and embedding invariants pass
  focused tests;
- the HTTP MCP 2026-07-28 contract remains green;
- `skills/agent-note` consistently teaches the new mutation workflow and passes its
  static validation; and
- documentation describes the implemented behavior without claiming REST changes.
