# Design Doc: Local-Inference Hybrid-Search Notes App

Handoff spec for implementation. Stack: Rust (Axum, ort/ONNX Runtime, libsql), Yew/Wasm frontend, MCP server. This doc specifies contracts and decisions; implementation details are left to the builder.

## 1. Design Philosophy

- **Pure core, effectful shell.** Validation, fusion scoring, and DTO assembly are pure transforms. All I/O (DB, inference) is pushed to explicit boundaries and threaded through one immutable context. Do not scatter DB calls or inference calls inside business logic — pipelines take pure data in, effects happen only at named boundary steps.
- **Fully offline.** No external embedding APIs. BGE-M3 runs in-process via ONNX Runtime.
- **One core, two front doors.** REST/MCP-over-HTTP and MCP-over-stdio both call the same pipeline functions. No duplicated business logic per transport.

## 2. System Topology

```
      ┌─────────────┐        ┌──────────────────────┐
      │ stdio entry │        │ Axum: /api/notes      │
      │ (subprocess)│        │ Axum: /mcp (Streamable│
      └──────┬──────┘        │        HTTP)           │
             │                └──────────┬────────────┘
             └───────────┬────────────────┘
                          ▼
              Functional Pipelines (shared core)
                          │
           ┌──────────────┴──────────────┐
           ▼                             ▼
  BGE-M3 Inference (ORT)          Turso/libsql Storage
  dense + sparse heads,          dense ANN index +
  int8 quantized                 sparse postings + notes/tags
```

stdio is the same binary, different entrypoint flag. It constructs its own context and calls pipelines directly — never proxies through Axum. Axum is the only process that needs connection pooling.

## 3. Storage Contract

Tags are normalized (join table), not JSON-serialized — filtering must compose with ANN retrieval via real indexes.

```sql
CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE note_tags (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (note_id, tag_id)
);

CREATE TABLE notes_embeddings (
    note_id TEXT PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    embedding F32_BLOB(1024) NOT NULL
);
CREATE INDEX idx_notes_embedding ON notes_embeddings(libsql_vector_idx(embedding));

CREATE TABLE notes_sparse_weights (
    note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    token_id INTEGER NOT NULL,
    weight   REAL NOT NULL,
    PRIMARY KEY (note_id, token_id)
);
CREATE INDEX idx_sparse_token ON notes_sparse_weights(token_id);
```

Write path: `notes`, `notes_embeddings`, `notes_sparse_weights` are written in one atomic transaction. A partial write (dense persisted, sparse missing) silently degrades that note to dense-only recall — treat this as a correctness bug, not a soft failure.

## 4. Embedding Pipeline (BGE-M3)

- **Quantization**: int8. Do not drop to 4-bit — CPU ONNX Runtime kernels for INT4 are not reliably faster than int8 on CPU EP, and quantization noise directly perturbs retrieval ordering (unlike LLM generation, where it's more forgiving). If corpus size later justifies revisiting this, it requires re-embedding the full corpus — quantization levels are not mixable in the same vector space.
- **Outputs used**: dense (1024-d, CLS-pooled, L2-normalized) + sparse (token_id → weight, thresholded to drop near-zero weights before persistence). Multi-vector (ColBERT) output is computed by the model but **not persisted or indexed** initially — cost is disproportionate for a personal notes corpus. Defer unless recall on short/ambiguous queries proves insufficient in practice.
- **Threading**: inference runs via `spawn_blocking`, never inline on the async reactor.
- **Backpressure**: bound concurrent inference calls with a semaphore sized to the ONNX session's thread count. Do not rely on `spawn_blocking`'s default pool to absorb load silently — queuing should be visible at the app layer, not hidden as creeping tail latency.

## 5. Retrieval Design

Dense and sparse retrieval are independent queries against their respective indexes, each producing a ranked candidate list. Scores are not directly comparable (cosine vs. sparse dot product) — fuse by rank, not raw score:

```
RRF(note) = Σ over retrievers r: 1 / (k + rank_r(note))     # k ≈ 60, tunable
```

If dense results consistently drown out obvious exact-term matches (or vice versa), `k` is the first tuning knob — before reaching for a hand-weighted score sum.

## 6. Pipeline Contracts

**save_note**: validate input → embed content (dense + sparse, one inference call) → atomic write across the three tables (§3) → return persisted `Note`.

**search_notes**: embed query (dense + sparse) → dense ANN query + sparse postings query (parallel, both against Turso) → RRF fusion (pure function, no I/O) → hydrate tags/metadata for top-k → return `Vec<(Note, f32)>` where the score is the fused RRF score, not raw cosine.

Context/environment: one struct holding a pooled DB handle and the shared embedder instance, constructed once per process (Axum) or per stdio invocation, passed explicitly — no global/implicit state.

## 7. Frontend (Yew + duskmoon-ui)

Strict MVU: single `AppState`, all mutations go through a reducer dispatching typed actions (no direct field mutation from components).

State surface: notes list, search results (with fused score attached — label it as such in the UI, not as "similarity," since it's a rank fusion not a raw distance), loading flag, error slot.

Components: `NoteEditor` (form → emits note on submit), `VectorSearch` (query input → renders fused-score results). Use yew-duskmoon-ui primitives (`Card`, `Input`, `TextArea`, `Tag`) rather than custom equivalents.

## 8. MCP Integration

**Tools**: `save_note_tool { title, content, tags }`, `semantic_search_tool { query, limit }`. Both call the pipeline contracts in §6 directly — no MCP-specific business logic.

**Transports**: stdio (subprocess, JSON-RPC over stdin/stdout, stderr for logs) and Streamable HTTP (single `/mcp` endpoint, POST+GET, optional SSE upgrade per-response for long calls). Do not implement legacy two-endpoint HTTP+SSE — it's deprecated protocol-side; add only if a specific client requires it, as a documented exception.

## 9. Open Decisions (resolve during implementation, not before)

- Sparse weight threshold value — start conservative, tune against measured postings-table size and recall.
- Whether tag filtering happens pre- or post-ANN at current corpus scale (post-filter is fine below ~low thousands of notes; revisit if that changes).
- DB pool sizing for Axum's concurrent path (stdio is single-client, doesn't need this).