# Design: Turso Storage Adapter and Runtime Configuration

Status: Approved

## 1. Goal

Replace the embedded libSQL database with Turso Database while introducing a
driver-neutral storage boundary that can support PostgreSQL in the next
release.

This release provides one working backend, selected as `embed`, backed by the
Rust `turso` crate. The configuration format also accepts `pg`, but startup
must report that PostgreSQL is not supported in this release. Only one backend
is active in a process; dual writes, replication, and synchronization between
backends are out of scope.

Existing databases contain test data only. The refactor is a clean break:
there is no libSQL data migration or compatibility layer.

## 2. Crate Architecture

The storage boundary is split into a contract crate and concrete adapter
crates:

| Crate | Responsibility |
|---|---|
| `note-storage` | Driver-neutral storage contracts, shared storage records, transaction modes, and backend-neutral errors. It contains no SQL and depends on no database driver. |
| `note-storage-turso` | Turso Database implementation of the storage contracts, including schema creation, SQL, connection handling, transactions, and exact vector retrieval. |
| `note-storage-pg` | Reserved future adapter. It is not created in this release. |

`note-pipelines` depends only on `note-storage`. It must not import `turso`,
database parameter macros, concrete connection types, or concrete transaction
types. The `note-server` entrypoint is the composition root: it depends on
`note-storage-turso`, loads runtime configuration, constructs the selected
backend, and injects it into `note_pipelines::Context` as
`Arc<dyn StorageBackend>`.

HTTP handlers, MCP transports, and other transport modules continue to call
pipelines and remain unaware of the selected database engine. Only the narrow
composition-root module may import a concrete storage adapter.

## 3. Storage Contracts

The contract crate groups persistence operations by responsibility rather than
exposing a generic SQL executor:

- note and trash operations;
- label-key and note-label operations;
- chunk, embedding-job, dense-retrieval, and sparse-retrieval operations;
- application settings;
- backend information used by the system endpoint.

These focused repository traits compose into a backend-neutral storage session.
`StorageBackend` creates ordinary sessions and transactional units of work.
Transactional units expose the same repository operations plus explicit
`commit` and `rollback` operations.

All async contract traits use an object-safe boxed-future strategy, implemented
with `async-trait`, so sessions and units of work can be returned as boxed trait
objects behind `Arc<dyn StorageBackend>`.

An ordinary session owns one configured adapter connection. A transactional
unit owns a separate, un-cloned connection and starts a transaction with
explicit `BEGIN` SQL on that connection; it does not retain Turso's borrowing
`Transaction<'_>` type. The contract defines
`TransactionMode::{Deferred, Immediate}`. `Immediate` means the backend
acquires writer serialization before the workflow performs its reads and
writes; Turso maps it to `BEGIN IMMEDIATE`, and a future PostgreSQL adapter must
provide the closest equivalent semantics required by the workflow.

`commit(self: Box<Self>)` and `rollback(self: Box<Self>)` consume the unit of
work. Pipelines explicitly commit successful work and explicitly roll back
handled failures so rollback errors can be reported. Dropping an unfinished
unit discards its private connection and is only a best-effort rollback
safeguard; no async or reportable rollback is attempted from `Drop`.

Pipelines retain responsibility for deciding transaction boundaries so
business workflows remain in `note-pipelines`; adapters retain responsibility
for SQL and transaction mechanics. Existing database mutation groups remain
atomic at their current database transaction boundaries, including note
creation and updates, restore and purge operations, import database writes,
chunk/job synchronization, and embedding-job claims. Filesystem attachment
finalization is not part of the database transaction and retains its current
sequencing; cross-resource atomicity is not introduced by this refactor.

No low-level database abstraction is introduced. PostgreSQL is expected to
provide its own SQL behind the same domain-level contracts rather than trying
to share SQLite parameter syntax or dialect-specific statements.

Backend information exposes the selected engine plus optional location and
size values. The embedded adapter supplies its file path and byte size; a
future PostgreSQL adapter may omit values that do not have an equivalent.

## 4. Runtime Configuration

`note-server` loads an optional TOML configuration. The default location is
`./config.toml`; `NOTE_CONFIG_PATH` selects a different file.

Example:

```toml
attachments_dir = "dev-data/attachments"

[database]
engine = "embed"
path = "dev-data/notes.db"
```

