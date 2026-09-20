# Agent Note MCP contract and compatibility

Read this before the first mutation, then inspect the tools and schemas exposed by
the connected server. Runtime schema is authoritative: a repository checkout and a
deployment can differ. Do not invent fields, broaden scope, or bypass a rejected guard.

## Reviewed implementation

The local feature checkout was reviewed against base `39106fe18d0fd79922c6ba6f57f6d26ddbbfaca9`
on 2026-09-21. `crates/note-mcp/src/http.rs` serves HTTP-only, stateless Streamable
HTTP endpoints: Note tools at `/mcp` and Org tools at `/org/mcp`. `server.rs` owns the
wire schemas and handlers; `tools.rs` is the transport-neutral wrapper boundary.

`/mcp` has exactly these 12 Note tools:

`bulk_update_note_labels`, `delete_note`, `delete_note_attachment`, `get_note`,
`get_note_attachment_content`, `list_notes`, `patch_note`, `put_note_attachment`,
`read_note_lines`, `replace_note`, `save_note`, and `semantic_search`.

Older connections that expose `edit_note` or `update_note` are superseded compatibility
profiles, not a recommended baseline. Do not select them when the live schema exposes
the current replacement/patch contract. There is no current `stdio.rs` transport source.

## Scope, reads, and labels

Use `label` selectors for scoped search/list calls, with a bounded explicit limit.
Search/list results are summaries: fetch likely notes with `get_note`, inspect their
raw labels, and verify the exact `project` pair before relying on or changing a note.
For ordinary values use `project=sample-project`; use an advertised exact selector only
when the live schema supports it. Never normalize, split, or broaden an unrepresentable
project value.

Labels are string pairs such as `[["project", "sample-project"]]`, not a map. Keep
exactly one project pair on a scoped note and do not silently repair scope drift.
`get_note` returns content, labels, attachment metadata, and `revision`; attachment
metadata does not contain attachment bytes.

## Current mutation workflow

1. Inspect the live schemas. Read `get_note`, verify target/scope/project labels, and
   retain `get_note.revision` as `expected_revision`.
2. Use `patch_note` for focused changes. `title`, `content`, `attachments`, and `labels`
   are optional only by omission: omitted preserves; supplied collection arrays replace
   the whole collection; `[]` clears it; explicit `null` is invalid. A patch with every
   mutable field omitted is `invalid_request`.
3. Send content either as a full string or as `{ "apply_patch": "..." }`. The patch
   contains one or more plain `@@` hunks; each hunk has context or deletion lines that
   match exactly once in the original content. It accepts no line numbers, tags,
   paths/file directives, patch envelopes, or fuzzy application. `read_note_lines` can
   help inspect Markdown but supplies no mutation token and is not required before a
   patch; its returned revision and tag are informational.
4. Use `replace_note` only for an intentional complete replacement. It requires ID,
   `expected_revision`, title, full content, full attachment set, and full label set.
   Empty arrays deliberately clear their collections.
5. Before a patch or replacement supplies an attachment set, call
   `get_note_attachment_content` for every attachment to retain; send each returned
   metadata record with exactly one of `content` or `content_base64`. Metadata without
   a payload, or both payload representations, is rejected before mutation. Omitting an
   existing attachment from the supplied set removes it if the mutation succeeds.
6. A successful semantic no-op reports `changed: false` and retains the revision. On
   `revision_conflict`, re-read and semantically recompute once; never replay stale
   content with a replacement revision. A second conflict ends the operation.
7. Read `get_note` after the write and verify intended state and the project label.

`replace_note` and `patch_note` return structured mutation results. Expected operational
error codes are `not_found`, `revision_conflict`, `patch_failed`, `invalid_request`,
`validation_error`, and `internal_error`; each provides `code`, `message`, `details`,
and `retryable`. Treat error details as diagnostic data, not authorization to retry.

## Authorized destructive and bulk work

Deletion, attachment removal, bulk relabeling, and project migration still need explicit
authorization and exact targets. Re-read exact targets and use the current guards. A
multi-note curation operation is not assumed atomic: verify the retained/replacement
note before marking older records superseded, and report partial completion honestly.

## Sources

- `crates/note-mcp/src/http.rs` — HTTP routing and Streamable HTTP behavior.
- `crates/note-mcp/src/server.rs` — Note inventory, schemas, handlers, result mapping.
- `crates/note-mcp/src/tools.rs` — Note detail, attachment, and mutation wrapper data.
- `crates/note-mcp/tests/org_inventory_test.rs` — split-route inventory coverage.
- `README.md`, "MCP servers" — published endpoint, inventory, and mutation guidance.
