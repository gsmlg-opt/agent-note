# System sync connection configuration validation — 2026-10-15

This preserves the initial mTLS configuration checkpoint. The subsequent direct,
automatic SSH and mTLS implementation is qualified in
[connection modes validation](connection-modes-validation-2026-10-15.md).

## Candidate and scope

Validated the uncommitted `codex/sync-configuration-ui` working tree based on
`91f96fc2c63f675feed06926446c5f7d9b90d51a`. This adds connection creation and editing
to System, durable server-owned connection profiles, and runtime reconnection.
The existing note protocol, peer identity, and rule semantics remain in place.

## Automated checks

| Check | Result |
| --- | --- |
| Frontend tests | PASS — 278 passed, 0 failed |
| Server config tests | PASS — 39 passed, 0 failed |
| Server sync tests | PASS — 17 passed, 0 failed, 2 ignored |
| Generated REST OpenAPI regression | PASS — 1 passed; 60 paths / 72 operations, including connection GET/PUT |
| `cargo check --locked -p note-server --all-targets` | PASS |
| `cargo build --locked -p note-server --bins` | PASS |
| Frontend Wasm check | PASS |
| Trunk browser build | PASS |
| Scoped Rust formatting and `git diff --check` | PASS |

The 335 passing tests include six new frontend draft/request cases. The two ignored
sync tests require the separate actual-Phoenix fixture and were not run for this
change. The live Rust mTLS test did run: replacing a profile cancels a blocked join,
connects to the new listener, and retains pending notes.

Backend regressions also cover redacted responses, certificate/key mismatch,
retain/clear CA behavior, rejected writes preserving old state, restart precedence,
concurrent creates, and HTTP cancellation during a persistence commit. Review found
that stale TOML seed semantics could prevent loading an authoritative saved catalog;
the regression failed before repair and passed after validation moved to the actual
fallback path. A missing catalog still validates seeds; malformed TOML syntax still
fails.

Frontend compilation reports three existing dead-code warnings. No compilation
errors occurred. The browser reported the existing preload-integrity warning and
no JavaScript errors after reload.

## Browser acceptance

Used the built Yew application and actual Rust server at `http://127.0.0.1:47681`,
with a fresh isolated embedded database, temporary certificates, local stub
embedding, a read-only `0444` runtime TOML, and writable data directory. Test
connections used `wss://localhost:1/note-sync/websocket?vsn=2.0.0`; sync stayed paused.
No production endpoint was contacted by the sync worker.

| User action / condition | Result |
| --- | --- |
| Add connection with an empty catalog | PASS — actionable editor with `gao_note`, canonical WSS URL, native CA roots, and 512 MiB defaults |
| Save without a client-key path | PASS — actionable validation; no profile created |
| Save a mismatched certificate/key pair | PASS — server rejects it; catalog remains empty |
| Choose custom CA and create a valid connection | PASS — profile saved; connection select displays `gao_note` |
| Save selected connection and `project=bear_note` while paused | PASS — settings persist and local node ID appears |
| Reload page | PASS — connection, rule, paused state, and node ID retained |
| Edit an existing connection | PASS — ID fixed, credential fields blank, CA displays Keep existing |
| Change URL and transfer limit with credential fields blank | PASS — new values apply immediately, certificate/key/custom CA retained |
| Hold a successful connection PUT response while editing other System drafts | PASS — duplicate-check and minimum-score changes remain and persist after release |
| Continue status polling during the held save | PASS — six status responses observed without discarding drafts |
| Attempt an invalid credential edit with a different URL | PASS — error shown; previously saved URL and catalog retained |
| Render editor at 390 × 844 | PASS — no horizontal document overflow; controls remain usable |
| Stop and restart the local Rust process | PASS — catalog, rules, paused state, and local node identity retained |

DOM testing caught a dropdown defect before the final build: the connection select
displayed an empty value while its draft held `gao_note`, and the CA select
displayed Path while its draft held Native. Explicit option selection fixed both.
The final browser verified Native on create, Path after choosing a custom file,
Keep on edit, and `gao_note` after save and restart. Unrelated category selectors
were outside this change.

The restarted sidecar has mode `0600`, contains resolved certificate file paths
without PEM contents, and remains separate from the note database/export. Existing
credential paths never appeared in connection GET responses.

## Evidence

Local browser artifacts are under
`/tmp/agent-note-sync-ui-20261015-qsa60i5z/`:

- `before-sync.png`, `after-sync.png`, `after-sync-mobile.png` — before/after UI.
- `sync-select-red.json`, `add-default-green.json`, `created-green.json`,
  `edit-loaded-green.json`, `restarted-dom-green.json` — dropdown regression evidence.
- `create-mismatched-key-rejection.json`, `edit-failed-preserved-green.json` — failed
  writes retain the previous catalog.
- `edited-and-settings-green.json` — changed connection plus preserved System drafts.
- `before-restart.json`, `after-restart.json`,
  `restart-and-persistence-green.json` — restart and credential-retention checks.
- `frontend-tests.log`, `frontend-wasm-check.log`, `trunk-build.log` — frontend checks.

Backend logs:
`/tmp/agent-note-sync-ui-backend-precedence-config.log`,
`/tmp/agent-note-sync-ui-backend-final-sync.log`,
`/tmp/agent-note-sync-ui-backend-openapi-final.log`, and
`/tmp/agent-note-sync-ui-backend-final-check.log`.

## Qualification boundary

This qualifies local UI configuration and server persistence/reconnection. It does
not establish a successful production GaoNote connection or production two-way
sync. No commit, merge, push, or deployment was performed for this UI change;
production configuration and notes were untouched.
