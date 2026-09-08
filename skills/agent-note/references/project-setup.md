# Project setup: configure AGENTS.md

Use this workflow only when the user asks to initialize/configure Agent Note for a
project, or to add/update the Agent Note section of an applicable `AGENTS.md`.
A question about whether setup is supported is read-only; an explicit instruction
to apply it authorizes only the scoped configuration edit described below.
`setup` is a natural-language workflow name, not a new MCP tool or executable command.

## Inputs and scope

Resolve the target repository/worktree and intended instruction scope from the user
and the established coding workspace. An explicit target path wins. Otherwise use
that workspace's repository-root `AGENTS.md` for a repository-wide setup. Do not guess
an unrelated checkout, edit every discovered repository, or target a user's global
instructions. For a subproject, use the explicitly requested instruction scope.

Read the applicable parent/target instructions and the target file before proposing
an edit. Account for known nested overrides affecting the requested scope; do not
rewrite those overrides unless included in the request. Preserve repository rules,
user edits, line endings, and unrelated sections.

Resolve the exact project label from an explicit user configuration request or an
unambiguous existing applicable declaration. Never infer it from the remote, folder,
package name, storage service, examples, or old notes. A one-task project override
is not permission to persistently change an existing declaration. A setup request
that explicitly supplies the intended new value may change that declaration; report
the before/after value and do not migrate existing notes. When the intended value
or target is unresolved, return the specific missing input and an unapplied template.
Do not insert a guessed value or leave a placeholder in the real instruction file.

## Apply the template, not the entire skill

Read [the canonical project entry point](../assets/AGENTS.snippet.md). Substitute
`<exact-project-label>` with the resolved value. Preserve the target document's
language and formatting; keep the declaration syntax and exact project value intact.
Keep the entry point short and reference the shared `agent-note` skill rather than
copying its quality gate, tool schema, or full workflow into every repository.

The template enables qualified capture at the stated work boundaries. If existing
policy is explicit-save-only or read-only, preserve that restriction unless the user
explicitly requests changing it. In that case use this policy sentence instead of
the template's automatic-capture permission:

> Recall project notes when relevant; create or modify notes only on my explicit request.

Never treat setup as permission to weaken other repository constraints, alter MCP
connections, install another skill copy, create label catalog entries, or grant bulk
curation/deletion permissions.

## Bounded, idempotent edit

- With a single clear Agent Note section, update that section in place. Preserve any
  still-applicable project-specific restrictions. Remove superseded instructions
  within that section, such as a requirement to save a note after every fix.
- With no section, append one appropriate `## Agent Note` section. If the selected
  file does not exist, an explicit setup request may create it with only this section;
  do not invent coding, build, or architecture guidelines.
- If a project declaration exists elsewhere, reuse/reconcile it within the authorized
  scope rather than appending a conflicting second declaration. Multiple ambiguous
  sections, malformed structure, or an unresolved nested override require a focused
  proposed diff, not a guessed rewrite or edits across the repository.
- If the effective configuration already matches, leave the file unchanged. Repeating
  setup with the same inputs must not add another section, declaration, or timestamp.
- Before applying, ensure the file has not changed since it was read. With guarded
  editing APIs use their preconditions; otherwise re-read and merge the bounded edit.
  Do not overwrite intervening work or silently force a remote commit.

Use the coding agent's authorized repository/file editor. Agent Note MCP manipulates
notes, not project files. Without edit access, return a patch or the exact replacement
section and do not claim it was applied. Do not commit, push, or open a PR solely
because setup was requested; follow the user's actual repository-write instructions.

## Verify and report

Re-read the edited file and inspect its diff. Verify the exact project value, one
intended section/declaration at the target scope, no unresolved placeholders, no
contradictory superseded capture rule, and no unrelated changes. Identify inherited
or nested restrictions that still affect the requested scope instead of claiming
all repository instructions were updated. For a no-op, report that it already matches.

Setup itself makes no Agent Note calls and must not save a note saying that setup
completed. Configuration can be prepared without a live MCP write. If the user also
requests a connectivity check, use a bounded read-only check under the resolved
project, report its outcome, and do not create a test note. A client may independently
require its declared MCP dependency before loading the skill; do not bypass it.

Report the target path, exact project value, preserved/selected capture policy, and
whether the change was applied, already present, or only proposed. Local configuration
is not proof that the skill was installed, discovered, or invoked by the user's agent,
or that the MCP connection works. State these separately when not verified.
