# Title FTS and Dense Content Retrieval Design

**Date:** 2026-07-18  
**Status:** Approved for planning

## Summary

Agent Note retrieval will use exactly two independent ranking channels:

1. Turso native full-text search over note titles.
2. Exact dense-vector search over note body chunks.

The title ranking receives three times the Reciprocal Rank Fusion weight of the
content ranking. The existing BGE-M3 sparse-postings path will be removed
end-to-end rather than retained as dormant infrastructure.

The Notes UI will describe this operation as retrieval. Existing HTTP and MCP
search contracts remain compatible.

## Goals

- Return notes whose titles match the user's query, even when their bodies do
  not contain or semantically resemble the title text.
- Give a title match three times the fusion contribution of a content-vector
  match at the same rank.
- Preserve exact dense-vector retrieval over note body chunks.
- Keep storage contracts backend-neutral for planned PostgreSQL support.
- Remove unused sparse indexing, storage, and worker payloads.
- Rename user-facing Notes UI search copy to retrieval terminology.

## Non-goals

- SQLite FTS5 virtual-table compatibility.
- Full-text indexing of note bodies, labels, or attachments.
- Approximate nearest-neighbor vector search.
- Migration of existing schema-v1 test databases.
- Renaming existing HTTP routes, request fields, MCP tools, or public response
  types.
- Making retrieval weights user-configurable.

## Turso Full-text Search

Rust Turso 0.7 does not provide SQLite's `fts5` virtual-table module. Its
supported full-text implementation is the experimental, Tantivy-backed FTS
index enabled by the crate's `fts` feature and database builder option.

The storage adapter will:

- construct the database with `experimental_index_method(true)`;
- create an FTS index only over `notes.title`;
- execute this Turso-0.7-compatible query:

```sql
SELECT id, fts_score(title, ?1) AS score
FROM notes
WHERE fts_match(title, ?1) AND deleted_at IS NULL
ORDER BY score DESC, id ASC
LIMIT ?2
```

The query must be bound as a parameter rather than interpolated into SQL.
This exact projection, predicate, ordering, and limit shape has been verified
against the workspace's Turso 0.7 dependency with native FTS enabled.

The FTS index is maintained by Turso when source rows are inserted, renamed, or
permanently deleted. Soft-deleted rows remain indexed but are excluded by the
active-note predicate. Retrieval observes committed index changes; no retrieval
is performed inside a note write transaction.

Reference: <https://docs.turso.tech/sql-reference/functions/fts>

## Schema Lifecycle

The schema version will increase from 1 to 2.

Schema v2 will:

- add the title FTS index;
- retain `note_chunk_embeddings`;
- remove `note_chunk_sparse` and its token index; and
- set `PRAGMA user_version = 2`.

There will be no v1-to-v2 migration. Once the expected schema version becomes
2, databases currently accepted as v1 will be rejected by preflight as
unsupported. This is acceptable because the current database contains
disposable test data; the operator must remove it before starting the
schema-v2 build, after which the application creates a fresh database.

## Backend-neutral Storage Contract

`RetrievalRepository` will expose only the operations required by the two
retrievers:

- insert a dense chunk embedding;
- exact dense search over body chunks; and
- ranked title full-text search.

The title method will use this backend-neutral contract:

```rust
async fn title_search(
    &self,
    query: &str,
    limit: usize,
) -> StorageResult<Vec<String>>;
```

The contract returns ranked note IDs, not Turso BM25 scores. A future
PostgreSQL adapter may implement the same contract with its own full-text
facilities.

Sparse-weight insertion and sparse-postings query methods will be removed from
the contract and Turso adapter.

## Dense-only Embedding Boundary

The embedding boundary will return one 1024-dimensional dense vector per input.

The clean break includes:

- changing `Embedder::embed` to return `DenseVector`;
- changing batch embedding to return `Vec<DenseVector>`;
- removing `SparseVector`;
- removing the sparse worker capability;
- changing worker RPC output to contain only the dense vector;
- bumping the internal worker protocol version;
- reading only `dense_vecs` from the ONNX result;
- removing sparse data from response-size estimation; and
- persisting only dense chunk embeddings when embedding jobs complete.

