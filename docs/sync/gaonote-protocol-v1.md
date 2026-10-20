# Agent Note ↔ GaoNote protocol v1

This describes the Agent Note client and GaoNote receiver/sender contract.
Local interoperability checks and production acceptance are separate results.

## Connection and authorization

The client initiates the connection and joins `note_sync:v1`; both directions of
data use that socket. GaoNote does not initiate a connection back to Agent Note.
The GaoNote address and connection method are configured in System:

- Direct: WS for a raw endpoint, or WSS with server chain/hostname verification
  and no client certificate.
- SSH: a WS endpoint reached from the configured SSH host over an automatically
  managed tunnel with explicit SSH identity and strict known_hosts verification.
- mTLS: WSS with a client certificate, defaulting to
  `wss://admin.gsmlg.org/note-sync/websocket?vsn=2.0.0`.

A configured server CA replaces the native trust store for WSS; without one the
client uses native roots. Legacy profiles with no method remain mTLS. These are
explicit choices, with no automatic plaintext, insecure-verifier or HTTP-redirect
fallback. Transport selection does not change the note wire messages below.

GaoNote must mount a dedicated `NoteSyncSocket` at `/note-sync`, with Phoenix
V2 JSON serialization and longpoll disabled. The existing anonymous `UserSocket`
and browser authentication do not authorize synchronization. For the mTLS route,
the trusted ingress must verify client certificates, overwrite identity headers,
and prevent direct access to the backend. Map the verified certificate fingerprint
to a preregistered node UUID and allowed selectors. Bind subscriptions, inbound
changes, inventories and attachments to that authenticated peer. Never derive an
actor from a client-supplied user ID. GaoNote's independent `NoteSyncSSHEndpoint`
accepts certificate-free SSH peers on its loopback listener (default port `4114`),
enabled separately with `ssh_enabled` and provisioned with `register_ssh_peer/1`.
Direct/SSH client modes do not synthesize certificate-verification headers. See
[SSH setup](ssh-tunnel.md); production interoperability must be verified against
the actual target environment.

The `phx_join` payload retains these exact four fields. Its successful reply
`response` contains the same fields, with optional capabilities described below:

```json
{
  "protocol_version": 1,
  "node_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa",
  "selectors": ["project=bear_note"],
  "max_blob_bytes": 536870912
}
```

Selectors are sorted, unique strings. Enabled selectors form an OR union;
conditions within a selector retain the existing typed AND semantics. The peer
must return exactly the authorized requested list, the same protocol version,
a different node UUID and a positive byte limit. The negotiated limit is the
smaller limit. Agent Note persists the peer identity per selected profile and
rejects an unexpected replacement node.
The selected profile can be corrected before successful peer registration; after
registration it is fixed for this database. Keep the profile selected when paused.

## Phoenix frames and transfers

Frames are JSON arrays `[join_ref, ref, topic, event, payload]`. References are
strings or null; payloads are objects. Join/negotiation replies use `phx_reply` with
`{"status":"ok","response":{...}}`. Server pushes must carry the current join
reference. Old join frames cannot complete current work. Actual transport loss
or heartbeat expiry causes reconnect. Business operation duration never declares
a task-mode connection failed. Explicit pause, revocation and unsafe protocol
rejection have separate lifecycle reasons.

### Task transport (`task_sync_v1`)

Both upgraded peers advertise and negotiate `task_sync_v1` and `state_query_v1`.
After raw WebSocket establishment, the independent socket pump services Phoenix
heartbeats during join/negotiation and all business work. There is no business
reply deadline. A receiver without task capability remains connected with
"Receiver upgrade required" and does not enter a reconnect loop.

Send each business operation as `task_submit` with exactly these fields:

```json
{"task_id":"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa:1","operation":"inventory_start","payload":{}}
```

The origin UUID is the authenticated node. Its task sequence starts at one,
increases contiguously and is scoped to the current join reference. Operations
reuse the payload codecs in the table below, including `blob_chunk`; additionally
`state_query` uses its existing query codec and `transfer_abort` carries only
`transfer_id`. No business `phx_reply` is required. Executors push task state:

