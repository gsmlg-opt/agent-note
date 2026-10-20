# Task-driven note synchronization

User direction, 2026-10-20: keep one WebSocket, determine connection health from
heartbeats, execute synchronization as tasks, exchange their states, and split
large data into chunks with visible progress. Paginate discovery from its first
database read and retain orchestration state in a manager on each peer. This supersedes the business
request deadlines in the incremental-progress design; its data integrity,
selection, finite-cohort and exact-ACK rules still apply.

## Observed defect

GaoNote scans and classifies the complete inventory before replying to
`inventory_start`. Both its scan worker and Agent Note's request have a 10-second
deadline. Live logs show useful classification continuing until the worker is
killed at exactly ten seconds; the client reconnects and repeats the scan. The
deployed queue has 445 pending notes at diagnosis time. Attachment chunking alone
does not repair a control path that treats elapsed work as a lost connection.

Agent Note also performs inbound attachment reads and durable apply inside its
message handler. Move this work outside the socket pump so a slow commit cannot
prevent heartbeat or opposite-direction traffic.

## Connection and task boundaries

After the physical connection is established, elapsed business operation time
never declares it failed. The pump continues reading, writing heartbeats and
dispatching task states during scanning, source preparation, disk/object-store
I/O, verification and database commit. Actual transport loss, heartbeat failure,
explicit pause/configuration change and authorization revocation retain their
appropriate connection lifecycle; a failed task is not a failed connection.

Do not add mTLS, change the configured address, clear pending work, reset node
identity or introduce a generic persisted job system. Existing durable pending
events, UUIDs, HLC/LWW versions, generations, fences, received-event receipts and
exact ACKs remain the recovery source of truth.

## Negotiation and wire contract

Advertise and negotiate `task_sync_v1` with `state_query_v1` using the existing
capability exchange. Preserve the four-field join and Phoenix V2 framing. New
task messages and statuses belong to the current join reference; old-session
messages cannot complete current work. Both upgraded directions use the same
contract.

After raw WebSocket connection, join and negotiation service heartbeats without
business reply deadlines. An older receiver without task support stays connected
with an upgrade-required status and performs no sync, without a reconnect loop.
Use a bounded independent I/O pump with heartbeat/control priority between
chunks. Count a heartbeat as pending when queued so a blocked writer cannot
disable its watchdog; bound queued frames and bytes.

`task_submit` contains exactly `task_id`, `operation`, and `payload`. Task IDs
are `<origin node UUID>:<positive decimal sequence>`, scoped to the current join.
Sequence starts at one and advances for each submission, independently of
Phoenix heartbeat references. Payloads reuse the existing operation codecs.

Supported operations are `inventory_start`, `state_query`, `transfer_begin`,
`attachment_begin`, `blob_chunk`, `blob_end`, `transfer_commit`,
`inventory_complete`, and `transfer_abort`. Abort carries `transfer_id` and
cannot interrupt a durable commit whose outcome is unresolved.

`task_status` contains `task_id`, `status_seq`, `state`, `phase`, `progress`,
`result`, and `error`. States are `queued`, `running`, `succeeded`, `failed`, and
`cancelled`. Sequence is monotonic for one task; terminal state is immutable.
Progress is bounded metadata; result and error are null until applicable.
Errors contain bounded, sanitized `code` and boolean `retryable`, never note bodies,
credential paths or storage keys. Status pushes drive execution; there is no
fixed wait-for-result deadline.

The successful query result retains its exact existing response. Successful
commit carries the exact durable `SyncAck`; queued/running status and complete
byte counts never acknowledge a note. Inventory task status reports real
checked/total coverage and becomes successful after local enumeration completes.
Admission lets both peers begin page comparison and delivery before complete
enumeration finishes. Subsequent comparison progress belongs to the manager,
not a change to that task's terminal result.

`task_get {task_id}` replays current state; `task_cancel {task_id}` requests safe
cancellation. Unknown, retired, or conflicting identity yields a bounded
`task_error {task_id, code}` without closing a healthy connection. These messages
do not introduce a second request/reply timeout mechanism.

## Bounds and duplicate handling

Maintain session high-water admission sequence and payload digests. At most four
active slots cover inventory, state query, transfer operation and housekeeping;
retain at most 64 terminal status records. An exact duplicate replays state and
never runs twice. A conflicting duplicate is rejected. An evicted ID below the
high-water mark is retired and must never execute again. Busy admission produces
a task failure, preserving ordering and the socket.

Keep existing 128 KiB frame and negotiated aggregate blob limits. Preserve
metadata then attachments then commit order and one note per direction. Transfer
operations use the existing transfer UUID to associate their task states.

## Chunk transfer and progress

Use existing chunks of at most 64 KiB, contiguous offsets, declared lengths and
SHA-256 checks. Initially allow one unconfirmed chunk per direction. After
writing a chunk, the receiver succeeds its task with `transfer_id`, `blob_id`,
and cumulative `received_bytes`. Advance sender byte progress only after this
confirmation. Delayed chunks or commits do not expire the connection while its
heartbeat remains healthy. No cross-connection partial-chunk resume is added;
durable event replay recovers an interrupted note.

## Paged discovery and peer managers

Each peer owns a session manager with a finite initial discovery run, source and
comparison cursors, a bounded page of candidates, a bounded page of pending
changes, and one outgoing note. Source SQL uses keyset pagination with a page
size of 32; descriptor queries have the same maximum. Do not read an unbounded
inventory into a vector/map and then split that vector for transmission.

