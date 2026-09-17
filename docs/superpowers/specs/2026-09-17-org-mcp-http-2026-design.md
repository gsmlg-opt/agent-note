# HTTP-only MCP endpoint split and 2026-07-28 support

## Contract

The HTTP server exposes two independent Streamable HTTP MCP endpoints:

| Endpoint | `tools/list` inventory | `tools/call` scope |
| --- | --- | --- |
| `/mcp` | 12 Markdown-note tools | Note tools only |
| `/org/mcp` | 40 `org_*` tools | Org tools only |

Calls to a tool outside an endpoint's inventory return the MCP unknown-tool
error. Both endpoints use the existing shared note and Org pipeline contexts,
stateless request processing, JSON responses for simple calls, and POST-only
transport. REST paths, Org offline commands, and their pipeline semantics do not
change. Both MCP endpoints remain outside OpenAPI.

The `--stdio` MCP runtime and its transport function are removed. Invoking the
binary with `--stdio` fails explicitly instead of starting HTTP. The shared MCP
tool handlers remain available to the HTTP transport. Legacy note import/export
stdin and the embedding worker's process stdio remain intact.

## Protocol and security

Upgrade the `rmcp` dependency to the 3.4 series, which implements the stable
MCP `2026-07-28` protocol. A 2026 client can use `server/discover`, then send
independent POST requests with protocol metadata and required MCP headers;
responses use the 2026 wire shape, including result type and cache fields where
required. The SDK continues to accept older protocol versions using its
compatibility behavior; those older requests also remain stateless in this app.
No application-level sessions or new optional MCP features are introduced.

The app accepts MCP requests without an `Origin` header. It rejects requests
that carry any `Origin` header with HTTP 403 via the SDK's explicit empty Origin
allowlist. The browser UI does not call MCP. Existing reverse-proxy controls
continue to own authentication, TLS, and public Host policy.

## Verification

Focused transport tests verify exact endpoint inventories, unknown-tool
rejection across endpoints, representative note and Org calls, HTTP method
rules, and 2026 discovery and POST wire behavior. They also verify Origin
rejection, explicit `--stdio` rejection, and OpenAPI exclusion. Existing Org
REST/MCP conformance tests call the new Org endpoint. The development Trunk
proxy forwards both MCP paths. Current README and design documentation describe
the new contract; historical plans and specifications remain historical records.
