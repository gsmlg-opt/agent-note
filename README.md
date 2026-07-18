# agent-note

Hybrid-search notes app with local inference by default. See `docs/design.md` for the full design.

A Rust workspace: `note-core` (pure types/validation/RRF fusion), `note-storage` (backend-neutral
repository contracts), `note-storage-turso` (embedded Rust Turso Database adapter with title FTS
and exact dense retrieval), `note-storage-pg` (external PostgreSQL adapter with title FTS and exact
pgvector retrieval), `note-embedding` (BGE-M3 via local ONNX or a self-hosted OpenAI-compatible
service, with a deterministic stub for offline dev), `note-pipelines` (save/search workflows),
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
   ▲
   ├── note-mcp     save_note / semantic_search tools over stdio AND Streamable HTTP (same server type)
   └── note-server  Axum REST (/api/notes, /api/labels) + /mcp; one binary, `--stdio` flag picks the door
                        ▲
                   note-frontend   Yew MVU (AppState + pure reducer) → talks to note-server over REST
```

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

## Runtime configuration

Every normal entrypoint—HTTP server, MCP over HTTP, MCP over stdio, `--import`, and `--export`—loads
a configuration file before opening storage. `NOTE_CONFIG_PATH` selects it; a relative selector is
resolved from the process working directory. Without that variable, the path is
`./dev-data/config.toml`.

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
startup resolves the named variable and fails when its value is missing or blank. `--import` and
`--export` do not resolve that value and remain embedding-service offline even when the named
variable is absent.

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

## MCP

The same binary also speaks MCP over stdio (for MCP clients that spawn a subprocess):

```
cargo run -p note-server -- --stdio
```

This mode loads the same mandatory runtime configuration as HTTP, import, and export modes.

It exposes `save_note`, `get_note`, `read_note_lines`, `edit_note`, `update_note`, `delete_note`,
`list_notes`, and `semantic_search`. Label-key management is REST/UI-only. Attachments in
`save_note` and `update_note` accept either UTF-8 `content` or padded Base64 `content_base64`;
attachment responses always include `content_base64` and additionally include `content` when the
bytes are valid UTF-8. The HTTP server additionally exposes the MCP Streamable HTTP transport at
`/mcp`.

`note-server` binds its backend to `0.0.0.0:6222` by default, and the Debug Trunk server binds to
`0.0.0.0:6221`, so the development UI, REST API, and MCP endpoint are reachable from the local
network. Run them only on a trusted network. Packaged builds do not start Trunk and retain their
configured bind/static-directory behavior.
