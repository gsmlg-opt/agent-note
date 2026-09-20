# MCP Replace and Patch Note Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the MCP-only `update_note` and line/tag-based `edit_note` tools with atomic `replace_note` and `patch_note` tools, then update the repository-owned Agent Note skill to teach that contract.

**Architecture:** Add strict contextual content patching to `note-core`, compose full desired note state and no-op detection in `note-pipelines`, and keep MCP request decoding and structured tool-result mapping in `note-mcp`. Reuse the current storage transaction, generated-attachment publication/cleanup, label validation, embedding synchronization, and notifier boundaries; keep the REST/frontend line-edit pipeline intact. The Note endpoint remains twelve tools, the Org endpoint remains forty tools, and transport remains HTTP-only MCP 2026-07-28.

**Tech Stack:** Rust 2021, Tokio, Serde, Schemars, rmcp 3.4, `agcodex-apply-patch` 0.1, repository storage/attachment traits, Markdown skill files, PyYAML skill validation.

---

The approved contract is [the design specification](../specs/2026-09-20-mcp-replace-patch-design.md). Use `/tmp/ant-design/agent-note` only as a behavioral reference. Adapt its patch parser and mutation semantics to this repository's storage traits and external attachment store; do not copy its SQLite/filesystem attachment assumptions.

## Task 1: Add strict contextual content patching to `note-core`

**Files:**

- Create: `crates/note-core/src/content_patch.rs`
- Create: `crates/note-core/tests/content_patch_test.rs`
- Modify: `crates/note-core/src/lib.rs`
- Modify: `crates/note-core/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Write the failing parser and byte-preservation tests**

Create `crates/note-core/tests/content_patch_test.rs` with focused cases for:

- a replacement string deserializing as `ContentUpdate::Replace`;
- `{ "apply_patch": "..." }` deserializing as `ContentUpdate::ApplyPatch`;
- one hunk and multiple ordered, non-overlapping hunks;
- Unicode, CRLF, mixed untouched line endings, a final newline, and an unterminated final line;
- deletion of an unterminated final line without adding a newline to its predecessor; and
- rejection of empty hunks, unanchored insertions, missing or ambiguous context, fuzzy whitespace, overlap/reversal, file directives, patch envelopes, line-number headers, and EOF markers.

Use exact output assertions, including this regression:

```rust
#[test]
fn deleting_unterminated_eof_does_not_add_a_newline() {
    assert_eq!(apply_content_patch("head\nold", "@@\n-old\n").unwrap(), "head");
    assert_eq!(
        apply_content_patch("head\r\nold", "@@\n-old\n").unwrap(),
        "head"
    );
}
```

- [ ] **Step 2: Run the focused test and confirm the missing API failure**

Run:

```bash
cargo test -p note-core --test content_patch_test
```

Expected: compilation fails because `note_core::ContentUpdate` and `note_core::apply_content_patch` do not exist.

- [ ] **Step 3: Implement the strict parser**

Add `agcodex-apply-patch = "0.1.0"` to `crates/note-core/Cargo.toml`, export a new `content_patch` module from `lib.rs`, and implement:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ContentUpdate {
    Replace(String),
    ApplyPatch(ContentPatch),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPatch {
    pub apply_patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentPatchError {
    pub message: String,
}

pub fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>;

pub fn apply_content_patch(content: &str, patch: &str) -> Result<String, ContentPatchError>;
```

The implementation must:

- require one or more plain `@@` hunks;
- allow only space, `+`, and `-` body prefixes;
- use `agcodex_apply_patch::parse_patch` only for parsing an internally constructed envelope;
- match every hunk's old lines exactly once against the original content;
- reject empty old-line anchors, overlap, and reversed hunk order;
- render replacements using the matched region's newline style;
- copy untouched slices byte-for-byte; and
- explicitly remove the separator left behind when deletion consumes an unterminated final line.

Do not expose filesystem patch execution or fuzzy matching.

- [ ] **Step 4: Run the focused core tests**

Run:

```bash
cargo test -p note-core --test content_patch_test --locked
```

Expected: all content patch tests pass and `Cargo.lock` contains `agcodex-apply-patch`.

- [ ] **Step 5: Commit the core slice**

