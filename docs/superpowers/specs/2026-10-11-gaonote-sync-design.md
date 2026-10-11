# Agent Note ↔ GaoNote synchronization, phase 1

Approved on 2026-10-11. Implement Agent Note first. GaoNote source is a read-only integration reference in this phase; do not modify or deploy it.

## Product contract

- One selected GaoNote connection, with multiple enabled label selectors combined by OR. Conditions inside a selector retain existing AND/typed-label semantics. Empty selectors are invalid.
- Full title, Markdown, labels, timestamps, attachments, trash, restore and permanent deletion synchronize in both directions. Identity is the note UUID, never title/body similarity.
- Initial synchronization merges existing matching notes from both ends. Absence from an inventory is not a deletion.
- Concurrent writes choose the last-modified complete snapshot. Every real mutation advances a persisted hybrid logical clock (milliseconds, logical counter, node UUID); existing note revision and updated_at are not synchronization cursors.
- Removing matching labels sends a frozen final snapshot and detaches the association without deleting either copy. New edits after detachment are independent. Explicit re-entry establishes a new generation; late old events/ACKs cannot reactivate or close it.
- The global switch pauses network activity while retaining change capture and pending state. Rule changes re-evaluate the enabled union; removing one rule does not detach a note still matched by another.

## Architecture and durability

Keep wire/domain types in note-core, storage effects in the existing adapters, synchronization composition in note-pipelines, and Phoenix/TLS I/O in note-server. System controls configuration and safe status, not credentials.

Persist a stable local node identity and clock, per-note version/hash and tombstones, per-connection membership/control versions, coalesced pending snapshots, incoming receipts, bootstrap progress and attachment references. A dedicated per-record table stores these typed records; do not serialize all notes into one settings blob. Both embedded Turso and PostgreSQL must implement the same contract.

Capture synchronization changes in the same Immediate transaction as all writers: create, full/partial edits, labels, bulk labels, attachment mutations, archive import, label catalog changes, trash/restore and permanent purge. Rollback removes both changes. In-memory notifications only wake the worker. Pending records are scanned directly; no maximum sequence checkpoint can skip a later commit.

Bootstrap starts mutation capture before paginated UUID enumeration. An older scan result cannot replace a newer pending version, tombstone or detached fence. Historical seeds use existing timestamps rather than marking all old data as modified now.

Incoming snapshots preserve source UUID and winning HLC, increment local CAS revision only when actually applied, reconcile labels/attachments atomically and queue local embeddings. Replayed/older events do not mutate revision; equal versions with different hashes are protocol errors. Missing label keys retain the supplied type; incompatible existing global definitions are errors, never silently overwritten.

## Attachment safety

Wire metadata uses note-scoped attachment IDs, normalized relative slash paths, MIME, description, byte count and SHA-256. Local object keys, storage generations and GaoNote internal storage IDs never cross the wire. Reject duplicate IDs/paths, traversal, absolute paths and URI schemes; preserve Markdown text.

Stage and verify received bytes before the final database transaction. Pending and active-read references keep immutable source generations available while offline or sending. Cleanup reservations and new references serialize through Immediate transactions: an object cannot become deleting while referenced, and references cannot be added to an object already deleting. Old ACKs release only old transfer references. Preserve the existing safe-orphan behavior for uncertain commits.

## Phoenix and mTLS

Connect to `wss://admin.gsmlg.org/note-sync/websocket?vsn=2.0.0`, join `note_sync:v1`, and use Phoenix V2 JSON frames. Implement explicit protocol-version/identity negotiation, both-side inventory, offers, chunked transfer, committed ACKs, heartbeat, reconnect and bounded retry. Handle stale join references and interrupted transfers without losing pending records.

Operator-provisioned profiles hold certificate-chain/private-key/optional-CA file paths relative to the runtime config. Only profile names and safe public status reach System. mTLS and server hostname/chain verification are mandatory; no insecure/plaintext fallback or redirects. Network/credential errors remain isolated from normal note use.

GaoNote phase 2 must mount a separate fail-closed SyncSocket. A trusted mTLS ingress verifies certificate chains, overwrites identity headers, and isolates the backend. The receiver binds verified certificate fingerprint to a preregistered node and allowed label scope; it cannot trust client-supplied user/actor identity or reuse the anonymous UserSocket/browser-only mTLS authentication.

## Acceptance boundary

Meaningful scoped tests must cover both adapters, atomic rollback, every mutation family, HLC ordering, idempotence, newer-pending/old-ACK races, detach/re-entry generations, initial-scan races, tombstones and attachment cleanup/read races. Validate real local WSS mTLS and actual Phoenix 1.8.15 Channel interoperability. Compile native and Wasm interfaces and verify System behavior.

Phase 1 completion proves the Agent Note implementation against local fixtures. Live GaoNote acceptance requires phase 2 receiver, ingress configuration, provisioned certificates and real end-to-end tests. This phase does not publish, merge to main or deploy.