```json
{"task_id":"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa:1","status_seq":2,"state":"running","phase":"checking","progress":{"checked_notes":32,"total_notes":1761},"result":null,"error":null}
```

States are `queued`, `running`, `succeeded`, `failed`, or `cancelled`. Status
sequence increases on changes; terminal status is immutable. `result` is the
existing operation result on success, otherwise null. Failed `error` is
`{"code":"<bounded stable code>","retryable":false}`; it contains no raw storage
errors, paths or note content. Inventory succeeds after the finite scan finishes;
admission permits independent local checking. Task errors remain visible while
the same healthy socket handles other work.

Both peers retain a session manager. Discovery begins with SQL keyset pages of
at most 32 records, and comparison creates work from the first available page.
A local snapshot producer publishes lightweight descriptor/ID pages to a
session-owned temporary disk spool, independently of network transfer. This
freezes the finite initial cohort without holding a database snapshot through
network waits or collecting the library into an in-memory list. Classification,
pending enumeration and live task queues are paged as well. After the initial
comparison, consume durable changes; no periodic full scan runs on a healthy
idle task session. Reconnect or explicit retry can start a new finite scan.

Manager progress continues independently after the source inventory task's
immutable success. In task mode, push `sync_manager_status`:

```json
{"round_id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","status_seq":3,"progress":{"mode":"initial_scan","page_size":32,"pages_scanned":2,"pages_checked":1,"notes_scanned":64,"scan_complete":false,"source_cursor":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","last_progress_at":1792454400000,"checked_notes":32,"total_notes":null}}
```

The envelope has exactly these three keys. The first valid push on the current
join establishes the peer's outbound round; sequence is positive u64, increasing
across all of that session's rounds. Ignore older sequences; equal sequences
must repeat identical content. Old producers cannot publish after manager
replacement. New sessions never accept old join-reference progress. These
messages describe activity and cannot acknowledge notes or authorize data.
Modes are `initial_scan` and `changes`; optional comparison counters are
`planned_notes`, `equal_notes`, `superseded_notes` and `blocked_notes`.
Classifications count distinct notes in the round, with blocked notes included
in planned work. Repeated polls and overlapping inventory pages do not add a
second classification. A delivered note remains planned after later equal audit
results, because its exact ACK already contributes to the receiver's confirmed
count. A later failed change must still be visible as blocked. During changes,
total and checked counts cover newly classified notes as well as the original
scan; source discovery counters continue to describe that original scan.
Sequenced blocked counts replace the previous remote count, including recovery
to zero, while preserving locally observed rejections.
Optional `state` is `running`, `idle` or `failed`; nullable `error` has the same
sanitized `{code,retryable}` shape as a task error. Manager failure can thus be
reported after the source task has completed, without altering its terminal
result or disconnecting the socket.
Emit actual changes at most every 250 ms or completed page, plus final state.
Keep total null until enumeration establishes it, and advance
`last_progress_at` only for actual work, never heartbeats or API sampling.

`task_get {task_id}` replays the latest state; `task_cancel {task_id}` requests
safe cancellation. These use state/error pushes too. `task_error {task_id,code}`
reports unknown, retired, invalid, or conflicting identity without failing the
connection. Keep at most four active operation slots and 64 terminal snapshots
with request digests. Retain a high-water sequence for the whole session: exact
duplicate submissions replay state, conflicting duplicates are rejected, and
evicted old IDs can never execute again.

Large metadata and attachments use at most 64 KiB raw chunks and the existing
contiguous offsets and hashes. Keep exactly one unconfirmed chunk per direction.
A successful chunk result is `{transfer_id,blob_id,received_bytes}` and must
confirm `offset + decoded_length`. Advance sender byte progress from this receipt,
not socket writes. Complete blobs still require size/hash verification; a note
counts as delivered only when `transfer_commit` succeeds with its exact durable
ACK. Task wrappers and responses must also fit the 128 KiB frame limit.

No elapsed note, scan, chunk, preparation or commit deadline closes the task-mode
session. Database/object-store errors fail the affected task after safe cleanup;
durable pending work is retained. Apply runs outside the socket pump and settles
before its files or session leases are reclaimed. Once publication/apply starts,
cancellation cannot assert rollback. An uncertain commit remains running with
phase `outcome_unknown` until durable reconciliation establishes the result.

