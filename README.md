# agent-note

Local-inference hybrid-search notes app. See `docs/design.md` for the full design.

A Rust workspace: `note-core` (pure types/validation/RRF fusion), `note-storage` (backend-neutral
repository contracts), `note-storage-turso` (embedded Rust Turso Database adapter with exact dense
retrieval), `note-embedding` (BGE-M3 via ONNX, with a deterministic stub for offline dev),
`note-pipelines` (save/search workflows), `note-mcp` (MCP over stdio + Streamable HTTP),
`note-server` (Axum REST + `/mcp`), and `note-frontend` (Yew/Wasm UI built with the
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
   │                    Rust Turso Database: notes + labels, exact dense cosine + sparse postings
   ├── note-embedding   Embedder trait → StubEmbedder (offline) | OrtEmbedder (BGE-M3); semaphore backpressure
   ▲
note-pipelines   Context + workflows that compose core/storage/embedding:
                   • save_note   — validate → atomically persist note/chunks/jobs/labels → return Note
                   • embedding worker — embed queued chunks → atomically persist dense+sparse data
                   • search_notes — embed query → exact dense + sparse postings → RRF → hydrate top-k
   ▲
   ├── note-mcp     save_note / semantic_search tools over stdio AND Streamable HTTP (same server type)
   └── note-server  Axum REST (/api/notes, /api/labels) + /mcp; one binary, `--stdio` flag picks the door
                        ▲
                   note-frontend   Yew MVU (AppState + pure reducer) → talks to note-server over REST
```

**One core, two front doors.** REST and both MCP transports call the exact same `note-pipelines`
functions — no business logic is duplicated per transport (design.md §1–2). **Hybrid search** runs
dense (vector) and sparse (token) retrieval independently, then fuses by *rank* via Reciprocal Rank
Fusion (scores aren't directly comparable), so a result's `score` is a fused rank score, not a raw
similarity — the UI labels it accordingly. **Labels** are Kubernetes-style: each key is registered
once in a catalog with a description, and notes attach known keys with a value (at most one value
per key); save auto-creates a missing key with an empty description.

See `docs/design.md` for the full contracts and `docs/superpowers/` for the spec and build plan.

## Setup

1. Install Rust (stable), [Trunk](https://trunkrs.dev/) (`cargo install trunk`), and the
   `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).
   - Note: if Rust is managed by rustup installed via Homebrew, rustup is keg-only — put
     `/usr/local/opt/rustup/bin` (Apple Silicon: `/opt/homebrew/opt/rustup/bin`) ahead of the
     Homebrew `rust` formula on your `PATH`, otherwise `cargo` resolves to a single-target rust
     that can't build wasm and `trunk build` fails.
2. (Optional, for real embeddings) Download the BGE-M3 int8-quantized ONNX model into the
   expected local path:
   ```
   hf download gpahal/bge-m3-onnx-int8 --local-dir ./models/bge-m3-int8.onnx
   ```
   The whole app runs without it using a deterministic stub embedder. Set `NOTE_MODEL_PATH` to
   `./models/bge-m3-int8.onnx/model_quantized.onnx` to use the real model. Embedding inference uses
   all detected physical CPU cores by default; set `NOTE_EMBEDDING_THREADS` to a positive integer
   to override it, or explicitly set it to `auto` to retain the default.

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
   `127.0.0.1:6222`. By default, the embedded Turso database is stored at
   `./dev-data/notes.db` and attachments under `./dev-data/attachments`.
   A note's unrendered Markdown is available at
   `GET /api/notes/{id}/raw` and `GET /notes/{id}/content`. During local development, use the
   backend URL `http://127.0.0.1:6222/notes/{id}/content` for the latter because Trunk owns the
   frontend `/notes/*` routes on port 6221. Add `?type=html` to the latter URL for a standalone,
   styled HTML document suitable for iframe embedding.

## Storage configuration

An optional `./config.toml` may configure storage:

```toml
attachments_dir = "dev-data/attachments"

[database]
engine = "embed"
path = "dev-data/notes.db"
```

Each field resolves independently with precedence `config.toml` > environment > default. The
supported variables are:

- `NOTE_CONFIG_PATH`: selects a configuration file. A relative selector is resolved from the
  process working directory. The default `./config.toml` is optional, but an explicitly selected
  missing file is an error.
- `NOTE_DB_ENGINE`: `embed` (the default embedded Rust Turso Database). `pg` is reserved for a
  future PostgreSQL adapter and currently returns
  `PostgreSQL storage is not supported in this release`.
- `NOTE_DB_PATH`: embedded database path, default `dev-data/notes.db`.
- `NOTE_ATTACHMENTS_DIR`: attachment directory, default `dev-data/attachments`.

Relative paths inside the TOML file are based on that file's directory. Relative environment and
default paths are based on the process working directory.

## Exact hybrid retrieval

Dense retrieval uses an exact linear cosine-distance scan over stored 1024-component chunk
embeddings. Notes are ranked by their closest matching chunk. Sparse postings are ranked
independently by the best matching chunk, and the two note rankings are fused with RRF. This avoids
an approximate-index lifecycle and gives deterministic exact results; the accepted trade-off is
linear dense-search cost, which is appropriate for the current personal-notes corpus. A future
`pg` backend can introduce a different physical retrieval strategy without changing pipeline or
transport code.

The adapter intentionally makes a clean break from databases created by the retired storage
implementation. Delete and recreate disposable test/development databases. For non-disposable
data, export or back up the database before upgrading; there is no in-place legacy migration.

## MCP

The same binary also speaks MCP over stdio (for MCP clients that spawn a subprocess):

```
cargo run -p note-server -- --stdio
```

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
