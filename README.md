# agent-note

Local-inference hybrid-search notes app. See `docs/design.md` for the full design.

## Setup

1. Install Rust (stable), [Trunk](https://trunkrs.dev/) (`cargo install trunk`), and the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).
2. Download the BGE-M3 int8-quantized ONNX model and place it at `models/bge-m3-int8.onnx`.
   The rest of the pipeline (`note-storage`, `note-pipelines`, `note-mcp`, `note-server`) works
   without this file, using a stub embedder — only the real `OrtEmbedder` needs it.
3. `cargo build --workspace` builds everything except the frontend.
4. `cd crates/note-frontend && trunk serve` runs the frontend dev server.
