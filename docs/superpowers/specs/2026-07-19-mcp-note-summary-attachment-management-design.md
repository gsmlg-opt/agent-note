# MCP Note Summaries and Standalone Attachment Management

**Date:** 2026-07-19

## Problem

The MCP note-returning tools currently reuse one full-note response type. That
type serializes every attachment as optional UTF-8 text plus canonical Base64.
As a result, listing notes that contain images can produce multi-megabyte
responses even when the caller only needs note metadata.

The same full hydration also performs one attachment-store read per attachment.
Missing or temporarily unavailable attachment objects can therefore break note
discovery and metadata reads that do not need attachment bytes.

## Goals

- Keep discovery responses small and predictable.
- Manage attachment bytes only through explicit single-attachment MCP tools.
- Preserve note content where callers explicitly request one note.
- Preserve attachment metadata on full-note reads and update responses.
- Avoid attachment-store reads in list, search, and note-detail metadata paths.
- Keep note and attachment lifecycles separate.

## Non-Goals

- Changing REST response contracts.
- Adding bulk attachment reads, writes, or deletes.
- Adding response projection flags or compatibility modes.
- Adding attachment sizes, checksums, ranges, or streaming.
- Changing REST attachment-management contracts.

## MCP Response Contracts

### `list_notes`

Each result contains:

- `id`
- `title`
- `labels`
- `created_at`
- `updated_at`

It does not contain note `content` or `attachments`.

### `semantic_search`

Each result contains:

- `id`
- `title`
- `score`
- `labels`
- `created_at`
- `updated_at`

It does not contain note `content` or `attachments`.

### `get_note`

The response contains:

- `id`
- `title`
- `content`
- attachment metadata
- `labels`
- `created_at`
- `updated_at`

Each attachment contains only:

- `id`
- `path`
- `mime`
- `description`

Attachment `content` and `content_base64` are not present.

### `update_note`

The response uses the same note-detail shape as `get_note`: note content and
attachment metadata are returned, but attachment bytes are not.

The update request contains only:

- `id`
- `title`
- `content`
- `labels`

It does not accept `attachments`. Updating note text or labels preserves the
existing attachment set unchanged.

### `save_note`

The save request contains only:

- `title`
- `content`
- `labels`

It does not accept `attachments`. Callers create the note first, then add
attachments with `put_note_attachment`.

### `put_note_attachment`

Add a write MCP tool with:

- `note_id`: note identifier
- `attachment_id`: attachment identifier within the note
- `path`: relative attachment path within the note
- `mime`: attachment MIME type
- `description`: optional human-readable description
- at least one of `content` or `content_base64`

The operation adds a new attachment or replaces the attachment with the same
attachment id. An existing id cannot implicitly change its normalized path,
and a new id cannot reuse another attachment's normalized path. Renaming uses
an explicit delete followed by a put. If both content representations are
provided, their decoded bytes must match.

The response returns `created` plus the stored attachment metadata without
content.

### `get_note_attachment_content`

Add a read-only MCP tool with:

- `note_id`: note identifier
- `attachment_id`: attachment identifier within the note

The response includes attachment metadata and exactly one content
representation:

- UTF-8 attachment: `content`
- non-UTF-8 attachment: `content_base64`

The response never contains both representations, avoiding duplicate payloads.

### `delete_note_attachment`

Add a write MCP tool with:

- `note_id`: note identifier
- `attachment_id`: attachment identifier within the note

The response contains `deleted: true` when an attachment existed and was
removed, otherwise `deleted: false`.

## Architecture and Data Flow

`list_notes` will reuse the existing summary pipeline used by the REST list
endpoint. This avoids loading note bodies and avoids attachment hydration.

Search retrieval still needs stored note rows to obtain metadata after ranking,
but it will no longer hydrate attachments. Its public result will expose the
summary fields plus the fused RRF score, preventing transports from depending
on full note content.

`get_note` will use a pipeline path that loads the stored note and attachment
metadata without hydrating attachment objects. The existing fully hydrated path
remains available to workflows that genuinely need complete note data, such as
export.

MCP `save_note` creates notes with an empty attachment set. MCP `update_note`
uses a note-fields pipeline that updates title, content, labels, chunks, and
embedding jobs while preserving stored attachment metadata. REST/UI workflows
that intentionally submit a complete attachment set keep their existing
pipeline.

