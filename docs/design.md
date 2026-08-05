# Design Doc: Hybrid-Search Notes App

Handoff spec for implementation. Stack: Rust (Axum, ort/ONNX Runtime, Turso Database, PostgreSQL
with pgvector), Yew/Wasm frontend, MCP server. This doc specifies contracts and decisions;
implementation details are left to the builder.

## 1. Design Philosophy

- **Pure core, effectful shell.** Validation, fusion scoring, and DTO assembly are pure transforms. All I/O (DB, inference) is pushed to explicit boundaries and threaded through one immutable context. Do not scatter DB calls or inference calls inside business logic — pipelines take pure data in, effects happen only at named boundary steps.
- **Offline by default.** The default local embedding engine starts a supervised worker subprocess
  and sends requests over local IPC. That worker runs BGE-M3 via ONNX Runtime when
  `embedding.model_path` (or its `NOTE_MODEL_PATH` fallback) is configured, or the deterministic
  stub otherwise. The same file can select a self-hosted OpenAI-compatible BGE-M3 service when
  remote inference is desired.
- **One core, shared interfaces.** REST/MCP-over-HTTP and MCP-over-stdio call the same pipeline
  functions. Storage-only Org commands call the same Org import/export pipelines before attachment
  or embedding startup. No interface duplicates business logic.

## 2. System Topology

```
      ┌─────────────┐        ┌──────────────────────┐
      │ stdio entry │        │ Axum: /api/notes,      │
      │ (subprocess)│        │ /api/org, /mcp         │
      └──────┬──────┘        │ (Streamable HTTP)      │
             │                └──────────┬────────────┘
             └───────────┬────────────────┘
                          ▼
              Functional Pipelines (shared core)
                          │
           ┌──────────────┴──────────────┐
           ▼                             ▼
  Embedding adapter                    Storage adapter
  local BGE-M3 ORT or                  Rust Turso Database or
  OpenAI-compatible HTTP               PostgreSQL external adapter
                          │
                  Attachment adapter
                  filesystem or S3-compatible object storage
```

The Org offline CLI is a third, storage-only branch into the shared pipelines; it deliberately
bypasses the embedding and attachment adapters shown below the pipeline boundary.

stdio is the same binary, selected by an entrypoint flag, and calls pipelines directly — never
proxies through Axum. The composition root resolves configuration once and passes explicit shared
contexts to the active transport. HTTP and stdio construct storage, embedding, and attachment
adapters. Legacy note import/export and Org offline modes also load configuration and storage, but
dispatch before attachment construction or embedding-secret/worker startup.

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
  (`NOTE_DB_PATH`, default `notes.db`). PostgreSQL uses `url` (`DATABASE_URL`) and
  `max_connections` (`NOTE_DB_MAX_CONNECTIONS`, default `10`).
- `[embedding]`: `engine` (`NOTE_EMBEDDING_ENGINE`, default `local`). Local inference accepts
  optional `model_path` (`NOTE_MODEL_PATH`). OpenAI-compatible inference accepts `base_url`,
  `model` (default `bge-m3`), optional `api_key_env`, `timeout_secs` (default `30`), and
  `max_retries` (default `3`), with `NOTE_EMBEDDING_BASE_URL`, `NOTE_EMBEDDING_MODEL`,
  `NOTE_EMBEDDING_API_KEY_ENV`, `NOTE_EMBEDDING_TIMEOUT_SECS`, and
  `NOTE_EMBEDDING_MAX_RETRIES` fallbacks.
- `[attachments]`: `engine` (`NOTE_ATTACHMENTS_ENGINE`, default `filesystem`). Filesystem uses
  `path` (`NOTE_ATTACHMENTS_DIR`, default `attachments`). S3 uses `bucket`, `prefix`,
  optional `region`, optional `endpoint`, and `force_path_style`, with `NOTE_S3_BUCKET`,
  `NOTE_S3_PREFIX`, `AWS_REGION`, `NOTE_S3_ENDPOINT`, and `NOTE_S3_FORCE_PATH_STYLE` fallbacks.