```bash
git add crates/note-core/src/content_patch.rs crates/note-core/tests/content_patch_test.rs crates/note-core/src/lib.rs crates/note-core/Cargo.toml Cargo.lock
git commit -m "feat(core): add strict contextual note patches"
```

## Task 2: Add atomic replacement and patch pipelines

**Files:**

- Create: `crates/note-pipelines/src/replace_patch_note.rs`
- Create: `crates/note-pipelines/tests/note_mutations_test.rs`
- Modify: `crates/note-pipelines/src/lib.rs`
- Modify: `crates/note-pipelines/src/update_note.rs`
- Reuse without semantic changes: `crates/note-pipelines/src/generated_attachments.rs`

- [ ] **Step 1: Write failing pipeline contract tests**

Create `crates/note-pipelines/tests/note_mutations_test.rs`. Build fixtures with the existing Turso test backend and filesystem attachment store. Cover:

- `replace_note` requires and replaces title, content, complete attachments, and complete labels with exactly one revision increment;
- `patch_note` changes each field independently and in combination;
- omitted patch fields are preserved, while explicit empty label and attachment vectors clear them;
- content accepts both a full replacement and a strict contextual patch;
- labels compare independent of ordering;
- logically identical attachments compare equal even though persisted attachments have `storage` metadata and request attachments do not;
- semantic no-ops preserve `revision`, `updated_at`, labels, attachment object keys, search chunks, and embedding/notifier event counts;
- stale revisions fail even when the requested state is otherwise a no-op;
- nonpositive revisions and an empty `PatchNoteInput` fail before any write;
- a missing/inactive note returns the typed `NoteMutationError::NotFound`;
- a failed later content hunk does not save an otherwise valid title, labels, or attachments;
- invalid labels/attachments and attachment publication failures leave the committed note and label catalog unchanged;
- a storage failure after object publication cleans up safe unreferenced objects and retains the last committed generation; and
- two concurrent requests using one revision produce exactly one changed result and one revision conflict.

Assert no-op notifier behavior with a counting implementation of `NoteMutationNotifier`; assert a changed commit notifies once and wakes embedding work only when work was queued.

- [ ] **Step 2: Run the new pipeline test and confirm the missing API failure**

Run:

```bash
cargo test -p note-pipelines --test note_mutations_test --locked
```

Expected: compilation fails because `PatchNoteInput`, `replace_note`, and `patch_note` do not exist.

- [ ] **Step 3: Define the transport-neutral mutation contract**

In `replace_patch_note.rs`, define and export:

```rust
#[derive(Debug, Default)]
pub struct PatchNoteInput {
    pub title: Option<String>,
    pub content: Option<note_core::ContentUpdate>,
    pub attachments: Option<Vec<note_core::NoteAttachment>>,
    pub labels: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteMutation {
    pub id: String,
    pub revision: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacePatchError {
    InvalidRequest(String),
    PatchFailed(String),
}
```

Expose:

```rust
pub async fn replace_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    input: SaveNoteInput,
) -> anyhow::Result<NoteMutation>;

pub async fn patch_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    input: PatchNoteInput,
) -> anyhow::Result<NoteMutation>;
```

Keep the existing `NoteMutationError` type unchanged for REST/frontend callers. New MCP error mapping will translate its `StaleRevision` variant to the new public code `revision_conflict` only for these two tools.

- [ ] **Step 4: Extract a crate-private commit primitive without changing old public behavior**

Refactor `update_note.rs` so its current transaction body is callable from the new module as a crate-private operation that accepts a fully computed title/content/labels plus `Option<Vec<NoteAttachment>>`:

- `None` means preserve the committed attachment references without reading blob bytes;
- `Some` means publish and atomically replace the complete attachment set;
- it retains validation, label-key insertion, typed label checks, chunk synchronization, generated-object cleanup, old-object cleanup enqueueing, one CAS revision increment, and post-commit embedding wakeup; and
- `update_note` and `update_note_fields` continue to call it with their current semantics, so REST/frontend behavior does not change.

Do not implement the new operations by calling MCP wrappers or by chaining standalone attachment mutations.

- [ ] **Step 5: Implement preflight, no-op, and race-safe commit behavior**

In `replace_patch_note.rs`:

1. Reject `expected_revision < 1`; reject a patch with all four optional fields omitted.
2. Read the active note and reject a stale revision before patching, blob publication, or embedding work.
3. Apply a `ContentUpdate::ApplyPatch` to the existing body and map `ContentPatchError` to `ReplacePatchError::PatchFailed`.
4. Resolve omitted fields from the existing note and validate the complete desired `NoteInput` plus typed labels.
5. Hydrate existing attachment bytes only when the request supplies `attachments`.
6. Compare attachment payloads by `id`, normalized path, MIME, description, and bytes; ignore `AttachmentStorageMetadata` when deciding whether content changed.
7. Compare labels as sorted `(key, value)` pairs.
8. For a semantic no-op, open an immediate transaction and re-read the note revision before returning `changed: false`; this prevents a stale request from succeeding during the preflight/return race. Commit no writes and send no notifications.
9. For a real change, pass `Some(attachments)` to the commit primitive only when the logical attachment set changed. A supplied-but-identical set must preserve existing object keys.
10. Let the transaction CAS catch any race after preflight. Return exactly one new revision, then call `ctx.notify_note_mutated()` once after the changed commit.

Use the existing generated-attachment cleanup policy. A known rollback cleans safe new objects; an unknown commit outcome retains them for recovery instead of risking deletion of committed data.

- [ ] **Step 6: Run focused pipeline regressions**

Run:

```bash
cargo test -p note-pipelines --test note_mutations_test --locked
cargo test -p note-pipelines --test save_note_test --locked
cargo test -p note-pipelines --test attachment_management_test --locked
```

Expected: new mutation tests pass, and existing update/line-edit/attachment behavior remains green.

- [ ] **Step 7: Commit the pipeline slice**

```bash
git add crates/note-pipelines/src/replace_patch_note.rs crates/note-pipelines/tests/note_mutations_test.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/src/update_note.rs
git commit -m "feat(pipelines): add atomic replace and patch mutations"
```

## Task 3: Replace the transport-independent MCP wrappers

**Files:**

- Modify: `crates/note-mcp/src/tools.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`

- [ ] **Step 1: Replace wrapper tests before wrapper code**

Update imports and old `update_note_tool`/`edit_note_tool` cases in `tools_test.rs` so they exercise:

- `ReplaceNoteToolInput` with all writable fields;
- `PatchNoteToolInput` with omitted preservation, string content, contextual content, and empty-vector clearing;
- `NoteMutationData { id, revision, changed }` only;
- attachment bytes flowing into the pipeline for text and binary input conversions supplied by the server layer;
- stale revision, no-op, validation, and patch failure propagation without wrapper-side remapping; and
- `read_note_lines_tool` remaining a read-only inspection helper.

Keep request deserialization and closed-schema assertions in the server tests, where the MCP wire DTOs live. Do not retain tests that present removed MCP wrappers as supported behavior.

- [ ] **Step 2: Run the focused wrapper test and confirm the missing types/functions**

Run:

```bash
cargo test -p note-mcp --test tools_test --locked
```

Expected: compilation fails on missing `ReplaceNoteToolInput`, `PatchNoteToolInput`, `replace_note_tool`, and `patch_note_tool`.

- [ ] **Step 3: Implement thin wrappers**

