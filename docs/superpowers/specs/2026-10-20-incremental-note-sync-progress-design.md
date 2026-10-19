# Incremental note synchronization and visible progress

Status: approved by the user on 2026-10-20 for coordinated implementation in both
peers and the progress UI. Production deployment is a separate operation.

## Problem and requested result

After joining GaoNote, Agent Note currently replays every membership snapshot and
its attachments before servicing its durable pending queue. Reconnect starts this
enumeration again. A read-only production sample on 2026-10-20 found 1,761 tracked
notes, 445 pending notes, and successive transfers of notes outside that queue.
The socket was active, but pending work waited behind acknowledged inventory.
These are observation-time counts, not current progress or proof of convergence.

The requested behavior is: check current synchronization progress first, transfer
one note at a time with its content before its attachments, and show the current
note, total, completed and remaining work at `/system/sync`.

## Options and recommendation

1. Prioritize pending notes and expose local progress with the existing protocol.
   This is compatible with the current receiver. It still requires background
   replay because historical acknowledgements cannot prove the current remote
   state. The UI must identify remote progress as unavailable.
2. Add a negotiated, read-only state comparison to both peers, then transfer only
   differing notes. This meets the request without retransmitting unchanged
   attachments during initial reconciliation. **Recommended.**
3. Also negotiate reuse of attachments inside a changed note. This additionally
   needs receiver object validation, pinning and cleanup coordination. It is
   outside this change; changed notes retain the existing full-attachment transfer.

## Connection, scope and invariants

Agent Note initiates one persistent Phoenix WebSocket. Both directions use that
connection; status polling by the browser does not open another sync connection.
Keep heartbeats, inbound handling and ordinary requests responsive while checking
or sending. Normal note completion does not close or rejoin the channel.

Preserve the configured Direct/SSH/mTLS method and address. This request does not
change authentication, add certificate requirements, reset identities or clear
queues. GaoNote's source has changes owned by another thread; coordinate the
receiver contract with its owner before modifying that checkout.

Preserve UUID identity, label selection, latest full snapshots, HLC/LWW ordering,
generation/control fences, deletion/restore/purge behavior, durable pending work,
attachment leases and exact acknowledgements. An inventory omission never means
deletion. An acknowledgement can confirm that an older event was safely ignored;
it does not prove that the sender's content is the current remote winner.

## Round and scheduling

A reconciliation round has an ID and a finite set of distinct note UUIDs with
their captured event descriptors. Include retained deletion/control records;
never derive the work list from live note rows alone. Capture descriptors, not
attachment bytes, so checking is small and bounded by pages.

After join, publish local counts and enter `checking`. Negotiate state comparison
before requesting remote inventory. Each sender compares its descriptors against
the other peer in bounded pages. Use at most 32 descriptors per request and obey
the existing frame limit. Build a distinct-note work plan from pending events,
unresolved control records and remote differences. Checking sends no attachment
data. Report checked coverage while the plan is incomplete.

Once the plan is known, service durable pending changes before audit differences.
A finite captured round prevents continuing edits from starving its audit work;
new edits belong to the next round. Revalidate a planned descriptor before using a
comparison or acquiring its transfer lease. A superseded descriptor is not sent:
resolve its round slot as superseded and schedule the replacement next round.
Show superseded separately from confirmed deliveries. Immediately start another
round when newer durable work exists. Also reconcile on reconnect and periodically
while idle; no timer or cursor is proof of remote equality.

Each direction has at most one outgoing note transfer. The two directions may
operate concurrently on the same socket. A rejected or locally unpreparable note
remains unresolved, releases its lease, and yields to the next planned note.
Manual retry or a replacement version makes blocked work eligible again. Unknown
commit outcomes and malformed protocol frames retain existing session-error
handling; do not start another transfer over uncertain staging.

On a legacy receiver, request ordinary inventory and interleave pending work with
bounded audit work (up to eight pending selections, then one audit selection).
Never wait for a full snapshot/attachment inventory before processing pending
notes. Preserve legacy `inventory_complete` semantics after that enumeration.
Label this fallback `legacy_replay`; reconnect must not claim a fresh comparison.

