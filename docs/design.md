# Design Doc: Local-Inference Hybrid-Search Notes App

Handoff spec for implementation. Stack: Rust (Axum, ort/ONNX Runtime, Turso Database), Yew/Wasm frontend, MCP server. This doc specifies contracts and decisions; implementation details are left to the builder.

## 1. Design Philosophy

- **Pure core, effectful shell.** Validation, fusion scoring, and DTO assembly are pure transforms. All I/O (DB, inference) is pushed to explicit boundaries and threaded through one immutable context. Do not scatter DB calls or inference calls inside business logic — pipelines take pure data in, effects happen only at named boundary steps.
- **Offline by default.** The default local embedding engine starts a supervised worker subprocess
  and sends requests over local IPC. That worker runs BGE-M3 via ONNX Runtime when
  `embedding.model_path` (or its `NOTE_MODEL_PATH` fallback) is configured, or the deterministic
  stub otherwise. A self-hosted OpenAI-compatible engine is configured by the same file but remains
  reserved until its adapter plan is implemented.
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
  Embedding adapter                    Storage adapter
  local BGE-M3 ORT today;              Rust Turso Database today;
  OpenAI-compatible reserved           PostgreSQL reserved
                          │
                  Attachment adapter
                  filesystem today; S3 reserved
```

stdio is the same binary, selected by an entrypoint flag, and calls pipelines directly — never
proxies through Axum. The composition root resolves configuration once, constructs one shared
storage backend, embedding backend, and attachment store, and passes them explicitly to whichever
transport is active. HTTP, stdio, import, and export all load the mandatory configuration file
before opening an adapter.

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

Runtime configuration is mandatory. `NOTE_CONFIG_PATH` selects the file, with relative selectors
resolved from the process working directory; otherwise the implicit path is
`./dev-data/config.toml`. A debug/dev build atomically creates that implicit file when missing and
never overwrites an existing file. Explicit missing files always fail, and release builds fail when
the implicit file is missing. The generated development shape is:

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

Every relative path is based on the selected configuration file's parent, including a path supplied
by an environment fallback. Each field resolves independently in file-over-environment-over-default
order:

- `[database]`: `engine` (`NOTE_DB_ENGINE`, default `embed`). Embedded Turso uses `path`
  (`NOTE_DB_PATH`, default `notes.db`). Reserved PostgreSQL uses `url` (`DATABASE_URL`) and
  `max_connections` (`NOTE_DB_MAX_CONNECTIONS`, default `10`).
- `[embedding]`: `engine` (`NOTE_EMBEDDING_ENGINE`, default `local`). Local inference accepts
  optional `model_path` (`NOTE_MODEL_PATH`). Reserved OpenAI-compatible inference accepts
  `base_url`, `model` (default `bge-m3`), optional `api_key_env`, `timeout_secs` (default `30`), and
  `max_retries` (default `3`), with `NOTE_EMBEDDING_BASE_URL`, `NOTE_EMBEDDING_MODEL`,
  `NOTE_EMBEDDING_API_KEY_ENV`, `NOTE_EMBEDDING_TIMEOUT_SECS`, and
  `NOTE_EMBEDDING_MAX_RETRIES` fallbacks.
- `[attachments]`: `engine` (`NOTE_ATTACHMENTS_ENGINE`, default `filesystem`). Filesystem uses
  `path` (`NOTE_ATTACHMENTS_DIR`, default `attachments`). Reserved S3 uses `bucket`, `prefix`,
  optional `region`, optional `endpoint`, and `force_path_style`, with `NOTE_S3_BUCKET`,
  `NOTE_S3_PREFIX`, `AWS_REGION`, `NOTE_S3_ENDPOINT`, and `NOTE_S3_FORCE_PATH_STYLE` fallbacks.

The reserved OpenAI-compatible adapter will call `/v1/embeddings` with model `bge-m3`;
`api_key_env` is optional and omission means no authentication. The reserved PostgreSQL adapter
will use title FTS and exact cosine pgvector search over dense content vectors. The reserved S3
adapter targets S3-compatible object storage. These values are parsed now. A mode that constructs a
reserved adapter returns a precise not-implemented error rather than silently falling back; import
and export use the stub embedder and therefore do not construct the reserved embedding adapter.

The current filesystem attachment adapter stages a complete note set before the database
transaction, publishes after commit, and aborts staging on database failure. Hydration and
single-object reads go through the adapter; permanent deletion removes attachment data only after
the database record is deleted. Per-note operations are coordinated within the process. The root
must be exclusively owned by one agent-note process, with no external mutation or symlinks, and
filesystem publication cannot be atomically committed with the database transaction.

## 4. Embedding Pipeline (BGE-M3)

- **Quantization**: int8. Do not drop to 4-bit — CPU ONNX Runtime kernels for INT4 are not reliably faster than int8 on CPU EP, and quantization noise directly perturbs retrieval ordering (unlike LLM generation, where it's more forgiving). If corpus size later justifies revisiting this, it requires re-embedding the full corpus — quantization levels are not mixable in the same vector space.
- **Output used**: the 1024-dimensional dense head. The worker persists one dense vector for each body chunk; no lexical or multi-vector embedding output is stored.
- **Local model selection**: an optional `embedding.model_path` selects BGE-M3 ONNX; otherwise the
  deterministic 1,024-component stub keeps development and tests self-contained.
- **Reserved remote selection**: `embedding.engine = "openai"` describes a self-hosted
  OpenAI-compatible `/v1/embeddings` endpoint using model `bge-m3`. Authentication is disabled when
  `api_key_env` is omitted. The HTTP adapter is not active in this implementation slice.
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

**save_note**: validate non-empty title/content and typed label values → prepare the attachment set
outside the database transaction → atomically persist the note, auto-created missing label keys,
attached labels, chunk records, and durable embedding jobs → commit the database transaction →
publish prepared attachments → return the persisted `Note`. A database failure aborts the prepared
set. The embedding worker later claims each job, performs one dense inference call outside the
database transaction, and atomically persists the dense body-chunk vector for the current chunk
revision.

**search_notes**: embed the query once for exact dense content retrieval → request the title FTS
ranking and dense note ranking → combine the two ranked ID lists with weighted RRF (pure function,
no I/O) → apply label filtering and hydrate labels/metadata for top-k → return
`Vec<(Note, f32)>` where the score is the fused weighted-RRF score, not raw BM25 or cosine.

Context/environment: one struct holds a shared `StorageBackend`, embedder, and `AttachmentStore`.
The application composition root constructs it once and passes it explicitly — no global or
implicit storage state. System information stays backend-neutral: database engine, optional
location and size, attachment engine, and optional attachment location. Credentials and embedding
authentication are never part of that response.

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
