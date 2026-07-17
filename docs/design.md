# Design Doc: Local-Inference Hybrid-Search Notes App

Handoff spec for implementation. Stack: Rust (Axum, ort/ONNX Runtime, Turso Database), Yew/Wasm frontend, MCP server. This doc specifies contracts and decisions; implementation details are left to the builder.

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
  BGE-M3 Inference (ORT)          Rust Turso Database Storage
  dense + sparse heads,          exact dense cosine +
  int8 quantized                 sparse postings + notes/labels
```

stdio is the same binary, selected by an entrypoint flag, and calls pipelines directly — never
proxies through Axum. The composition root resolves configuration once, constructs one shared
storage backend, and passes it explicitly to whichever transport is active.

## 3. Storage Contract

Labels are normalized (join table), not JSON-serialized. Filtering and retrieval are exposed
through backend-neutral repository contracts rather than physical database details.

Notes are categorized with Kubernetes-style labels: key=value pairs where each *key* has one catalog
entry and each *note* attaches its own value. Keys may be registered explicitly with a human-readable
description or auto-created with an empty description on first save. A note can have at most one
value per key (same semantics as k8s labels) — this is not a free-form tagging system.

```sql
CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    note_revision INTEGER NOT NULL DEFAULT 1,
    deleted_at INTEGER
);

CREATE TABLE label_keys (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    value_type TEXT NOT NULL DEFAULT 'text'
);

CREATE TABLE note_labels (
    note_id      TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id INTEGER NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value        TEXT NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);
CREATE INDEX idx_note_labels_key_value ON note_labels(label_key_id, value);

CREATE TABLE note_chunks (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    chunk_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    note_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE TABLE embedding_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    chunk_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    note_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(note_id, chunk_idx, chunk_hash)
);