The OpenAI-compatible adapter preserves the normalized `base_url` path, removes a trailing empty
segment, and appends `v1/embeddings`. A root `base_url = "http://embedding.internal:8000"`
therefore produces `http://embedding.internal:8000/v1/embeddings`, while `/gateway` and
`/gateway/` both produce `/gateway/v1/embeddings`. Requests use `encoding_format = "float"` and no
`dimensions` field. `api_key_env` is optional and names the environment variable containing the
bearer token; omission means no authentication. Normal remote HTTP and MCP startup resolves that
variable and rejects a missing or blank value. Import and export do not resolve it and remain
embedding-service offline. Responses must contain exactly one uniquely indexed, finite
1,024-component vector per input. The adapter retries connection failures, timeouts, HTTP 429, and
HTTP 5xx with bounded backoff. `max_retries` counts retries after the initial request and cannot
exceed `10`. Successful response bodies are limited to 1 MiB; unauthenticated error bodies are
limited to 4 KiB and authenticated error bodies are redacted.

The S3 adapter targets AWS S3 and compatible object storage. For example, a local MinIO
configuration is:

```toml
[attachments]
engine = "s3"
bucket = "agent-note"
prefix = "attachments"
region = "us-east-1"
endpoint = "http://127.0.0.1:9000"
force_path_style = true
```

AWS S3 normally omits `endpoint` and leaves `force_path_style = false`; MinIO and similar local
services provide an endpoint and commonly require path style. `region` may be omitted to use the
standard AWS region provider chain. Credentials are never stored in TOML. They come from the
standard AWS credential chain, including `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional
`AWS_SESSION_TOKEN`, shared profiles, web identity, container credentials, and instance roles.

Final object keys are `<prefix>/<note-id>/<canonical-relative-path>`, or
`<note-id>/<canonical-relative-path>` for an empty prefix. Writes upload a complete set beneath a
unique `.staging` prefix, commit note metadata, copy every expected object to its final key, remove
obsolete final keys, and remove staging. The AWS SDK permits four total attempts (the initial
request plus at most three retries) for SDK-classified transient failures. List operations consume
every continuation token, and delete requests contain at most 1,000 keys. A single attachment read
issues one `GetObject`; note and export hydration read every object declared by note metadata.

S3 publication and deletion preserve the same database boundary as the filesystem adapter.
Permanent deletion commits the database removal before object cleanup, so failure can leave safe
orphan objects. Publication happens after database commit and failure can leave committed metadata
temporarily unreadable; the safe staging location is retained for manual repair. There is no
durable outbox or reconciler in this release. Backend-neutral System information reports only
engine `s3` and `s3://bucket/prefix`, never endpoint user information, profiles, access keys,
secret keys, or session tokens.

`note-storage-pg` is the complete external database adapter. Before Agent Note starts, the operator
must provision pgvector in the selected database:

```sql
CREATE EXTENSION vector;
```

The application verifies the extension but never installs it. Its role needs normal privileges to
run Agent Note's schema migrations and create or update its tables and indexes. The PostgreSQL
schema stores title search data as
`to_tsvector('simple'::regconfig, title)` and uses a GIN index. Body-chunk embeddings are
`vector(1024)`, and dense retrieval orders exact cosine distance with `<=>`; there is no ANN index.
Backend-neutral System information reports the `pg` engine without the database URL or
credentials.

The current filesystem attachment adapter stages a complete note set before the database
transaction, publishes after commit, and aborts staging on database failure. Hydration and
single-object reads go through the adapter; permanent deletion removes attachment data only after
the database record is deleted. Per-note operations are coordinated within the process. The root
must be exclusively owned by one agent-note process, with no external mutation or symlinks, and
filesystem publication cannot be atomically committed with the database transaction.

