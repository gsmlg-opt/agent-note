# Task-driven note sync implementation plan

> Execute against the user-directed task protocol in the matching design.
> Work directly on main. Workers own separate files and preserve concurrent edits.

**Goal:** synchronize through paged discovery, peer managers, task states and
confirmed chunks on one healthy WebSocket without business-operation deadlines
reconnecting it.

**Architecture:** the socket pump dispatches session-scoped task submissions and
states. Existing domain events/receipts provide durable recovery. Background
workers settle I/O and commits independently of the pump; exact ACKs confirm
notes. Session task ledgers are bounded and deduplicate admissions. Local
snapshot producers publish lightweight pages to temporary spools, so network
waits retain neither full-library memory collections nor database snapshots.

**Tech stack:** Rust/Tokio, Phoenix/OTP, existing SQLite/PostgreSQL storage, Yew.

## 1. Freeze the shared protocol and reproduce the failure

- [x] Capture live ten-second inventory task failure and client reconnects.
- [x] Add real PostgreSQL lock regressions before changing receiver production
  code. Command: `/tmp/incremental-gaonote-pg/run mix test
  apps/gsmlg_admin_web/test/gsmlg/admin_web/channels/note_sync_concurrency_test.exs
  --only capable`. Baseline: two expected missing-immediate-confirmation failures.
- [x] Record task submit/status/get/cancel, exact result codecs, bounded ledgers,
  cumulative chunk confirmations and durable-worker ownership in the design.

## 2. Agent Note task transport (client worker)

Files: `crates/note-server/src/sync_client/{tasks,runtime,mod,progress}.rs` and
scoped sync tests. Keep protocol parsing/ledger logic in `tasks.rs`; the runtime
owns socket dispatch and background task handles.

- [x] Write failing ledger tests for same-ID replay, payload conflict, sequence
  admission, bounded terminal eviction and immutable terminal status.
- [x] Write paused-time socket regressions for healthy-heartbeat work beyond old
  deadlines, failed-task continuation and confirmed chunk offsets.
- [x] Implement strict task wire types and negotiated task dispatch.
- [x] Move complete inbound commit ownership to a worker which cannot be aborted
  between publication and transaction cleanup; drain before lease reclamation.
- [x] Report direction task state and receiver-confirmed attachment progress.
- [x] Run `cargo test -p note-server sync_client` and native all-target checking.

## 3. GaoNote task transport (receiver worker, parallel with 2)

Files: `apps/gsmlg_admin_web/lib/gsmlg/admin_web/channels/note_sync_channel.ex`, a
transport task module, `apps/gsmlg_gao_note/lib/gsmlg/gao_note/sync/state_query.ex`,
inventory progress callback in `engine.ex`, and scoped tests.

- [x] Write failing task admission/replay/eviction tests and socket tests holding
  real inventory/apply dependencies beyond the former ten-second cutoff.
- [x] Negotiate task capability and emit queued/running/terminal states.
- [x] Adapt current operation handlers and outbound state machine to task states;
  remove negotiated business deadlines from connection failure paths.
- [x] Confirm chunk received offsets and isolate task failures with scoped
  staging/lease cleanup. Do not discard pending events.
- [x] Make guard/resource cleanup wait for durable apply settlement.
- [x] Run scoped sync domain/channel/socket tests using only the disposable
  PostgreSQL fixture, then strict compile and changed-file formatting.

## 4. UI and protocol documentation (parent, parallel)

Files: `crates/note-frontend/src/api.rs`,
`crates/note-frontend/src/components/system_sync.rs`, and `docs/sync/`.

- [x] Decode optional active_task while retaining older JSON compatibility.
- [x] Render operation, task state, phase, sample time and sanitized task error
  beside existing note/attachment progress, independently of connection health.
- [x] Run frontend logic tests and Wasm check; exercise real page in browser.
- [x] Update wire documentation to distinguish task mode and legacy behavior.

## 4a. Paginated peer managers (latest user direction)

