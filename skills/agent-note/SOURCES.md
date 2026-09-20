# Review basis and provenance

Prepared 2026-09-21 in the local `gsmlg-opt/agent-note` feature checkout on branch
`codex/mcp-replace-patch`, reviewed against base
`39106fe18d0fd79922c6ba6f57f6d26ddbbfaca9`. Version 1.2.0 updates the repository-owned
skill to the approved replace/patch MCP contract. It is documentation/instruction work;
it does not claim a live deployment schema or model behavior.

## Current local implementation reviewed

1. `crates/note-mcp/src/http.rs` defines the HTTP-only Streamable HTTP router: Note
   tools at `/mcp`, Org tools at `/org/mcp`, stateless JSON behavior, and origin policy.
2. `crates/note-mcp/src/server.rs` defines the exact 12-tool Note inventory, runtime
   schemas, `replace_note`/`patch_note` request rules, structured mutation error mapping,
   and server instructions.
3. `crates/note-mcp/src/tools.rs` defines detail metadata, attachment-content retrieval,
   and transport-independent replace/patch wrappers.
4. `crates/note-mcp/src/org.rs` and `crates/note-mcp/tests/org_inventory_test.rs` confirm
   the separated Note/Org inventories and the Note tool names.
5. `README.md` ("MCP servers") and `docs/design.md` document the implemented public
   endpoint, inventory, replacement, patch, attachment, and error behavior.
6. `docs/superpowers/specs/2026-09-20-mcp-replace-patch-design.md` and
   `docs/superpowers/plans/2026-09-20-mcp-replace-patch.md` are the approved design and
   implementation plan for the contract taught here.
7. `skills/agent-note/agents/openai.yaml` was inspected: its `agent_note` dependency
   alias and live-schema prompt remain correct, so it is intentionally unchanged.

## Contract conclusions reflected in the package

- `get_note.revision` is the mutation guard. `patch_note` preserves omitted fields,
  replaces supplied arrays, rejects explicit null, and rejects a wholly empty patch.
- Content is either a full string or strict contextual `apply_patch`; line-oriented
  tags/tokens and fuzzy/file-based patches are not part of the current mutation API.
- `replace_note` supplies all writable fields. Attachment metadata from `get_note` is
  insufficient to retain bytes; each retained attachment needs a content retrieval and
  exactly one content representation in the replacement request.
- Mutations are atomic, semantic no-ops retain revision, and structured errors include
  conflict, invalid-request, patch, validation, not-found, and internal categories.

Historical `edit_note`, `update_note`, and `stdio.rs` references describe superseded
interfaces only and are not a current baseline. No public GitHub-only review, unavailable
local clone claim, or inherited unreviewed-contract claim is used for this version.

## External packaging format

- [Agent Skills specification](https://agentskills.io/specification) — package frontmatter
  and relative-resource conventions.
- [OpenAI Codex skills documentation](https://developers.openai.com/codex/build-skills) —
  local discovery and optional `agents/openai.yaml` metadata.

These packaging references do not validate the repository's runtime contract. The local
source/design review does not replace inspecting the connected server schema before use.