Supported engine values are:

- `embed`: construct `note-storage-turso`;
- `pg`: return a clear startup error stating that PostgreSQL is not supported
  in this release.

Configuration resolution is per setting and follows this precedence:

1. a value present in the configuration file;
2. the corresponding environment variable;
3. the built-in default.

A configuration file may be partial. Only fields present in it override the
corresponding environment value.

The environment variables are:

- `NOTE_DB_ENGINE`;
- `NOTE_DB_PATH`;
- `NOTE_ATTACHMENTS_DIR`.

The built-in values are:

- database engine: `embed`;
- embedded database path: `dev-data/notes.db`;
- attachments directory: `dev-data/attachments`.

If `NOTE_CONFIG_PATH` is set, the selected file must exist and parse
successfully. A relative `NOTE_CONFIG_PATH` is resolved from the process
working directory. Without `NOTE_CONFIG_PATH`, an absent `./config.toml` is
normal and resolution continues with environment variables and defaults.

Relative setting values read from TOML resolve relative to the parent
directory of the selected configuration file. Relative values from environment
variables and built-in defaults resolve relative to the process working
directory.

Unknown TOML fields, unknown engine values, and malformed files are startup
errors. Configuration must be loaded once and shared by HTTP, stdio, import,
export, and other command modes. Pipelines no longer read attachment
environment variables directly; the resolved attachment directory is injected
through `Context`.

## 5. Turso Adapter

`note-storage-turso` uses `turso = "0.7.0"` and provides the only database
implementation in this release.

The fresh schema keeps the existing logical tables and constraints. Dense
embeddings remain `F32_BLOB(1024)` values inserted through `vector(...)`.
The following libSQL-specific objects are removed:

- `libsql_vector_idx(...)`;
- `vector_top_k(...)`;
- the DiskANN index and its shadow tables;
- vector-index optimization settings;
- vector-index maintenance code.

The `--optimize-vector-index` command and its coupled `--vacuum` mode are
removed from the server and documentation.

Every Turso connection enables and verifies `PRAGMA foreign_keys = ON` before
it is exposed as a session or unit of work. This is required for the existing
`ON DELETE CASCADE` constraints. Turso sessions also retain the existing
30-second busy timeout. Transaction modes map to the explicit Turso transaction
behavior described above. Turso-specific errors, connections, rows, values,
and parameter macros never cross into `note-storage` or `note-pipelines`.

## 6. Exact Dense Retrieval

Indexed ANN search is replaced with exact cosine-distance ranking. A note is
ranked by the closest of its chunks:

```sql
SELECT note_id
FROM note_chunk_embeddings
GROUP BY note_id
ORDER BY MIN(vector_distance_cos(embedding, vector(?1))) ASC, note_id ASC
LIMIT ?2
```

This produces one dense-ranking entry per note without application-side
deduplication and gives equal-distance results a deterministic order. It is a
linear scan over stored chunk embeddings. This performance trade-off is
explicitly accepted for this release.

Sparse-postings retrieval, label filtering, retrieval limits, and RRF fusion
retain their current behavior. The embedding model, chunking, background job
flow, and stored vector dimensionality do not change.

## 7. Existing Database Handling

There is no automatic deletion and no legacy migration.

Fresh databases are marked with SQLite `PRAGMA application_id = 0x414E4F54`
(`ANOT`) and `PRAGMA user_version = 1`. Schema creation and marker assignment
occur in one transaction, with the marker written only after the schema is
created.

Before Turso opens a configured path, the adapter performs a read-only SQLite
header preflight:

- a missing or zero-length file is eligible for fresh initialization;
- a nonempty SQLite file without the `ANOT` application identifier is rejected
  as an incompatible database without being opened;
- an `ANOT` database with an unsupported `user_version` is rejected as an
  unsupported schema version;
- filesystem read failures and invalid or truncated SQLite headers retain
  their actual I/O or corrupt-database category.

This marker check rejects every old unmarked libSQL database, including one
whose vector-index objects were manually removed, without claiming that an
arbitrary unmarked SQLite file is definitely libSQL. The error explains that
legacy test databases may be removed and recreated. Once a file passes the
header preflight, later Turso open failures retain their real error category
instead of being relabeled as legacy failures.

Tests always use temporary databases. Implementation and verification must not
silently delete a database from `dev-data` or any configured path.

