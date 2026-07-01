# Design: Note App — Workspace Architecture & Implementation Roadmap

Status: Approved (pending spec review)
Source of truth for product/pipeline contracts: [`docs/design.md`](../../design.md) (unchanged, referenced throughout).
This document adds the missing layer between that handoff spec and an implementation plan: concrete crate boundaries, directory layout, tooling choices, and phased build order.

## 1. Purpose

`docs/design.md` specifies *what* the system does (pipeline contracts, storage schema, retrieval fusion, MCP tools) but deliberately leaves crate/workspace structure as an implementation decision (§9). This document resolves that decision so an implementation plan can be written against a concrete Cargo workspace layout.

## 2. Crate Architecture

A 7-crate Cargo workspace under `crates/`:

| Crate | Kind | Responsibility |
|---|---|---|
| `note-core` | lib | Pure types (`Note`, `LabelKey`, `Label`, `SearchResult`), input validation (including "label key must be in catalog"), RRF fusion (§5). Zero I/O — this is the "pure core" the design's philosophy (§1) calls for. |
| `note-storage` | lib | libsql schema (§3) and all DB access: CRUD for `notes`/`label_keys`/`note_labels`, dense ANN query, sparse postings query, connection pooling. |
| `note-embedding` | lib | BGE-M3 inference via `ort` (ONNX Runtime): dense + sparse heads, int8 quantized (§4). `spawn_blocking` + semaphore backpressure. Exposes an `Embedder` trait so callers don't depend on the concrete ONNX backend. |
| `note-pipelines` | lib | The shared `Context` struct (pooled DB handle + embedder instance, §6) and the `save_note` / `search_notes` pipeline contracts (§6), composed from `note-core` + `note-storage` + `note-embedding`. This is the one core both front doors call (§1, §2). |
| `note-mcp` | lib | `save_note_tool` / `semantic_search_tool` definitions (§8), stdio transport, Streamable HTTP transport. Calls `note-pipelines` directly — no business logic of its own. |
| `note-server` | bin | Single binary. An entrypoint flag dispatches to either the Axum HTTP server (REST `/api/notes` + `/mcp`) or the stdio MCP loop (§2) — same binary, different startup path, each constructing its own `Context`. |
| `note-frontend` | bin | Yew/Wasm MVU app (§7), built with [Trunk](https://trunkrs.dev/). |

Dependency direction: `note-core` has no internal deps. `note-storage` and `note-embedding` depend only on `note-core` (for shared types). `note-pipelines` depends on all three. `note-mcp` and `note-server` depend on `note-pipelines`. `note-frontend` depends on none of the native crates (wasm target — talks to `note-server` over HTTP only).

## 3. Directory Layout & Tooling

```
agent-note/
├── Cargo.toml                 # workspace root
├── .gitignore                 # target/, models/*.onnx, dist/
├── README.md                  # manual BGE-M3 ONNX model download/placement steps
├── crates/
│   ├── note-core/
│   ├── note-storage/
│   │   └── schema.sql          # §3 DDL, applied at startup
│   ├── note-embedding/
│   ├── note-pipelines/
│   ├── note-mcp/
│   ├── note-server/
│   └── note-frontend/
│       └── Trunk.toml
├── models/                     # gitignored; BGE-M3 int8 ONNX weights placed here manually
└── docs/
    ├── design.md                # existing handoff spec (source of truth for contracts)
    └── superpowers/specs/       # this document
```

Decisions:
- **Storage**: local libsql file (not remote Turso) — matches design's "fully offline" principle (§1). Path configurable via env/config.
- **Model acquisition**: BGE-M3 int8 ONNX weights are **not** fetched automatically. The plan documents manual download/placement into `models/` in the README. Code is written against that expected path/format.
- **Embedder abstraction**: `note-embedding` defines an `Embedder` trait with two implementations — `StubEmbedder` (deterministic hash-based vectors, no model needed) and `OrtEmbedder` (real BGE-M3 via `ort`). Everything above the embedding layer is built and tested against the trait, so `note-storage`, `note-pipelines`, `note-mcp`, and `note-server` don't require model weights to develop or test. The trait's single method returns dense + sparse output together from one call (e.g. `embed(&self, text: &str) -> (DenseVector, SparseVector)`), preserving design.md's "one inference call" requirement (§4, §6) — it must not be split into two separate trait methods.
- **Frontend build**: Trunk (standard Yew tooling — wasm-bindgen + asset bundling + dev server).
- **Version control**: repo is git-initialized as part of this work; first commit includes `docs/design.md` and this spec.

## 4. Phased Build Order

Each phase is a vertical increment that compiles and has its own tests before the next phase starts:

1. **Workspace scaffold** — all 7 crates created with stub `lib.rs`/`main.rs`, Cargo.toml deps wired, `cargo build` succeeds across the workspace. Git init, `.gitignore`, README stub. No business logic yet.
2. **note-core** — pure types, validation, RRF fusion function (§5). Unit tests only, no I/O — TDD-friendly.
3. **note-storage** — `schema.sql` matching §3 (including `label_keys`/`note_labels`), CRUD for notes and the label-key catalog, ANN query, sparse postings query, against a local libsql file. Integration tests using a temp DB per test.
4. **note-embedding** — `Embedder` trait + `StubEmbedder`. `OrtEmbedder` implemented against the trait but only manually exercised (needs downloaded model weights, not available in-session). Unit tests use the stub.
5. **note-pipelines** — `Context` struct + `define_label_key`/`list_label_keys`/`save_note`/`search_notes` contracts (§6), composed from phases 2–4. Integration tests with `StubEmbedder` + temp DB, proving the atomic 3-table write (§3), label-key validation, and RRF fusion (§5) end-to-end.
6. **note-mcp** — tool definitions, stdio transport, Streamable HTTP transport (§8), calling `note-pipelines` directly.
7. **note-server** — Axum REST `/api/notes` + `/api/labels` (label-key catalog, REST/UI-only per §8) + `/mcp`, entrypoint flag dispatch (stdio vs HTTP), DB pool sizing for the Axum path (§9).
8. **note-frontend** — Yew MVU `AppState`/reducer, `NoteEditor` + `VectorSearch` components using yew-duskmoon-ui primitives (§7), Trunk build talking to `note-server`'s REST API.

## 5. Testing Strategy

Favor real integration over mocks wherever feasible — real libsql (temp file per test), real RRF fusion logic, real pipeline composition. The **only** stubbed dependency is the ONNX model itself (`StubEmbedder`), since real weights can't be fetched in this environment. `OrtEmbedder` is implemented but its correctness against a real model is verified manually once weights are downloaded, not in the automated test suite for phases 1–8.

## 6. Out of Scope (per docs/design.md §9 and this session's scoping)

- Tuning the RRF `k` constant, sparse weight threshold, and tag-filtering pre/post-ANN placement — explicitly deferred to implementation-time tuning per design §9.
- Multi-vector (ColBERT) output — not persisted or indexed (design §4).
- Legacy two-endpoint MCP HTTP+SSE transport — not implemented unless a specific client requires it (design §8).
- Actually downloading/converting BGE-M3 model weights — a manual, documented step outside this implementation plan's automation.