Freeze the initial cohort with a local read snapshot. An independent producer
reads and publishes complete lightweight pages to a session-owned temporary
disk spool; consumers can compare and transfer its first page immediately.
The producer never waits for network operations. Release its database snapshot
after local discovery, rather than retaining a connection/MVCC history for the
duration of slow transfers. Agent Note captures descriptors and pending flags
in its existing consistent snapshot. GaoNote captures the finite union of note
and member IDs in a read snapshot, then preserves its existing per-note
authorization and descriptor classification before use. A maximum UUID alone
does not freeze a cohort and must not substitute for the snapshot.

Only lightweight IDs/descriptors go in the spool, never note bodies or blobs.
Publish pages atomically, keep page notifications bounded/coalesced, and clean
only the owning session's files after producers settle. Disk failure is a
local discovery-task failure with pending work preserved. Live candidate
queues and deduplication must not grow with the entire library.

Compare a page, create its required note work, and process that work with exact
ACKs before releasing page state. The source snapshot has disjoint pending and
audit lanes, each read with a bounded keyset page; emit eight pending candidates
then an audit candidate when both are available. High-UUID pending work therefore
does not wait behind all lower-UUID audit pages. Concurrent insertions/edits do not extend or
rewrite the captured cohort; durable pending records carry them into subsequent
paged change passes, including IDs below a completed source cursor. After initial
comparison, the healthy session consumes durable changes instead of periodically
rescanning the full library. Reconnect or explicit retry can start a fresh
finite comparison. Existing domain receipts remain the cross-session recovery
record; a new general-purpose persisted jobs database is unnecessary.

Each direction exposes optional `manager` progress:

```json
{"mode":"initial_scan","page_size":32,"pages_scanned":2,"pages_checked":1,
 "notes_scanned":64,"scan_complete":false,"source_cursor":"<UUID>",
 "last_progress_at":1792454400000}
```

Mode is `initial_scan` or `changes`. Page and note counters count actual work;
the total remains null until enumeration determines it. Existing checked,
planned, completed, blocked and remaining counts describe discovered work.
`last_progress_at` advances for real source, comparison or delivery progress,
not heartbeats or UI sampling. The UI distinguishes discovery still in progress
from a completed scan with known totals. Old progress with healthy heartbeats
is visible as unchanged activity, never an automatic task or connection failure.

Exact per-round note resolution and rejected-event markers reside on disk rather
than in library-sized RAM sets. Rejection markers belong to the runtime across
reconnects, until a changed event or manual retry permits another attempt.
The status API returns at most 64 diagnostic note samples plus
`blocked_notes_total`; the UI labels a partial sample as such.

In negotiated task mode, `sync_manager_status` pushes exactly `round_id`,
`status_seq` and `progress`. The first two are a UUID and a positive monotonic
u64. The first valid push establishes the peer's outbound round on this session;
sequence increases globally across its rounds. Older frames cannot update a new
run and old producers cannot emit after replacement. Identical sequence/content
may replay; conflicting content at the same sequence is invalid. Progress contains the manager fields
above plus `checked_notes` and nullable `total_notes`; optional `planned_notes`,
`equal_notes`, `superseded_notes` and `blocked_notes` describe comparison results.
Optional `state` is `running`, `idle` or `failed`, with nullable `error` containing
a stable bounded `code` and boolean `retryable`. This exposes a comparison-page
failure even after the source task's immutable success. The API maps the error
to sanitized local text. Preserve cursors and pending work on manager failure.
Emit at most every 250 ms or completed page and immediately at completion.
The receiver maps the peer's outbound manager to its own download display;
remote claimed completion never substitutes for local exact-ACK/apply counting.

## Errors, cancellation and durable work

Task failure records an actionable reason. Clean only its temporary files and
exact session-owned lease, retain unresolved durable events, and continue with
eligible work on the same socket. Inventory/query errors remain visible and may
be retried as new tasks without rejoining. Do not convert missing state updates
into fabricated success or acknowledgement.

Once immutable publication or durable apply begins, allow it to settle and
perform its success/error cleanup. Track this work outside the cancellable
socket future and drain it before session-resource reclamation or configuration
replacement. Do not abort the apply future on drop. After connection loss, exact
event receipts and retry ACKs recover a commit whose reply was lost.

## UI and acceptance

Keep current per-direction note counts and attachment progress. Add optional
`active_task` with `id`, `operation`, `state`, `phase`, `updated_at` and nullable
sanitized `error`. Display task state independently from connection state.
Older status JSON remains readable. A healthy connection can show a running or
failed task; byte completion still waits for durable note confirmation.

Required evidence:

- A real inventory dependency blocked beyond ten seconds emits prompt task
  admission, remains responsive to heartbeats/duplex traffic, then completes.
- Scan/query/preparation/apply run beyond former work deadlines with healthy
  heartbeats and retain the same socket; missing heartbeat still disconnects.
- Large and zero-byte blobs retain chunk/hash/order guarantees; sender byte
  progress follows confirmed offsets and duplicate tasks cannot append twice.
- A failed task preserves pending work and permits another note on the same
  connection. Old-session, conflicting, retired and duplicate states are safe.
- Slow inbound durable apply cannot block heartbeat and cannot be cancelled
  between publication and transaction cleanup. Lost ACK recovery remains exact.
- Scoped native, receiver and frontend tests pass; real two-peer interoperability
  covers task negotiation, chunk confirmation, failure isolation and reconnect.
- Live acceptance reports advancing checking/delivery, pending-count changes
  and any unresolved tasks, separately from image and HTTP availability.
- More than one source page begins comparison/delivery before discovery ends;
  memory queues stay bounded, a concurrent lower-UUID insertion is handled by
  the next durable-change pass, and slow network tasks retain no discovery
  database snapshot. A healthy idle session performs no periodic full scan.