## 8. Errors

The contract exposes backend-neutral storage errors with stable categories
needed by callers, while preserving the concrete driver error as an internal
source for diagnostics. Pipelines and transports must not match Turso error
variants.

Errors must clearly distinguish:

- invalid or incomplete configuration;
- unsupported `pg` selection;
- incompatible or legacy-unmarked embedded databases;
- unsupported embedded schema versions;
- constraints or conflicting writes;
- transaction commit or rollback failure;
- unavailable, corrupt, or otherwise failed backend operations.

Existing HTTP and MCP behavior remains unchanged unless a current endpoint
already maps a storage error category explicitly.

## 9. Testing

Adapter-specific tests move with the implementation into
`note-storage-turso`. They cover:

- fresh schema creation and repeated open;
- notes, labels, settings, chunks, jobs, and retrieval operations;
- busy timeout and transaction behavior;
- commit and rollback atomicity;
- foreign-key cascade behavior for note deletion and label-key deletion;
- exact dense ranking by the closest chunk;
- one dense result per note;
- deterministic ordering for equal dense distances;
- sparse retrieval and vector dimension failures;
- rejection of an unmarked legacy libSQL database without modification.

Pipeline tests construct `Arc<dyn StorageBackend>` backed by Turso. Existing
pipeline behavior must remain covered, especially:

- atomic note save and update;
- concurrent immediate writes;
- import rollback;
- embedding-job queue behavior;
- hybrid dense/sparse search and RRF ordering;
- trash restore and purge behavior.

Pipeline tests must use the storage contracts rather than adding a generic raw
SQL escape hatch. Assertions that require Turso schema introspection belong in
the Turso adapter tests.

Configuration tests cover:

- all built-in defaults;
- optional default configuration file;
- explicit `NOTE_CONFIG_PATH`;
- a partial configuration file with omitted fields falling back to environment
  and defaults independently;
- configuration-file-over-environment precedence;
- environment-over-default precedence;
- attachment-directory resolution;
- relative `NOTE_CONFIG_PATH` resolution from the working directory;
- paths relative to the configuration file;
- unknown fields and engines;
- the explicit unsupported `pg` error.

A reusable multi-backend contract-test package is deferred until the
PostgreSQL adapter exists. With only one implementation, adapter tests and
pipeline tests provide the contract coverage without adding a test abstraction
prematurely.

Verification commands are:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo tree --workspace
```

The resolved dependency tree, manifests, and lockfile must contain no
`libsql`, `libsql-sys`, or `libsql-ffi` packages. Rust sources must contain no
libSQL imports, types, macros, or API calls. Explanatory documentation and
incompatible-database error text may still name libSQL.

## 10. Documentation

Update the README and active architecture documentation to describe:

- Turso Database as the embedded engine;
- the optional `config.toml` and precedence rules;
- embedded and future PostgreSQL engine selection;
- attachment-directory configuration;
- exact vector search and its linear-scan behavior;
- removal of libSQL vector-index maintenance.

Historical implementation plans remain historical records and are not
rewritten.

## 11. Out of Scope

- PostgreSQL implementation;
- simultaneous use of embedded and PostgreSQL backends;
- data synchronization or replication;
- migration of existing libSQL databases;
- ANN or external vector indexing;
- performance targets or benchmarks for exact vector scans;
- transactional atomicity across database writes and filesystem attachments;
- changes to embedding generation, chunking, sparse retrieval, or RRF;
- changes to frontend behavior.

## 12. Acceptance Criteria

The implementation is complete when:

1. `note-storage` contains driver-neutral contracts and no concrete database
   dependency.
2. `note-storage-turso` implements every contract required by current
   pipelines.
3. Only the `note-server` composition root imports `note-storage-turso`;
   `note-pipelines`, HTTP handlers, MCP transports, and other transport modules
   contain no Turso or libSQL API usage.
4. Optional configuration selects `embed`, resolves attachment and database
   paths with file-over-environment-over-default precedence, and rejects `pg`
   clearly.
5. Exact dense search returns notes ordered by their closest chunk while sparse
   retrieval and RRF remain correct.
6. Existing unmarked libSQL databases are rejected before Turso opens them and
   are neither modified nor deleted.
7. libSQL dependencies and vector-index maintenance are removed.
8. All native formatting, checks, and workspace tests pass.
