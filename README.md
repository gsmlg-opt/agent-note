# agent-note

Hybrid-search notes app with local inference by default. See `docs/design.md` for the full design.

A Rust workspace: `note-core` (pure types/validation/RRF fusion), `note-storage` (backend-neutral
repository contracts), `note-storage-turso` (embedded Rust Turso Database adapter with title FTS
and exact dense retrieval), `note-storage-pg` (external PostgreSQL adapter with title FTS and exact
pgvector retrieval), `note-embedding` (BGE-M3 via local ONNX or a self-hosted OpenAI-compatible
service, with a deterministic stub for offline dev), `note-org` (pure Org source projection/editing,
workflow policy, dependency validation, and readiness), `note-pipelines` (save/search workflows),
`note-mcp` (MCP over stdio + Streamable HTTP), `note-server` (Axum REST + `/mcp`), and
`note-frontend` (Yew/Wasm UI built with the
[`yew-duskmoon`](https://crates.io/crates/yew-duskmoon) component library).

The frontend styling comes from the `@duskmoon-dev/core` design system, vendored as a prebuilt
stylesheet at `crates/note-frontend/duskmoon-core.css` (Trunk links it; no JS build step needed).
To refresh it: `bun add @duskmoon-dev/core` in `crates/note-frontend`, copy
`node_modules/@duskmoon-dev/core/dist/index.css` over `duskmoon-core.css`, then remove
`node_modules`/`package.json`. With no `data-theme` on `<html>`, DuskMoon automatically uses
`sunshine` in light mode and `moonlight` when the operating system prefers dark mode.

## Architecture

**Pure core, effectful shell.** Business logic is a pure, I/O-free core; all side effects (DB,
inference) live at explicit boundaries and are threaded through one `Context` value rather than
global state. The crates form a strict dependency stack — each depends only on those above it:

```
note-org         pure Org source projection/editing, workflow policy,
                 dependency validation, and readiness (no I/O)

note-core        pure: Note/Label/LabelKey types, input validation, RRF rank-fusion (no I/O)
   ▲
   ├── note-storage     backend-neutral repository/session/transaction contracts
   ├── note-storage-turso
   │                    Rust Turso Database: notes + labels, title FTS/BM25 + exact dense cosine
   ├── note-storage-pg  PostgreSQL: notes + labels, title FTS/GIN + exact pgvector cosine
   ├── note-embedding   Embedder trait → StubEmbedder | local ONNX | OpenAI-compatible BGE-M3
   ▲
note-pipelines   Context + workflows that compose core/storage/embedding:
                   • save_note   — validate → atomically persist note/chunks/jobs/labels → return Note
                   • embedding worker — embed queued body chunks → atomically persist dense vectors only
                   • search_notes — title FTS + exact dense content → weighted RRF → hydrate top-k
                   • org — transport-free, revision-safe workspace/document/item commands,
                     append-only audit reads, and recovery-context assembly
   ▲
   ├── note-mcp     11 Markdown-note + 36 Org tools over stdio and Streamable HTTP (one registry)
   └── note-server  Axum REST (/api/notes, /api/labels, /api/org) + /mcp
                        ▲
                   note-frontend   Yew MVU (AppState + pure reducer) → talks to note-server over REST
```

**Org canonical persistence.** `note-org` supplies the pure Org domain and workspace-time behavior.
Storage schema v5 persists canonical Org workspaces and documents, durable execution attempts,
exclusive leases, richer append-only events, idempotent operation results, and derived, rebuildable
work-item projections. The embedded Turso adapter atomically upgrades marked v2, v3, or v4
databases to v5; PostgreSQL applies the ordered `0002_org_canonical.sql`,
`0003_org_workflow_audit.sql`, and `0004_org_claims_operational_views.sql` migrations. Markdown
notes and their storage remain unchanged. Projection rebuilds preserve attempts, events, leases,
and operation results.

The transport-free `note_pipelines::org` boundary provides atomic, revision-safe commands for
workspace and document management and for item creation, follow-ups, movement, reparenting,
assignment, scheduling, dependencies, and Markdown-note links. It also provides workspace-sequenced
audit pages and recovery context assembled from canonical document revisions, projections,
dependencies, weak-note availability, attempts, artifacts, origins, and cross-workspace event
lineage. It also provides deterministic, cursor-paged ready, assigned, running, blocked, review,
scheduled, upcoming-deadline, failed, expired-lease, and completed operational views. Archived
workspaces remain readable and auditable while rejecting new mutations.

Org execution and review ownership use exclusive leases with opaque fencing tokens. A claim creates
the token. Heartbeats, releases, progress, results, review actions, and ownership-sensitive state
transitions validate the current unexpired token. Reclaim atomically expires the old ownership,
records recovery, creates a new attempt and lease, and permanently invalidates stale tokens. Retry
can claim only closed failed work within its retry budget. The shared workspace concurrency limit
counts active execution and review leases and is checked in the same transaction as its effects;
idempotent operation replay returns the original result without duplicate attempts, events, state
changes, or tokens. Actor IDs are client-asserted audit data, not trusted identities.

Delivery Slice 6 exposes the same 36 Org operations through REST/OpenAPI, MCP, and four
storage-only offline commands where applicable. REST and MCP call the same pipeline boundary and
share the same `Arc<OrgContext>`; no transport duplicates policy, revision, idempotency, or lease
logic. Delivery Slice 7 adds a read-only Org-content and operations console. The workspace
management slice adds only create, complete structured update, and reversible archive over REST;
Org source and workflow operations remain read-only. The browser adds no polling or application
authentication layer.
Agent Note implements no inbound authentication, authorization, proxy-identity-header, or workspace
ACL behavior. A front proxy owns TLS, authentication, authorization, and network access; direct
exposure to an untrusted network is unsupported.

**One core, two front doors.** REST and both MCP transports call the exact same `note-pipelines`
functions — no business logic is duplicated per transport (design.md §1–2). **Hybrid retrieval**
runs Turso full-text ranking over note titles and exact dense-vector ranking over body chunks, then
fuses the two ranked lists with deterministic weighted Reciprocal Rank Fusion. Title FTS has weight
`3.0`; dense body retrieval has weight `1.0`. The resulting `score` is a fused rank score, not raw
BM25 or cosine similarity, so the UI labels the action as retrieval. **Labels** are
Kubernetes-style: each key is registered once in a catalog with a description, and notes attach
known keys with a value (at most one value per key); save auto-creates a missing key with an empty
description.

See `docs/design.md` for the full contracts and `docs/superpowers/` for the spec and build plan.

## Setup

1. Install Rust (stable), [Trunk](https://trunkrs.dev/) (`cargo install trunk`), and the
   `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).
   - Note: if Rust is managed by rustup installed via Homebrew, rustup is keg-only — put
     `/usr/local/opt/rustup/bin` (Apple Silicon: `/opt/homebrew/opt/rustup/bin`) ahead of the
     Homebrew `rust` formula on your `PATH`, otherwise `cargo` resolves to a single-target rust
     that can't build wasm and `trunk build` fails.
2. (Optional, for real local embeddings) Download the BGE-M3 int8-quantized ONNX model:
   ```
   hf download gpahal/bge-m3-onnx-int8 --local-dir ./models/bge-m3-int8.onnx
   ```
   The whole app runs without it using a deterministic stub embedder. To use the real model, edit
   the generated `dev-data/config.toml` so its embedding table is:
   ```toml
   [embedding]
   engine = "local"
   model_path = "../models/bge-m3-int8.onnx/model_quantized.onnx"
   ```
   `NOTE_MODEL_PATH` is the fallback when `model_path` is omitted. Embedding inference uses all
   detected physical CPU cores by default; set `NOTE_EMBEDDING_THREADS` to a positive integer to
   override it, or explicitly set it to `auto` to retain the default.

## Build & test

- `cargo build --workspace` — builds all native crates (everything except the wasm frontend).
- `cargo test --workspace` — runs the backend test suite.
- `cd crates/note-frontend && cargo test` — runs the frontend's native logic tests (the frontend is
  excluded from the workspace because it targets wasm; test it from inside its directory).

Run the PostgreSQL suite against a real pgvector-capable server with:

```sh
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres cargo test -p note-storage-pg
```

Without `TEST_DATABASE_URL`, PostgreSQL integration tests emit visible skip notices instead of
accessing an external database.

## Run (end to end, dev)

1. Install Trunk once:
   ```
   cargo install --locked trunk
   ```
2. From the repo root, start the backend and hot-reloading frontend together:
   ```
   cargo run
   ```
   Then open http://0.0.0.0:6221. Trunk proxies `/api` and `/mcp` to `note-server` on
   `127.0.0.1:6222`. A debug build atomically creates `./dev-data/config.toml` on first start,
   without replacing an existing file. Its generated settings store the embedded Turso database
   at `./dev-data/notes.db` and attachments under `./dev-data/attachments`.
   A note's unrendered Markdown is available at
   `GET /api/notes/{id}/raw` and `GET /notes/{id}/content`. During local development, use the
   backend URL `http://127.0.0.1:6222/notes/{id}/content` for the latter because Trunk owns the
   frontend `/notes/*` routes on port 6221. Add `?type=html` to the latter URL for a standalone,
   styled HTML document suitable for iframe embedding.

### HTTP API documentation

With the HTTP server running, open the interactive Swagger UI directly at
`http://127.0.0.1:6222/api/docs` or fetch the generated OpenAPI document from
`http://127.0.0.1:6222/api/openapi.json`. During frontend development, Trunk proxies the same
paths on port `6221`, so `/api/docs` and `/api/openapi.json` are also available there.

The OpenAPI document covers the REST API only. `/mcp` remains outside the document and retains its
own MCP protocol discovery and schemas. Swagger UI has **Try it out** enabled, including for
destructive operations, and the HTTP API is unauthenticated. Use it only on a trusted network or
behind an authenticating reverse proxy.

### Org Web operations console and workspace management

The Org console is available at five client-side routes:

- `/org` — active/archived workspace directory with counts for all operational views;
- `/org/new` — create a workspace from the structured engineering-default policy;
- `/org/:workspace_id` — one workspace's server-computed operational ledger;
- `/org/:workspace_id/settings` — edit the complete structured workspace policy;
- `/org/:workspace_id/items/:item_id` — recovery context and subject-filtered event history.

The workspace ledger exposes exactly ten views: `ready`, `assigned`, `running`, `blocked`,
`review`, `scheduled`, `upcoming_deadline` (shown as **Due soon**), `failed`, `expired_lease`, and
`completed`. The selected view, filters (`item_type`, `state`, `priority`, `tags`, `assignee`,
`from`, and `to`), opaque cursor, and page limit live in the URL. Item links carry that complete
typed state with `return_` parameters, so **Back** restores the exact originating ledger URL.
Workspace-directory archive selection, cursor, and limit are also URL-backed.

Data loads once on navigation and changes only on an explicit **Refresh** action; there is no
automatic polling. Server-provided workspace time is the primary timestamp and browser-local time
is secondary. Invalid timestamps or timezones render as **Unavailable**, and weak Markdown-note
links whose target is missing remain visible as unavailable rather than disappearing. Loading,
empty, and structured error states are explicit.

The browser uses same-origin REST. Its only mutations are `POST /api/org/workspaces`,
`PATCH /api/org/workspaces/:workspace_id`, and
`POST /api/org/workspaces/:workspace_id/archive`, attributed to `actor_id: "web-ui"` with generated
idempotent operation IDs. Org documents, work items, claims, transitions, and reviews remain
read-only. The browser never calls MCP or stores a fencing token. Agent Note itself has no
authentication, session, trusted proxy-identity-header, or workspace ACL feature; the front proxy
must own TLS, authentication, authorization, Host/origin policy, and network access.

For local development, Trunk proxies `/api/` to `127.0.0.1:6222` while retaining `/org` routes for
the Yew router. Packaged builds set `NOTE_STATIC_DIR`; any unclaimed direct load or refresh of the
five routes returns the frontend `index.html` with HTTP 200, while `/api/org` remains claimed by
the REST router.

### Org REST API

The Org REST surface has exact one-to-one parity with the 36 `org_*` MCP tools. Each generated
OpenAPI `operationId` is the matching MCP tool name:

| Method | Path | `operationId` |
| --- | --- | --- |
| `GET` | `/api/org/workspaces` | `org_list_workspaces` |
| `POST` | `/api/org/workspaces` | `org_create_workspace` |
| `GET` | `/api/org/workspaces/{workspace_id}` | `org_get_workspace` |
| `PATCH` | `/api/org/workspaces/{workspace_id}` | `org_update_workspace` |
| `POST` | `/api/org/workspaces/{workspace_id}/archive` | `org_archive_workspace` |
| `GET` | `/api/org/workspaces/{workspace_id}/documents` | `org_list_documents` |
| `GET` | `/api/org/documents/{document_id}` | `org_get_document` |
| `PUT` | `/api/org/documents/{document_id}` | `org_put_document` |
| `POST` | `/api/org/documents/{document_id}/move` | `org_move_document` |
| `POST` | `/api/org/items/{item_id}/move` | `org_move_item` |
| `POST` | `/api/org/workspaces/{workspace_id}/import` | `org_import_workspace` |
| `GET` | `/api/org/workspaces/{workspace_id}/export` | `org_export_workspace` |
| `POST` | `/api/org/workspaces/{workspace_id}/items` | `org_create_item` |
| `GET` | `/api/org/items/{item_id}` | `org_get_item` |
| `GET` | `/api/org/items/{item_id}/context` | `org_get_item_context` |
| `POST` | `/api/org/items/{item_id}/follow-ups` | `org_create_follow_up` |
| `POST` | `/api/org/items/{item_id}/assignment` | `org_assign_item` |
| `POST` | `/api/org/items/{item_id}/schedule` | `org_schedule_item` |
| `GET` | `/api/org/queue` | `org_query_queue` |
| `GET` | `/api/org/agenda` | `org_query_agenda` |
| `POST` | `/api/org/items/{item_id}/claim` | `org_claim_item` |
| `POST` | `/api/org/items/{item_id}/claim/heartbeat` | `org_heartbeat_claim` |
| `POST` | `/api/org/items/{item_id}/claim/release` | `org_release_claim` |
| `POST` | `/api/org/items/{item_id}/progress` | `org_report_progress` |
| `POST` | `/api/org/items/{item_id}/result` | `org_submit_result` |
| `POST` | `/api/org/items/{item_id}/transition` | `org_transition_item` |
| `POST` | `/api/org/items/{item_id}/retry` | `org_retry_item` |
| `POST` | `/api/org/items/{item_id}/review/request` | `org_request_review` |
| `POST` | `/api/org/items/{item_id}/review/approve` | `org_approve_item` |
| `POST` | `/api/org/items/{item_id}/review/reject` | `org_reject_item` |
| `POST` | `/api/org/items/{item_id}/dependencies` | `org_add_dependency` |
| `DELETE` | `/api/org/items/{item_id}/dependencies/{dependency_item_id}` | `org_remove_dependency` |
| `POST` | `/api/org/items/{item_id}/note-links` | `org_link_note` |
| `DELETE` | `/api/org/items/{item_id}/note-links` | `org_unlink_note` |
| `GET` | `/api/org/notes/{note_id}/work-items` | `org_list_note_work_items` |
| `GET` | `/api/org/workspaces/{workspace_id}/events` | `org_list_events` |

Read inputs use path and query parameters. Mutations, including both `DELETE` operations, use a
JSON envelope containing `schema_version`, `workspace_id` when it is not supplied by the path,
client-asserted `actor_id`, idempotent `operation_id`, and operation-specific revision, lease, and
action fields. A path ID is authoritative; any duplicated ID in a body must match it. `actor_id`
is audit attribution supplied by the caller, not a verified identity or authorization decision.
An identical operation replay returns its original result, including after restart; reusing the
operation ID for different input returns `idempotency_conflict`.

REST success bodies serialize the same pipeline DTOs returned as MCP structured content. REST
errors are always JSON with this common shape:

```json
{
  "code": "active_lease",
  "message": "work item already has an active lease",
  "details": {},
  "retryable": false
}
```

`invalid_input` and `unsupported_semantic_edit` map to HTTP 400; missing requested resources map to
404; workflow, revision, idempotency, dependency, review, lease, and retry conflicts map to 409;
`concurrency_limit` maps to 429 with `retryable: true`; and storage failures map to 500 with a safe
generic message. Protocol framing and HTTP status are transport metadata; result fields and the
four error fields have MCP/REST parity.

Raw fencing tokens are sensitive ownership proofs. Only successful claim and retry/reclaim
responses return them, and only lease-bound mutation requests accept them. They must not be logged
or copied into errors, events, queues, context, source documents, exports, or general application
state. OpenAPI marks token-bearing request fields as sensitive and contains no token-bearing read
schemas or examples.

Agent Note deliberately implements no authentication, authorization, session, or workspace ACL
behavior. It does not consume a trusted proxy-identity header and the OpenAPI document defines no
security scheme. A front proxy must own TLS, authentication, and access control, as well as the
deployment's Host/origin and network restrictions. Direct exposure to an untrusted network is
unsupported.

## Runtime configuration

Every normal entrypoint—HTTP server, MCP over HTTP, MCP over stdio, legacy `--import`/`--export`,
and the four `org` offline modes—loads a configuration file before opening storage.
`NOTE_CONFIG_PATH` selects it; a relative selector is resolved from the process working directory.
Without that variable, the path is `./dev-data/config.toml`.

The server atomically creates the implicit `./dev-data/config.toml` when it is missing. An
explicitly selected missing file is always an error. Existing configuration files that do not
contain `server.bind_addr` are atomically updated with the resolved value while preserving their
other settings. An existing bind address is never overwritten. This is the generated
configuration:

```toml
[server]
bind_addr = "0.0.0.0:6222"

[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
```

See [`config.example.toml`](config.example.toml) for every supported option, environment fallback,
default, and adapter-specific example.

The HTTP bind address independently uses `server.bind_addr`, then `NOTE_BIND_ADDR`, then
`0.0.0.0:6222`. The default exposes the REST API and `/mcp` on every network interface; run it only
on a trusted network or protect it with an authenticating reverse proxy.

All relative path values—including values supplied by environment fallbacks—are resolved from the
configuration file's parent directory. Each field independently uses configuration file, then its
supported environment variable, then its built-in default. The active schemas and their fallbacks
are:

- `[database]`: `engine` (`NOTE_DB_ENGINE`, default `embed`); for `embed`, `path`
  (`NOTE_DB_PATH`, default `notes.db`); for `pg`, `url` (`DATABASE_URL`) and
  `max_connections` (`NOTE_DB_MAX_CONNECTIONS`, default `10`). The embedded adapter is Rust Turso
  Database.
- `[embedding]`: `engine` (`NOTE_EMBEDDING_ENGINE`, default `local`); for `local`, optional
  `model_path` (`NOTE_MODEL_PATH`). No model path selects the deterministic stub; a path selects
  local BGE-M3 ONNX inference. For `openai`, `base_url` (`NOTE_EMBEDDING_BASE_URL`), `model`
  (`NOTE_EMBEDDING_MODEL`, default `bge-m3`), optional `api_key_env`
  (`NOTE_EMBEDDING_API_KEY_ENV`), `timeout_secs` (`NOTE_EMBEDDING_TIMEOUT_SECS`, default `30`), and
  `max_retries` (`NOTE_EMBEDDING_MAX_RETRIES`, default `3`).
- `[attachments]`: `engine` (`NOTE_ATTACHMENTS_ENGINE`, default `filesystem`); for `filesystem`,
  `path` (`NOTE_ATTACHMENTS_DIR`, default `attachments`). For `s3`, `bucket`
  (`NOTE_S3_BUCKET`), `prefix` (`NOTE_S3_PREFIX`, default empty), optional `region`
  (`AWS_REGION`), optional `endpoint` (`NOTE_S3_ENDPOINT`), and `force_path_style`
  (`NOTE_S3_FORCE_PATH_STYLE`, default `false`).

### PostgreSQL

PostgreSQL is a complete external storage adapter. A mandatory configuration file selecting it can
use:

```toml
[database]
engine = "pg"
url = "postgresql://user:password@db/agent_note"
max_connections = 10

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
```

Before starting Agent Note, a PostgreSQL operator with extension-management privileges must install
pgvector in the target database:

```sql
CREATE EXTENSION vector;
```

Agent Note verifies that `vector` is already installed and never runs `CREATE EXTENSION` itself.
The configured application role needs the normal schema and migration privileges required to
create and update Agent Note's tables and indexes. Title retrieval generates a `tsvector` with the
PostgreSQL `simple` text-search configuration and indexes it with GIN. Dense retrieval performs an
exact cosine-distance scan using `<=>` over `vector(1024)` body-chunk embeddings; it creates no ANN
index. System information reports the `pg` engine but never the PostgreSQL URL or credentials.

### OpenAI-compatible BGE-M3

Use a self-hosted OpenAI-compatible embeddings service with:

```toml
[embedding]
engine = "openai"
base_url = "http://embedding.internal:8000"
model = "bge-m3"
api_key_env = "EMBEDDING_API_KEY"
timeout_secs = 30
max_retries = 3
```

`api_key_env` names an environment variable that contains the bearer token; it never contains the
secret itself. Set `EMBEDDING_API_KEY` in the server process environment for the example above.
Omit `api_key_env` for a service that requires no authentication. Normal remote HTTP or MCP server
startup resolves the named variable and fails when its value is missing or blank. Legacy
`--import`/`--export` and all four `org` offline modes do not resolve that value and remain
embedding-service offline even when the named variable is absent.

The adapter preserves any non-root path in `base_url`, normalizes its trailing slash, and appends
the `v1/embeddings` suffix. The root example above therefore posts to
`http://embedding.internal:8000/v1/embeddings`; a base URL ending in `/gateway` or `/gateway/`
would post to `/gateway/v1/embeddings`. Requests use `encoding_format = "float"` and do not send
`dimensions`. Each response must contain exactly one uniquely indexed, finite 1,024-component
vector per input. Connection failures, timeouts, HTTP 429, and HTTP 5xx responses are retried with
bounded backoff; other transport errors and HTTP statuses fail immediately. `max_retries` is the
number of retries after the initial request and must be at most `10`. A successful response body is
limited to 1 MiB. Unauthenticated HTTP error bodies are limited to 4 KiB, while authenticated
response bodies are redacted.

The local engine without `model_path` uses the deterministic stub and reports fingerprint
`stub:1024`. A local engine with `model_path` and an OpenAI-compatible engine using model `bge-m3`
share the real vector-space fingerprint `bge-m3:1024`. Reconciliation has three states. A missing
stored fingerprint, including in a legacy database, adopts the configured fingerprint without
deleting existing vectors or jobs. An equal stored fingerprint leaves them unchanged. A present
but different fingerprint—including switching from the stub to BGE-M3—atomically deletes old
vectors, replaces existing embedding jobs, and queues one pending job for every active body chunk
before the scheduler starts. Import and export still load the mandatory configuration and storage
adapters, but use the deterministic stub internally and never contact the configured embedding
service; import queues missing chunks for later processing by a normal server run.

A blank retrieval query returns no results before embedding or storage access. Normal note saves
reject blank content; if an import contains blank note content, it produces no chunks or embedding
jobs. The System API exposes only `embedding_engine`, `embedding_model`, and
`embedding_fingerprint` for the embedding backend—never the base URL, API-key
environment-variable name, or secret.

### Filesystem attachments

The filesystem attachment adapter stages a complete attachment set before the database
transaction, publishes it after the database commit, and aborts staged data when the transaction
fails. Reads load bytes through the same adapter, and permanent deletion removes the note's
attachment directory after deleting its database record. Its configured root must be exclusively
owned by this agent-note process: external mutation, symlinks, and multiple writers are unsupported.
The database and attachment store do not share one transaction. Because the database commit
precedes publication, a publication error can leave committed metadata referring to unavailable
bytes or to the previous published bytes. A cleanup error after permanent database deletion can
leave orphan attachment files or objects. Do not blindly retry: a create or update may already be
committed, so retrying a create can duplicate it; retrying a permanent delete may return not found
and cannot rerun attachment cleanup. Inspect the committed database and attachment-store state,
then manually repair or publish bytes or remove orphan data as appropriate. Errors are returned or
logged, but there is no built-in retry or reconciliation command, worker, or durable outbox.
Publication failure after database commit must not be treated as a database rollback.

### S3 attachments

The S3 adapter supports AWS S3 and compatible object stores such as MinIO. A local MinIO
configuration can use:

```toml
[attachments]
engine = "s3"
bucket = "agent-note"
prefix = "attachments"
region = "us-east-1"
endpoint = "http://127.0.0.1:9000"
force_path_style = true
```

For AWS S3, normally omit `endpoint` and leave `force_path_style = false`. MinIO and similar local
services use their service endpoint and commonly require path-style addressing. `region` is
optional; omitting it uses the standard AWS region provider chain. The bucket must already exist.

Credentials never belong in TOML. They come from the standard AWS credential provider chain,
including `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional `AWS_SESSION_TOKEN`, shared AWS
profiles, web identity, container credentials, and instance roles. The adapter allows four total
SDK attempts—the initial request and at most three retries—for failures the SDK classifies as
transient.

Final object keys are `<prefix>/<note-id>/<canonical-relative-path>`, or
`<note-id>/<canonical-relative-path>` when `prefix` is empty. A write first uploads the complete
attachment set beneath a unique `.staging` prefix. After the database transaction commits, it
copies every staged object to its final key, removes obsolete final keys, and then removes staging.
Listing consumes every continuation token, and each delete request contains at most 1,000 keys.
One attachment read issues one `GetObject`; full note and export hydration read every attachment
declared by the note metadata.

The database and S3 do not share a transaction. Permanent deletion commits the database removal
before object cleanup, so a cleanup failure can leave safe orphan objects. Final publication also
happens after the database commit. If it fails, committed metadata can be temporarily unreadable;
the safe staging location is retained for manual repair. This release has no durable outbox or
reconciler, so inspect database and object state before retrying or repairing an operation.

The System API reports backend-neutral `database_engine`, optional `database_path` and
`database_size_bytes`, `attachments_engine` and optional `attachments_location`, plus the three
safe embedding fields described above. It does not expose database credentials or embedding
authentication. For S3 it reports only engine `s3` and `s3://bucket/prefix`; it never exposes
endpoint user information, profiles, access keys, secret keys, or session tokens.

## Exact hybrid retrieval

The two retrieval channels cover different note fields. Rust Turso Database FTS ranks title
matches with BM25, while dense retrieval uses an exact linear cosine-distance scan over stored
1024-component body-chunk embeddings and ranks each note by its closest matching chunk. The title
ranking has weight `3.0` and the dense body ranking has weight `1.0`; deterministic weighted RRF
combines them using `weight / (60 + rank)` and breaks equal fused scores by note ID.

The pipeline requests `clamp(requested_limit × 32, 128, 4096)` ranked IDs from each channel before
fusion, label filtering, hydration, and the requested top-k limit are applied. Dense retrieval uses
that overfetch length directly. Embedded Turso title FTS physically materializes a fixed pool of up
to 4,096 hits, batch-filters deleted notes, orders equal BM25 scores by note ID, and returns only
the requested overfetch length. Its larger fixed pool bounds physical FTS work and makes ties
deterministic within that pool.

PostgreSQL title FTS queries the generated `simple`-configuration `tsvector` through its GIN index,
filters active notes in SQL, ranks matches with `ts_rank_cd` and a note-ID tie-break, and applies the
requested overfetch length directly as its SQL `LIMIT`. Both adapters feed the resulting title and
content rankings into the same weighted RRF pipeline.

Exact dense scanning avoids an approximate-index lifecycle and gives deterministic results; the
accepted trade-off is linear dense-search cost, which is appropriate for the current personal-notes
corpus. The PostgreSQL adapter keeps the same logical channels using title FTS and exact cosine
pgvector retrieval without changing pipeline or transport code.

The adapter intentionally makes a clean break from databases created by the retired storage
implementation, and no in-place legacy-data migration is guaranteed. Delete and recreate only
disposable test/development databases. Export or back up any non-disposable database before
upgrading so its data can be recovered or imported deliberately.

### Ordinary-note revisions and concurrent mutations

Every ordinary-note read returns a positive `revision`. Detail, list, and Trash responses expose
that value; clients must keep it with the data they loaded. Every mutation of an existing note is
an atomic compare-and-swap and requires that caller-held value as `expected_revision`. A successful
logical mutation increments the revision exactly once. A rejected stale mutation changes neither
the note nor its labels, chunks, attachment metadata, or embedding jobs.

REST full updates include `expected_revision` in the `PUT /api/notes/{id}` JSON body. Soft delete
and permanent delete send it as the `expected_revision` query parameter. Trash restore posts
`{"notes":[{"id":"...","expected_revision":5}]}`; each selected note has its own token. Missing
tokens are invalid requests. A stale token returns HTTP 409 using the common JSON error envelope:

```json
{
  "code": "stale_revision",
  "message": "the note changed after it was read",
  "details": {
    "note_id": "note-id",
    "expected_revision": 5,
    "current_revision": 6
  },
  "retryable": false
}
```

Refetch and deliberately merge or reapply local work after a conflict; never blindly retry the
stale payload. The bundled editor preserves its local draft and provides an explicit refetch and
reapply action. Note and Trash delete dialogs remain open on conflict and require a reload before
retrying.

## MCP

The same binary also speaks MCP over stdio (for MCP clients that spawn a subprocess):

```
cargo run -p note-server -- --stdio
```

This mode loads the same mandatory runtime configuration as HTTP, import, and export modes.

It exposes `save_note`, `get_note`, `read_note_lines`, `edit_note`, `update_note`, `delete_note`,
`list_notes`, `semantic_search`, `put_note_attachment`, `get_note_attachment_content`, and
`delete_note_attachment`. Label-key management is REST/UI-only. `list_notes` returns exactly `id`,
`title`, `labels`, `created_at`, `updated_at`, and `revision` for each result. `semantic_search`
returns `id`, `title`, `labels`, `created_at`, `updated_at`, and `score`. Neither response includes
note content or attachments.

`get_note` and `update_note` return note content plus attachment metadata (`id`, `path`, `mime`, and
`description`) without attachment bytes, plus the authoritative note `revision`. `read_note_lines`
returns both `revision` and the content-specific `tag`; `edit_note` requires both. `update_note`,
`delete_note`, `put_note_attachment`, and `delete_note_attachment` also require
`expected_revision`. Successful save, update, edit, and attachment-put results expose the resulting
revision. MCP `save_note` and `update_note` do not accept inline
attachments. Create or replace one attachment with `put_note_attachment`, and remove one with
`delete_note_attachment`; both address it by its stable `attachment_id`. An existing attachment id
cannot change its normalized path, so rename an attachment with delete followed by put.
`get_note_attachment_content` is the only MCP tool that reads attachment bytes. It returns exactly
one content representation: direct `content` when the bytes are valid UTF-8, otherwise canonical
Base64 in `content_base64`. These changes apply only to MCP; REST attachment behavior is unchanged.

Ordinary-note MCP conflicts use the same `code`, `message`, `details`, and `retryable` fields as
REST, including `stale_revision` with the expected and current revision. Missing required inputs
are protocol caller errors; missing notes are `not_found`; storage failures use safe messages.

The same registry also exposes exactly these 36 Org tools over both transports (47 tools total):

```text
org_list_workspaces       org_create_workspace      org_get_workspace
org_update_workspace      org_archive_workspace     org_list_documents
org_get_document          org_put_document           org_move_document
org_move_item             org_import_workspace       org_export_workspace
org_create_item           org_get_item               org_get_item_context
org_create_follow_up      org_assign_item            org_schedule_item
org_query_queue           org_query_agenda           org_claim_item
org_heartbeat_claim       org_release_claim          org_report_progress
org_submit_result         org_transition_item        org_retry_item
org_request_review        org_approve_item            org_reject_item
org_add_dependency        org_remove_dependency      org_link_note
org_unlink_note           org_list_note_work_items   org_list_events
```

Client-supplied Org `actor_id` values are asserted audit attribution, not authenticated identities.
Opaque fencing tokens are sensitive ownership proofs: only successful claim and retry/reclaim
results return them. Do not log them or expose them through general reads, errors, events, or
exports. Agent Note has no inbound authentication, authorization, sessions, or trusted
proxy-identity-header contract; a front proxy is responsible for TLS, authentication,
authorization, Host/origin, and network restrictions. The exact same 36 operation names are REST
`operationId`s under `/api/org`; the Org console consumes the GET subset plus only workspace
create/update/archive and adds no application-authentication behavior.

### Org offline commands

The binary provides four storage-only modes:

```text
note-server org export-workspace --workspace-id <uuid> --output <directory>
note-server org import-workspace --input <directory> --mode create|update \
  --actor-id <id> --operation-id <id>
note-server org export-document --document-id <uuid> --output <file.org>
note-server org import-document --workspace-id <uuid> --document-id <uuid> \
  --path <org-path> --input <file.org> --mode create|update \
  --actor-id <id> --operation-id <id> [--expected-revision <n>]
```

Document `update` requires `--expected-revision`; `create` rejects it and rejects an existing ID.
Workspace update uses the workspace and document revisions embedded in its export. A workspace
snapshot has this portable layout:

```text
manifest.json
documents/<document-uuid>.org
```

The manifest records its format version, workspace metadata/policy/revision, and each document's
stable ID, canonical Org path, revision, content hash, and relative UUID filename. Imports validate
the complete snapshot before mutation; exports write atomically and do not implicitly replace a
non-empty destination. On Linux, Android, and Apple-vendor targets, publication uses an atomic
`NOREPLACE` rename. On Windows and other targets without that primitive, exports fail closed with
an `Unsupported` publish error; imports remain supported. Every mode writes a machine-readable
report to stdout, starts only storage, and does not initialize attachments, embeddings, workers,
HTTP, MCP, or Markdown-note data.

The HTTP server additionally exposes the MCP Streamable HTTP transport at `/mcp`.

`note-server` binds its backend to `0.0.0.0:6222` by default, and the Debug Trunk server binds to
`0.0.0.0:6221`, so the development UI, REST API, and MCP endpoint are reachable from the local
network. Run them only on a trusted network. Packaged builds do not start Trunk and retain their
configured bind/static-directory behavior.
