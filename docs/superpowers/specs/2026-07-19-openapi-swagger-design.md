# OpenAPI and Swagger UI Design

## Goal

Publish a generated OpenAPI contract and an interactive Swagger UI for every
REST endpoint exposed by `note-server`. Keep the Streamable HTTP MCP endpoint at
`/mcp` outside the OpenAPI surface because MCP provides its own protocol-level
discovery and schemas.

## Scope

The OpenAPI document covers these REST paths and operations:

- `GET` and `POST /api/notes`
- `GET /api/notes/count`
- `GET /api/notes/{id}`
- `PUT /api/notes/{id}`
- `DELETE /api/notes/{id}`
- `GET /api/notes/{id}/raw`
- `GET /notes/{id}/content`
- `GET /api/notes/{id}/attachments/{path}`
- `POST /api/notes/search`
- `POST /api/render`
- `GET /api/trash`
- `POST /api/trash/restore`
- `DELETE /api/trash/{id}`
- `GET /api/dashboard`
- `GET` and `POST /api/labels`
- `PUT /api/labels/{key}`
- `DELETE /api/labels/{key}`
- `GET /api/system/config`
- `PUT /api/system/config`
- `GET /api/system/info`
- `GET /api/system/backup`

The feature adds two endpoints:

- Swagger UI at `/api/docs`
- OpenAPI JSON at `/api/openapi.json`

The Swagger UI and JSON specification are available whenever the HTTP server is
running. There is no separate feature flag or runtime configuration switch.

The following are out of scope:

- Documenting `/mcp` in OpenAPI
- Adding authentication or authorization
- Changing any existing REST request, response, status, or route
- Generating client SDKs
- Publishing a separate static specification artifact
- Refactoring business logic outside the HTTP transport

## Chosen Approach

Use code-first OpenAPI generation with:

- `utoipa` major version 5 for schemas and operation metadata
- `utoipa-axum` version 0.2 for Axum-aware route registration
- `utoipa-swagger-ui` major version 9 with the `axum` and `vendored` features

These versions support the repository's Axum 0.8 stack. Vendored Swagger UI
assets avoid CDN access at runtime and network downloads during normal builds.

This approach is preferred over Aide because it requires less restructuring of
the existing extractors and response types. It is preferred over a checked-in
OpenAPI JSON or YAML file because route registration and documentation remain
coupled in Rust code instead of relying on manual synchronization.

## Architecture

OpenAPI remains a `note-server` transport concern. No OpenAPI dependencies or
annotations are added to `note-core`, `note-storage`, `note-pipelines`,
`note-mcp`, or the frontend.

Each existing REST module continues to own its handlers and route definitions:

- `notes_api` owns notes, trash, dashboard, rendering, and attachment routes.
- `labels_api` owns label-catalog routes.
- `system_api` owns configuration, system information, and backup routes.

Their router factories return `OpenApiRouter<Arc<Context>>` instead of plain
`Router<Arc<Context>>`. Each handler is registered with Utoipa's route macro so
the Axum method router and OpenAPI operation are collected together.

`main` merges the three REST OpenAPI routers and then splits the result into:

1. The existing Axum REST router, which receives `Context` state.
2. One generated `OpenApi` value.

The REST router is merged with the unchanged MCP router. A `SwaggerUi` router is
then merged with the application and receives the generated document at
`/api/openapi.json`. Existing request-body limits and static-SPA fallback
behavior remain unchanged. Because the documentation endpoints are registered
before the fallback, packaged builds serve the documentation rather than
returning the frontend shell.

An `openapi` module owns top-level API metadata, shared documentation-only
schemas, tag descriptions, and Swagger UI construction. The document title is
`Agent Note HTTP API`; its version comes from `CARGO_PKG_VERSION`. Operations
are grouped under the tags `notes`, `trash`, `dashboard`, `rendering`, `labels`,
and `system`.

## Schemas

Request and response DTOs declared in `note-server` derive `ToSchema` in
addition to their existing Serde derives. Schema annotations describe defaults,
optional values, limits, allowed label value types, and binary/base64 fields
where those constraints already exist in runtime behavior.

`SystemConfig` and `SystemInfo` are defined outside `note-server`. Their runtime
types remain unchanged and do not gain an OpenAPI dependency. The `openapi`
module defines documentation-only schema mirrors for these two JSON shapes and
their nested configuration types. Operation metadata references the local
schema mirrors while handlers continue to deserialize and serialize the real
types.

Documentation-only mirrors must exactly match the serialized field names,
optionality, nesting, and primitive types of the runtime types. Focused tests
compare property names from representative serialized runtime values with the
published schema properties so a future runtime shape change cannot silently
leave the contract stale.

Common error responses remain `text/plain`; this work does not introduce a new
JSON error envelope. Each operation documents only status codes its handler can
currently return.

## Non-JSON Responses

The generated contract explicitly declares:

- `text/markdown` and `text/html` for note content endpoints
- The stored attachment MIME type as a binary response for attachment downloads
- `text/html` for `/api/render`
- `application/gzip` for `/api/system/backup`
- Empty bodies for successful `204 No Content` responses
- `text/plain` for handler errors

The two note-content paths are both documented even though they use the same
handler. The attachment catch-all is represented as a required string path
parameter named `path`.

## Security

The server currently has no application-level authentication, so the OpenAPI
document declares no security scheme. Swagger UI's request execution remains
enabled, including destructive note/label operations and system-configuration
updates.

The README must identify `/api/docs` and `/api/openapi.json`, state that Swagger
UI has the same unauthenticated exposure as the REST API, and repeat the
trusted-network or authenticating-reverse-proxy requirement. This feature does
not weaken or strengthen the existing security boundary.

## Testing

Implementation follows a red-green sequence with focused `note-server` tests.

The first failing test builds the real generated documentation router and
asserts:

- `GET /api/openapi.json` returns `200` and valid JSON.
- The document contains every in-scope REST path and operation.
- The document contains neither `/mcp` nor an MCP tag or schema.
- Path and query parameters are marked required or optional correctly.
- Representative JSON request and response schemas are present.
- System configuration and information schemas contain the runtime fields.
- Markdown, HTML, binary attachment, gzip, plain-text error, and empty `204`
  responses declare the correct media types.

A second failing test asserts that `GET /api/docs` either returns Swagger HTML
or redirects to the canonical Swagger UI index, and that the resulting HTML
references `/api/openapi.json`.

After implementation, run:

```sh
cargo test -p note-server openapi
cargo test -p note-server
cargo check -p note-server --all-targets
cargo fmt --all -- --check
```

No frontend tests are required because the Swagger UI is served by
`note-server` and does not modify the Yew application.

## Success Criteria

- `/api/docs` provides an interactive, same-origin Swagger UI without external
  runtime assets.
- `/api/openapi.json` provides a generated OpenAPI document.
- Every current REST operation is documented with its actual payload and media
  types.
- `/mcp` is absent from the document and its runtime behavior is unchanged.
- Existing REST behavior and static frontend serving remain unchanged.
- The README documents access and security expectations.
- Focused and full `note-server` verification passes.
