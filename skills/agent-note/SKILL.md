---
name: agent-note
description: Use the Agent Note MCP server to save, retrieve, search, edit, update, or delete durable notes and attachments. Trigger when the user asks to remember, record, capture, find, or maintain notes, or when repository work needs durable project decisions, findings, plans, progress, or handoff context. Require every saved note to carry the repository's configured `project` label.
---

# Agent Note

Use Agent Note as durable project memory. Prefer concise Markdown that records reusable facts,
decisions, rationale, outcomes, or next steps. Do not store credentials, secrets, or disposable
conversation detail.

Tool names below omit any server prefix. Use the available Agent Note MCP tool with the matching
name, such as `save_note` or `semantic_search`.

Read or search notes when durable project context is relevant. Create or modify a note when the
user asks to capture it or the active task explicitly includes maintaining durable project memory.

## Resolve the project first

Resolve the project before searching or writing notes:

1. Honor a project value explicitly supplied by the user for the current work.
2. In a Git repository, read the applicable `AGENTS.md` files and use the nearest clear Agent Note
   project declaration. Prefer this unambiguous form:

   ```markdown
   Agent Note project: agent-note
   ```

3. Do not infer the value from the directory name, remote URL, package name, or note content.
4. If the value is absent or ambiguous, stop before writing the note and ask the user to set the
   Agent Note project, preferably in the repository's `AGENTS.md`. Ask which exact value to use.

Use the resolved value exactly and consistently. Scope `list_notes` and `semantic_search` with
`project=<value>` unless the user explicitly requests a cross-project search.

## Create a note and its labels

Pass labels as `(key, value)` pairs represented by two-element arrays. Use the exact lowercase key
`project`; label keys are not normalized. Every call to `save_note` must include exactly one
`project` pair:

```json
{
  "title": "Decision: keep label writes atomic",
  "content": "## Decision\n\nUse an all-or-nothing label update.\n\n## Rationale\n\nAvoid partially classified notes.",
  "labels": [
    ["project", "agent-note"],
    ["type", "decision"]
  ]
}
```

Attaching a previously unseen key through `save_note` or `update_note` auto-creates that label key
as a text-valued key with an empty description. Keys must be non-empty and must not contain `&`,
`=`, `!`, `<`, `>`, `^`, `$`, or `~`. Include a key only once per note. Prefer stable, short
project values without `&` so they remain easy to use in selectors.

This creates or attaches a label on a note. Explicit catalog management—setting a label key's
description or value type, renaming it, or deleting it—is REST/UI-only, not an Agent Note MCP
operation.

After `save_note` returns an ID, use `get_note` when verification matters and confirm that the
stored labels contain `project=<resolved value>`.

## Retrieve before duplicating

- Use `list_notes` with `label: "project=<value>"` for recent project notes.
- Use `semantic_search` with the same label selector and an explicit result `limit` for topic-based
  retrieval.
- Use `get_note` when an ID is already known or full content is required; list and semantic results
  are summaries.
- Search for an existing note on the same durable topic before saving. Update the existing note
  when continuity is more useful than a second note.

Label selector terms joined by `&` are ANDed. Bare keys test presence. Supported operators are
`=`, `!=`, `>`, `>=`, `<`, `<=`, `^=`, `$=`, and `~=`.

## Modify notes safely

- Use `read_note_lines`, then `edit_note` with the returned content tag for targeted Markdown body
  changes. Line edits preserve labels.
- Before `update_note`, call `get_note`. `update_note` replaces the complete title, content, and
  label set, so carry forward every label that should remain and ensure exactly one required
  `project` label is present.
- Use attachment tools for attachment bytes; `get_note` returns attachment metadata only.
- Delete a note or attachment only when the user requested that destructive change and the exact
  target is known.

## Finish the operation

Report the note ID and the resolved project after a successful write. Distinguish a completed MCP
write from a proposed note when the tool was unavailable or the project value was not configured.
