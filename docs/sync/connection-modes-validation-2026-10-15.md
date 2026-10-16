# System connection modes validation — 2026-10-15

## Candidate and scope

Validated the uncommitted `codex/sync-configuration-ui` working tree based on
`91f96fc2c63f675feed06926446c5f7d9b90d51a`, in `.trees/codex-note-sync`.
This extends the [initial UI checkpoint](ui-configuration-validation-2026-10-15.md)
with configurable environment addresses and explicit direct, automatic SSH and
mTLS methods. The note protocol, label rules, durable queue and paired UUIDs
retain their existing behavior.

## Automated checks

| Check | Result |
| --- | --- |
| Frontend suite | PASS — 288 passed, 0 failed |
| Server sync suite | PASS — 22 passed, 0 failed, 3 ignored |
| Explicit real OpenSSH integration | PASS — 1 passed, 0 failed |
| Server configuration suite | PASS — 40 passed, 0 failed |
| Generated REST OpenAPI regression | PASS — 1 passed; 60 paths / 72 operations |
| Server all-target check and binary build | PASS |
| Frontend Wasm check and Trunk build | PASS |
| Changed Rust file formatting and diff whitespace | PASS |

The combined suites passed **352 tests**. The sync suite's three ignored cases
include the real OpenSSH test, which was then explicitly executed. The two
existing actual-Phoenix fixture tests were not rerun for this change.

The SSH integration starts a temporary loopback sshd with generated host and
identity keys and a pinned known_hosts file. It transfers 150 KB through the
OpenSSH stdin/stdout stream, exercises the public `connect_socket` runtime path,
and verifies the original WebSocket path/query. Incorrect host pins and client
keys fail. Missing files produce redacted errors. Canceled and HTTP 403 handshakes
terminate and reap the owned SSH child, verified through Linux process state.
Fixture credential paths contain spaces. Control characters, expansion tokens
and directories are rejected by shared credential validation.

Other regressions cover certificate-free WS, server-verified WSS without a client
certificate, strict existing mTLS negatives, legacy omitted-mode defaults,
mode persistence/restart, credential clearing, atomic failure behavior and hot
replacement preserving pending notes. An uppercase-WSS regression verifies that
parsed schemes cannot bypass credential validation before persistence.

An initial concurrent run exceeded the existing three-second hot-replacement
fixture deadline. The same test passed in isolation, the complete serial suite
passed, and the final default-thread suite passed in 1.19 seconds. No deadline or
assertion was weakened. Frontend compilation retains three existing unrelated
dead-code warnings.

## Browser acceptance

Used the final Yew/Wasm bundle with the actual Rust server at
`http://127.0.0.1:43209/system`, a fresh embedded database, local stub embedding,
temporary credentials and a read-only `0444` runtime TOML. Sync remained paused.
No production sync endpoint was contacted.

| Action or condition | Result |
| --- | --- |
| Switch among direct, SSH and mTLS with an entered test URL | PASS — address retained; only relevant fields displayed |
| Create mTLS profile with custom CA | PASS — server paths accepted, summaries expose presence flags |
| Change mTLS to direct WSS | PASS — client certificate/key removed, selected CA retained |
| Change direct WSS to WS | PASS — CA hidden and removed; no credentials in the request |
| Change to SSH without required settings | PASS — actionable local validation |
| Save SSH host, port, username and server file paths | PASS — no TLS credentials, safe SSH metadata returned |
| Edit SSH with blank credential paths | PASS — saved paths retained; request omits replacements |
| Submit a missing SSH identity with a changed URL | PASS — redacted HTTP 400; previous profile unchanged |
| Hold a successful Add response while changing duplicate-check and score | PASS — other drafts retained, new connection selected and settings persisted |
| Save selected profile and `project=bear_note` while paused | PASS — local node UUID displayed |
| Restart the server and reload the page | PASS — complete catalog, rules, method, address and node UUID retained |
| Reopen SSH editor after restart | PASS — metadata restored; existing credential paths remain blank |
| Render SSH editor at 390 × 844 and desktop widths | PASS — no horizontal overflow; inputs and actions usable |

The delayed-Add scenario was investigated after a static review raised a stale
callback concern. The actual Yew 0.23.0 state handles read shared current state;
the runtime check confirmed draft preservation, so no speculative repair was made.
The review has no remaining blocking findings.

Twelve assertions over browser/runtime artifacts passed. The connection sidecar
has mode `0600`, stores resolved paths without key contents, and remains separate
from the note database and exports. Reloaded browser logs show the existing
preload-integrity warning and no JavaScript errors. Temporary server and sshd
processes were stopped after validation.

## Evidence

Browser artifacts and frontend logs are under
`/tmp/agent-note-sync-modes-ui-20261015/`:

- `browser-assertions.json`, `verify_browser.py` — twelve final assertions.
- `mode-switches-address-and-fields.json` — address retention and conditional fields.
- `created-mtls.json`, `direct-wss-strips-mtls-keeps-ca.json`,
  `direct-ws-clears-ca.json` — saved mode changes and request-field omission.
- `ssh-created-redacted.json`, `ssh-edit-write-only.json`,
  `ssh-blank-path-edit-retained.json` — SSH create/edit and hidden file paths.
- `invalid-ssh-edit-retains-catalog.json` — rejected edit preserves the catalog.
- `add-save-drafts-preserved.json` — delayed successful Add response and saved drafts.
- `before-restart.json`, `after-restart.json`, `restarted-page-state.json`,
  `restarted-ssh-editor.json` — restart, reload and node identity.
- `ssh-editor-desktop.png`, `ssh-editor-mobile.png`,
  `ssh-editor-mobile-actions.png` — actual UI screenshots.
- `frontend-tests.log`, `frontend-wasm-check.log`, `trunk-build.log` — frontend checks.

Backend logs use `/tmp/agent-note-sync-transports-backend-` with suffixes
`final-sync.log`, `final-parallel.log`, `real-ssh.log`, `config.log`, `openapi.log`,
`all-targets.log` and `build.log`.

## Deployment boundary

### Publication recheck — 2026-10-16

Reran the frontend, configuration, sync and generated OpenAPI checks before
publication. The real OpenSSH test initially failed twice because its readiness
probe temporarily bound the sshd port, racing with the daemon. Replaced that
test-only probe with a TCP connection check, retaining the existing timeout and
all transfer, authentication and process-cleanup assertions. The real SSH test
and scoped sync suite then passed; the combined checks again passed 352 tests.
Logs, the original failures and source fingerprints are preserved under
`/tmp/agent-note-deploy-20261016-validation/`.

### Initial checkpoint boundary

GaoNote's certificate-free SSH receiver is available in upstream commit
`7c1bdbaa2fa86dc95721e01ce42d050e07b097b5`, which closed issue #11. Its independent
loopback endpoint defaults to port `4114` and uses the existing join protocol.
See [SSH setup](ssh-tunnel.md) for receiver enablement and peer registration.

This qualifies client configuration, local transport and persistence behavior.
Production GaoNote connectivity and two-way note synchronization were NOT RUN.
The runtime image now installs `openssh-client`; a new container image was not
built or deployed for this UI change. No commit, merge or push was performed, and
production configuration and notes were untouched.
