# External PostgreSQL, BGE-M3, and S3 Adapters Design

**Date:** 2026-07-18

**Status:** Approved

## 1. Summary

Agent Note will support three independently selected infrastructure adapters:

- embedded Turso or external PostgreSQL for application storage;
- the existing local BGE-M3 worker or a self-hosted OpenAI-compatible
  BGE-M3 embeddings endpoint;
- the local filesystem or S3-compatible object storage for attachments.

The server will always start from a TOML configuration file. Debug and
development builds will create `dev-data/config.toml` with local defaults when
the implicit default file is missing. Release builds and explicit missing
configuration paths will fail.

This release supports one active Agent Note server process. PostgreSQL and S3
may be highly available or replicated services, but application-level
multi-replica coordination is out of scope.

## 2. Goals

- Add a complete PostgreSQL implementation of the existing storage contracts.
- Keep title-only full-text retrieval and exact content-vector retrieval.
- Use PostgreSQL native FTS for titles and `pgvector` for 1,024-dimensional
  content vectors.
- Add a self-hosted OpenAI-compatible `/v1/embeddings` adapter for BGE-M3.
- Keep local and remote BGE-M3 vectors in the same 1,024-dimensional storage
  contract.
- Add filesystem and S3-compatible attachment adapters.
- Make configuration mandatory while preserving per-field precedence:
  configuration file, then environment, then default.
- Preserve JSON export/import as the supported cross-backend data transfer
  mechanism.

## 3. Non-goals

- Multiple concurrently active Agent Note server replicas.
- Embedding-job leases, distributed worker ownership, or distributed note
  revision coordination.
- PostgreSQL or S3 service deployment and replication.
- Automatic `pgvector` extension installation.
- Approximate nearest-neighbor indexes such as HNSW or IVFFlat.
- A distributed transaction between PostgreSQL and S3.
- A durable S3 attachment reconciliation outbox.
- Migration of existing Turso database files into PostgreSQL.
- Arbitrary embedding dimensions or mixing embedding models in one database.
- OpenAI-hosted embedding models as a default or requirement.

## 4. Architecture

The application composition root constructs three effects from resolved
configuration:

```text
RuntimeConfig
  ├── StorageBackend
  │     ├── TursoStorage
  │     └── PgStorage
  ├── Embedder
  │     ├── local BGE-M3 process
  │     └── OpenAiCompatibleEmbedder
  └── AttachmentStore
        ├── FilesystemAttachmentStore
        └── S3AttachmentStore
```

`note-pipelines::Context` continues to own the storage and embedding trait
objects and will replace its filesystem path with
`Arc<dyn AttachmentStore>`. Pipelines remain independent of the selected
adapters.

The existing `note-storage` traits remain the database boundary. A new
`note-storage-pg` crate implements them. A new `note-attachments` crate owns the
attachment boundary and its two initial adapters. The OpenAI-compatible
implementation belongs in the existing `note-embedding` crate.

## 5. Mandatory Configuration

### 5.1 Configuration selection

- The implicit configuration path is `dev-data/config.toml`.
- `NOTE_CONFIG_PATH` selects a different path.
- In debug/development builds, a missing implicit configuration file is
  created atomically with local defaults, then read through the normal parser.
- A missing explicit `NOTE_CONFIG_PATH` is always an error.
- In release builds, a missing implicit or explicit configuration file is an
  error.
- Server, stdio, import, and export modes load the configuration file.
- The internal child embedding-worker role may continue using the configuration
  passed by its supervised parent rather than independently selecting the
  server configuration file.

### 5.2 Generated development configuration

Because relative paths resolve from the configuration file's parent, the
generated values do not repeat the `dev-data` prefix:

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

This produces:

- `dev-data/notes.db`;
- `dev-data/attachments`.

### 5.3 Resolution precedence

Each resolved value uses:

1. a value present in the TOML file;
2. its supported environment fallback;
3. its built-in default.

A generated complete local configuration therefore remains authoritative until
the user edits or removes a field. Secrets are not generated.

### 5.4 External-adapter example

