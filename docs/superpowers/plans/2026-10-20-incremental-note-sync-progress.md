# Incremental Note Sync and Progress Implementation Plan

> **For agentic workers:** Use subagent-driven-development with independent file ownership; verify spec compliance before code quality. User approved parallel execution and both-peer protocol changes.

**Goal:** Check both peers' current note states before replay, prioritize real pending work, transfer each note with its attachments sequentially, and expose truthful progress.

**Architecture:** Keep one client-originated Phoenix socket. Add negotiated read-only state queries, conditional recovery and finite/fair work scheduling; retain exact durable ACK and complete-note publication. Add progress to the existing status endpoint and Yew page.

**Tech Stack:** Rust/Tokio/Serde, existing storage transactions, Yew/Wasm, Phoenix/Ecto, local mocked Playwright browser tests.

## Task 1: State comparison and pipeline boundaries (parent)

Files: `crates/note-core/src/sync.rs`,
`crates/note-pipelines/src/sync.rs`, `crates/note-pipelines/tests/sync_test.rs`.

- [x] Add failing tests for a nonpending normalized Snapshot comparing equal to a remote Attach, missing association/head/attachments, changed control and mismatched hashes. The equality predicate must ignore only allowed Attach normalization and require real head equality.
- [x] Run `cargo test -p note-core sync --lib` and `cargo test -p note-pipelines --test sync_test`; record baseline/red failures.
- [x] Add Serde query/result/head types and pure comparison in core. Add pipeline descriptor-state reads under a transaction, pending lookup, conditional missing-association Attach recovery, and revalidated reservations. Probe reads must not ACK or alter pins.
- [x] Rerun both focused targets and preserve pending/fencing/attachment regressions.

The comparison's essential assertion is:

```rust
assert!(!state.matches_snapshot(&event.acknowledgement()));
```

for every unavailable head/attachment or changed membership case, while a matching
nonpending Snapshot may be skipped only after the caller rechecks pending/member.

## Task 2: Rust negotiation, scheduling and progress (parent)

Files: `crates/note-server/src/sync_client/runtime.rs`,
`crates/note-server/src/sync_client/progress.rs`,
`crates/note-server/src/sync_client/mod.rs`, protocol documentation.

- [x] Run `cargo test -p note-server sync_client --lib` as baseline.
- [x] Add socket-fixture regressions showing unchanged four-field joins; no queries to legacy peers; mutual probes during requests; high-UUID pending before audit replay; equal notes skip all transfer frames; sequential metadata/attachments/commit; disconnect preserves durable work.
- [x] Run new regressions red before implementation.
- [x] Negotiate `state_query_v1` after join and before `inventory_start`, process inbound queries while awaiting replies, and validate echoed queries/cardinality/session.
- [x] Check descriptor pages before blobs, revalidate before skipping/sending, conditionally recover missing associations, then process pending before audit. Keep finite rounds and legacy 8:1 bounded fairness.
- [x] Publish round and current-note status around preparation, metadata, attachment chunks, apply and exact ACK. Deduplicate by UUID and descriptor. Clear current activity on disconnect/pause; do not confuse processing ACK with remote equality.
- [x] Rerun scoped server tests. Test invalid negotiated messages and count partition, unknown remote totals, blocked notes, retries and stale ACKs.

Progress API is additive: `progress: Option<SyncProgress>`, with timestamp, optional
`tracked_notes` (last full local cohort size), and
`upload`/`download`. Each direction has `run_id`, `phase`, `basis`, nullable
`notes_total`/`notes_remaining`, `notes_checked`, `notes_equal`, `notes_planned`,
`notes_completed`, `notes_superseded`, `blocked_notes`, and optional `current_note`.
Current-note fields are `id`, optional `title`, `phase`, `attachments_total`,
`attachments_completed`, `attachment_bytes_total`, `attachment_bytes_done`.

## Task 3: Progress UI (frontend worker, parallel)

Files: `crates/note-frontend/src/api.rs`,
`crates/note-frontend/src/components/system_sync.rs`, optional progress component,
`crates/note-frontend/app.css`, `browser-tests/test_sync_progress.py`.

- [x] Add failing logic/browser cases for missing optional status, unknown totals, both directions, ACK waiting at 100% bytes, blocking and reconnect.
- [x] Add typed optional progress, two compact panels, native labelled progress bars and two-second guarded polling. Preserve save/retry/diagnostics and escaped wrapped titles.
- [x] Run frontend native tests and Wasm check with the existing frontend target cache.
- [x] Serve local Trunk on `127.0.0.1:6221`, run mocked browser scenarios and capture desktop/mobile screenshots. Do not mutate production configuration.

## Task 4: GaoNote matching receiver/sender (Elixir worker, parallel)

Files: umbrella's `apps/gsmlg_admin_web/.../channels/note_sync_channel.ex`,
`apps/gsmlg_gao_note/.../sync/{engine,store,outbound}.ex`, scoped sync tests/docs.

- [x] Preserve the existing dirty checkout and capture baseline sync diffs; inspect applicable instructions. No restart/deploy or unrelated edits.
- [x] Add failing negotiation/query/skip/fairness tests. Run only the sync engine/channel targets using an isolated test database.
- [x] Advertise capability in join reply, implement immutable negotiation before inventory, consistent peer-scoped query replies and symmetric outbound querying. Retain legacy behavior, exact ACKs, pre-transfer staging/failure isolation and metadata-first ordering.
- [x] Add negotiated `inventory_complete` cohort totals; never count unauthorized candidates or treat completion as convergence.
- [x] Rerun scoped Elixir tests, formatting and strict scoped compile. Return exact delta and validation evidence without committing unrelated work.

## Task 5: Integration and review (parent plus available reviewers)

- [x] Confirm shared wire fixtures on both sides: simultaneous queries, reconnect equality skip, changed-note transfer, missing-association recovery and legacy fallback.
- [x] Run relevant backend suites and frontend validation once after integration; avoid unrelated targets. Run `git diff --check` in both checkouts.
- [x] Review implementation against approved spec, then review code quality; repair concrete findings and rerun affected checks.
- [x] Update protocol/spec and validation evidence with actual results. Keep implementation separate from publication/deployment; report remaining limitations explicitly.
