# Sync connection configuration in System

## Scope

The System page must let the user create and edit a GaoNote connection before
selecting it and configuring label rules. An empty connection catalog must have
an actionable setup form. The existing protocol, paired node identity, note
selection, pause, and delivery semantics stay in place.

This follows the request to make synchronization configurable in the UI. It
extends the operator-only profile setup described in the original sync design.
Certificate and private-key contents remain provisioned on the server.
The user extended this scope to configurable test-environment addresses and three
explicit methods: direct connection, automatic SSH tunnel, and mTLS. SSH is managed
by Agent Note, as selected by the user, rather than an existing host-managed forward.

## UI and API

- Add and edit connections from the GaoNote sync section. Default a new connection
  to `gao_note` and
  `wss://admin.gsmlg.org/note-sync/websocket?vsn=2.0.0`.
- Edit the GaoNote socket URL and method independently, preserving an entered
  address when changing methods. Direct accepts WS or WSS without a client
  certificate; mTLS requires WSS and client certificate/key paths. WSS can use native
  roots or a selected server CA. SSH uses a WS target address reached from the SSH
  host, SSH host/port/username, and server-side identity/known_hosts file paths.
- Paths refer to the Agent Note server or container, rather than the user's browser
  computer. The UI does not upload private-key contents. Show only fields relevant
  to the selected method, and omit hidden fields from save requests.
- Existing credential paths are write-only: omitting a replacement retains them.
  CA replacement and switching to native roots are separate explicit choices.
  GET responses and runtime errors never return credential paths or contents.
- Keep an existing connection ID stable. Updating its URL or credentials does not
  reset the registered remote node identity.
- Save a connection independently of the System rule settings. A new connection
  becomes selected in the System draft; the existing Save settings action applies
  selection, rules, and enable/pause. Block that action while the editor is open.
- Preserve drafts across runtime polling and show validation and save failures in
  the editor. Failed saves keep the previous usable connection.
- Show the local node ID once selection is saved, so the receiver can register it
  while synchronization is paused. Show the paired server ID when available.

`GET /api/system/sync/connections` returns public configuration summaries.
`PUT /api/system/sync/connections` creates or updates one stable ID. Summaries
include ID, URL, byte limit, and credential-presence flags. The existing runtime
status API keeps its public ID/URL profile contract. Connection deletion is outside
this change.

Profiles and public summaries include `mode = direct | ssh | mtls`. An omitted mode
in legacy TOML or sidecar profiles means mTLS. An omitted mode in an update retains
the existing method. SSH summaries return host, port, username, and credential
presence flags; SSH credential paths are write-only, with omitted paths retained
when editing. Changing methods clears saved credentials that do not apply.

## Persistence and activation

The deployed runtime TOML is a read-only mount. Store UI-managed connections in a
separate server-owned JSON catalog, outside the note database and note exports.
When present, this catalog is authoritative; otherwise TOML profiles seed the
catalog. The first successful save writes the entire effective catalog so that
profile precedence is deterministic across restarts.

Select the catalog path from `[sync].profiles_path`, then
`NOTE_SYNC_PROFILES_PATH`. Otherwise use `sync-connections.json` beside an embedded
database, or in the parent of a filesystem attachments directory, with the runtime
config directory as the fallback. Explicit relative paths resolve from the runtime
config directory. Resolve credential paths from that same directory and store
absolute paths in the catalog.

Validate connection syntax, method-specific credentials and readable CA/files
before saving. Atomically persist the catalog with restrictive permissions before
publishing it to the worker. A write failure must retain the old catalog and
runtime configuration. Malformed existing catalogs fail explicitly rather than
reactivating older TOML profiles.

On successful replacement, cancel the current connection session and re-read the
catalog before reconnecting. Preserve pending events and peer identity fences,
reclaiming leases through the existing cancellation path. Normal note operations
remain independent of connection failures.

## Transport lifecycle

Agent Note initiates a single WebSocket connection to GaoNote; both directions of
note data use that connection. Direct WS uses TCP; direct WSS verifies server TLS
without a client certificate; mTLS adds the configured client certificate.

SSH uses OpenSSH `ssh -W host:port` with piped stdin/stdout. The destination comes
from the configured GaoNote URL and is resolved from the SSH host. The WebSocket
handshake keeps the original URL host, path, and query. The stream owns the SSH
child, so failed handshakes, timeouts, pause, connection changes and shutdown clean
it up. Reconnection establishes a new tunnel. There is no local forwarding port
to allocate or separate tunnel daemon to manage.

Use explicit SSH identity and known_hosts files, strict host-key checking and
noninteractive key authentication. Do not invoke a shell or inherit ambient SSH
configuration that changes routing or authentication. Package OpenSSH in the
runtime image. Keep private paths and subprocess output out of runtime errors.

Changing client methods does not invent receiver authentication. GaoNote added a
dedicated loopback SSH receiver and certificate-free peer registration in commit
`7c1bdbaa2fa86dc95721e01ce42d050e07b097b5` (issue #11). Use its existing join
contract. Qualify client transports and report deployed interoperability separately.

## Verification

Cover route availability, write-only credentials, keep/clear CA behavior, strict
TLS validation, atomic failure behavior, restart precedence, and hot replacement.
Cover direct WS and server-authenticated WSS, automatic SSH over a real temporary
sshd, rejected SSH host keys and credentials, child cleanup, and legacy profile
defaults. Verify that changing methods preserves the address and drops irrelevant
credentials.
Run frontend logic tests and Wasm compilation. Exercise the actual System page
against an isolated local database through create, edit, save and reload, including
validation failures. Production connectivity requires the target environment's
GaoNote receiver and its selected method's credentials/configuration.