CREATE TABLE note_chunk_embeddings (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    embedding F32_BLOB(1024) NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE TABLE note_chunk_sparse (
    note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    token_id INTEGER NOT NULL,
    weight   REAL NOT NULL,
    PRIMARY KEY (note_id, chunk_idx, token_id)
);
CREATE INDEX idx_note_chunk_sparse_token ON note_chunk_sparse(token_id);
```

The note write transaction atomically persists note metadata, labels, chunks, and durable embedding
jobs. A worker claims a job in a short immediate transaction, performs inference without holding a
database transaction, then atomically persists dense and sparse chunk data. Partial dense/sparse
completion is a correctness bug, not a soft failure.

Runtime storage configuration is optional. The default `./config.toml` shape is:

```toml
attachments_dir = "dev-data/attachments"

[database]
engine = "embed"
path = "dev-data/notes.db"
```

Each field resolves independently in file-over-environment-over-default order. `NOTE_CONFIG_PATH`
selects the file, `NOTE_DB_ENGINE` selects `embed` or the parsed-but-not-yet-supported future `pg`
adapter, `NOTE_DB_PATH` selects the embedded database, and `NOTE_ATTACHMENTS_DIR` selects attachment
storage. Relative TOML values are based on the selected file's directory; relative environment and
default values are based on the process working directory.

## 4. Embedding Pipeline (BGE-M3)

- **Quantization**: int8. Do not drop to 4-bit — CPU ONNX Runtime kernels for INT4 are not reliably faster than int8 on CPU EP, and quantization noise directly perturbs retrieval ordering (unlike LLM generation, where it's more forgiving). If corpus size later justifies revisiting this, it requires re-embedding the full corpus — quantization levels are not mixable in the same vector space.
- **Outputs used**: dense (1024-d, CLS-pooled, L2-normalized) + sparse (token_id → weight, thresholded to drop near-zero weights before persistence). Multi-vector (ColBERT) output is computed by the model but **not persisted or indexed** initially — cost is disproportionate for a personal notes corpus. Defer unless recall on short/ambiguous queries proves insufficient in practice.
- **Threading**: inference runs via `spawn_blocking`, never inline on the async reactor.
- **Backpressure**: bound concurrent inference calls with a semaphore sized to the ONNX session's thread count. Do not rely on `spawn_blocking`'s default pool to absorb load silently — queuing should be visible at the app layer, not hidden as creeping tail latency.

## 5. Retrieval Design

Dense and sparse retrieval are independent queries, each producing a ranked note list. Dense
retrieval performs an exact linear cosine-distance scan over chunk embeddings, grouping by note and
using the closest chunk as that note's rank. Sparse retrieval likewise uses each note's best
matching chunk. Scores are not directly comparable (cosine vs. sparse weight sum), so fuse by rank,
not raw score:

```
RRF(note) = Σ over retrievers r: 1 / (k + rank_r(note))     # k ≈ 60, tunable
```

Exact scanning avoids approximate-index build and maintenance, produces deterministic results, and
fits the expected personal-notes corpus. Its accepted trade-off is linear dense-search cost. If
corpus measurements outgrow that choice, a future backend can change its physical retrieval
strategy without changing pipeline callers.

If dense results consistently drown out obvious exact-term matches (or vice versa), `k` is the
first tuning knob — before reaching for a hand-weighted score sum.

## 6. Pipeline Contracts

**define_label_key**: validate that `key` is non-empty, then insert the key, description, and value
type into `label_keys`. Descriptions may be empty and filled in later. Missing keys referenced by a
note are also auto-created by `save_note` with an empty description.

**list_label_keys**: read-only → return the full label-key catalog (`key` + `description`), used by
clients and the UI to populate suggestions and explain known keys.

**save_note**: validate non-empty title/content and typed label values → atomically persist the note,
auto-created missing label keys, attached labels, chunk records, and durable embedding jobs → commit
prepared attachment files → return the persisted `Note`. The embedding worker later claims each job,
performs one dense+sparse inference call outside the database transaction, and atomically persists
both retrieval representations for the current chunk revision.

**search_notes**: embed query (dense + sparse) → exact dense cosine ranking + sparse postings
ranking → RRF fusion (pure function, no I/O) → hydrate labels/metadata for top-k → return
`Vec<(Note, f32)>` where the score is the fused RRF score, not raw cosine.

Context/environment: one struct holds a shared `StorageBackend` and embedder. The application
composition root constructs it once and passes it explicitly — no global or implicit storage state.

## 7. Frontend (Yew + duskmoon-ui)

Strict MVU: single `AppState`, all mutations go through a reducer dispatching typed actions (no direct field mutation from components).

State surface: notes list, search results (with fused score attached — label it as such in the UI, not as "similarity," since it's a rank fusion not a raw distance), loading flag, error slot.

Components: `NoteEditor` (form → emits note on submit; label picker lists registered keys with their descriptions, lets the user pick a key and enter a value), `VectorSearch` (query input → renders fused-score results). Use yew-duskmoon-ui primitives (`Card`, `Input`, `TextArea`, `Tag`) rather than custom equivalents — `Tag` renders each attached label as `key=value`, with the key's description as a tooltip/hint.

## 8. MCP Integration

**Tools**: `save_note_tool { title, content, labels }`, `semantic_search_tool { query, limit }`.
Both call the pipeline contracts in §6 directly — no MCP-specific business logic. Saving through
MCP uses the same missing-label-key auto-creation behavior; explicit catalog management remains
REST/UI-only.

**Transports**: stdio (subprocess, JSON-RPC over stdin/stdout, stderr for logs) and Streamable HTTP (single `/mcp` endpoint, POST+GET, optional SSE upgrade per-response for long calls). Do not implement legacy two-endpoint HTTP+SSE — it's deprecated protocol-side; add only if a specific client requires it, as a documented exception.

## 9. Open Decisions (resolve during implementation, not before)

- Sparse weight threshold value — start conservative, tune against measured postings-table size and recall.
- Whether label filtering should move deeper into backend retrieval at larger corpus sizes.
- Storage-session concurrency sizing for Axum's concurrent path.

## 10. Future Extensions (v2, not built now)

- **Resources**: file attachments (image, PDF, etc.) attached to a note. Naturally keyed by `note_id`, so no schema changes are needed now to accommodate this later.
- **References**: links/URIs pointing to outside resources, attached to a note. Same as above — additive, `note_id`-keyed, deferred rather than designed now.
