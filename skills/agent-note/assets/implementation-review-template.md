# <Subsystem / feature — implemented architecture and review>

> Project: <exact scope> | Repositories: <relevant repos> | Status: proposed / implemented / verified / released / deployed (separate each) | Basis: <commit and date> | Last verified: <date>

## Executive summary
Describe actual behavior, change impact and top unresolved risk in a few paragraphs.

## Problem and requirements
State original problem, desired behavior, constraints and non-goals.

## Architecture and ownership
Identify module/service/process/data responsibilities, boundaries and key invariants.
Use an accurate Mermaid component/flow diagram **only when it adds clarity**.

## How it works
Follow a normal request or operation end-to-end. Explain failure, cancellation,
retry, recovery, resource ownership and security boundaries where relevant.
For multi-party interactions use a grounded sequence diagram; for lifecycle states
use a state diagram. Explain diagrams in prose.

## Design decisions and alternatives
Why these abstractions? Which viable alternatives were rejected and why?
What tradeoffs and future constraints follow?

## Implementation history (curated)
Describe phases, pivotal discoveries, design changes and final resulting mechanisms.
Avoid per-commit/per-agent logs; link historical evidence instead.

## API, storage and compatibility
Document external contracts, persistence, migration/rollback, compatibility,
configuration and operator impact, as applicable.

## Verification and evidence
Separate personally reproduced checks from other-agent/CI claims. Link tests,
PRs, source files and benchmarks. Distinguish verified boundaries from assumptions.

## Known limits and open decisions
What remains not implemented, untested or blocked? What breaks/needs revalidation?

## Related records and provenance
Canonical docs, source revisions, old note IDs, replacement/supersession scope.

---
Remove template instructions before saving. Never invent missing evidence or diagrams.