Remove the public MCP wrappers and input DTO for `edit_note_tool` and `update_note_tool`. Add:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceNoteToolInput {
    pub id: String,
    pub expected_revision: i64,
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub labels: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct PatchNoteToolInput {
    pub id: String,
    pub expected_revision: i64,
    pub patch: note_pipelines::PatchNoteInput,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NoteMutationData {
    pub id: String,
    pub revision: i64,
    pub changed: bool,
}
```

`replace_note_tool` and `patch_note_tool` should convert inputs once, call their matching pipeline functions once, and map `NoteMutation` to `NoteMutationData`. They must not read the note, call the standalone attachment tools, or implement retries.

- [ ] **Step 4: Run the focused wrapper tests**

Run:

```bash
cargo test -p note-mcp --test tools_test --locked
```

Expected: all wrapper tests pass.

- [ ] **Step 5: Commit the wrapper slice**

```bash
git add crates/note-mcp/src/tools.rs crates/note-mcp/tests/tools_test.rs
git commit -m "feat(mcp): add replace and patch tool wrappers"
```

## Task 4: Expose the new schemas and structured MCP results over HTTP

**Files:**

- Modify: `crates/note-mcp/src/server.rs`
- Modify: `crates/note-mcp/src/org.rs`
- Modify: `crates/note-mcp/tests/org_inventory_test.rs`
- Modify: `crates/note-mcp/tests/org_transports_test.rs`

- [ ] **Step 1: Change inventory and server contract tests first**

Update tests to require the exact Note inventory:

```rust
[
    "bulk_update_note_labels",
    "delete_note",
    "delete_note_attachment",
    "get_note",
    "get_note_attachment_content",
    "list_notes",
    "patch_note",
    "put_note_attachment",
    "read_note_lines",
    "replace_note",
    "save_note",
    "semantic_search",
]
```

Keep the Org count at 40 and aggregate uniqueness/count behavior at 52. Explicitly assert `update_note` and `edit_note` are absent.

In `server.rs` unit tests, replace old schema and handler cases with assertions that:

- `replace_note` is a closed object and requires `id`, positive `expected_revision`, `title`, `content`, `attachments`, and `labels`;
- `patch_note` is closed, requires only `id` and `expected_revision` at the JSON Schema level, advertises optional non-null writable values, and rejects an all-omitted patch operationally;
- every writable patch field rejects explicit `null` during deserialization;
- `content` accepts only a string or a closed `{ "apply_patch": string }` object;
- attachment entries require `id`, `path`, `mime`, and exactly one of `content`/`content_base64`, with optional `description`;
- success output schema has exactly `id`, `revision`, and `changed`; and
- malformed arguments are JSON-RPC `INVALID_PARAMS`, while well-formed operational failures are `CallToolResult` values with `isError: true` and structured `code`, `message`, `details`, and `retryable`.

Add handler behavior cases for success, no-op, not found, revision conflict, patch failure, validation error, and sanitized internal attachment/storage failure.

In `org_transports_test.rs`, call both new tools through `/mcp` with MCP version `2026-07-28`; assert `structuredContent`, `isError`, and the 2026 `resultType`/cache discriminator behavior already enforced by the harness. Retain existing split-route rejection, JSON-only POST, Origin, and `/org/mcp` checks.

- [ ] **Step 2: Run the focused tests and confirm inventory/schema failures**

Run:

```bash
cargo test -p note-mcp --lib --locked
cargo test -p note-mcp --test org_inventory_test --locked
cargo test -p note-mcp --test org_transports_test --locked
```

Expected: tests fail because the router still advertises `edit_note`/`update_note` and does not expose the new schemas or result mapping.

- [ ] **Step 3: Add reusable attachment and mutation request schemas**

In `server.rs`, extract a closed `AttachmentRequestSchema` for full-set mutation entries. Its conversion must call `note_core::decode_attachment_content`, reject both/neither content representations, and construct a `NoteAttachment` with `storage: None`.

Add `ReplaceNoteRequest`, untagged `ContentUpdateSchema`, closed `ContentPatchSchema`, `PatchNoteRequest`, and closed `NoteMutationResponse`. Use `#[schemars(range(min = 1))]` for revisions. For each optional patch field use `deserialize_present` plus a schema transform that removes Schemars' default/nullable representation:

```rust
#[serde(default, deserialize_with = "note_core::deserialize_present")]
#[schemars(with = "String", transform = remove_schema_default)]
pub title: Option<String>,
```

Repeat the same pattern with the correct advertised type for `content`, `attachments`, and `labels`. This preserves omission while making explicit `null` invalid.

- [ ] **Step 4: Replace handlers and add custom argument dispatch**

Remove `EditOpSchema`, `EditNoteRequest`, `UpdateNoteRequest`, and their handlers from the MCP server only. Keep `NoteLinesResponse` and update its documentation so its revision/tag are informational rather than `edit_note` write tokens.

Register:

- `replace_note`: full replacement, all writable fields required;
- `patch_note`: omitted fields preserved, provided vectors replace, contextual diff supported, no-op keeps revision.

Both handlers return `CallToolResult`, with the output schema explicitly declared as `NoteMutationResponse`.

Override `ServerHandler::call_tool` for only these two names. Deserialize their raw argument objects with `serde_json::from_value` and return `ErrorData::invalid_params` on malformed JSON/schema input; delegate all other names to the existing `ToolRouter`. This keeps malformed arguments as JSON-RPC errors instead of unstructured tool errors.

- [ ] **Step 5: Implement stable operational result mapping**

Add one helper that converts `anyhow::Result<NoteMutationData>` into either structured success or `CallToolResult::structured_error`. Every error body must have all four keys:

```json
{
  "code": "revision_conflict",
  "message": "the note changed after it was read",
  "details": {
    "note_id": "note-1",
    "expected_revision": 5,
    "current_revision": 6
  },
  "retryable": false
}
```

Map:

- `NoteMutationError::NotFound` -> `not_found`;
- `NoteMutationError::StaleRevision` -> `revision_conflict`;
- `ReplacePatchError::PatchFailed` -> `patch_failed`;
- `ReplacePatchError::InvalidRequest` -> `invalid_request`;
- note/label/attachment validation types -> `validation_error`; and
- all remaining failures -> `internal_error` with a stable caller-safe message and no storage/object details.

Use an empty JSON object for `details` when a typed error has no structured fields. Keep `retryable: false`; conflict recovery requires a fresh read and semantic recomputation, not an automatic replay.

- [ ] **Step 6: Update endpoint instructions and exact inventory**

Replace the two names in `NOTE_TOOL_NAMES`, preserving its sorted order. Update Note-only and aggregate server instructions to tell clients to read `get_note.revision`, use `patch_note` for partial changes, use `replace_note` only with every writable field, and re-read after conflicts. Do not alter Org-only instructions.

- [ ] **Step 7: Run all focused MCP tests**

Run:

```bash
cargo test -p note-mcp --lib --locked
cargo test -p note-mcp --test tools_test --locked
cargo test -p note-mcp --test org_inventory_test --locked
cargo test -p note-mcp --test org_transports_test --locked
```

Expected: all MCP unit, wrapper, inventory, and HTTP 2026 tests pass; `/mcp` has 12 tools, `/org/mcp` has 40, and removed names are rejected cross-contract.

- [ ] **Step 8: Commit the server slice**

```bash
git add crates/note-mcp/src/server.rs crates/note-mcp/src/org.rs crates/note-mcp/tests/org_inventory_test.rs crates/note-mcp/tests/org_transports_test.rs
git commit -m "feat(mcp): expose replace and patch note contracts"
```

## Task 5: Update repository documentation

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Rewrite the public MCP mutation descriptions**

In both files:

- list the exact twelve Note tools with `replace_note` and `patch_note`;
- remove normative MCP references to `update_note` and `edit_note`;
- describe `get_note.revision` as the mutation guard;
- state that `read_note_lines` is optional inspection and its tag is not a write precondition;
- distinguish required full-set replacement from omitted-field patch preservation;
- describe string replacement versus strict contextual `apply_patch` content;
- document explicit empty-vector clearing, semantic no-op behavior, and structured errors;
- explain that full attachment sets carry bytes via `content` or `content_base64`, while `get_note` returns metadata and `get_note_attachment_content` retrieves bytes; and
- state that the standalone attachment tools and REST/frontend line-edit behavior remain separate and unchanged.

- [ ] **Step 2: Scan documentation for stale normative names**

Run:

```bash
rg -n 'edit_note|update_note' README.md docs/design.md
```

Expected: no active MCP guidance names either removed tool. A mention is allowed only when explicitly saying the MCP tool was removed or distinguishing an internal REST/frontend API.

- [ ] **Step 3: Commit the documentation slice**

```bash
git add README.md docs/design.md
git commit -m "docs(mcp): document replace and patch mutations"
```

## Task 6: Update the repository-owned Agent Note skill

**Files:**

- Modify: `skills/agent-note/SKILL.md`
- Modify: `skills/agent-note/references/mcp-contract.md`
- Modify: `skills/agent-note/references/examples.md`
- Modify: `skills/agent-note/evals/cases.md`
- Modify: `skills/agent-note/README.md`
- Modify: `skills/agent-note/SOURCES.md`
- Modify: `skills/agent-note/VALIDATION.md`
- Inspect, normally unchanged: `skills/agent-note/agents/openai.yaml`
- Do not modify: `/home/gao/.agents/skills/agent-note`

- [ ] **Step 1: Update metadata and operational guidance**

Bump the source package version from `1.1.0` to `1.2.0` in `SKILL.md` and other version-bearing package documents.

Preserve project resolution, project-label checks, mutation authorization, duplicate detection, label safeguards, one-retry conflict policy, direct readback verification, and curation/destructive-operation boundaries.

Replace the write workflow with:

1. inspect live tool schemas;
2. read `get_note` and verify scope/project labels;
3. use `patch_note` by default for a focused title/body/label/attachment-set change;
4. use a full content string or a uniquely anchored contextual `apply_patch` hunk;
5. use `replace_note` only when deliberately providing title, content, the full attachment set, and the full label set;
6. fetch every attachment's bytes with `get_note_attachment_content` before carrying it through a full replacement because `get_note` exposes metadata only;
7. on conflict, re-read and recompute once; never swap only the revision token; and
8. verify the resulting note and project label with `get_note`.

State that `read_note_lines` is optional inspection, not a mutation prerequisite.

- [ ] **Step 2: Rewrite the contract reference against this checkout**

Replace the obsolete source-vs-authoring matrix and line-edit recipe in `references/mcp-contract.md` with the implemented HTTP-only contract. Cite repository paths `crates/note-mcp/src/server.rs`, `crates/note-mcp/src/http.rs`, `crates/note-mcp/src/tools.rs`, and the relevant README section. Document omit-versus-empty-versus-null semantics, strict patch constraints, attachment byte preservation, no-op behavior, structured error codes, conflict handling, and runtime-schema-first compatibility.

Keep fallback advice only for an actually older deployment discovered through live schema inspection. Label `edit_note`/`update_note` historical and superseded; never recommend them as the reviewed baseline.

- [ ] **Step 3: Refresh examples and evaluation cases**

In `references/examples.md`, replace the stale fresh-tag/unguarded-update example with examples for:

- focused content patch preserving labels and attachments by omission;
- full replacement that first retrieves and carries forward attachment bytes;
- label clearing with `labels: []`; and
- conflict re-read and semantic recomputation.

In `evals/cases.md`, replace legacy E15/E16/E18 expectations with current cases for explicit-null rejection, empty patch rejection, omit-versus-empty collection behavior, attachment preservation, and refusal to issue a full replacement without attachment bytes. Keep case IDs unique and sequential and do not claim they were executed.

- [ ] **Step 4: Refresh package provenance and usage docs**

Update `README.md`, `SOURCES.md`, and `VALIDATION.md` so they describe version 1.2.0 and the current local checkout. Remove stale claims that a local clone was unavailable, that the reviewed source was `stdio.rs`, or that an eight-tool tag-only connection is the current baseline. Record the approved design, implementation paths, and exact validation commands actually run.

Leave `agents/openai.yaml` unchanged unless validation finds a mutation-specific statement; its `agent_note` dependency alias and live-schema inspection prompt remain correct.

- [ ] **Step 5: Run static skill validation**

Run:

```bash
uv run --no-project --with pyyaml python /home/gao/.codex/skills/.system/skill-creator/scripts/quick_validate.py skills/agent-note
```

Expected: `Skill is valid!`

Run a package-specific integrity check:

```bash
python - <<'PY'
from pathlib import Path
import re
import yaml

root = Path("skills/agent-note")
yaml.safe_load((root / "agents/openai.yaml").read_text())
skill = (root / "SKILL.md").read_text()
frontmatter = skill.split("---", 2)[1]
assert yaml.safe_load(frontmatter)["metadata"]["version"] == "1.2.0"

for path in root.rglob("*.md"):
    text = path.read_text(encoding="utf-8")
    assert "\r" not in text, path
    assert text.endswith("\n"), path
    assert all(line == line.rstrip() for line in text.splitlines()), path
    for target in re.findall(r"\[[^]]+\]\(([^)]+)\)", text):
        if "://" not in target and not target.startswith("#"):
            assert (path.parent / target.split("#", 1)[0]).resolve().exists(), (path, target)

cases = (root / "evals/cases.md").read_text()
ids = [int(value) for value in re.findall(r"\| E(\d{2}) \|", cases)]
assert ids == list(range(1, len(ids) + 1)), ids
print("agent-note skill integrity: ok")
PY
```

Expected: `agent-note skill integrity: ok`.

- [ ] **Step 6: Scan for stale active mutation guidance**

Run:

```bash
rg -n 'edit_note|update_note|tag-only|line-number' skills/agent-note
```

Expected: matches, if any, are explicitly historical/superseded statements or negative evaluation fixtures. No active instruction recommends either removed tool, line-number mutations, or a tag as a write guard.

- [ ] **Step 7: Record only checks that actually passed and commit the skill slice**

Update `VALIDATION.md` after the commands finish, distinguishing static validation from unexecuted model/live-connection evaluation.

```bash
git add skills/agent-note
git commit -m "docs(skill): teach replace and patch note mutations"
```

## Task 7: Run the scoped completion gate

**Files:**

- Verify only; modify affected files solely for failures caused by this change.

- [ ] **Step 1: Format and rerun every affected test target**

Run:

```bash
cargo fmt --all -- --check
cargo test -p note-core --test content_patch_test --locked
cargo test -p note-pipelines --test note_mutations_test --locked
cargo test -p note-pipelines --test save_note_test --locked
cargo test -p note-pipelines --test attachment_management_test --locked
cargo test -p note-mcp --lib --locked
cargo test -p note-mcp --test tools_test --locked
cargo test -p note-mcp --test org_inventory_test --locked
cargo test -p note-mcp --test org_transports_test --locked
uv run --no-project --with pyyaml python /home/gao/.codex/skills/.system/skill-creator/scripts/quick_validate.py skills/agent-note
```

Expected: every scoped check passes. Do not expand to workspace/frontend/Org suites unless a scoped failure demonstrates that this change crossed that boundary. Report unrelated failures and stop rather than repairing out-of-scope code.

- [ ] **Step 2: Verify inventories, removed names, and untouched boundaries**

Run:

```bash
rg -n 'name = "(replace_note|patch_note|update_note|edit_note)"' crates/note-mcp/src
rg -n 'edit_note|update_note' README.md docs/design.md skills/agent-note
git diff --check
git status --short
```

Expected:

- MCP registrations exist only for `replace_note` and `patch_note`;
- any old-name documentation match is clearly historical, superseded, or internal REST/frontend discussion;
- `git diff --check` is clean; and
- the unrelated pre-existing `docs/agent-note-export-implementation-plan.md` remains untracked and untouched.

- [ ] **Step 3: Inspect the task diff and commit any verification-only correction**

Review from the approved design commit:

```bash
git diff --stat c5c8c4446e7958368b5ffed5e5374670a2489c12..HEAD
git diff c5c8c4446e7958368b5ffed5e5374670a2489c12..HEAD -- crates/note-core crates/note-pipelines crates/note-mcp README.md docs/design.md skills/agent-note
```

Every changed line must map to this contract. If verification required a source correction, commit only that correction with an appropriately scoped Conventional Commit. If no correction was needed, do not create an empty commit.

## Final acceptance checklist

- [ ] `/mcp` advertises exactly 12 tools, including `replace_note` and `patch_note`, excluding `update_note` and `edit_note`.
- [ ] `/org/mcp` still advertises exactly 40 Org tools and rejects Note tools with the existing JSON-RPC behavior.
- [ ] `replace_note` requires every writable field; `patch_note` preserves omitted fields and rejects explicit null/empty requests.
- [ ] strict contextual patches preserve bytes/newline state and reject ambiguous, fuzzy, overlapping, unanchored, or file-level patches.
- [ ] changed mutations increment once; semantic no-ops preserve revision, timestamp, blobs, labels, search data, embedding work, and notifications.
- [ ] malformed arguments are JSON-RPC invalid params; operational failures are structured MCP tool errors with stable four-field bodies.
- [ ] attachments use the current store/recovery model and never replace the last committed generation on failure.
- [ ] REST/frontend line editing, standalone attachment tools, bulk labels, HTTP-only transport, and MCP 2026 behavior remain unchanged.
- [ ] `skills/agent-note` version 1.2.0 consistently teaches the new revision-guarded mutation workflow and passes static validation.
- [ ] only focused affected tests were run; unrelated work remains untouched.

