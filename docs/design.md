# Design Doc: Local-Inference Hybrid-Search Notes App

Handoff spec for implementation. Stack: Rust (Axum, ort/ONNX Runtime, Turso Database), Yew/Wasm frontend, MCP server. This doc specifies contracts and decisions; implementation details are left to the builder.

## 1. Design Philosophy

- **Pure core, effectful shell.** Validation, fusion scoring, and DTO assembly are pure transforms. All I/O (DB, inference) is pushed to explicit boundaries and threaded through one immutable context. Do not scatter DB calls or inference calls inside business logic — pipelines take pure data in, effects happen only at named boundary steps.
- **Fully offline.** No external embedding APIs. The default `NOTE_EMBEDDING_MODE=process` starts
  a supervised local worker subprocess and sends embedding requests over local IPC; that worker
  runs BGE-M3 via ONNX Runtime when `NOTE_MODEL_PATH` is configured, or the deterministic stub
  otherwise. The reserved `thread` and `remote` modes are not implemented.
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
  1024-d dense output only,      title FTS/BM25 + exact dense
  int8 quantized                 cosine + notes/labels
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

The following is a retrieval-relevant excerpt of the embedded adapter's schema v2. Idempotency
clauses, unrelated operational indexes, and non-retrieval tables such as `app_settings` are omitted:

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
CREATE INDEX idx_notes_title_fts ON notes USING fts (title);

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
    note_revision INTEGER NOT NULL DEFAULT 1,
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
    note_revision INTEGER NOT NULL DEFAULT 1,
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
```

The note write transaction atomically persists note metadata, labels, chunks, and durable embedding
jobs. A worker claims a job in a short immediate transaction, performs inference without holding a
database transaction, then atomically persists the dense body-chunk vector and marks the current
chunk revision embedded.

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
- **Output used**: the 1024-dimensional dense head. The worker persists one dense vector for each body chunk; no lexical or multi-vector embedding output is stored.
- **Threading**: inference runs via `spawn_blocking`, never inline on the async reactor.
- **Backpressure**: the worker wraps its embedder in `BoundedEmbedder` with concurrency `1`, so
  inference calls are serialized behind a semaphore. The local IPC worker queue is also bounded
  (default capacity `8`). `NOTE_EMBEDDING_THREADS` controls ONNX intra-op CPU threads separately;
  it does not change request concurrency.

## 5. Retrieval Design

Title FTS and dense content retrieval are independent queries, each producing a ranked note list.
Rust Turso Database FTS ranks title matches with BM25. Dense retrieval performs an exact linear
cosine-distance scan over body-chunk embeddings, grouping by note and using the closest chunk as
that note's rank. The scores are not directly comparable, so fuse by weighted rank rather than raw
score:

```
weighted_RRF(note) = Σ over retrievers r: weight_r / (60 + rank_r(note))
title FTS weight = 3.0
dense content weight = 1.0
```

Fusion ordering is deterministic: equal fused scores are broken by note ID. The pipeline requests
`clamp(requested_limit × 32, 128, 4096)` ranked IDs from each channel so fusion can consider notes
beyond the requested top-k. Dense retrieval uses that overfetch length directly. Title FTS first
materializes a fixed bounded pool of up to 4,096 BM25 hits, batch-filters deleted notes, and orders
score ties by note ID before returning only the requested overfetch length. Fusion therefore still
receives the requested ranking length from both channels; the fixed title pool is the physical FTS
work bound needed for deterministic ties within the accepted pool. Label filtering and hydration
happen after fusion, and iteration stops at the requested result limit.

Exact scanning avoids approximate-index build and maintenance, produces deterministic results, and
fits the expected personal-notes corpus. Its accepted trade-off is linear dense-search cost. If
corpus measurements outgrow that choice, a future backend can change its physical retrieval
strategy without changing pipeline callers.

The weights and RRF constant are ranking-policy parameters and should be tuned only against measured
retrieval quality.

## 6. Pipeline Contracts

**define_label_key**: validate that `key` is non-empty, then insert the key, description, and value
type into `label_keys`. Descriptions may be empty and filled in later. Missing keys referenced by a
note are also auto-created by `save_note` with an empty description.

**list_label_keys**: read-only → return the full label-key catalog (`key` + `description`), used by
clients and the UI to populate suggestions and explain known keys.

**save_note**: validate non-empty title/content and typed label values → atomically persist the note,
auto-created missing label keys, attached labels, chunk records, and durable embedding jobs → commit
prepared attachment files → return the persisted `Note`. The embedding worker later claims each job,
performs one dense inference call outside the database transaction, and atomically persists the
dense body-chunk vector for the current chunk revision.

**search_notes**: embed the query once for exact dense content retrieval → request the title FTS
ranking and dense note ranking → combine the two ranked ID lists with weighted RRF (pure function,
no I/O) → apply label filtering and hydrate labels/metadata for top-k → return
`Vec<(Note, f32)>` where the score is the fused weighted-RRF score, not raw BM25 or cosine.

Context/environment: one struct holds a shared `StorageBackend` and embedder. The application
composition root constructs it once and passes it explicitly — no global or implicit storage state.

## 7. Frontend (Yew + duskmoon-ui)

Strict MVU: single `AppState`, all mutations go through a reducer dispatching typed actions (no direct field mutation from components).

State surface: notes list, retrieval results (with fused score attached — do not label it as
"similarity," since it is rank fusion rather than raw distance), loading flag, error slot.

The Notes page retrieval bar says “Retrieve by title or content” and renders fused-score results.
Use yew-duskmoon-ui primitives (`Card`, `Input`, `TextArea`, `Tag`) rather than custom equivalents
— `Tag` renders each attached label as `key=value`, with the key's description as a tooltip/hint.

## 8. MCP Integration

**Tools**: `save_note_tool { title, content, labels }`, `semantic_search_tool { query, limit }`.
Both call the pipeline contracts in §6 directly — no MCP-specific business logic. Saving through
MCP uses the same missing-label-key auto-creation behavior; explicit catalog management remains
REST/UI-only.

**Transports**: stdio (subprocess, JSON-RPC over stdin/stdout, stderr for logs) and Streamable HTTP (single `/mcp` endpoint, POST+GET, optional SSE upgrade per-response for long calls). Do not implement legacy two-endpoint HTTP+SSE — it's deprecated protocol-side; add only if a specific client requires it, as a documented exception.

## 9. Open Decisions (resolve during implementation, not before)

- Whether label filtering should move deeper into backend retrieval at larger corpus sizes.
- Storage-session concurrency sizing for Axum's concurrent path.

## 10. Future Extensions

- **Structured references**: external links currently live in Markdown content. A dedicated
  reference model with its own metadata remains deferred until a concrete use case requires it.