```toml
[database]
engine = "pg"
# May be omitted when DATABASE_URL is set.
url = "postgresql://user:password@db/agent_note"
max_connections = 10

[embedding]
engine = "openai"
base_url = "http://embedding.internal:8000"
model = "bge-m3"
# Optional. Omit for an unauthenticated trusted endpoint.
api_key_env = "EMBEDDING_API_KEY"
timeout_secs = 30
max_retries = 3

[attachments]
engine = "s3"
bucket = "agent-note"
prefix = "attachments"
region = "us-east-1"
endpoint = "http://minio.internal:9000"
force_path_style = true
```

PostgreSQL uses `DATABASE_URL` when `database.url` is absent. S3 credentials
come from the standard AWS credential provider chain. `embedding.api_key_env`
names an environment variable; the secret itself is not stored in TOML.

Configuration parsing continues to reject unknown fields. Validation applies
to the active adapter: filesystem paths are not required for S3, PostgreSQL
URLs are not required for embedded Turso, and an embedding API key is not
required when `api_key_env` is absent.

## 6. PostgreSQL Storage Adapter

### 6.1 Crate and lifecycle

`note-storage-pg` implements `StorageBackend`, `StorageSession`,
`StorageTransaction`, and every repository contract currently implemented by
`note-storage-turso`.

Startup will:

1. create the configured PostgreSQL connection pool;
2. verify connectivity;
3. verify that the `vector` extension is available;
4. run application-owned schema migrations;
5. expose `BackendInfo` with engine `pg` and no local filesystem path.

The application will not run `CREATE EXTENSION vector`. Deployment must
provision pgvector using an appropriately privileged database role. A missing
extension produces a precise startup error.

### 6.2 Schema

The PostgreSQL schema preserves the current logical entities:

- notes and soft-deletion metadata;
- label keys and note labels;
- note chunks;
- durable embedding jobs;
- chunk embeddings;
- application settings;
- schema migration metadata.

Recommended PostgreSQL representations include:

- `text` identifiers and content;
- `bigint` Unix timestamps and note revisions;
- `jsonb` attachment metadata and application configuration;
- `vector(1024)` chunk embeddings;
- foreign keys and uniqueness constraints equivalent to the Turso schema.

Attachment bytes never enter PostgreSQL.

### 6.3 Transactions

- Deferred operations use normal PostgreSQL transactions.
- Immediate operations additionally acquire a stable
  transaction-scoped PostgreSQL advisory lock.
- The advisory lock preserves the current contract that immediate workflows
  serialize before reading and writing shared state.
- Commit and rollback remain explicit through `StorageTransaction`.
- PostgreSQL constraint, conflict, availability, and transaction failures map
  to the existing `StorageErrorKind` categories.

Embedding-job claiming uses an atomic
`FOR UPDATE SKIP LOCKED ... UPDATE ... RETURNING` operation. This is safe and
deterministic for the current single Agent Note process without promising
application-level multi-replica support.

### 6.4 Retrieval

PostgreSQL preserves the existing retrieval split:

- title FTS searches only note titles;
- dense retrieval searches only content-chunk embeddings;
- pipelines combine ranked note IDs using the existing weighted reciprocal-rank
  fusion.

Title retrieval uses:

- a stored title `tsvector`;
- the PostgreSQL `simple` dictionary for language-neutral tokenization;
- a GIN index;
- normalized literal terms combined with OR semantics;
- `ts_rank_cd` ordering;
- note ID as the deterministic final tie-breaker;
- `deleted_at IS NULL` filtering.

Dense retrieval uses:

- exact pgvector cosine distance through `<=>`;
- no ANN index;
- exactly 1,024 finite components;
- the best chunk distance per note;
- note ID as the deterministic final tie-breaker;
- active notes only.

Raw PostgreSQL and Turso relevance scores do not need to be numerically equal.
The storage contract requires stable ranked note IDs with equivalent behavioral
semantics.

## 7. Self-hosted BGE-M3 Embedding Adapter

### 7.1 Selection

- `embedding.engine = "local"` retains the existing supervised BGE-M3 worker
  process.
- `embedding.engine = "openai"` constructs an asynchronous HTTP adapter and
  does not start the local worker.