On disconnect a new join gets a new task namespace and reconciles durable events.
Lost ACKs recover through exact idempotent event replay; partial blob bytes restart
with their note. This version does not promise cross-session chunk resumption.
See the [task design](../superpowers/specs/2026-10-20-task-driven-note-sync-design.md)
for ownership, cancellation and acceptance requirements.

### Operation codecs and legacy direct transport

The operation payloads below are also the existing legacy direct-event transport.
Their legacy reply/deadline behavior does not apply inside `task_sync_v1`.

| Event | Payload | Successful response |
| --- | --- | --- |
| `inventory_start` | `{}` | `{}`; start sending the remote inventory |
| `transfer_begin` | `transfer_id`, `size_bytes`, `checksum_sha256` | `{}` |
| `blob_chunk` | `transfer_id`, `blob_id`, `offset`, `data` | No request ref needed |
| `blob_end` | `transfer_id`, `blob_id` | `{}` after size/hash verification |
| `attachment_begin` | `transfer_id`, `blob_id` | `{}` |
| `transfer_commit` | `transfer_id` | The complete committed ACK |
| `inventory_complete` | `{}` for legacy; negotiated round ID and total below | `{}` |
| `ack` | The complete ACK | Notification after an inbound commit |
| `note_rejected` | `code`, `event_id`, `note_id`, `reason` | Rejection notification when `transfer_commit` had no request ref |

`transfer_id` is a fresh UUID. `transfer_begin` describes the UTF-8 JSON encoding
of one `SyncEvent`; send it as blob `event`. After `blob_end` verifies and decodes
the event, send each attachment as `attachment:<note-scoped attachment ID>`.
The prefix distinguishes an attachment whose native ID is literally `event`
from the event metadata blob. The manifest retains the unprefixed native ID. Attachment
size/hash come from the manifest, not from `attachment_begin`. Send a `blob_end`
even for a zero-byte attachment, then `transfer_commit`.

Prepare and verify every source attachment before sending `transfer_begin`.
GaoNote stages these verified bytes in session-owned temporary files, bounded
by the negotiated aggregate limit, and sends local 64 KiB chunks afterward.
Slow S3 preparation therefore does not create a gap within an announced transfer.
GaoNote continues servicing incoming frames and heartbeats during preparation.
Legacy transfer chunks and final commit gaps retain their 10-second inactivity deadline.
Heartbeats and unrelated messages do not extend transfer progress deadlines.

Chunks contain standard padded Base64, at most 65,536 decoded bytes. Offsets are
contiguous byte offsets beginning at zero. The client accepts one inbound transfer
at a time, caps JSON frames at 131,072 bytes, and caps event JSON plus all attachment
bytes at the negotiated aggregate limit, at most 512 MiB. It rejects undeclared,
duplicate, incomplete, oversized or corrupt blobs. Temporary files bound network
assembly memory. Final attachment reads/apply use bounded byte vectors because
the attachment store currently accepts complete objects.
Legacy attachments without stored size/checksum metadata also use bounded reads
when seeding snapshots. A pre-metadata legacy file above 512 MiB cannot seed a
snapshot; migrate, replace or remove it before initializing that scan. Immutable
objects use their stored metadata, so oversized current notes can remain pending
and editable while transfer is blocked by the negotiated limit.

Both peers must service incoming frames while sending an inventory or large blob.
Absence from an inventory never means deletion. Reconnect restarts comparison or
legacy enumeration; event IDs, versions and membership fences make replay safe. ACK only after the
final business transaction commits. A crash or disconnect before confirmation
keeps the latest pending snapshot available for retry.

## Optional incremental state comparison

An upgraded join reply advertises `"capabilities":["state_query_v1"]`. The client
installs its inbound query handler, requests `sync_negotiate` with that exact
capabilities object and requires an identical successful response before sending
`inventory_start {}`. The server originates no queries before `inventory_start`;
the client must service queries arriving before its reply. Selection is immutable
after inventory starts. Without the advertisement, neither peer sends extension
events. Failed advertised negotiation is a session error.

