# GaoNote synchronization phase 1 validation

Source: `codex/note-sync` in the isolated `.trees/codex-note-sync` worktree,
based on `1a16690d3f0bcdb08e00752e46d76f234313416b`. This validates the Agent Note
implementation against local storage and actual Phoenix fixtures. GaoNote's
production receiver, ingress and deployment remain phase 2.

## Delivered behavior

System selects one server-provisioned mTLS profile and an OR union of typed label
selectors, including `project=bear_note`. Bidirectional snapshots preserve UUID,
title, Markdown, typed labels, timestamps, attachment IDs/paths/checksums and
trash/restore/purge. Durable HLC/LWW, coalesced pending snapshots, membership
generations, fences and exact ACK matching survive replay and restart.

All writer families capture within the business transaction. Pending snapshots
and inflight read leases protect immutable attachments from cleanup. Pausing
retains capture and pending work while stopping the complete socket session.
Per-note validation/catalog rejections preserve the pending event and allow other
notes to continue. System shows safe status and fixed rejection reasons; private
keys and credential paths stay server-side.

## Final verification

| Check | Result |
| --- | --- |
| `note-core --lib` | PASS: 79 tests |
| Turso settings, migration and shared storage contracts | PASS: 4 + 16 + 1 tests |
| PostgreSQL settings, repositories and shared storage contracts | PASS: 5 + 8 + 2 tests against PostgreSQL 17 / pgvector 0.8.2 |
| Pipeline sync acceptance, including real PostgreSQL | PASS: 21 tests; 2 environment-gated tests separately executed against live PostgreSQL, both PASS |
| Cleanup writer rollback and bounded legacy-read regressions | PASS: 2 tests |
| Nine affected ordinary pipeline targets | PASS: 178 tests |
| Server Phoenix/mTLS contracts, including both actual Phoenix tests | PASS: 11 tests |
| Server configuration | PASS: 36 tests |
| System OpenAPI/runtime configuration agreement | PASS: 1 focused test |
| Independent Elixir canonical-hash vectors | PASS: 1 ExUnit test covering both vectors |
| Frontend logic | PASS: 272 tests |
| Frontend Wasm check and Trunk build | PASS |
| Final native binary build and workspace all-target check | PASS |
| Final native/frontend formatting and `git diff --check` | PASS |

The PostgreSQL checks used an isolated disposable container and generated test
databases/schemas; the container was removed afterward. Tests outside the affected targets were not run. Hosted CI and
production connectivity were not run in this phase.

Ordinary pipeline counts: `attachment_management_test` 34, `batch_delete_notes_test`
7, `batch_update_note_labels_test` 17, `bulk_update_note_labels_test` 18,
`export_test` 15, `label_keys_test` 4, `note_mutations_test` 18, `save_note_test` 57,
and `system_test` 8. The live PostgreSQL sync tests exercise snapshot replication
and mutation-family capture; they were explicitly run, not counted from skipped
environment-gated tests.

Final local logs are retained under `/tmp/`: `agent-note-sync-core-final.log`,
`agent-note-sync-complete.log`, `agent-note-sync-postgres-complete.log`,
`agent-note-sync-unit-final.log`, `agent-note-sync-ordinary-complete.log`,
`agent-note-sync-final-pg-20261011.log`,
`agent-note-sync-review-final-strict-20261011.log`,
`agent-note-sync-config-final-20261011.log`, and
`agent-note-sync-review-mix-20261011.log`. Final compilation logs are
`agent-note-sync-final-workspace-check-20261011.log` and
`agent-note-sync-final-native-build-20261011.log`; schema verification is recorded
in `agent-note-sync-final-openapi-20261011.log`. Native all-target compilation and
Wasm compilation reported only existing Org dead-code warnings.

The real Phoenix fixture pins Phoenix 1.8.15 and uses an OTP TLS listener that
requires a trusted client certificate. Tests exercise initial bidirectional
inventory, native attachment ID `event` without metadata-blob collision, attachment
bytes in both directions, lost ACK replay, pushed ACK before commit reply,
pause/scope change during inventory, rejected-note progress across reconnect,
manual retry and shutdown lease reclamation. TLS negatives cover wrong hostname,
untrusted server CA, and missing, untrusted or expired client certificates.

## Browser verification

An actual native server and Trunk build used disposable local data, a localhost
profile, and intentionally missing credential files. Browser checks proved empty
selectors disable Save, a stable rule UUID and `project=bear_note` persist,
credential errors omit file paths, normal note saving still succeeds, manual retry
works, and pausing retains the one pending snapshot. No horizontal overflow was
observed at 1440×1000 or 390×844; the only console warning was the existing preload
integrity warning.

Local screenshots and captured API state are retained under
`/tmp/agent-note-sync-smoke-Q0z7U9/`: `system-sync-desktop.png`,
`system-sync-mobile.png`, `desktop-state.json` and `mobile-state.json`.
The smoke server was stopped after verification.

## Reviewed and repaired invariants

- PostgreSQL replication reads include soft-deleted notes while ordinary reads
  continue to hide them. The new regression failed before the query fix and passed
  afterward.
- Incoming fences and losing detaches do not allow a matching copy to reattach on
  a body edit or global pause/resume. Label/rule reentry establishes a new generation.
- Recreating a purged UUID from an archive advances its version and generation.
- Existing catalog description/type conflicts reject the entire winning snapshot
  without changing global definitions or recording an unapplied snapshot hash.
- ACK descriptor mismatches preserve both latest pending state and attachment leases.
- A configuration watcher cancels join, inventory, preparation and chunk sending,
  including when the steady-state loop has not yet started.
- Rejected inventory events remain cached across reconnect so later notes progress;
  a new event or manual retry makes that note eligible again.
- Concurrent cleanup waits for a held reference transaction and observes its commit;
  a prior deletion reservation prevents committing a new reference or read lease.
- Never-configured installations retain the ordinary attachment cleanup path.
  Configured but paused installations retain synchronization pins.
- Legacy checksum capture uses the existing bounded attachment read API.
- The all-target compilation caught one MCP test fixture that needed the newly
  added default `sync` field. That constructor was updated without changing MCP
  behavior; MCP runtime tests were not executed in this phase.

The final bounded independent review found no new material defects after the
repairs. Review was read-only and used the frozen source above.

## Limits and receiver handoff

An event, including JSON metadata and all attachment bytes, is capped at the
negotiated aggregate limit, at most 512 MiB. Current immutable attachments use
stored size/checksum metadata; oversized notes remain editable and pending until
they fit. Pre-metadata legacy files above 512 MiB cannot seed a checksum and stop
the affected scan/capture until migrated, replaced or removed. Final inbound apply
uses bounded complete byte objects because the attachment store accepts them.

The [v1 receiver contract](gaonote-protocol-v1.md) and
[cross-language hash vectors](hash-vectors-v1.json) are the phase 2 handoff.
Live acceptance requires GaoNote's dedicated authenticated SyncSocket, durable
capture/apply, certificate-to-peer/scope binding at trusted ingress, and real
end-to-end deployment tests. This phase does not modify GaoNote, merge main, push,
publish or deploy.