- `base_url` is the service root; the adapter appends `/v1/embeddings`.
- `model` defaults to `bge-m3`.
- Authentication is disabled when `api_key_env` is absent.
- When `api_key_env` is present, the named environment variable must exist at
  startup and is sent as a bearer token.

The adapter follows the OpenAI-compatible embeddings request and indexed
response shape documented at
<https://developers.openai.com/api/reference/resources/embeddings/methods/create>.

### 7.2 Requests and responses

The request body contains:

```json
{
  "model": "bge-m3",
  "input": ["first text", "second text"],
  "encoding_format": "float"
}
```

The adapter does not send a `dimensions` field. BGE-M3 naturally supplies the
required 1,024-dimensional vectors.

Response validation requires:

- exactly one response item per input;
- unique indices within the requested range;
- reordering by response index before returning the batch;
- exactly 1,024 finite floating-point values per vector.

Empty inputs are rejected locally. Blank retrieval queries return no results
without calling the endpoint. Content chunking must not enqueue an empty remote
embedding request.

### 7.3 Reliability and security

The adapter applies bounded exponential backoff with jitter for:

- connection failures;
- timeouts;
- HTTP 429;
- HTTP 5xx.

Authentication, authorization, and malformed-request failures are not retried.
Errors never include authorization headers or secret values, and captured
response bodies are length-limited.

### 7.4 Embedding fingerprint

Application settings store a stable embedding fingerprint. The default local
and remote BGE-M3 configurations use `bge-m3:1024`, so moving execution to the
self-hosted endpoint does not force reindexing when it is the same vector
space.

When the configured fingerprint changes, startup clears derived chunk vectors
and queues all active content chunks for re-embedding. This prevents vectors
from incompatible model spaces from being searched together.

## 8. Attachment Store

### 8.1 Boundary

`note-attachments` defines an asynchronous `AttachmentStore` contract for:

- preparing a complete attachment set and returning metadata;
- publishing a prepared set;
- aborting a prepared set;
- reading one attachment;
- hydrating all attachments;
- removing all attachments for a permanently deleted note;
- reporting backend type and location.

Prepared writes allow the database pipeline to retain its existing shape:
prepare effects, execute the database transaction, then publish or abort the
prepared attachment set.

### 8.2 Filesystem adapter

The filesystem implementation preserves the current behavior:

- write into a unique temporary note directory;
- remove the temporary directory on abort;
- replace the final note directory with a rename on publish;
- read and delete beneath the configured attachment root.

All relative-path validation remains in effect.

### 8.3 S3-compatible adapter

The S3 adapter supports AWS S3, MinIO, R2, and compatible services through:

- bucket;
- optional key prefix;
- region;
- optional endpoint;
- path-style selection;
- the standard AWS credential provider chain.

Final object keys use:

```text
<prefix>/<note-id>/<relative-attachment-path>
```

The write flow is:

1. validate every relative attachment path;
2. upload the complete new set to a unique staging prefix;
3. execute the database transaction using attachment metadata only;
4. on database failure, remove the staged objects;
5. on database success, copy the staged set to the final note prefix;
6. remove obsolete final objects;
7. remove the staging prefix.

Transient S3 operations use bounded retries. Permanent deletion removes the
final note prefix after the database row is removed. A failed delete can leave
orphan objects but cannot make a retained note unreadable.

Direct attachment downloads call the single-object read operation instead of
hydrating every attachment for the note. Full note and export responses may
still hydrate all attachment bytes.

### 8.4 Consistency limitation

PostgreSQL and S3 do not share an atomic commit. If final S3 publication fails
after its retries, the database may already reference attachment metadata whose
final objects are not yet available. The operation returns an error and logs
the staging location without credentials.

This minimal adapter release does not add a durable reconciliation outbox.
Operators may repair or remove the staged objects manually. A later release may
add immutable attachment generations and a durable promotion/cleanup queue.

## 9. Composition and Runtime Flows

### 9.1 Startup

1. Select or create the mandatory configuration file.
2. Parse and resolve configuration.
3. Validate the selected adapters.
4. Construct the storage backend.
5. Construct the attachment store.
6. Construct the local or remote embedder.
7. Verify the stored embedding fingerprint and queue regeneration if needed.
8. Start the embedding scheduler and selected transports.