`get_note_attachment_content` will call a pipeline that resolves the attachment
id against stored metadata and then delegates to the existing path-based
single-object read. The pipeline reads only the selected attachment object.

The current attachment-store abstraction stages and publishes a complete
attachment set. Standalone writes must not hydrate and rewrite every existing
attachment, so the abstraction will gain staged single-object put and delete
operations. Filesystem put publication uses the existing same-filesystem
staging and atomic rename model. S3 put publication stages one object and then
copies it to its final key. A prepared delete captures the selected object's
path before metadata removal. The same-note mutation lock serializes puts and
deletes with other attachment publications.

The storage contract will gain an attachment-metadata-only update performed
inside a transaction. While the per-note mutation lock is held, the transaction
re-reads metadata and validates attachment-id and normalized-path uniqueness.
A put stages the new object, commits the updated metadata, then publishes the
staged object. This preserves the existing database-before-publication ordering
used by note saves and updates.

A delete resolves the attachment id to its stored path, commits metadata
removal first, then removes the captured physical object. Failure after the
database commit can leave an unreferenced object, but cannot leave active
metadata pointing to deliberately deleted content. This matches the
repository's existing preference for recoverable orphans over broken active
references.

Attachment-only mutations update the note's `updated_at` timestamp but do not
change its content revision, chunks, or embedding jobs.

Transport-specific request and response schemas remain in
`crates/note-mcp/src/stdio.rs`; transport-independent data and tool wrappers
remain in `crates/note-mcp/src/tools.rs`; storage and attachment access remain
behind `note-pipelines::Context`.

## Error Handling

- A missing note from `put_note_attachment` returns a clear MCP not-found
  error.
- A missing note or attachment from `get_note_attachment_content` returns a
  clear MCP not-found error.
- `delete_note_attachment` is idempotent and returns `deleted: false` when the
  note or attachment is absent.
- Invalid or traversal-style put paths are rejected as invalid parameters.
- Duplicate attachment ids, attempts to move an existing id to another path,
  and normalized-path collisions are rejected without mutation.
- Attachment-store failures affect only standalone attachment tools and
  workflows that explicitly request attachment bytes.
- List, search, and `get_note` metadata responses remain usable when an
  attachment object is missing.

## Compatibility

This is an intentional MCP output-schema break:

- `list_notes` no longer returns full notes.
- `get_note` and `update_note` no longer return attachment content.
- `semantic_search` adds labels and timestamps.
- `save_note` and `update_note` no longer accept inline attachments.
- Attachment writes and deletes move to standalone tools.

Callers needing note content use `get_note`. Callers needing attachment bytes
use `get_note_attachment_content`. Callers add, replace, or remove attachments
with `put_note_attachment` and `delete_note_attachment`.

Input schemas for list filters and search filters do not change.

## Testing

Tests will verify:

- `list_notes` JSON and advertised output schema exclude `content` and
  `attachments`.
- `semantic_search` includes labels and timestamps while excluding `content`
  and `attachments`.
- `get_note` and `update_note` include attachment metadata but exclude
  attachment `content` and `content_base64`.
- Save and update input schemas do not expose `attachments`.
- Updating note text or labels preserves existing attachments without reading
  them.
- `put_note_attachment` validates paths, ids, content encoding, add semantics,
  and same-id replacement.
- `get_note_attachment_content` returns text directly for UTF-8 bytes and
  Base64 only for binary bytes.
- `get_note_attachment_content` reads only the selected attachment.
- `delete_note_attachment` is idempotent and removes only the selected
  attachment.
- Put/delete update metadata without queuing embedding work.
- Filesystem and S3 tests cover staged single-object publication, cleanup, and
  same-note mutation serialization.
- List, search, and `get_note` perform no attachment-store reads and tolerate a
  missing attachment object.
- Existing REST/UI attachment editing and export behavior continue to pass.

## Documentation

Update the README MCP section to distinguish:

- attachment metadata returned by note-detail tools;
- attachment bytes returned only by `get_note_attachment_content`;
- attachment changes performed through `put_note_attachment` and
  `delete_note_attachment`;
- the absence of inline attachments from MCP save/update;
- summary fields returned by list and search.
