# Default Public Bind Design

## Goal

Make the HTTP server, including its Streamable HTTP MCP endpoint, reachable
through the host's network interfaces by default and persist the selected bind
address in the runtime configuration.

## Configuration

The configuration key is:

```toml
[server]
bind_addr = "0.0.0.0:6222"
```

Resolve the bind address in this order:

1. `server.bind_addr` from `config.toml`.
2. `NOTE_BIND_ADDR` when the file does not set `server.bind_addr`.
3. `0.0.0.0:6222` when neither source sets it.

The resolved address becomes part of `RuntimeConfig`; server startup must not
read `NOTE_BIND_ADDR` independently.

## Configuration Persistence

- When the selected implicit configuration file does not exist, create it with
  the default sections and `server.bind_addr`.
- When an existing configuration file does not contain `server.bind_addr`, add
  the resolved value atomically while preserving all existing settings.
- Never overwrite an existing `server.bind_addr`.
- Continue honoring the existing explicit-config policy: an explicitly selected
  missing configuration remains an error rather than being created silently.

MCP routing, transport behavior, and tool schemas do not change.

## Documentation

Update the README with the new `[server]` setting, precedence rules, and default
network exposure. Keep the trusted-network warning because the server does not
add authentication as part of this change.

## Verification

- Add focused configuration tests for file, environment, and default
  precedence.
- Test creation of a missing implicit configuration and migration of an
  existing configuration that lacks `server.bind_addr`.
- Assert that migration preserves existing settings and never overwrites an
  existing bind address.
- Run the focused `note-server` tests.
- Start the server without `NOTE_BIND_ADDR` and verify that an MCP
  `initialize` request succeeds through `10.100.10.10:6222/mcp`.
