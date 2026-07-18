# agent-note

Local-inference hybrid-search notes app. See `docs/design.md` for the full design.

A Rust workspace: `note-core` (pure types/validation/RRF fusion), `note-storage` (backend-neutral
repository contracts), `note-storage-turso` (embedded Rust Turso Database adapter with title FTS
and exact dense retrieval), `note-embedding` (BGE-M3 via ONNX, with a deterministic stub for
offline dev), `note-pipelines` (save/search workflows), `note-mcp` (MCP over stdio + Streamable
HTTP), `note-server` (Axum REST + `/mcp`), and `note-frontend` (Yew/Wasm UI built with the
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
   ├── note-embedding   Embedder trait → StubEmbedder (offline) | OrtEmbedder (BGE-M3); semaphore backpressure
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

A debug/dev build atomically creates the implicit `./dev-data/config.toml` when it is missing. It
never overwrites an existing file. An explicitly selected missing file is always an error, even in
a debug build, and a release build also rejects a missing implicit file. This is the generated
development configuration:

```toml
[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
```

All relative path values—including values supplied by environment fallbacks—are resolved from the
configuration file's parent directory. Each field independently uses configuration file, then its
supported environment variable, then its built-in default. The active schemas and their fallbacks
are:

- `[database]`: `engine` (`NOTE_DB_ENGINE`, default `embed`); for `embed`, `path`
  (`NOTE_DB_PATH`, default `notes.db`). The embedded adapter is Rust Turso Database.
- `[embedding]`: `engine` (`NOTE_EMBEDDING_ENGINE`, default `local`); for `local`, optional
  `model_path` (`NOTE_MODEL_PATH`). No model path selects the deterministic stub; a path selects
  local BGE-M3 ONNX inference.
- `[attachments]`: `engine` (`NOTE_ATTACHMENTS_ENGINE`, default `filesystem`); for `filesystem`,
  `path` (`NOTE_ATTACHMENTS_DIR`, default `attachments`).

The filesystem attachment adapter stages a complete attachment set before the database
transaction, publishes it after the database commit, and aborts staged data when the transaction
fails. Reads load bytes through the same adapter, and permanent deletion removes the note's
attachment directory after deleting its database record. Its configured root must be exclusively
owned by this agent-note process: external mutation, symlinks, and multiple writers are unsupported.
The database and attachment store do not share one transaction. Because the database commit
precedes publication, a publication error can leave committed metadata referring to unavailable
bytes or to the previous published bytes. A cleanup error after permanent database deletion can
leave orphan attachment files or objects. These errors are returned or logged, but there is no
automatic reconciliation worker or durable outbox; an operator must retry the failed action,
repair publication, or remove orphan data as appropriate. Publication failure after database
commit must not be treated as a database rollback.

The following reserved values are parsed into typed active variants and receive basic
active-variant checks now. Full URL, service, credential, connectivity, and operational validation
is deferred until each adapter is activated by its implementation plan. A mode that needs one
returns a precise “not implemented yet” error rather than silently falling back. Import and export
deliberately use the stub embedder, so those modes parse OpenAI-compatible settings without
constructing that reserved embedding adapter.

```toml
[database]
engine = "pg"
url = "postgresql://agent-note@db.example.invalid/agent_note"
max_connections = 10

[embedding]
engine = "openai"
base_url = "http://embedding.example.invalid"
model = "bge-m3"
# api_key_env = "EMBEDDING_API_KEY"
timeout_secs = 30
max_retries = 3

[attachments]
engine = "s3"
bucket = "agent-note-example"
prefix = "notes"
# region = "us-east-1"
# endpoint = "http://object-store.example.invalid"
force_path_style = false
```

The corresponding fallbacks are `DATABASE_URL` and `NOTE_DB_MAX_CONNECTIONS`;
`NOTE_EMBEDDING_BASE_URL`, `NOTE_EMBEDDING_MODEL`, `NOTE_EMBEDDING_API_KEY_ENV`,
`NOTE_EMBEDDING_TIMEOUT_SECS`, and `NOTE_EMBEDDING_MAX_RETRIES`; and `NOTE_S3_BUCKET`,
`NOTE_S3_PREFIX`, `AWS_REGION`, `NOTE_S3_ENDPOINT`, and `NOTE_S3_FORCE_PATH_STYLE`.
`api_key_env` names the environment variable that a future adapter will read; omitting it means no
authentication. The self-hosted OpenAI-compatible adapter is designed to call `/v1/embeddings` with
model `bge-m3`. PostgreSQL is designed to use title FTS plus exact cosine search over 1,024-element
pgvector content vectors. S3 configuration is intended for S3-compatible object storage; it does
not create the bucket.

The System API reports backend-neutral `database_engine`, optional `database_path` and
`database_size_bytes`, plus `attachments_engine` and optional `attachments_location`. It does not
expose database credentials or embedding authentication.

## Exact hybrid retrieval

The two retrieval channels cover different note fields. Rust Turso Database FTS ranks title
matches with BM25, while dense retrieval uses an exact linear cosine-distance scan over stored
1024-component body-chunk embeddings and ranks each note by its closest matching chunk. The title
ranking has weight `3.0` and the dense body ranking has weight `1.0`; deterministic weighted RRF
combines them using `weight / (60 + rank)` and breaks equal fused scores by note ID.

The pipeline requests `clamp(requested_limit × 32, 128, 4096)` ranked IDs from each channel before
fusion, label filtering, hydration, and the requested top-k limit are applied. Dense retrieval uses
that overfetch length directly. Title FTS physically materializes a fixed pool of up to 4,096 hits,
batch-filters deleted notes, orders equal BM25 scores by note ID, and returns only the requested
overfetch length. Fusion therefore receives the same requested ranking length from both channels;
the larger fixed title pool bounds physical FTS work and makes ties deterministic within that pool.

Exact dense scanning avoids an approximate-index lifecycle and gives deterministic results; the
accepted trade-off is linear dense-search cost, which is appropriate for the current personal-notes
corpus. The planned PostgreSQL adapter keeps the same logical channels using title FTS and exact
cosine pgvector retrieval without changing pipeline or transport code.

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

`note-server` still binds its backend to loopback by default. The Debug Trunk server deliberately
binds `0.0.0.0:6221`, so the development UI and its proxied endpoints are reachable from the local
network; run it only on a trusted network. Packaged builds do not start Trunk and retain their
explicit bind/static-directory configuration.
