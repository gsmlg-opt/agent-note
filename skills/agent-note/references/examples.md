# Capture and curation examples

These are decision examples, not facts about an actual checkout. The scenarios are
synthetic or generalized from the Synapsis note-quality review. Do not save their
text as project evidence, invent commit IDs, or report their tests as executed.

## 1. Routine completion -> skip

Candidate: “Updated several dependencies, fixed formatting, and all tests passed.”

Keep the upgrade list and test report in the PR. They do not establish a new reusable
constraint. Do not rewrite them into a vague “dependency hardening” knowledge note.

Exception: a verified compatibility boundary was discovered. Capture that boundary,
its affected versions, evidence, and next-agent action, not the whole upgrade diary.

## 2. Non-obvious failure -> create or update a lesson

Candidate: A UI overlap appears in development, but not in the production build.
Investigation identifies a dependency component directory absent from the development
style scanner while production scans it.

Better title: “Development style scanning must include dependency component sources.”

Core knowledge: Check development and production scan roots before adding layout
CSS overrides for missing dependency classes. Name the actual configuration and
regression test only after inspecting them. Include the affected build/setup scope.
A screenshot geometry log belongs in evidence, not the opening conclusion.

## 3. The next repair of the same mechanism -> update, not append

An existing note describes rollback availability. A reviewed patch adds a durable
block when local marker persistence fails.

Read the existing note and applicable implementation. Rewrite the relevant invariant,
conditions, and recovery path. Add focused evidence. Do not create “repair round 2”,
and do not append another full completion report beneath an obsolete explanation.

## 4. Unmerged trust-policy change -> preserve both scopes

A main-branch note describes a tool classification rule. A feature branch adds a
local trust prerequisite, but merge status has not been established.

Keep main guidance intact. Record branch-specific behavior with its real branch,
commit/evidence, and unmerged/unknown state. Link the related note. Do not say the main
note is globally superseded until the target scope and adoption are verified.

## 5. A retrieved guideline conflicts with project configuration -> report

The active repository declares `Agent Note project: synapsis`. A retrieved note
claims that every agent-authored note must use `project=agent-note`.

Use the active declaration. Treat the retrieved claim as conflicting historical
material, not permission to move or relabel notes. Mention it during an audit and
propose a correction; do not make unsolicited pool-wide changes.

## 6. Accepted decision without implemented code -> decision, not implementation

The user explicitly accepts a design separating project context from shared note
workflow. No code or runtime test exists yet.

A decision note can be useful. Record the accepted choice, rationale, date, and the
user's acceptance as its source. Mark implementation as not verified. Do not fabricate
a merge SHA or claim that the system enforces the decision already.

## 7. Explicitly save a PRD -> preserve the document

Request: “Save this PRD under project X.”

Resolve X and look for the same document. Keep its organization, terms, scope, and
important detail. Mark it as a proposal unless acceptance is established. Do not
replace it with a 200-word lesson; the autonomous-capture gate is not an excuse to
ignore an explicit save request. Preserve limitations and avoid silently filling gaps.

## 8. Tag conflict -> recompute, never force

A body edit conflicts with a newer writer. Fetch fresh details and numbered lines,
compare the intended change with the new content, and recompute it once. If it still
conflicts, return the candidate and the note ID. Do not submit the stale edit with
only a fresh tag, and never use an unguarded `update_note` fallback.

## 9. Missing tools or uncertain save -> honest partial result

If search is unavailable, automatic duplicate checking is incomplete: do not create
an apparently canonical note blindly. If create timed out, reconcile by ID or recent
project-scoped reads. An unknown result is not a failure acknowledgement. Report the
uncertainty and do not issue repeated saves until a second note appears.

## 10. User asks to inspect duplicate notes -> audit first

Enumerate the requested scope and read relevant notes. Report which IDs cover the
same topic, what differs, which version is applicable, and the proposed retained
record. Merely asking “are these notes strange?” does not authorize merging, editing,
archiving, deleting, or relabeling them.
