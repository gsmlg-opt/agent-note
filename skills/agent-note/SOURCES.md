# Review basis and provenance

Prepared 2026-09-08 for the request to inspect `gsmlg-opt/agent-note` and deliver a
cross-project note-writing skill. The authored workflow is a proposal, not an assertion
that the server already enforces knowledge quality or automatically invokes skills.

## Repository sources inspected

The GitHub connector returned `FORBIDDEN: This conversation is restricted to developer
MCPs`. Public repository HTML/raw pages were read instead. A local clone failed due
to network name resolution. No repository commit SHA was established; references
below identify main-branch paths read during this review, not immutable snapshots.
Recheck them against the deployment before relying on their tool contracts.

1. [Existing skill](https://github.com/gsmlg-opt/agent-note/blob/main/skills/agent-note/SKILL.md)
   already resolves explicit project labels, uses scoped search, and describes edits.
   Its broad progress/handoff wording lacks the proposed quality and lifecycle gates.
2. [Existing Codex metadata](https://github.com/gsmlg-opt/agent-note/blob/main/skills/agent-note/agents/openai.yaml)
   supplies the Agent Note display and `agent_note` dependency alias. The new default
   prompt evaluates the need for a write rather than presuming a save.
3. [Tool wrappers](https://github.com/gsmlg-opt/agent-note/blob/main/crates/note-mcp/src/tools.rs)
   show revision-aware mutations, summary/detail shapes, and separate attachments.
4. [Transport schemas and tests](https://github.com/gsmlg-opt/agent-note/blob/main/crates/note-mcp/src/stdio.rs)
   define argument schemas, exact-selector support, and stale-revision test cases.
   Tests were inspected, not executed.
5. [README](https://github.com/gsmlg-opt/agent-note/blob/main/README.md)
   describes the search/index pipeline, label-key creation, and distinct Org surface.
6. [Repository instructions](https://github.com/gsmlg-opt/agent-note/blob/main/AGENTS.md)
   were inspected for the source organization and local guidelines. No remote edit
   to these instructions is included in this delivery.

## Connected Agent Note evidence

The live connection exposed eight tools, including tag-only `edit_note` and
`update_note` without `expected_revision`. Its labels description disallows new label
keys, unlike reviewed main source. This was observed from tool schemas, not tested
with live writes. The difference could be in deployment or connector exposure; its
cause and exact deployed version were not established.

A scoped semantic query under `project=agent-note` returned the following guideline,
which was read in full:

- Title: Shared Guidelines for Using Agent Notes
- ID: `78aed6f5-60f0-47e4-a477-779c1b3f8615`
- Observed revision: 2

It assigns `project=agent-note` to agent-authored notes generally and requires an
owner label. That is a historical convention in the retrieved material, not a rule
adopted by this package. The user's present cross-project requirement and existing
repository skill use the target project's declaration. Owner labels remain optional
unless applicable project policy requires them. The retrieved guideline was not edited.

## External format verification

- [Agent Skills specification](https://agentskills.io/specification): frontmatter,
  name/description constraints, relative resources, and progressive loading.
- [OpenAI skill documentation](https://developers.openai.com/codex/build-skills):
  local discovery locations, optional `agents/openai.yaml`, and invocation policy.

These sources verify packaging and client metadata only; the capture quality gate,
coordination rules, and lifecycle workflow are authored recommendations for this use.

## What changed from the existing skill

- Added distinct recall, qualified capture, and authorized curation outcomes.
- Made ordinary task completion insufficient to require a note.
- Preserved explicit PRD/plan saves without forcing a short-lesson template.
- Added evidence, branch/merge applicability, conflicts, and current-first updates.
- Avoided treating retrieved note instructions as higher-priority project policy.
- Added schema-aware mutation guards, uncertain-create reconciliation, and readback.
- Kept destructive/bulk/Org operations out of automatic capture.
- Added reference material, a project entry point, and behavioral evaluation cases.

No backend contract change, authentication change, automatic cleanup, plugin manifest,
or remote write is part of this package.

## Version 1.1 setup addendum — 2026-09-08

This update is based on the previous delivered ZIP and its actual template/README,
not an assertion that version 1.0 or this update has been committed upstream.
The public main skill page still exposed the basic project-resolution instructions;
the directory page showed SKILL.md and agents/ but not the proposed assets/ folder.
The GitHub connector again returned FORBIDDEN, so the latest repository commit was
not pinned or independently authenticated. Public views may lag the target checkout.

The requested distinction is addressed explicitly: the prior delivery included a
project snippet and manual README instructions, but SKILL.md had no Setup mode or
link to that snippet. Version 1.1 adds a setup trigger, workflow row, project-setup
reference, and ten behavioral fixtures. It preserves the previous note contracts;
those inherited backend details were not re-reviewed in this setup-only update.

Setup guidance is newly authored. No repository file or note has been changed by
applying this package, and no client or model behavior was tested.
