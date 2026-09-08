# Agent Note — shared project knowledge skill

This is a proposed replacement for the existing `skills/agent-note/` package in
`gsmlg-opt/agent-note`. It keeps the skill name and project-declaration convention,
adds explicit setup/recall/capture/curate workflows, and separates durable knowledge from
routine task reporting. It is instruction-only: no runtime scripts or new server
features are required.

## Package contents

- `SKILL.md`: operational workflow, quality gate, scope, and safe-write rules.
- `agents/openai.yaml`: optional Codex presentation, invocation, and dependency metadata.
- `references/project-setup.md`: scoped, idempotent AGENTS.md setup and verification.
- `references/mcp-contract.md`: observed interface differences and safe adaptation.
- `references/examples.md`: positive, negative, and borderline examples.
- `assets/note-template.md`: optional evidence-first knowledge template.
- `assets/AGENTS.snippet.md`: small per-project entry point.
- `evals/cases.md`: 42 behavioral cases plus trigger checks; not executed results.
- `SOURCES.md`: review basis, limitations, and differences from the existing skill.
- `VALIDATION.md`: checks performed on the delivered files.

Only the main file is needed to discover the skill. Keep the supporting files beside
it; they are loaded when referenced, not pasted into every project's instructions.

## Use in the repository

Apply the package's contents under `skills/agent-note/`, reviewing the diff against
any concurrent or local changes. Leave `.agents/skills/deploy/` and unrelated source
code untouched. Keep one maintained source package rather than independently editing
copies in every consuming repository.

## Install for Codex locally

Place this complete `agent-note/` directory under one chosen discovery root:

- Shared across your projects: `$HOME/.agents/skills/agent-note/`.
- Repository-scoped: `<repository>/.agents/skills/agent-note/`.

Do not install the same skill in multiple scopes accidentally. Existing installations
should be merged/replaced deliberately, preserving local connection settings. Codex
also documents symlink support for local skill directories; a managed checkout can
serve as the single local source. Pin or review updates to that checkout as needed.

Other clients should receive the same directory through their own supported skill
loader. The format is portable; this delivery does not certify every client's loader
or implicit invocation behavior. This ZIP is a standalone skill, not a plugin package
and not a promise of one-click installation in ChatGPT web.

The optional `agents/openai.yaml` retains the repository's dependency alias
`agent_note`. Match that alias to your existing MCP connection when needed. It is not
a URL, project label, automatic connection configuration, or permission grant. The
skill must inspect tools exposed by the actual connection. No endpoint or secret is
bundled. Implicit invocation is enabled for workflow selection, not unrestricted writes.

## Configure each project

Version 1.1 adds an explicit Setup workflow. Ask a coding agent with this skill
available to read `references/project-setup.md` and `assets/AGENTS.snippet.md`, then
configure the current repository's `AGENTS.md` using an exact project value. Example:

```text
Use the agent-note skill to set up Agent Note in this project's AGENTS.md.
Agent Note project: synapsis
Update the existing Agent Note section in place, preserve unrelated instructions,
and show the diff. Do not create or modify notes, or commit/push these changes.
```

This is a natural-language request, not a new CLI/MCP command. It requires authorized
file editing for application; without that access the result is an unapplied patch.
Keep an existing read-only or explicit-save-only policy unless changing it is part
of the request. Repeating the same setup should leave the file unchanged.

The manual alternative remains:

Replace `<exact-project-label>` in `assets/AGENTS.snippet.md` and add that section to
the applicable `AGENTS.md`. For Synapsis the declaration is:

```markdown
Agent Note project: synapsis
```

For another project use its intended existing label, not a value inferred from its
repository name. Multiple repositories may intentionally share one project value.
Do not use `agent-note` for all projects merely because it is the storage service.

The supplied snippet authorizes qualified capture at the stated boundaries. For a
read-only policy, replace that permission with “Recall only; write only on my explicit
request.” A skill description alone does not guarantee invocation at task completion;
the project entry point provides the operational trigger, and a harness can invoke
it explicitly when stronger coordination is required.

## Example invocations

```text
$agent-note recall the decisions relevant to this task in the configured project.

$agent-note evaluate the validated findings from this work slice;
update existing knowledge where appropriate and skip routine progress.

$agent-note audit the notes in project synapsis and propose consolidation;
do not modify notes yet.
```

`setup`, `recall`, `capture`, and `curate` are natural-language workflow choices, not MCP
operations or a new command-line parser.

## Adoption sequence

Install the skill and project declarations first. Run read-only recall and audit
cases against real notes. Run mutation cases only against an isolated test project.
Then use it for normal scoped capture. Existing contradictory notes will not be
rewritten automatically; approve a separate curation task for them.

## Validation limits

The files were checked for YAML validity, required skill metadata, internal references,
UTF-8/LF text, evaluation-case integrity, and archive integrity. No live note was
created, edited, relabeled, or deleted. No Rust tests or model-behavior suite was run.
The source review used public GitHub pages because the connector returned FORBIDDEN;
a local clone was unavailable. See `SOURCES.md` and `VALIDATION.md` for the exact basis.

## Authoring references

- [Agent Skills specification](https://agentskills.io/specification)
- [OpenAI local skills and optional metadata](https://developers.openai.com/codex/build-skills)

Plugin packaging and distribution can be added later without changing this workflow.
