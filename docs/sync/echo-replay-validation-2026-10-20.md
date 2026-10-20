# Unchanged-note replay correction — 2026-10-20

## Reproduced behavior

The receiving panel reported 520 confirmations with zero equal/planned notes and
unknown remaining work. These confirmations count actual completed wire
transfers, including snapshots whose durable apply was ignored as unchanged.
They do not mean 520 notes acquired a new version.

The live PostgreSQL store had 1,761 shared heads. All 556 attachment-bearing
shared heads used legacy attachment references without a `storage` field.
`query_sync_state` reported unknown attachment availability for every such head,
even when its original files remained intact. GaoNote therefore could not skip
an equal snapshot. An ignored equal-version apply retains the original references,
so replaying the note did not change that classification.

A read-only comparison of all 518 exported GaoNote outbound durable ACK records
found the same original HLC and snapshot hash in Agent Note for every record:
518 matches, zero different versions/hashes, zero missing heads. The UI's 520
confirmations and this 518-record sender export are different observations and
are not assumed to be identical cohorts.

GaoNote also omitted the optional classification counters. The receiving UI
could therefore show confirmations while equal/planned remained at their defaults.

## Correction

- Verify committed legacy attachment files using descriptor-bounded reads,
  exact length and SHA-256, without rewriting metadata or synchronization records.
  Missing, corrupt, truncated and oversized files still require repair;
  unsupported or transient storage access stays unknown.
- Retain original HLC/hash, UUID identity, membership fences, immutable-object
  checks and exact durable ACK requirements. Business `updated_at` alone does
  not establish equality or attachment availability.
- GaoNote emits distinct-note classification counts, with disk markers preventing
  audit/poll overlap from counting twice. Exact ACKs retain a planned/delivered
  classification; later failures remain visible and retries can recover.
- Completed source discovery stays frozen while total/checked counts cover new
  classified notes. Agent Note accepts the extended checked count only when a
  completed scan supplies a sufficient total.
- Remote blocked counts accept sequenced recovery to zero. Successful transfers
  clear only their own local rejection, preserving other unresolved failures.

The existing Direct WebSocket endpoint, authentication and selectors are unchanged.
The live Agent Note backend is PostgreSQL; no storage migration is part of this fix.
GaoNote remains a local development service and is restarted only through its
named devenv application process. PostgreSQL is not restarted.

## Reproduction

```sh
cargo test -p note-pipelines --test sync_test
cargo test -p note-server sync_client --lib -- --test-threads=1
cargo fmt --all -- --check
```

The two opt-in PostgreSQL pipeline contracts require a disposable PostgreSQL
database with pgvector installed. They were qualified in a temporary pgvector
container bound to a random loopback port; that container was removed afterward.
Neither the active GaoNote database nor Agent Note data was used by these tests.

## Qualified source results

| Check | Result |
| --- | --- |
| Pipeline synchronization integration | PASS: 38, including 3 new legacy regressions |
| PostgreSQL snapshot/mutation contracts | PASS: 2 in isolated pgvector PostgreSQL |
| Pipeline synchronization units and library check | PASS: 2 units; library check |
| Agent Note synchronization client | PASS: 94; 4 opt-in external fixtures ignored |
| GaoNote synchronization engine | PASS: 130; engine unchanged by this correction |
| Updated GaoNote manager/task channel | PASS: 12 |
| Related GaoNote query/task/deadline cases | PASS: 40 |
| Rust formatting; GaoNote strict compile, formatting and Credo; diff checks | PASS |

The legacy-byte regressions, remote-block recovery, post-scan counter extension
and unrelated-note blocker preservation each failed before their corresponding
repair and passed afterward. An earlier concurrent client-suite run exceeded
wall-clock waits in two existing connection tests during release compilation;
the complete final serialized suite passed without changing their deadlines.

## Runtime deployment

Both fixes are committed on `main` and pushed:

- Agent Note: `950901f82c2fde80d2c914943ae08437750727eb`.
- GaoNote: `483f0750fb6063dd9bec7c3bb11190f624fa91ba`.

Agent Note's local build, image staged on ms04, published manifest config and
running container all match image ID
`sha256:021182dec86ee507194560547aa7df2e04494ae0e0e54bbce07fe4f1b6b3fb1e`.
The published manifest digest is
`sha256:858092cccca8a0a49b3146ebce6d5f56470cc71c9d926b2650f12883f0ee4788`.
Only `podman-agent-note.service` was restarted on ms04; its unit is active and
both the host and public HTTP checks returned 200.

Only the named local `gsmlg-umbrella` devenv application was stopped and started.
Its BEAM PID changed from 17533 to 481976, while the original PostgreSQL PID
2919577 was preserved. No GaoNote production restart was performed.

## Live Direct WebSocket acceptance

PASS after both full reconciliation scans completed and a further 72.1 seconds
of connected, error-free idle observation, spanning two 30-second heartbeat
periods. All 219 samples used the same connection attempt
(`last_attempt_at = 1792493179004`); none reported pending work, blocked notes,
errors or completed transfers.

| Final counter | Sending to GaoNote | Receiving from GaoNote |
| --- | ---: | ---: |
| Notes discovered / checked / already equal | 1,761 / 1,761 / 1,761 | 1,761 / 1,761 / 1,761 |
| Pages discovered / checked | 56 / 56 | 56 / 56 |
| Planned / delivered / remaining / blocked | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |
| Manager | Idle; initial scan complete | Idle; watching durable changes |

Initial attachment verification is still slow: the observer recorded full
convergence after approximately 17.4 minutes. Read-only runtime inspection
showed object-storage metadata requests temporarily holding the GaoNote peer
row lock while the other comparison direction waited. Queue/counter movement
confirmed continuing progress. The observer's initial 15-minute observation
budget was extended without restarting or resetting the WebSocket. No storage
or connection deadlines were changed by this correction.

A real Chromium visit to `https://agent-note.gsmlg.net/system/sync` returned
HTTP 200 and displayed these same counts. Both 1440-pixel desktop and 390-pixel
mobile layouts had no horizontal overflow. There were no page errors or failed
requests. The receiving panel displayed "Watching for changes" with zero
delivered notes.

Runtime evidence was captured in
`/tmp/agent-note-sync-echo-acceptance/{result.json,samples.jsonl,latest-status.json}`
and `/tmp/agent-note-sync-echo-browser-after/{result.json,counts.png,desktop.png,mobile.png}`.
These files are local validation artifacts and are not committed.