Either direction may issue `state_query` as a Phoenix request:

```json
{"query_id":"<fresh UUID>","items":["<complete SyncAck descriptor>"]}
```

The illustrative string represents a descriptor object, including `event_id`,
`note_id`, `generation`, `control`, `kind`, `version` and `snapshot_hash`. Requests
contain 1–32 distinct note UUIDs and must fit the 128 KiB frame limit. Each direction
allows one outstanding query; opposite queries, transfers and heartbeats continue
on the same socket. Queries read coherent committed state and never acknowledge
events, alter queues, or acquire transfer leases.

The successful response contains `query_id` and one item per request, in request
order, with no omitted, duplicate or additional results:

```json
{
  "query_id":"<same UUID>",
  "items":[{
    "requested":"<exact requested descriptor object>",
    "association":"attached",
    "member":"<current descriptor object or null>",
    "head":{
      "version":{"millis":1,"counter":0,"node_id":"<UUID>"},
      "snapshot_hash":"<64 lowercase SHA256 hex characters>",
      "attachments_available":true
    }
  }]
}
```

Association is `missing`, `attached` or `detached`. Missing association returns
null member and head. Detached associations retain the frozen control descriptor
and return null head, so later private edits are not disclosed. An attached head
may be null when its committed state is unavailable. Availability is true, false
or null (unknown); true requires actual committed objects with matching metadata,
including storage references. A legacy attachment without immutable storage
metadata can instead be verified by reading its committed note/path file with
the descriptor's size as a hard bound and checking its exact length and SHA-256.
Reject legacy descriptors above 512 MiB before reading. Missing, oversized,
truncated or checksum-mismatched files are unavailable; unsupported or transient
storage access remains unknown. These probes do not migrate attachment metadata
or alter heads, memberships, pending events, clocks, receipts or ACKs.
Old ACK records or manifests alone are insufficient.
Validate descriptors, clocks, hashes, association coherence and exact echoes.
Malformed responses and equal versions with conflicting hashes fail the session.

Only a revalidated nonpending Snapshot can skip transfer. The remote association
must be attached, generation/control must match, both its member and committed
head must match the requested version/hash, and attachments must be available.
Acknowledged Attach normalization may change event ID; this narrow comparison
accepts Attach/Snapshot content equivalence. Pending events still require exact
final ACKs, including after a lost ACK. Control records always retain fence rules.
After durably accepting an Attach, the receiver also stores its nonpending
outbound member as a fresh Snapshot with the same clocks/hash. Its receipt and
ACK retain the original Attach descriptor. A newer local winner queued as an
Attach must still obtain its own exact ACK before normalization.

A missing receiver association for an unchanged attached Snapshot conditionally
queues a fresh Attach event ID with its existing generation, control, version,
hash and attachments. This also covers an unchanged pending Snapshot. Recovery
cannot overwrite a newer pending event or invent a generation to bypass a fence.

Each sender captures a finite cohort consistently before opening attachment data,
compares and schedules it page by page, then services pending work before audit
differences with an 8:1 bound. Revalidate each slot before skipping or reserving it. Concurrent edits
supersede that slot and remain eligible in the next round. Local preparation or
definitive note rejection blocks that version and yields to the next note. Normal
completion keeps the socket open; task-mode pending work is serviced in pages
while idle. The older non-task client performs a full audit every five minutes.
Legacy peers use pending priority with bounded
audit replay, without claiming freshly verified equality.

For negotiated sessions, `inventory_complete` carries
`{"inventory_id":"<sender round UUID>","total_notes":1761}`. The total is the
distinct authorized classified cohort, including blocked and control entries.
It proves enumeration completion, not convergence. Legacy sessions retain `{}`.

The status API exposes optional dated upload/download progress: round ID, basis,
checked/equal/planned/confirmed/superseded/remaining/blocked counts and current
note/stage/attachment bytes. Optional `progress.tracked_notes` retains the local
cohort size from the last full check across smaller pending-only rounds. For a known local round,
`total = equal + confirmed + superseded + remaining`; blocked is part of remaining.
Download remaining stays unknown when only the sender's cohort total is known.
Attachment bytes at 100% still show awaiting confirmation until durable ACK.
Pause/disconnect clears current activity and retains dated round counts.

