# Agent Note MCP contract and compatibility

Read this before the first mutation, then inspect the tools actually exposed by the
client. This is a writing workflow, not a replacement RPC schema. Prefer the live
schema for arguments and returned values; an error that contradicts it indicates a
connection/version mismatch, not permission to invent fields or bypass a guard.

## Reviewed evidence

Public `gsmlg-opt/agent-note` main sources were inspected on 2026-09-08. The reviewed
`crates/note-mcp/src/stdio.rs` and `tools.rs` differ from the eight-tool connection
exposed in the authoring conversation. Neither deployment version nor a common
commit was established. These are observed interface profiles, not version labels.

| Operation | Reviewed repository source | Authoring connection |
| --- | --- | --- |
| `save_note` | Title, content, labels; no embedded attachments | Also exposes optional attachments |
| `edit_note` | ID, expected revision, tag, edits | ID, tag, edits; no revision argument |
| `update_note` | ID, expected revision, title, content, labels; preserves attachments | No revision argument; also exposes attachments |
| `delete_note` | ID and expected revision | ID only |
| Label keys | Missing keys can be created by note writes | Description restricts writes to existing keys |
| Additional tools | Separate attachment tools and bulk label updates | Not exposed in this connection |

No mutation was performed to probe these differences. Runtime behavior was not
verified. Read the deployment schema again rather than identifying it by tool count.

## Scope, labels, and reads

Use the `label` selector argument on search/list calls. Use an explicit positive
`limit`; `list_notes` also supports `offset`. List/search return summaries: fetch
bodies by ID before relying on or changing them. Titles and ranks are search aids,
not correctness or applicability scores. Verify the exact project label in results.

For ordinary project values, use `project=synapsis`. The reviewed repository also
documents exact selectors of the form `~<percent-encoded-key>==<percent-encoded-value>`;
for example `~project==team%26service`. Use this only when the live schema advertises
it. A legacy connection must not silently strip, split, normalize, or broaden an
unrepresentable project selector. Report the limitation instead. Do not change the
configured project value merely to make a query succeed.

Write labels as string pairs, for example `[["project", "synapsis"]]`; this is not a
map and `project: synapsis` is not one combined key. Do not duplicate keys. Keep type,
status, scope, and provenance in the body unless the project configures such labels.
Never drop the project label to recover from a validation error, and never use a
bulk operation or REST fallback to repair a catalog without authorization.

## Body edit sequence

1. Fetch note details; confirm target ID, project, intended scope, and existing data.
2. Read numbered lines. When both reads expose revisions and they differ, re-read
   details and lines until they describe one coherent edit base or stop.
3. Build edits against the returned original 1-based line numbers. `swap` and
   `delete` ranges are inclusive. Do not renumber later operations as though earlier
   operations had already executed; avoid overlapping operations.
4. Supply the returned tag, and the revision as `expected_revision` wherever the
   live tool supports it. Treat tokens as opaque and tied to that read.
5. On conflict, inspect fresh content and redo the decision, then rebuild edits.
   Permit one fresh attempt; another conflict ends the operation with a report.
6. Read details again to verify content and project scope.

A tag-only legacy edit guards the body, not all metadata or project-label races.
Keep it body-only; do not claim note-wide concurrency protection. Coordinate writers
and defer edits when project ownership or scope is concurrently changing.

## Complete updates and attachments

A complete update must preserve the full intended label set, title, content, and
unrelated scopes. With a revision-aware tool, read fresh details and send that
revision. If it lacks a full-update guard, use a supported body edit instead or
return a proposed title/label change; do not force a full overwrite.

Do not put attachments into current source-profile save/update requests. Separate
attachment tools, when exposed, handle bytes; note details contain metadata. Do not
pass metadata as content or an empty attachment array to an older full-update tool
without confirming its replacement semantics. Routine knowledge capture needs no
attachments. An unavailable attachment operation is a partial capability, not a
reason to lose existing data or claim an upload succeeded.

## Ambiguous results and multi-note changes

Before retrying any uncertain write, read the target when its ID is known. After an
uncertain create with no ID, inspect recent notes in the same project and fetch
plausible matches to compare scope/content. An inconclusive check is not proof that
creation failed. Stop with an unknown outcome instead of blindly creating again.

Use direct reads to verify writes: the README describes queued body embeddings, so
immediate semantic visibility is not a reliable save acknowledgement. Search plus
save also does not provide a cross-agent uniqueness transaction.

For approved consolidation, save/update and verify the retained note first. Then
mark each replaced note with the retained ID, reason, and applicable scope. Never
claim these calls are atomic. Keep a list of completed and outstanding IDs; do not
delete either side to compensate for a timeout or partial failure.

For an explicitly authorized deletion, re-read the exact target and use every guard
the live tool accepts. Distinguish `deleted`, `already absent`, and unknown outcome.
Legacy ID-only deletion has no advertised revision protection; do not claim otherwise.
Bulk relabeling and migration remain separate explicit operations, not curation shortcuts.

## Sources

- [Transport schemas and contract tests](https://github.com/gsmlg-opt/agent-note/blob/main/crates/note-mcp/src/stdio.rs)
- [Tool wrappers and data shapes](https://github.com/gsmlg-opt/agent-note/blob/main/crates/note-mcp/src/tools.rs)
- [Repository architecture and indexing pipeline](https://github.com/gsmlg-opt/agent-note/blob/main/README.md)
- Authoring-session tool schemas: `Agent_Note`, observed 2026-09-08. These are not a
  repository release or a guarantee about another client's connection.
