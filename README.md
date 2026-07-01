# agent-note

Local-inference hybrid-search notes app. See `docs/design.md` for the full design.

A Rust workspace: `note-core` (pure types/validation/RRF fusion), `note-storage` (libsql + vector
search), `note-embedding` (BGE-M3 via ONNX, with a deterministic stub for offline dev),
`note-pipelines` (save/search contracts), `note-mcp` (MCP over stdio + Streamable HTTP),
`note-server` (Axum REST + `/mcp`), and `note-frontend` (Yew/Wasm UI).

## Setup

1. Install Rust (stable), [Trunk](https://trunkrs.dev/) (`cargo install trunk`), and the
   `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).
2. (Optional, for real embeddings) Download the BGE-M3 int8-quantized ONNX model and place it at
   `models/bge-m3-int8.onnx`. The whole app runs without it using a deterministic stub embedder —
   only the real `OrtEmbedder` needs it (and wiring it into `note-server` is a one-line swap in
   `crates/note-server/src/main.rs`, marked with a comment).

## Build & test

- `cargo build --workspace` — builds all native crates (everything except the wasm frontend).
- `cargo test --workspace` — runs the backend test suite.
- `cd crates/note-frontend && cargo test` — runs the frontend's native logic tests (the frontend is
  excluded from the workspace because it targets wasm; test it from inside its directory).

## Run (end to end, dev)

1. Start the backend (HTTP server on `127.0.0.1:8080`, serving REST + `/mcp`):
   ```
   NOTE_DB_PATH=notes.db cargo run -p note-server
   ```
2. In another terminal, serve the frontend (on `127.0.0.1:8081`, proxying `/api` + `/mcp` to the
   backend — see `crates/note-frontend/Trunk.toml`):
   ```
   cd crates/note-frontend && trunk serve
   ```
   Then open http://127.0.0.1:8081.

## MCP

The same binary also speaks MCP over stdio (for MCP clients that spawn a subprocess):

```
cargo run -p note-server -- --stdio
```

It exposes two tools — `save_note` and `semantic_search`. Label-key management is REST/UI-only.
The HTTP server additionally exposes the MCP Streamable HTTP transport at `/mcp`.

Both `note-server` (HTTP) and the `/mcp` transport bind to loopback only — this is a fully-offline,
single-user, unauthenticated personal app. Exposing it beyond localhost is a deliberate opt-in that
also requires widening the MCP `Host` allowlist (see `crates/note-mcp/src/http.rs`).