The database commit precedes attachment publication. A publication error can therefore leave
committed metadata referring to unavailable or previously published bytes; it cannot roll back the
database commit. A cleanup error after permanent database deletion can leave orphan files or
objects. Callers must not blindly retry: create or update may already be committed, and retrying a
create can duplicate it; a permanent-delete retry may return not found and cannot rerun attachment
cleanup. Recovery requires inspecting committed database and object state, then manually repairing
or publishing bytes or removing orphan data. Errors are returned or logged, but there is no
built-in retry or reconciliation command, worker, or durable outbox.

There is no guaranteed in-place migration from databases created by the retired storage
implementation. Delete and recreate only disposable test/development databases. Export or back up
non-disposable data before upgrading so recovery or deliberate import remains possible.

## 4. Embedding Pipeline (BGE-M3)

- **Local quantization**: int8. Do not drop to 4-bit — CPU ONNX Runtime kernels for INT4 are not reliably faster than int8 on CPU EP, and quantization noise directly perturbs retrieval ordering (unlike LLM generation, where it's more forgiving). If corpus size later justifies revisiting this, it requires re-embedding the full corpus — quantization levels are not mixable in the same vector space.
- **Output used**: the 1024-dimensional dense head. The worker persists one dense vector for each body chunk; no lexical or multi-vector embedding output is stored.
- **Local model selection**: an optional `embedding.model_path` selects BGE-M3 ONNX; otherwise the
  deterministic 1,024-component stub keeps development and tests self-contained.
- **Remote selection**: `embedding.engine = "openai"` selects a self-hosted OpenAI-compatible
  `/v1/embeddings` endpoint. A runnable configuration is:

  ```toml
  [embedding]
  engine = "openai"
  base_url = "http://embedding.internal:8000"
  model = "bge-m3"
  api_key_env = "EMBEDDING_API_KEY"
  timeout_secs = 30
  max_retries = 3
  ```

  `api_key_env` is optional; omit it for no authentication.
- **Fingerprint lifecycle**: local mode without `model_path` identifies the deterministic stub as
  `stub:1024`. Local BGE-M3 with `model_path` and remote model `bge-m3` both identify their real
  vector space as `bge-m3:1024`. A missing stored fingerprint, including in a legacy database,
  adopts the configured fingerprint without deleting existing vectors or jobs. An equal stored
  fingerprint preserves them unchanged. A present but different fingerprint—including a switch
  from the stub to BGE-M3—causes one storage transaction to remove old vectors and jobs, queue one
  pending job for every active chunk, and record the new fingerprint before the scheduler starts.
- **Endpoint-free paths**: a blank retrieval query returns no results before embedding or storage
  access. Normal note saves reject blank content; blank imported note content creates no chunks or
  embedding jobs. Import and export use the deterministic stub and never contact the configured
  embedding service; import queues missing chunks for later processing.
- **Threading**: local ONNX inference runs via `spawn_blocking`, never inline on the async reactor;
  remote inference uses asynchronous HTTP.
- **Backpressure**: the local worker wraps its embedder in `BoundedEmbedder` with concurrency `1`,
  so inference calls are serialized behind a semaphore. The local IPC worker queue is also bounded
  (default capacity `8`). `NOTE_EMBEDDING_THREADS` controls ONNX intra-op CPU threads separately;
  it does not change request concurrency. The scheduler processes one durable job at a time for
  either backend.

## 5. Retrieval Design

Title retrieval and dense content retrieval are independent queries, each producing a ranked note
list. Rust Turso Database FTS ranks title matches with BM25. PostgreSQL title FTS uses its generated
`simple`-configuration `tsvector` and GIN index. Both adapters perform an exact linear
cosine-distance scan over body-chunk embeddings, grouping by note and using the closest chunk as
that note's rank; PostgreSQL uses exact `<=>` over `vector(1024)` with no ANN index. The scores are
not directly comparable, so the existing pipeline fuses the title and content-vector rankings by
weighted rank rather than raw score:

```
weighted_RRF(note) = Σ over retrievers r: weight_r / (60 + rank_r(note))
title retrieval weight = 3.0
dense content weight = 1.0
```

Fusion ordering is deterministic: equal fused scores are broken by note ID. The pipeline requests
`clamp(requested_limit × 32, 128, 4096)` ranked IDs from each channel so fusion can consider notes
beyond the requested top-k. When label selectors are present, they are parsed before embedding. In
a short storage session, storage evaluates selector operators and value types with the shared
`note-core` matcher over distinct active stored label values, then turns the accepted raw values
into SQL presence/value predicates. Those predicates constrain list pages before pagination,
counts, and matching-active-ID selection. An empty matching collection returns before embedding.

For a filtered search, the same allowed note IDs constrain both title and exact dense retrieval
before their ranking limits, and the normal overfetch is capped by the allowed collection size.
PostgreSQL adds optional text-array predicates to both its title FTS and dense SQL before `LIMIT`.
Turso dense retrieval applies a JSON allowed-ID predicate before distance ranking and `LIMIT`.
Deleted notes remain excluded in every path.

Unfiltered Turso title retrieval retains its fixed bounded pool of up to 4,096 FTS/BM25 hits. The
current Turso adapter cannot combine allowed-ID filtering with that bounded BM25 plan, so filtered
title retrieval uses JSON-encoded allowed IDs to drive note-ID/primary-key lookups of active titles.
It ranks only those titles by case-insensitive whole-token frequency, retaining a bounded top-K heap
with a note-ID tie-break. This avoids the unbounded corpus-wide FTS fallback and SQL sorter that a
residual allowed-ID predicate would trigger in the current adapter. PostgreSQL filtered title
retrieval retains native FTS ranking. The Turso distinction should be reevaluated after backend
upgrades.

Both rankings feed weighted RRF. Final allowed-ID membership is checked defensively before the
top-k notes are hydrated, and returned scores are fused RRF scores rather than raw BM25 or cosine
scores.

Exact scanning avoids approximate-index build and maintenance, produces deterministic results, and
fits the expected personal-notes corpus. Its accepted trade-off is linear dense-search cost. If
corpus measurements outgrow that choice, a future backend can change its physical retrieval
strategy without changing pipeline callers.

The weights and RRF constant are ranking-policy parameters and should be tuned only against measured
retrieval quality.

## 6. Pipeline Contracts

**define_label_key**: validate that `key` is non-empty and contains none of the selector-reserved
characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`, then insert the key, description, and value
type into `label_keys`. Descriptions may be empty and filled in later. Missing keys referenced by a
note are also auto-created by `save_note` with an empty description after the same key validation.
Catalogs created before this grammar was enforced can still contain reserved characters; those keys
remain attachable but must be renamed with a storage migration before selector filtering can address
them unambiguously.

**list_label_keys**: read-only → return the full label-key catalog (`key` + `description`), used by
clients and the UI to populate suggestions and explain known keys.

**save_note**: validate non-empty title/content and typed label values → prepare the attachment set
outside the database transaction → atomically persist the note, auto-created missing label keys,
attached labels, chunk records, and durable embedding jobs → commit the database transaction →
publish prepared attachments → return the persisted `Note`. A database failure aborts the prepared
set. A publication failure is returned after the database commit and may leave committed metadata
referring to unavailable or previous bytes; it does not roll back that commit. The embedding worker
later claims each job, performs one dense inference call outside the database transaction, and
atomically persists the dense body-chunk vector for the current chunk revision.

**search_notes**: a blank query or zero limit returns without calling storage or embedding
endpoints. Otherwise, when label selectors are present, parse them → acquire a short filter storage
session → resolve matching active IDs → return before embedding if the collection is empty →
release the filter session. Unfiltered searches skip that branch and proceed directly to embedding.
Embed the query once → acquire a fresh retrieval session for retrieval and hydration → constrain
title and exact dense candidate selection before their limits → apply bounded
`clamp(requested_limit × 32, 128, 4096)` overfetch, capped by the allowed collection size when
filtered → combine both rankings with weighted RRF (pure function, no I/O) → defensively check
allowed-ID membership and hydrate the top-k → return `Vec<SearchResult>`, each containing a
`NoteListItem` summary and fused weighted-RRF score rather than raw BM25 or cosine.

Context/environment: one struct holds a shared `StorageBackend`, embedder, and `AttachmentStore`.
The application composition root constructs it once and passes it explicitly — no global or
implicit storage state. System information stays backend-neutral: database engine, optional
location and size, attachment engine, and optional attachment location. Credentials and embedding
authentication are never part of that response. Embedding information is limited to engine, model,
and fingerprint.

## 7. Frontend (Yew + duskmoon-ui)

Strict MVU: single `AppState`, all mutations go through a reducer dispatching typed actions (no direct field mutation from components).

State surface: notes list, retrieval results (with fused score attached — do not label it as
"similarity," since it is rank fusion rather than raw distance), loading flag, error slot.

The Notes page retrieval bar says “Retrieve by title or content” and renders fused-score results.
Use yew-duskmoon-ui primitives (`Card`, `Input`, `TextArea`, `Tag`) rather than custom equivalents
— `Tag` renders each attached label as `key=value`, with the key's description as a tooltip/hint.

## 8. MCP Integration

One `NoteMcpServer` registry exposes 47 tools through both transports. The eleven Markdown-note
tools are `save_note`, `get_note`, `read_note_lines`, `edit_note`, `update_note`, `delete_note`,
`list_notes`, `semantic_search`, `put_note_attachment`, `get_note_attachment_content`, and
`delete_note_attachment`. Explicit label-catalog management remains REST/UI-only.

The exact 36 Org tools are:

```text
org_list_workspaces       org_create_workspace      org_get_workspace
org_update_workspace      org_archive_workspace     org_list_documents
org_get_document          org_put_document           org_move_document
org_move_item             org_import_workspace       org_export_workspace
org_create_item           org_get_item               org_get_item_context
org_create_follow_up      org_assign_item            org_schedule_item
org_query_queue           org_query_agenda           org_claim_item
org_heartbeat_claim       org_release_claim          org_report_progress
org_submit_result         org_transition_item        org_retry_item
org_request_review        org_approve_item            org_reject_item
org_add_dependency        org_remove_dependency      org_link_note
org_unlink_note           org_list_note_work_items   org_list_events
```

Every handler delegates to the corresponding pipeline operation. The composition root constructs
one Markdown-note `Context` and one `Arc<OrgContext>` over the same storage backend and injects
those exact contexts into stdio or HTTP. Stdio uses JSON-RPC over process stdin/stdout with logs on
stderr. Streamable HTTP is stateless JSON POST at the single `/mcp` endpoint; it has no GET/SSE
transport, MCP sessions, authentication middleware, or legacy two-endpoint HTTP+SSE path.

### 8.1 Org REST/OpenAPI Interface

The Axum server exposes every Org pipeline operation under `/api/org`. The generated OpenAPI 3.1
document is available at `/api/openapi.json` and interactive Swagger UI at `/api/docs`; `/mcp` is
not part of that document. Each REST operation has the exact corresponding MCP tool name as its
unique `operationId`:

| Method and path | `operationId` |
| --- | --- |
| `GET /api/org/workspaces` | `org_list_workspaces` |
| `POST /api/org/workspaces` | `org_create_workspace` |
| `GET /api/org/workspaces/{workspace_id}` | `org_get_workspace` |
| `PATCH /api/org/workspaces/{workspace_id}` | `org_update_workspace` |
| `POST /api/org/workspaces/{workspace_id}/archive` | `org_archive_workspace` |
| `GET /api/org/workspaces/{workspace_id}/documents` | `org_list_documents` |
| `GET /api/org/documents/{document_id}` | `org_get_document` |
| `PUT /api/org/documents/{document_id}` | `org_put_document` |
| `POST /api/org/documents/{document_id}/move` | `org_move_document` |
| `POST /api/org/items/{item_id}/move` | `org_move_item` |
| `POST /api/org/workspaces/{workspace_id}/import` | `org_import_workspace` |
| `GET /api/org/workspaces/{workspace_id}/export` | `org_export_workspace` |
| `POST /api/org/workspaces/{workspace_id}/items` | `org_create_item` |
| `GET /api/org/items/{item_id}` | `org_get_item` |
| `GET /api/org/items/{item_id}/context` | `org_get_item_context` |
| `POST /api/org/items/{item_id}/follow-ups` | `org_create_follow_up` |
| `POST /api/org/items/{item_id}/assignment` | `org_assign_item` |
| `POST /api/org/items/{item_id}/schedule` | `org_schedule_item` |
| `GET /api/org/queue` | `org_query_queue` |
| `GET /api/org/agenda` | `org_query_agenda` |
| `POST /api/org/items/{item_id}/claim` | `org_claim_item` |
| `POST /api/org/items/{item_id}/claim/heartbeat` | `org_heartbeat_claim` |
| `POST /api/org/items/{item_id}/claim/release` | `org_release_claim` |
| `POST /api/org/items/{item_id}/progress` | `org_report_progress` |
| `POST /api/org/items/{item_id}/result` | `org_submit_result` |
| `POST /api/org/items/{item_id}/transition` | `org_transition_item` |
| `POST /api/org/items/{item_id}/retry` | `org_retry_item` |
| `POST /api/org/items/{item_id}/review/request` | `org_request_review` |
| `POST /api/org/items/{item_id}/review/approve` | `org_approve_item` |
| `POST /api/org/items/{item_id}/review/reject` | `org_reject_item` |
| `POST /api/org/items/{item_id}/dependencies` | `org_add_dependency` |
| `DELETE /api/org/items/{item_id}/dependencies/{dependency_item_id}` | `org_remove_dependency` |
| `POST /api/org/items/{item_id}/note-links` | `org_link_note` |
| `DELETE /api/org/items/{item_id}/note-links` | `org_unlink_note` |
| `GET /api/org/notes/{note_id}/work-items` | `org_list_note_work_items` |
| `GET /api/org/workspaces/{workspace_id}/events` | `org_list_events` |

Reads use path/query extraction. Every mutation, including `DELETE`, uses a JSON envelope with
`schema_version`, client-asserted `actor_id`, idempotent `operation_id`, workspace identity, and
operation-specific action, revision, or lease fields. IDs supplied by the path and body must
match. `actor_id` is audit attribution, not an authenticated identity. An identical operation
replay returns its durable original result, including after restart; a divergent reuse returns
`idempotency_conflict` without applying effects.

REST success JSON and MCP structured content serialize the same pipeline DTO. Every REST failure
is `{code, message, details, retryable}` JSON. Invalid input maps to 400, missing requested
resources to 404, workflow/revision/idempotency/lease conflicts to 409, concurrency limits to 429,
and safe storage failures to 500. HTTP status and JSON-RPC framing are transport metadata; semantic
result and error fields remain equal.

Raw fencing tokens are sensitive ownership proofs. Only successful claim and retry/reclaim results
return them, and only lease-bound mutation bodies accept them. General reads, errors, events,
queues, context, exports, logs, and OpenAPI examples must never disclose a raw token or its hash;
token-bearing request schema fields are documented as sensitive.

Agent Note implements no authentication, authorization, session, trusted proxy-identity-header,
or workspace ACL behavior. OpenAPI intentionally defines no security scheme. The front proxy owns
TLS, authentication, access control, Host/origin policy, and network restriction. Direct untrusted
exposure of Agent Note is unsupported.

### 8.2 Org Offline Interfaces

The storage-only command surface is:

```text
note-server org export-workspace --workspace-id <uuid> --output <directory>
note-server org import-workspace --input <directory> --mode create|update \
  --actor-id <id> --operation-id <id>
note-server org export-document --document-id <uuid> --output <file.org>
note-server org import-document --workspace-id <uuid> --document-id <uuid> \
  --path <org-path> --input <file.org> --mode create|update \
  --actor-id <id> --operation-id <id> [--expected-revision <n>]
```

Document update requires the expected revision; create rejects that option and an existing ID.
Workspace update consumes the workspace/document revisions in the exported manifest. The portable
layout is `manifest.json` plus `documents/<document-uuid>.org`. The manifest carries its format
version, workspace metadata/policy/revision, and stable document ID, canonical Org path, revision,
content hash, and relative UUID filename. Import validates path containment, file inventory,
uniqueness, hashes, format, and UTF-8 before mutation. Export uses sibling temporary output and an
atomic `NOREPLACE` rename on Linux, Android, and Apple-vendor targets and never silently overwrites
a non-empty destination. Windows and other targets without that primitive fail export closed with
an `Unsupported` publish error; imports remain supported. All modes emit structured stdout reports
and start storage only; they do not touch Markdown notes or construct attachment, embedding,
worker, HTTP, or MCP runtime state.

### 8.3 Org Web Operations Console

The Yew frontend exposes a read-only operations console on three client-side routes:

```text
/org
/org/:workspace_id
/org/:workspace_id/items/:item_id
```

The directory lists active workspaces by default and can include archived workspaces. Each row
shows counts for the ten server-computed operational views: ready, assigned, running, blocked,
review, scheduled, upcoming deadline, failed, expired lease, and completed. The workspace route
selects exactly one of those views and delegates it to `GET /api/org/queue` or
`GET /api/org/agenda`; the browser does not reproduce eligibility rules. Type, state, priority,
tags, assignee, UTC range, opaque cursor, and page limit are canonical URL state. Directory archive
selection, cursor, and limit are URL state too. An item link encodes the complete originating
workspace query under typed `return_` fields; malformed or cross-workspace return state falls back
to the ready view, and a valid Back link restores the exact origin URL.

The item route displays the pipeline recovery context and subject-filtered workspace event page,
including hierarchy, dependencies, readiness/blockers, attempts, sanitized lease status,
artifacts, origins, weak note links, recovery data, and event sequence. A weak Markdown-note link
whose note is missing remains visible and is marked **Unavailable**. Workspace-time rendering is
primary; browser-local time is secondary. Invalid timezones or timestamps are explicit
**Unavailable** values rather than silently using a different zone.

Navigation performs one load and does not poll. Directory, workspace, and item pages have explicit
manual Refresh controls plus loading, empty, and structured error states. The browser issues only
same-origin `GET /api/org/...` requests: no MCP call, mutation, fencing-token storage/display,
claim/review/edit action, or application authentication/session/ACL behavior is present. Agent
Note trusts no proxy identity header. The front proxy owns TLS, authentication, authorization,
Host/origin validation, and network restriction.

In development, Trunk's `/api/` proxy forwards REST calls to `note-server` while `/org` remains a
frontend route. In a packaged build, `NOTE_STATIC_DIR` enables the Axum SPA fallback: direct loads
and refreshes of all three routes return `index.html` with HTTP 200, but the composed REST router
claims `/api/org` first and therefore cannot be shadowed by the fallback.

## 9. Org Orchestration Pipeline

Org is a separate subsystem from canonical Markdown notes. Org source documents and workspace
policy are canonical, while ordered events, execution attempts, leases, and idempotent operation
results are durable runtime data. Work-item projections are derived and rebuildable from canonical
Org documents without discarding that runtime history. A weak note link may refer to an existing
Markdown note without changing the note storage contract. Turso schema v5 and PostgreSQL migration
`0004_org_claims_operational_views.sql` add the lease and operational-query data needed by this
pipeline.

All Org commands execute through `note_pipelines::org` with explicit command envelopes, a
controllable clock, and storage transactions. Source- and workspace-mutating commands also carry
expected revisions; lease-only heartbeat and event-only progress do not change document revision.
The client supplies `actor_id` for audit attribution and `operation_id` for idempotency. Actor IDs
are assertions, not authenticated or authorized identities. Once an operation result commits, an
identical replay returns the original result and cross-command or different-input reuse is an
idempotency conflict. Rejected or rolled-back calls generally do not reserve the operation ID.
Revision, policy, dependency, assignment, schedule, retry, archive, and workspace-capacity checks
keep the primary command effects atomic. The explicit exception is expired-lease observation: a
command may atomically persist independently idempotent expiry bookkeeping before returning
`stale_lease`, without applying the requested mutation.

Execution and review claims create exclusive leases and return an opaque, unguessable fencing token
only in the claim result (and its exact idempotent replay). Storage persists the token hash rather
than the raw token. Heartbeat, release, progress, result submission, ownership-sensitive
transitions, review, failure, and completion require the current unexpired token. General
reads, context, queues, events, errors, and logs never expose a raw token or token hash. Expiry,
release, reassignment, unassignment, reclaim, and terminal transitions permanently invalidate the
old token. Treat the raw token as a sensitive bearer-like ownership proof: clients must not log it
or copy it into audit metadata, source documents, exports, or general application state.

Heartbeat changes lease state without changing the Org document revision. Voluntary execution
release closes the active attempt and returns the item to the configured recovery state; default
review release closes only the review lease and preserves the submitted execution attempt.
Reclaiming expired running work atomically records expiry, closes the old attempt and lease, passes
through the configured recovery state, and creates a new attempt and token. Failure marks the
attempt failed; review rejection marks the attempt failed and returns the item to its configured
review-rejection state. A later retry/readiness decision is budget-gated. One shared workspace
concurrency limit counts all unexpired execution and review leases and is enforced atomically,
including claim/claim and move-to-target/claim races.

The read boundary provides workspace/document reads and export, item context, note-to-item lookup,
and ten deterministic operational views: ready, assigned, running, blocked, review, scheduled,
upcoming deadline, failed, expired lease, and completed. Queue and agenda predicates share one
fixed evaluation time per cursor chain. Cursors are opaque, query-bound, HMAC-protected,
process-scoped continuation tokens carrying the last scanned tuple; pages overfetch in bounded
batches and never derive eligibility in a transport.

Archived workspaces remain readable and auditable but reject mutation. Archive always rejects a
workspace with any active lease and accepts no ownership proof. Raw import may change an actively
leased item only when it supplies and validates that item's exact current proof. Import reuses the
same lifecycle decisions and typed events as structured commands rather than bypassing fencing.

Delivery Slice 6 exposes Org through 36 matching MCP and REST/OpenAPI operations plus four
storage-only offline commands where applicable. Delivery Slice 7 adds the read-only Web console
defined in §8.3, with explicit manual refresh and no browser mutation, session, or polling
behavior. Actor IDs are asserted audit attribution, not authenticated identity.
Agent Note implements no inbound authentication, authorization, trusted proxy-identity protocol,
or workspace ACL. A front proxy owns TLS, authentication, authorization, Host/origin validation,
and network restriction; direct exposure of the service to an untrusted network is unsupported.

## 10. Open Decisions (resolve during implementation, not before)

- Storage-session concurrency sizing for Axum's concurrent path.

## 11. Future Extensions

- **Structured references**: external links currently live in Markdown content. A dedicated
  reference model with its own metadata remains deferred until a concrete use case requires it.