The receiver may push an `ack` before returning the matching `transfer_commit`
reply. Both confirmations repeat the full descriptor and are idempotent. Validate
every field against the durable sent descriptor before releasing its read lease.
An unsolicited or mismatched ACK must never release another event's attachments.

After discarding staging and successfully rolling back a rejected note, the receiver
may return a `transfer_commit` error with this fixed response shape:

```json
{"status":"error","response":{"code":"note_rejected","event_id":"bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb","note_id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","reason":"catalog_conflict"}}
```

The only remote reasons are `validation_failed` and `catalog_conflict`. These are
definitive per-note rejections: preserve pending work, show the affected note, and
continue the inventory and other pending notes. Cache rejected event IDs across
reconnects; a new event version or manual retry makes them eligible again. Do not
echo arbitrary receiver error text to System. In task mode, proven storage
failures settle only their task and uncertain commits remain unresolved until
durable reconciliation. Legacy direct-mode failures may reconnect.
If the commit has no request ref, push `note_rejected` with that same response
object. Only a rejection matching the active outbound event ID and note UUID may
terminate that transfer; stale notifications cannot block its replacement.

The following deadline details describe legacy direct mode only; task mode
retains connection/heartbeat limits without business-operation deadlines.
Agent Note sends a Phoenix heartbeat every 30 seconds and uses a 10-second
connection, ordinary request, heartbeat reply, and active-blob inactivity deadline.
For compatibility with senders that fetch attachments between blobs, a verified
`blob_end` starts a preparation allowance of
`40 + ceil(largest_unstarted_attachment_bytes / 1 MiB)` seconds when attachments
remain and all begun blobs are verified. This covers GaoNote's read budget plus
10 seconds for delivery, capped at 552 seconds under the 512 MiB limit. V1 permits
any attachment order, so the largest unstarted manifest entry determines the
allowance. Local verification completes before the timer starts.
`attachment_begin` restores the 10-second
blob deadline; metadata and the gap after the final attachment also retain that
short deadline. Heartbeats and unrelated messages never extend preparation.
For `transfer_commit`, GaoNote's apply budget in seconds is
`min(900, 60 + 2 * ceil(total_transfer_bytes / 1 MiB) + 5 * attachment_count)`.
Total transfer bytes include the encoded event metadata and all declared
attachment bytes. The sender waits for that budget plus 30 seconds for the
commit response. This is an absolute deadline; unrelated messages do not extend
it. An incoming handler already in progress completes, including its durable
commit and ACK, before expiry is reported; only that handler's remaining work
can delay observing the deadline. Scheduled heartbeat sends and liveness checks
continue between handlers while waiting, even when no other messages arrive.
Agent Note reconnects with jittered backoff
bounded by 60 seconds. Manual retry wakes the worker. Configuration
changes are checked every 125 milliseconds; once read, they cancel the entire
session, including inventory and chunk transmission.
Pausing retains pending snapshots; scope changes rejoin with the updated
authorized selectors.

## Event, snapshot and ACK

```json
{
  "event_id": "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
  "note_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
  "generation": {"millis": 1791676800000, "counter": 0, "node_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"},
  "control": {"millis": 1791676800000, "counter": 0, "node_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"},
  "version": {"millis": 1791676800000, "counter": 1, "node_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"},
  "kind": "attach",
  "snapshot": {
    "title": "Example",
    "content": "# Example\n",
    "labels": [{"key": "project", "value": "bear_note", "description": "", "value_type": "text"}],
    "created_at": 1791676800,
    "updated_at": 1791676800,
    "deleted_at": null,
    "attachments": []
  },
  "snapshot_hash": "<canonical snapshot SHA-256>"
}
```

The note UUID is the identity on both sides. Timestamps in snapshots are Unix
**seconds**; HLC `millis` is Unix **milliseconds**. Compare HLC tuples
`(millis, counter, node_id)` lexicographically. Seed historical versions from
`max(updated_at, deleted_at)` without making old notes appear newly modified.
Real local changes advance the durable clock, including while globally paused.
Accepting a remote winner preserves its version. Equal versions with different
hashes are protocol errors. Reusing an event ID with different content is an error.