Files: Rust client scan helper, pipeline snapshot page API and related sync
tests; GaoNote channel inventory manager helper, Engine/Store source-page and
pending-page APIs and related domain/channel tests. No storage schema changes.

- [x] Reproduce whole-library candidate/queue accumulation with >32 records.
- [x] Publish atomic lightweight source pages from a consistent snapshot to a
  session spool, with first-page consumption before the producer finishes.
- [x] Close the producer snapshot after local enumeration; prove slow network
  tasks do not hold it. Cancel/settle producers before owned-spool cleanup.
- [x] Classify/compare one page and create bounded per-note tasks from it;
  preserve exact snapshot descriptors, authorization, fences and ACKs.
- [x] Keep pending enumeration keyset-paged and fair, including higher IDs;
  send concurrent lower-UUID changes in a later durable-change pass.
- [x] Replace periodic healthy-session full audits with pending-only passes.
- [x] Add monotonic session/round-scoped `sync_manager_status` pushes, map
  remote manager progress to download, and render source/comparison pages plus
  last actual progress. Keep totals unknown until enumeration completes.
- [x] Preserve exact round note counting without full-library in-memory sets;
  test repeated delivery across pages and fresh-round counting separately.
- [x] Test >one page, page-local failures, no phantom completion while scanning,
  finite initial cohort during edits, bounded queues, and cleanup on disconnect.

## 5. Cross-peer and live acceptance (sequential after 2–4)

- [x] Run isolated real GaoNote interop with task negotiation, slow inventory,
  chunk receipts, task failure followed by next-note success, and lost ACK retry.
- [x] Review final diffs, exact ACK/fences, cancellation and status size bounds.
- [x] Commit/push qualified scoped changes on main; verify remote SHA matches.
- [x] Restart only the local GaoNote application after qualification. If Agent
  Note changes, follow deploy skill: local build, direct ms04 load, publish same
  image, inspect manifest, restart only Agent Note, verify exact image and HTTP.
- [x] Observe one persistent connection advancing scans and note/attachment
  confirmations; record pending/remaining/blocked counts without equating an
  active socket or green HTTP response with completed synchronization.

## Local qualification

- Agent Note task regressions: 89 passed, 0 failed, 4 opt-in cases ignored.
  The attach/fence COMMIT matrix checks exact durable receipt reconciliation
  and uncertain outcomes; the Fence marker omission reproduced before repair.
- Source snapshot/paging tests: 35 passed, 2 PostgreSQL opt-ins ignored.
  Shared Turso audit pagination contracts: 4 passed. PostgreSQL SQL compiles;
  runtime contract opt-ins require pgvector, unavailable in this fixture.
- Frontend: 310 native tests passed, Wasm check passed, 12 browser scenarios
  passed with desktop/mobile captures and sanitized task/manager failures.
- GaoNote: 130 sync domain tests and 96 admin sync tests passed against the
  disposable PostgreSQL fixture, including the final Styx prerequisite. Strict
  receiver compilation, changed-file formatting and scoped Credo passed.
- Real GaoNote + Styx loopback interoperability: passed with more than 64 notes
  each way, paged discovery/comparison, chunked metadata and attachments, exact
  pending drain, reconnect without blob reads, same-socket changes and membership
  repair. An initial 30-second test wait failed; the clean-fixture rerun passed
  without changing production code or that test limit.

Native workspace qualification passed: 1,453 tests passed, 0 failed, 7 ignored;
all-target checking passed. PostgreSQL opt-in cases and early-return test wrappers
did not establish live PostgreSQL coverage without `TEST_DATABASE_URL`.

Publication and live acceptance are recorded below after the rollout follow-ups.

## Rollout follow-up regressions

- The first deployed task session stayed connected while pending uploads fell
  from 445 to fewer than 200. One CSV attachment failed before durable apply.
  GaoNote sampled 4,096 bytes across a UTF-8 character boundary, so finalization
  disagreed with the complete valid CSV despite its exact size and SHA-256.
