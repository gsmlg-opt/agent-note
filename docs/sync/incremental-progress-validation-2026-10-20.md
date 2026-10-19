# Incremental synchronization and progress validation — 2026-10-20

The coordinated change implements the approved
[design](../superpowers/specs/2026-10-20-incremental-note-sync-progress-design.md)
in Agent Note and GaoNote. This record covers local qualification. Neither
production service has been restarted, deployed or reconfigured by this work.

## Checkouts and boundaries

- Agent Note: `/data/development/gsmlg-opt/agent-note`, `main`, starting at
  `b1d07f44846a5e7f7bc9cd4c65f30b604e696a28`.
- GaoNote: `/home/gao/Workspace/gsmlg-dev/gsmlg_umbrella`, `main`, starting at
  `7c1bdbaa2fa86dc95721e01ce42d050e07b097b5` with existing unrelated dirty work.
  This task's seven-file delta is isolated in
  `/tmp/incremental-gaonote-task.patch` and its manifest in
  `/tmp/incremental-gaonote-task-files.txt`.
- No branches or worktrees were created or retained after the user's direct-main
  instruction. Direct/SSH/mTLS configuration and authentication were preserved.
  The interoperability connection uses loopback Direct WS without certificates.
- PostgreSQL port 55421 and temporary databases belong to this task. The real
  GaoNote domain, channel, PostgreSQL storage and HTTP S3 client run against an
  isolated loopback listener with a temporary in-memory S3 HTTP fixture. This
  does not qualify production ingress, production S3 or production data.

## Behavior covered

The existing four-field join is unchanged. Both peers negotiate `state_query_v1`
before originating probes, service opposite-direction probes while awaiting
replies, compare current membership/head/object availability, and keep pending
events subject to exact durable ACKs. A finite descriptor cohort precedes source
attachment preparation. Pending work receives priority with bounded audit fairness.
Changed notes retain the full content → attachments → commit/ACK sequence.

Accepted incoming Attach events become nonpending outbound Snapshots without
changing the original receipt or ACK. This prevents replay immediately after
duplex initial synchronization. Newer local winner Attach responses remain
pending. Missing membership recovery preserves generation/control/version and
cannot override a fence or newer pending event. Detached queries return the
frozen control descriptor and no later private head.

The UI separates local tracked count at the last full check from current-round
counts. It shows sending/receiving phases, current note, attachment bytes,
awaiting confirmation, blocked/superseded work and dated progress. Remote totals
and remaining work stay unknown where the protocol does not establish them.

## Results

| Check | Result |
| --- | --- |
| `cargo test -p note-core sync::tests --lib` | PASS: 8 |
| `cargo test -p note-pipelines --test sync_test` | PASS: 32; 2 PostgreSQL cases ignored |
| `cargo test -p note-server sync_client --lib` | PASS: 63; 4 opt-in fixtures ignored, including the separately executed GaoNote case below |
| Frontend native suite | PASS: 307 |
| Frontend Wasm check | PASS; existing unrelated warnings retained |
| Local progress browser scenarios | PASS: old status, both directions, unknown totals, stages, ACK wait, blocked/reconnect, escaping, mobile layout, polling failure/recovery/overlap, save and retry |
| Existing diagnostics browser scenarios | PASS; no page errors or unexpected requests |
| Actual Agent Note ↔ GaoNote incremental interoperability | PASS: duplex inventory, reconnect skips all unchanged bodies, changed note on the same socket, missing membership restored |
| GaoNote scoped domain/channel suites | PASS: 130 domain + 68 transport; strict compile, scoped formatting and Credo passed |
| Native/frontend formatting and diff whitespace | PASS |
| Production deployment / production end-to-end acceptance | NOT RUN |

Regression-first failures established detached private-head exposure, malformed
response acceptance, invalid closure/duplicate-event queries, impossible progress
totals, lost inbound-round counts, received-Attach replay, and oversized query
responses before the corresponding fixes. The final regression suites retain
exact receipts, pending work, attachment leases and newer-winner behavior.

Local evidence is under `/tmp/agent-note-sync-progress-ui-20261020/final/`,
`/tmp/agent-note-sync-server-final.log`,
`/tmp/agent-note-incremental-interop-final.log` and
`/tmp/incremental-gaonote-final-normalized.log`.

## Reproduction

The focused native commands above and these frontend checks require no live peer:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
env NO_COLOR=true trunk serve --address 127.0.0.1 --port 6221
```

Run Trunk from `crates/note-frontend`, then the mocked browser scenarios from the
repository root:

```sh
uv run --with playwright python browser-tests/test_sync_progress.py --executable /run/current-system/sw/bin/chromium
uv run --with playwright python browser-tests/test_sync_diagnostics.py --executable /run/current-system/sw/bin/chromium
```

The optional Rust interoperability case requires the isolated GaoNote fixture
described in `/tmp/incremental-gaonote-pg/listener.exs`; never point its control API
at a live service. Its addresses are emitted to
`/tmp/incremental-gaonote-pg/listener.json`:

```sh
GAONOTE_INTEROP_WS='ws://127.0.0.1:<port>/note-sync/websocket?vsn=2.0.0' \
GAONOTE_INTEROP_HTTP='http://127.0.0.1:<port>/__interop' \
cargo test -p note-server real_gaonote_reconnect_skips_blobs_transfers_changes_and_restores_membership --lib -- --ignored --nocapture
```

The test creates isolated notes and a real attachment, synchronizes both ways,
reconnects and checks that no unchanged note or attachment bodies are accessed,
transfers one edited note over the existing socket, then removes only its test
membership and verifies fresh Attach recovery against the real receiver.

Independent spec-compliance and code-quality re-review found no remaining concrete correctness findings after repairs. Production behavior remains unverified until both services are deployed and observed. Rust PostgreSQL-specific pipeline cases and the other opt-in native transport fixtures were not run.