An ACK repeats **all** event fields except `snapshot`: `event_id`, `note_id`,
`generation`, `control`, `version`, `kind`, `snapshot_hash`. Confirmations must
match the sent event exactly; an old ACK cannot remove a newer pending snapshot
or its attachment references. ACKing an ignored, correctly processed old event
confirms processing, not that its content won the merge.

| Kind | Snapshot | Meaning |
| --- | --- | --- |
| `attach` | Complete snapshot | Establish or explicitly reenter an association |
| `snapshot` | Complete snapshot | Update an active association |
| `detach` | Frozen final snapshot | Close the association and preserve both notes |
| `purge` | `null` | Permanently delete within the active association |
| `fence` | `null` | Inventory of an acknowledged closure; never rewrite content |

Membership `generation` and `control` are independent of content `version`.
`control >= generation`. Reentry requires a newer generation. Late old-generation
snapshots, detaches and ACKs cannot reopen or close the newer association. A
detached note can be edited independently. Rule changes affect membership control
without inventing a newer content version. A fence retains the closing hash for
identity checks, carries no attachments, and does not compare/apply content.
Permanent tombstones and detached fences survive removal of the local note row.
Even if a remote closure leaves labels that still match, ordinary body edits keep
the note detached. Reentry needs a false-to-true label match or an explicit rule
change that reselects the note.

Labels carry their exact key, value, description and type. Create missing catalog
keys with the supplied type. Preserve existing global catalog definitions; reject
incompatible types or descriptions atomically instead of changing the catalog,
dropping a label, or recording a hash for metadata that did not apply. Reconcile
labels and attachments as a complete set, including path swaps and removal.

Attachment descriptors contain `id`, `path`, `mime`, `description`, `size_bytes`
and lowercase `checksum_sha256`. Use normalized relative `/` paths, without `./`,
absolute paths, traversal or URI schemes. Do not rewrite Markdown links. IDs are
note-scoped public IDs; local object keys, generations and GaoNote storage IDs
never cross the wire. Duplicate IDs/paths reject the snapshot.

## Canonical snapshot hash

Hash the snapshot value alone, including all metadata and attachment checksums;
`purge` hashes the literal UTF-8 bytes `null`. SHA-256 output is lowercase hex.
The event blob also has a separate hash over its exact transmitted bytes.

1. Require labels ordered by `(key, value)` and attachments by `(path, id)`;
   do not silently reorder received arrays.
2. Recursively sort object keys by UTF-8 order. Emit compact JSON with no whitespace.
3. Emit UTF-8 Unicode directly, decimal integers, `null`, and JSON escapes for
   quote, backslash and control characters. Do not escape `/`, HTML characters,
   non-ASCII characters or U+2028. Do not normalize Unicode or line endings.
4. Keep every field, including empty descriptions and explicit `deleted_at:null`.

[hash-vectors-v1.json](hash-vectors-v1.json) contains independently generated
canonical strings and expected digests, including Unicode, control escapes and
a zero-byte attachment. The receiver must pass these vectors before integration.

## Phase 2 receiver implementation

Implement authenticated inventory, durable node/clock, UUID-preserving internal
apply, coalesced pending snapshots, membership/control fences, receipts and
committed ACKs. Capture GaoNote native and compatibility writers in the same
transaction as note/version/pending changes. Pin pending and active-read attachment
objects; serialize cleanup reservations against adding references.

GaoNote's existing attachment prepare path cannot run inside an external Repo
transaction. Stage files first, then lock/version-check/reconcile in the final
transaction; clean losing/rolled-back staging. Local revision and embedding work
advance only when content actually applies. Preserve the source winning HLC.

Acceptance needs authorization for the selected transport: live ingress and
positive/negative certificate tests for mTLS, or SSH authentication and host-key
checks for SSH. Also verify initial two-way UUID merge, both-direction labels,
attachments, trash/restore/purge, offline reentry and stale ACKs, crash/reconnect
and backup/restart evidence.
Phase 1 has no production connection, receiver deployment or production acceptance.
