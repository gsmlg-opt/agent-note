# Synchronization through an automatic SSH tunnel

The System connection editor supports direct, SSH and mTLS connections with a
configurable GaoNote address. In SSH mode, Agent Note establishes the tunnel
automatically using the OpenSSH client installed on its server or container.

## Setup

SSH carries a plain WebSocket from the Agent Note server to GaoNote's private
Phoenix port. This method uses an SSH identity file and verified SSH host key,
without a client TLS certificate. Key and known_hosts contents remain on the
Agent Note server.

Agent Note initiates the only WebSocket connection to GaoNote. Both sides can send
note data over that established connection. GaoNote does not connect back to Agent
Note, and no reverse tunnel or inbound Agent Note sync listener is required.

In **System → GaoNote sync → Add connection**, select **SSH tunnel** and enter:

- The GaoNote WebSocket address, such as
  `ws://127.0.0.1:4114/note-sync/websocket?vsn=2.0.0`. This address is reached
  **from the SSH host**; loopback refers to that host.
- SSH host, port (normally 22), and username.
- The SSH private-key file path on the Agent Note server/container.
- A known_hosts file path on that same server/container, containing the verified
  SSH host key for the chosen hostname and port.

Provision and mount these files before saving. Existing paths are hidden during
editing; leave them blank to keep their current values. Authentication is
noninteractive and does not prompt for a password or an unknown host key. The
profile save validates local files; actual authentication occurs when sync runs.
Provision an identity usable without an interactive passphrase. File paths may
contain spaces, but cannot contain control characters or `%`/`$` expansion tokens.

The equivalent runtime TOML seed is:

```toml
[[sync.connections]]
id = "gao_note_test"
mode = "ssh"
url = "ws://127.0.0.1:4114/note-sync/websocket?vsn=2.0.0"

[sync.connections.ssh]
host = "test.gsmlg.org"
port = 22
username = "gao"
identity_file = "/run/credentials/agent-note/ssh-key"
known_hosts_file = "/run/credentials/agent-note/known_hosts"
```

Agent Note uses `ssh -W host:port` to stream traffic through the SSH process's
stdin/stdout. It owns the process for the lifetime of the socket and cleans it up
when the attempt fails, the socket closes, sync pauses, the profile changes, or
the service shuts down. Reconnection creates a new tunnel automatically. There
is no local forwarded TCP port to configure.

## GaoNote receiver setup

GaoNote added a certificate-free SSH receiver in
[commit 7c1bdbaa](https://github.com/gsmlg-dev/gsmlg_umbrella/commit/7c1bdbaa2fa86dc95721e01ce42d050e07b097b5),
closing [issue #11](https://github.com/gsmlg-dev/gsmlg_umbrella/issues/11).
Its independent `NoteSyncSSHEndpoint` listens only on `127.0.0.1`, with default
port `4114`. Enable `[note_sync].ssh_enabled`, apply the receiver migration, and
register the local node ID shown in Agent Note System with `register_ssh_peer/1`
and the allowed selectors. Follow the receiver's
[setup and provisioning guide](https://github.com/gsmlg-dev/gsmlg_umbrella/blob/7c1bdbaa2fa86dc95721e01ce42d050e07b097b5/docs/sync/ssh-tunnel.md)
for the exact operator steps.

The ordinary mTLS endpoint still requires its verified ingress; target the SSH
endpoint for certificate-free access. The SSH receiver uses the existing join
payload and does not require certificate headers, identity query parameters, or
extra sync credentials. Agent Note establishes its SSH stream automatically, so
the receiver guide's manual local-forward command is unnecessary in SSH mode.

The target loopback listener must be reachable from the SSH daemon's network
namespace. For containers, configure that private connection explicitly. Existing
mTLS peers and SSH peers have separate registration requirements; changing the
client method does not migrate the receiver's registration or reset paired UUIDs.

The receiver source and its local tests are available. Its deployment, actual SSH
access, and production synchronization still require environment validation.
Saving a client profile does not establish production interoperability.