Only local embedded-database and filesystem adapters cause local data
directories to be created.

### 9.2 Save and update

1. Validate note and attachment input.
2. Prepare attachment bytes through the selected store.
3. Begin a storage transaction.
4. Persist note metadata, labels, chunks, and embedding jobs.
5. Commit the storage transaction.
6. Publish prepared attachments.
7. Wake the embedding scheduler.

Storage failure aborts prepared attachment work. Attachment publication failure
after storage commit follows the limitation in section 8.4.

### 9.3 Import and export

Import and export use the configured storage and attachment adapters. Export
hydrates attachment bytes through `AttachmentStore`. Import prepares and
publishes attachment sets using the same flow as normal writes.

Import queues embeddings but does not require the local worker or remote
embedding endpoint to be available during the import command. The next normal
server run processes queued work.

Embeddings, embedding jobs, and application settings remain derived or
environment-specific and are not transferred through JSON export.

## 10. System Information

Backend-neutral system information replaces the assumption that every backend
has a local path.

The System API and UI report:

- database engine and safe location information;
- attachment engine and safe location information;
- embedding engine, model, and fingerprint.

Credentials, bearer-token environment values, PostgreSQL passwords, and AWS
credentials are never returned. No Notes UI retrieval behavior changes are
required.

## 11. Error Handling

- Missing mandatory configuration fails before constructing adapters.
- Invalid active-adapter fields identify the exact TOML field.
- Missing configured embedding API-key environment variables fail at startup.
- PostgreSQL connectivity, migration, and pgvector errors fail startup.
- Remote embedding and S3 operational failures retain their source category
  while redacting secrets.
- Wrong-dimensional or non-finite embeddings fail before storage writes.
- Adapter error messages include operation context without request
  authorization headers or database credentials.

## 12. Verification

### 12.1 Configuration

- debug implicit config generation;
- no overwrite of an existing config;
- explicit missing config failure;
- release missing config failure;
- config-file, environment, and default precedence;
- relative path resolution from `dev-data/config.toml`;
- selected-adapter validation;
- generated config contains no credentials.

### 12.2 PostgreSQL

- run the shared storage behavior contract against Turso and PostgreSQL;
- migration and pgvector validation;
- deferred and immediate transaction behavior;
- notes, labels, duplicate rules, soft deletion, restore, and purge;
- embedding-job enqueue, claim, failure, and completion;
- title-only FTS update/delete/restore behavior;
- exact 1,024-dimensional dense retrieval;
- deterministic ties and literal query normalization;
- PostgreSQL error mapping.

CI uses a real PostgreSQL service with pgvector.

### 12.3 Embeddings

Mock HTTP tests cover:

- unauthenticated requests;
- optional bearer authentication;
- default and configured model names;
- batched request bodies;
- reordered response indices;
- missing, duplicate, or out-of-range response indices;
- non-finite and wrong-dimensional vectors;
- transient retries and permanent failures;
- timeouts and redacted error output;
- fingerprint preservation and regeneration.

### 12.4 Attachments

- existing filesystem behavior and cleanup;
- S3-compatible operations against MinIO;
- staging, publication, abort, replacement, and permanent deletion;
- path traversal rejection;
- single-object download;
- import/export attachment round trips;
- transient retry behavior and redacted errors.

### 12.5 Completion gates

- `cargo fmt --all -- --check`;
- `cargo check --workspace --all-targets`;
- `cargo test --workspace`;
- `cd crates/note-frontend && cargo test`;
- `cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown`.

## 13. Delivery Boundaries

Implementation should proceed in dependency order:

1. mandatory configuration and composition types;
2. attachment contract and filesystem migration;
3. PostgreSQL adapter and integration tests;
4. OpenAI-compatible BGE-M3 adapter;
5. S3-compatible adapter;
6. system information, documentation, and full verification.

The feature is complete when every supported adapter combination can be
constructed from the mandatory config, all shared contracts pass, PostgreSQL
performs title-only FTS plus exact content-vector retrieval, and local defaults
continue to work from an auto-generated debug configuration.