- GaoNote main `d6943ff5` validates text through EOF with bounded reads and a
  UTF-8 carry. Storage regressions: 39 passed; existing sync staging/attachment
  tests: 19 passed. Malformed bytes after the header still fail; checksum and
  declared MIME guards remain. Strict compile, format and Credo passed.
- Agent Note now retains validated task failure codes after cleanup and the
  next successful note; diagnostics retain operation and task identity. The
  same-socket two-note regression failed before repair; 90 sync tests then
  passed, with four opt-in cases ignored.
- During unfinished page comparison, the UI identifies remaining work as
  currently known work. The new browser assertion failed against the previous
  assets; the final build passed all 13 browser scenarios. Frontend native
  tests: 310 passed; Wasm and native all-target checks passed.
- GaoNote runtime acceptance uses the local `devenv` application only. No
  GaoNote production deployment is part of this rollout.

## Published artifacts and runtime acceptance

- Agent Note task implementation `8374bee` and follow-up `bd2496d` are on remote
  main. GaoNote task implementation `5efe3566`, approved Styx prerequisite
  `785df7d7`, and storage follow-up `d6943ff5` are on its remote main.
- Local GaoNote was stopped and started with named `devenv processes` commands.
  The old BEAM exited; replacement PID 17533 owns ports 4110/4111. PostgreSQL
  PID 2919577 remained running. Runtime modules come from the qualified local
  dev build.
- Agent Note `bd2496d` was built locally, loaded into ms04, and published before
  restarting only `podman-agent-note.service`. Local, staged, registry config,
  and running image IDs match
  `sha256:5b19be59914597f44e3c4022c9b95cc70d6a38083b28ba01661f20566aa02bc0`.
  Published manifest:
  `sha256:d32d8ac04a47429c11c1d01f4cce7706e32d8ea036ce02071d69d21ea0fd591f`.
  The replacement started at 2026-10-20 13:50:47 Asia/Shanghai; service active,
  host HTTP 200, and public `/system/sync` HTTP 200 were verified.
- Exact GaoNote receipt `b1998088-532a-4ca7-9466-121f27b66f44` committed at
  05:45:34 UTC with all three CSV-note attachments. Receipt
  `32529d9a-7406-4a34-bb09-e56fb99b7360` committed at 05:45:54 UTC with all
  eleven attachments after the interrupted development reload was recovered.
  Both receipts retain valid event hashes.
- During the final observation, 05:51:29–05:55:42 UTC, pending uploads fell
  84 to 0, with zero blocked notes or connection/synchronization task errors.
  Connection attempt `1792475447236` remained unchanged. Upload confirmations advanced
  36 to 120; download confirmations advanced 7 to 39. Upload comparison
  advanced 3 to 6 pages; download comparison advanced 1 to 3 pages.
- The initial library audit was still running at that sample: 6 of 57 upload
  source pages and 3 of 52 download source pages compared. Zero durable pending
  uploads is not full duplex audit completion; remaining work stays qualified
  as known work until comparison finishes.
- At 05:56:30 UTC, GaoNote had 3,098 durable receipts, zero pending records
  and zero leases; both target receipts and their attachments remained present.
  Four separate background `StorageFilePurgeWorker` Postgrex lock-wait errors
  were observed. The zero-error statement above applies to synchronization
  tasks and their connection, not all background application logs.
- Public browser acceptance sampled eight times over 44 seconds. The new
  remaining-work wording, task progress, save/retry controls, and desktop/mobile
  layout passed; no page/console errors, failed requests, HTTP errors or overflow.

Local evidence is retained in `/tmp/agent-note-task-followup-deployment-evidence.json`,
`/tmp/agent-note-task-live-final-deploy.jsonlog`,
`/tmp/gaonote-csv-local-runtime-acceptance.json`, and
`/tmp/agent-note-task-live-browser-bd2496d-20261020T055127Z/`.
