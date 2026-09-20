# Agent Note — shared project knowledge skill

Version 1.2.0 is the repository-owned Agent Note instruction package. It supports
setup, recall, qualified capture, and authorized curation of project-scoped knowledge.
It is instruction-only: it neither packages/installs itself nor changes the MCP server,
even though this repository contains the server contract it teaches.

## Package contents

- `SKILL.md`: operational workflow, scope/authority rules, and mutation guidance.
- `agents/openai.yaml`: optional Codex display, invocation, and MCP alias metadata.
- `references/project-setup.md`: scoped, idempotent AGENTS.md setup.
- `references/mcp-contract.md`: current HTTP MCP replacement/patch contract.
- `references/examples.md`: capture, curation, patch, replacement, and conflict examples.
- `assets/`: optional note template and project entry-point snippet.
- `evals/cases.md`: 42 expected-behavior fixtures, not executed results.
- `SOURCES.md` and `VALIDATION.md`: local review basis and recorded checks.

Keep the source package under `skills/agent-note/`. Do not edit an installed copy as
the source of truth. Installation into a client discovery root is a separate, deliberate
operation and must preserve its local connection configuration.

## Use in a project

Ask an agent with this skill to configure an exact project declaration in the applicable
`AGENTS.md`, or use it for read-only recall, a qualified capture, or authorized curation.
Setup is a repository edit, not an MCP mutation. The skill never guesses the project
from a directory, remote, or note; it preserves an existing explicit-write-only policy.

```text
Use the agent-note skill to set up Agent Note in this project's AGENTS.md.
Agent Note project: synapsis
Update the existing Agent Note section in place, preserve unrelated instructions,
and show the diff. Do not create or modify notes, or commit/push these changes.
```

For normal note work, the client must inspect its live schema first. In the reviewed
current server, Note MCP is HTTP-only at `/mcp`; Org MCP is separately available at
`/org/mcp`. Use `patch_note` for focused revision-guarded changes and `replace_note`
only with the complete intended note state. See [the MCP contract](references/mcp-contract.md).

## Positioning and limits

The skill keeps setup/recall/capture/curate distinct and treats routine progress as a
valid skip. It requires exact scope verification, duplicate checks, explicit write
authority, direct readback, cautious conflict handling, and special authorization for
deletion, attachment removal, and bulk changes. It never stores secrets.

No model evaluation, client-discovery run, or live note mutation is implied by this
package. The behavioral fixtures are expected behavior only. See `VALIDATION.md` for
the checks actually run for this version and `SOURCES.md` for the local source review.