The BGE-M3 model file may internally contain additional output heads, but they
will not be part of the application contract or persisted retrieval state.

## Retrieval and Ranking

For a non-empty query and positive limit, the pipeline will:

1. Create the dense query vector.
2. Obtain a ranked title list from storage FTS.
3. Obtain a ranked content list from exact dense-vector search.
4. Apply any label selector filtering through the existing allowed-note set.
5. Fuse the two rankings.
6. Hydrate active notes and attachments until the requested limit is reached.

Weighted Reciprocal Rank Fusion is:

```text
score(note) =
  3.0 / (k + title_rank(note))
  + 1.0 / (k + content_rank(note))
```

Missing ranks contribute zero. The existing `k = 60` remains unchanged.
Equal scores must use note ID as a deterministic secondary ordering key.
The `3:1` ratio guarantees that a title match contributes three times as much
as a content match at the same rank. It does not require every low-ranked title
match to outrank every high-ranked content match.

The pure fusion layer will replace the current unweighted `rrf_fuse` function
with an explicit `weighted_rrf_fuse` function. Its input pairs each ranked note
list with its weight; the search pipeline is the only production caller.

Without label selectors, both retrievers request `limit` candidates. With label
selectors, both request `limit * 32`, with a minimum of 128 candidates, matching
the current bounded over-fetch policy. The pipeline filters that single
candidate set and returns up to `limit` notes; it does not issue iterative
refill queries. A title hit that does not pass the label selectors must not be
returned.

## Note Mutation Behavior

- Saving a note persists its title and queues dense embeddings for body chunks.
- Updating only a title does not re-embed unchanged body content; Turso updates
  the title FTS index as part of the committed note update.
- Updating body content reuses the existing chunk-hash and dense-embedding job
  lifecycle.
- Soft deletion removes a note from retrieval results through the active-note
  predicate.
- Restoration makes the committed title eligible for FTS retrieval and queues
  missing dense body embeddings through the existing restoration flow.
- Permanent deletion cascades dense chunk data and removes the source row from
  the FTS index.

## Notes UI

Only user-facing Notes page copy changes:

- input placeholder: `Retrieve by title or content`
- submit button: `Retrieve`

The existing route state, `/api/notes/search` endpoint, `query` request field,
and MCP `semantic_search` tool remain unchanged. Internal CSS class names do
not need cosmetic renaming.

## Error Handling

- A zero result limit returns an empty result without running retrieval.
- Storage and FTS errors remain normal storage errors and propagate through the
  existing transport error mapping.
- Invalid vector dimensions retain the current validation error.
- Schema-v1 databases report the existing unsupported-schema error.
- FTS setup failure prevents database initialization; the application must not
  silently fall back to a linear title scan.

## Verification

Implementation will follow test-driven development.

### Core

- Weighted RRF applies `3:1` title/content weights.
- Missing rankings contribute zero.
- Equal fused scores use deterministic note-ID ordering.

### Embedding

- Stub and ONNX embedders expose dense vectors only.
- Worker handshakes advertise dense and batch capabilities only.
- RPC round trips and response-size checks contain no sparse payload.

### Turso storage

- Fresh schema-v2 databases contain the title FTS index and no sparse table.
- Title search matches title text but not body-only text.
- BM25 ordering is returned deterministically.
- Title updates become searchable after commit.
- Soft-deleted notes are excluded.
- Dense exact search remains unchanged.

### Pipeline and transports

- A rank-one title-only hit outranks a rank-one content-only hit.
- Dense content retrieval still returns semantically matching notes.
- Label filtering applies to both retrieval channels.
- Existing HTTP and MCP search contracts continue to pass.

### Frontend

- The Notes page renders the new retrieval placeholder and button copy.
- Frontend compilation and existing logic tests pass.

## Acceptance Criteria

- Search results are derived only from title FTS and dense body vectors.
- Title matches contribute three times the RRF weight of content-vector
  matches.
- No production schema, storage interface, embedding payload, or search path
  retains sparse retrieval data.
- The Notes page uses retrieval terminology.
- All scoped native and frontend checks pass with a fresh schema-v2 database.
