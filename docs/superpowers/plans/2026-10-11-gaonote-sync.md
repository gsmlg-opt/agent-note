# GaoNote synchronization implementation plan

> For agentic workers: execute in the shared isolated worktree, following subagent-driven-development with explicit file ownership and scoped tests. Preserve unrelated work.

**Goal:** Deliver Agent Note's complete Phoenix/mTLS bidirectional synchronization client and a reviewable receiver contract.

**Architecture:** Pure synchronization types and HLC belong in note-core; per-record persistence belongs in the Turso/PostgreSQL adapters; atomic capture, membership and inbound apply belong in note-pipelines. note-server owns socket/TLS/runtime profiles and System HTTP status; Yew owns configuration/status presentation.

**Tech stack:** Rust, Tokio, existing storage traits, Turso, PostgreSQL, tokio-tungstenite, rustls, Yew and a pinned Phoenix test fixture.

## Implementation and verification checklist

- [x] Define backward-compatible SyncConfig, HLC, canonical snapshots, descriptor/ACK and generation semantics; validate core ordering and selector configuration.
- [x] Add per-record durable storage and additive migrations to both adapters; prove persistence, keyset enumeration, rollback and restart in shared contracts.
- [x] Capture all writer families atomically, including bulk labels, import and catalog cascades; prove pending coalescing, tombstones and bootstrap overlap.
- [x] Implement version-preserving inbound apply and local revision invalidation; prove LWW, duplicate replay, UUID preservation, labels, trash/restore/purge and no echo.
- [x] Protect pending/read attachment generations against cleanup with coordinated reservations; prove old-transfer/new-version and cleanup/reference races.
- [x] Implement preprovisioned credential profiles, Phoenix V2 client, inventory/chunks/committed ACK/heartbeat/retry/shutdown and safe status/retry HTTP APIs.
- [x] Add System profile/rule editor, live status and retry feedback without exposing credentials; run relevant frontend logic tests and Wasm compilation.
- [x] Run local mTLS negative/positive tests and actual Phoenix Channel fixture interoperability; distinguish fixtures from production qualification.
- [x] Review requirements and code independently, repair material findings, update protocol/handoff documentation and record exact validation results.

See [the phase 1 validation report](../../sync/phase-1-validation-2026-10-11.md) for
scoped test counts, actual Phoenix evidence, browser checks and remaining limits.

## Ownership and sequencing

Backend worker owns note-core, note-storage, storage adapters/contract tests and note-pipelines. Transport worker owns note-server, its tests and root dependency lock changes. Parent owns frontend, example configuration, documentation and final integration/review. The backend publishes shared API types early so transport and frontend can proceed independently; final integration follows storage and pipeline availability.

## Finish boundary

Keep focused local commits on codex/note-sync. Do not change GaoNote, publish, merge main or deploy. Report any unrun environmental checks explicitly; never substitute framing mocks for actual Phoenix interoperability or claim production availability.
