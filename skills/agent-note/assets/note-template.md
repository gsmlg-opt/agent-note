# <Searchable conclusion, boundary, or failure cause>

<Put the actionable conclusion in one or two sentences.>

## Context and rationale

<What made this non-obvious? Explain the cause or decision tradeoff, not the task
chronology. State conditions that matter and alternatives rejected, when relevant.>

## Applicability

- Project: <exact configured value>
- Repository/subsystem: <actual scope; list repositories when necessary>
- Basis: <branch + commit, explicitly uncommitted work, or document revision/date>
- Merge status: <merged / unmerged / unknown / not applicable>
- Knowledge state: <observed / accepted decision / proposal / unresolved / superseded>
- Last checked: <YYYY-MM-DD; describe what was actually checked>

## Apply this when

<Condition or symptom -> action to take. Include exceptions and when NOT to apply
this guidance. Avoid repeating general coding advice.>

## Evidence and limits

<Identify source paths/symbols, a focused test case, an issue/PR/ADR, or a recorded
user decision. For checks, say what ran and the result, or that it was NOT run.
Attribute external or other-agent reports. State unverified behavior and limitations.>

## Related knowledge

<Existing note IDs and their relationship; omit when not relevant. A superseded note
must also have a prominent first-line notice linking the replacement and its scope.>

---

Template instructions — remove before saving:

Use only fields relevant to the knowledge. Never invent values to fill a template;
write `unknown` or omit inapplicable fields with a brief explanation. These fields
are Markdown conventions, not MCP arguments, registered label keys, or server states.
Do not update “Last checked” merely because wording was edited. Preserve the actual
verification date and identify any later editorial change separately when needed.

For a blocker, add the reproducible symptom, evidence, unknowns, and unblocking
condition. For an explicitly requested PRD, plan, or source document, preserve its
structure instead of forcing this template. Add only necessary provenance/status.