## State-comparison contract

The current four-field `phx_join` request remains unchanged. The upgraded receiver
advertises `state_query_v1` in an optional `capabilities` array in its join reply.
Missing capabilities mean legacy mode. Do not send extension events to a receiver
that has not advertised them: the existing receiver closes on unknown events.

After installing its inbound query handler, Agent Note requests `sync_negotiate`
with `{"capabilities":["state_query_v1"]}`. GaoNote echoes the accepted capabilities
in its normal Phoenix reply. Validate unique advertised capabilities and exact
negotiation fields. Negotiation is immutable after inventory begins. Agent Note
then sends the existing `inventory_start {}`. GaoNote must not originate probes
before that event; Agent Note must not originate probes before negotiation
succeeds. Handle probes arriving before the inventory-start reply. Each direction
allows one outstanding query while still processing opposite-direction requests.
Legacy sessions send no negotiation or query events. A failed advertised
negotiation is a session error. The same rules apply in both directions.

A state query contains a fresh query ID and the requested exact event descriptors
(`event_id`, `note_id`, `generation`, `control`, `kind`, `version`, `snapshot_hash`).
Each response echoes the query ID and every requested descriptor exactly once,
then reports:

- association: `missing`, `attached` or `detached`;
- the peer-specific current membership descriptor, or null;
- the actual committed head version/hash, or null (deletion is included in hash);
- attachment availability: true, false or unknown.

Read membership and head consistently within the authenticated peer's scope. A
positive availability result must satisfy the receiver's committed-object
durability contract, not just find an old receipt or manifest. Missing storage
references cannot report available. Bound response sizes too. Reject wrong-session,
wrong-query, duplicate, omitted, unexpected and mismatched results. A malformed
response from a peer advertising support is an error, not evidence of equality.

A query is read-only. It must never clear pending work, retire control events,
release pins or manufacture an acknowledgement.

Skip transfer only for an unchanged, nonpending Snapshot when the remote
association is attached, generation/control match, the remote membership and
actual head both agree with the local version/hash, and all declared attachments
are confirmed available. Revalidate the local member before accepting equality.
An acknowledged Attach becomes a Snapshot with a new event ID locally, so event
ID equality is not required for this narrowly defined content comparison. Do not
apply that equivalence to fences, purge, detach, reentry or changed controls.

Pending events still require exact final ACK even if a query reports equal
content. This permits a small safe retry after a lost ACK. Remote newer versions
remain governed by existing LWW receive processing. Equal versions with different
hashes are errors. A remote fence cannot be overridden by inventing a generation.

If the remote association is missing while the unchanged local member is an
attached Snapshot, conditionally queue a fresh Attach event ID carrying the same
generation, control, version and snapshot. Preserve existing receiver fence rules.
An ignored Snapshot ACK must not be reported as restored remote membership.
Exercise this recovery transition across both peers before enabling the extension.

## One-note transfer and durability

For each planned note, use the existing wire order:

1. Prepare and verify source files locally before announcing the transfer.
2. Send the note content and full attachment manifest as the event metadata blob.
3. Send the declared attachments sequentially with byte progress.
4. Request commit; wait for the exact durable ACK before advancing delivery count
   or beginning the next note in that direction.

Notes without attachments proceed directly from metadata to commit. A note is
published as a complete note only after its attachments are present and verified.
Sending content first does not expose a broken partial note. Byte completion does
not imply durable completion. Retain current staging and time budgets; supporting
mid-transfer cancellation is a separate protocol change.

## Status API and UI

Add optional `progress` to the existing sync status response. Preserve existing
connection state, diagnostics, pending count, save and retry behavior. Report a
sample timestamp and separate upload/download rounds. Each direction reports:

- round ID, phase and basis (`remote_verified`, `local_queue`, `legacy_replay`);
- tracked/round total, checked coverage, freshly verified equal count;
- planned transfer count, durably confirmed count, superseded count;
- unresolved count and whether that count is exact, plus blocked count;
- current note UUID and bounded optional title;
- current stage: preparing, metadata, attachments, applying or awaiting commit;
- completed/total attachments and bytes sent or received for that note.

Unknown totals are null. During checking show checked coverage and "remaining
being checked" or a stated lower bound, never an invented zero. Historical ACK
counts are not "synchronized" counts. Count distinct UUIDs once per round;
retries and duplicate ACKs cannot inflate completion. Inbound progress uses its
own advertised round/observed transfers; never infer its total from local pending.
Legacy peers have no reliable download total, even after their initial inventory.
In negotiated sessions `inventory_complete` carries
`{"inventory_id":"<sender-generated UUID>","total_notes":1761}`; legacy sessions
retain `{}`. It reports the sender's distinct, authorized, classified cohort,
including blocked/control entries, and proves enumeration completion only. Until
then the download total is unknown. A total alone cannot reveal how many remote
notes still need transfer: keep download remaining unknown unless separately
established. Never derive it by subtracting received transfers from total.

For a known local round, the partition is:
`total = equal + confirmed + superseded + remaining`; blocked is a subset of
remaining. Tracked total is separate informational state. During checking the
remaining count is unknown. An exact durable ACK means confirmed processing,
not current content equality.

Place compact "Sending to GaoNote" and "Receiving from GaoNote" panels before
diagnostics. Show checking, current title/UUID, phase, total, confirmed deliveries,
remaining and blocked work. Label counts as belonging to the current round. Show
freshly equal notes separately from delivered notes. Report round completion
separately from global convergence. An "Up to date" claim requires fresh equality
after both direction rounds complete, no unresolved/blocked notes, and no newer
pending/captured changes; outbound ACKs alone cannot establish this. Show
"Awaiting confirmation" at 100% attachment bytes while commit is outstanding.

Use labelled native progress elements; unknown totals are indeterminate. Escape
and wrap titles; do not return note content, attachment paths, object keys or
credentials. Clear current activity on pause/disconnect, retain dated last-round
counts, and report reconnecting without pretending to resume a verified remote
checkpoint. Reuse status polling with its overlap guard at a two-second interval;
retain the last dated sample and show the existing error on polling failure.

## Implementation ownership and acceptance

Agent Note domain comparison belongs in `note-core`; state capture, conditional
recovery and leases belong in `note-pipelines`; Phoenix negotiation, scheduling
and runtime progress belong in `note-server`; status types and panels belong in
`note-frontend`. Keep storage/transport I/O out of core. GaoNote's owner implements
the matching negotiation, consistent state reads and sender scheduling contract.
Enable advertised support only when both query directions are implemented.

Required regressions and scoped validation:

- High-UUID pending work advances before legacy acknowledged low-UUID replay;
  continuing edits and rejected notes do not starve other work.
- Check/equal paths transfer no blobs. Stale query results cannot skip new edits;
  pending and control events retain exact-ACK processing.
- Lost ACK, Attach normalization, missing membership, fences, tombstones, newer
  heads and equal-version/hash conflicts take the specified safe paths.
- Old peers get the exact existing join request and no unadvertised events;
  negotiation/query/round identifiers and response cardinality are validated.
- Content precedes attachments; a second note waits for the first commit/rejection;
  disconnects preserve durable pending work and release temporary leases.
- Progress handles partial checks, unknown remote totals, no attachments, zero
  work, retries, blocked/superseded notes and concurrent edits without double counts.
- Frontend logic and Wasm checks pass. Browser tests exercise checking, metadata,
  attachments, ACK wait, completion, reconnect and polling recovery, with long
  escaped titles, responsive layout, accessible labels and existing save/retry.
  Older status JSON without `progress` continues to render existing controls.
- Local two-peer interoperability proves reconnect skips unchanged notes and
  attachments, transfers a changed note in order, and restores missing membership.

Implementation, source commits, remote publication and production deployment are
separate results. This proposal does not deploy or restart either service.
