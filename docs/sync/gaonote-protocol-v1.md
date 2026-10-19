# Agent Note ↔ GaoNote protocol v1

This is the receiver contract for the Agent Note client implemented in phase 1.
GaoNote's production receiver and ingress are phase 2. The local Phoenix fixture
tests interoperability; it does not provide a production receiver.

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

The `phx_join` payload and its successful reply `response` have the same shape:

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
strings or null; payloads are objects. Request replies use `phx_reply` with
`{"status":"ok","response":{...}}`. Server pushes must carry the current join
reference. Old join frames cannot complete a current request. Channel close/error
and WebSocket disconnect trigger reconnect.

| Event | Payload | Successful response |
| --- | --- | --- |
| `inventory_start` | `{}` | `{}`; start sending the remote inventory |
| `transfer_begin` | `transfer_id`, `size_bytes`, `checksum_sha256` | `{}` |
| `blob_chunk` | `transfer_id`, `blob_id`, `offset`, `data` | No request ref needed |
| `blob_end` | `transfer_id`, `blob_id` | `{}` after size/hash verification |
| `attachment_begin` | `transfer_id`, `blob_id` | `{}` |
| `transfer_commit` | `transfer_id` | The complete committed ACK |
| `inventory_complete` | `{}` | `{}` |
| `ack` | The complete ACK | Notification after an inbound commit |
| `note_rejected` | `code`, `event_id`, `note_id`, `reason` | Rejection notification when `transfer_commit` had no request ref |

`transfer_id` is a fresh UUID. `transfer_begin` describes the UTF-8 JSON encoding
of one `SyncEvent`; send it as blob `event`. After `blob_end` verifies and decodes
the event, send each attachment as `attachment:<note-scoped attachment ID>`.
The prefix distinguishes an attachment whose native ID is literally `event`
from the event metadata blob. The manifest retains the unprefixed native ID. Attachment
size/hash come from the manifest, not from `attachment_begin`. Send a `blob_end`
even for a zero-byte attachment, then `transfer_commit`.

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
Absence from an inventory never means deletion. Reconnect restarts enumeration;
event IDs, versions and membership fences make replay safe. ACK only after the
final business transaction commits. A crash or disconnect before confirmation
keeps the latest pending snapshot available for retry.

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
echo arbitrary receiver error text to System. Storage failures, malformed replies
and uncertain commits remain session errors and retry after reconnect.
If the commit has no request ref, push `note_rejected` with that same response
object. Only a rejection matching the active outbound event ID and note UUID may
terminate that transfer; stale notifications cannot block its replacement.

Agent Note sends a Phoenix heartbeat every 30 seconds and uses a 10-second
connection, ordinary request, heartbeat reply, and stalled-transfer deadline.
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
