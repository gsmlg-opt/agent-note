# Title FTS and Dense Content Retrieval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace sparse content retrieval with a two-channel system that ranks Turso title FTS three times higher than exact dense body-vector retrieval and labels the Notes UI action as retrieval.

**Architecture:** Keep title full-text ranking behind the backend-neutral storage contract and keep body embeddings behind the dense-only embedding contract. Fuse ranked title IDs and exact dense-vector IDs with deterministic weighted Reciprocal Rank Fusion, while preserving existing HTTP/MCP request shapes and label filtering.

**Tech Stack:** Rust 2021, Turso Database 0.7 native FTS/Tantivy, ONNX Runtime/BGE-M3 dense vectors, Tokio, Axum, MCP, Yew/Wasm, Cargo tests.

---

## Execution prerequisites

Use the `using-git-worktrees` skill before execution and create the worktree
under this repository's `.trees/` directory, for example:

```sh
git worktree add .trees/codex/title-fts-dense-retrieval -b codex/title-fts-dense-retrieval
```

From that worktree, establish the clean baseline:

```sh
cargo fmt --all -- --check
cargo test --workspace
(cd crates/note-frontend && cargo test)
```

Expected: all commands pass before feature edits. If the baseline fails, stop
and report it rather than mixing unrelated repairs into this feature.

## File map

### Pure ranking

- Modify `crates/note-core/src/fusion.rs`
  - Add deterministic weighted RRF.
  - Remove unweighted RRF after the pipeline changes to the weighted API.

### Dense-only embedding

- Modify `crates/note-embedding/src/embedder.rs`
- Modify `crates/note-embedding/src/stub.rs`
- Modify `crates/note-embedding/src/bounded.rs`
- Modify `crates/note-embedding/src/ort_embedder.rs`
- Modify `crates/note-embedding/src/rpc.rs`
- Modify `crates/note-embedding/src/worker.rs`
- Modify `crates/note-embedding/src/process.rs`
- Modify `crates/note-pipelines/src/embedding_queue.rs`
- Modify `crates/note-pipelines/src/search_notes.rs`
- Modify `crates/note-pipelines/tests/save_note_test.rs`
- Modify `crates/note-server/tests/embedding_worker_process_test.rs`

### Turso schema and storage

- Modify `crates/note-storage/src/repositories.rs`
- Modify `crates/note-storage-turso/schema.sql`
- Modify `crates/note-storage-turso/src/lib.rs`
- Modify `crates/note-storage-turso/src/preflight.rs`
- Modify `crates/note-storage-turso/src/connection.rs`
- Modify `crates/note-storage-turso/src/retrieval.rs`
- Modify `crates/note-storage-turso/src/notes.rs`
- Modify `crates/note-storage-turso/tests/support/mod.rs`
- Modify `crates/note-storage-turso/tests/retrieval_test.rs`
- Modify `crates/note-storage-turso/tests/schema_test.rs`
- Modify `crates/note-storage-turso/tests/transactions_test.rs`

### Retrieval integration and transports

- Modify `crates/note-pipelines/tests/search_notes_test.rs`
- Modify `crates/note-server/src/notes_api.rs`
- Modify `crates/note-mcp/src/stdio.rs`

### Notes UI and current documentation

- Modify `crates/note-frontend/src/pages/notes.rs`
- Modify `README.md`
- Modify `docs/design.md`

---

### Task 1: Add deterministic weighted Reciprocal Rank Fusion

**Files:**

- Modify: `crates/note-core/src/fusion.rs`
- Test: `crates/note-core/src/fusion.rs`

- [ ] **Step 1: Add failing weighted-fusion tests**

Keep the existing unweighted tests temporarily and add:

```rust
#[test]
fn weighted_fusion_gives_title_three_times_the_same_rank_content_score() {
    let title = vec!["title-only".to_string()];
    let content = vec!["content-only".to_string()];

    let result = weighted_rrf_fuse(
        &[(3.0, title.as_slice()), (1.0, content.as_slice())],
        60.0,
    );

    assert_eq!(result[0].0, "title-only");
    let title_score = result
        .iter()
        .find(|(id, _)| id == "title-only")
        .unwrap()
        .1;
    let content_score = result
        .iter()
        .find(|(id, _)| id == "content-only")
        .unwrap()
        .1;
    assert!((title_score - 3.0 / 61.0).abs() < 1e-6);
    assert!((content_score - 1.0 / 61.0).abs() < 1e-6);
}

#[test]
fn weighted_fusion_breaks_equal_scores_by_note_id() {
    let first = vec!["b".to_string()];
    let second = vec!["a".to_string()];

    let result = weighted_rrf_fuse(
        &[(1.0, first.as_slice()), (1.0, second.as_slice())],
        60.0,
    );

    assert_eq!(
        result.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}
```

- [ ] **Step 2: Run the tests and verify the red state**

Run:

```sh
cargo test -p note-core fusion::tests::weighted_fusion
```

Expected: compilation fails because `weighted_rrf_fuse` does not exist.

- [ ] **Step 3: Implement weighted RRF alongside the current function**

Add this function above the existing `rrf_fuse`:

```rust
/// Weighted Reciprocal Rank Fusion:
/// score(note) = sum(weight / (k + one_based_rank)).
pub fn weighted_rrf_fuse(
    rankings: &[(f32, &[NoteId])],
    k: f32,
) -> Vec<(NoteId, f32)> {
    let mut scores: HashMap<NoteId, f32> = HashMap::new();
    for &(weight, ranking) in rankings {
        for (idx, note_id) in ranking.iter().enumerate() {
            let rank = (idx + 1) as f32;
            *scores.entry(note_id.clone()).or_insert(0.0) += weight / (k + rank);
        }
    }
    let mut fused: Vec<(NoteId, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}
```

Do not remove `rrf_fuse` yet; `search_notes.rs` still calls it until Task 4.

- [ ] **Step 4: Verify core tests**

Run:

```sh
cargo fmt --all
cargo test -p note-core fusion::tests
```

Expected: all fusion tests pass, including deterministic weighted ordering.

- [ ] **Step 5: Commit the weighted primitive**

```sh
git add crates/note-core/src/fusion.rs
git commit -m "feat(core): add weighted rank fusion"
```

---

### Task 2: Make embedding and its consumers dense-only

**Files:**

- Modify: `crates/note-embedding/src/embedder.rs`
- Modify: `crates/note-embedding/src/stub.rs`
- Modify: `crates/note-embedding/src/bounded.rs`
- Modify: `crates/note-embedding/src/ort_embedder.rs`
- Modify: `crates/note-embedding/src/rpc.rs`
- Modify: `crates/note-embedding/src/worker.rs`
- Modify: `crates/note-embedding/src/process.rs`
- Modify: `crates/note-pipelines/src/embedding_queue.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-server/tests/embedding_worker_process_test.rs`
- Test: unit tests in the files above

- [ ] **Step 1: Change embedding tests to express the dense-only contract**

In `crates/note-embedding/src/rpc.rs`, add:

```rust
#[test]
fn default_capabilities_are_dense_and_batch_only() {
    assert_eq!(
        default_capabilities(),
        vec![CAPABILITY_DENSE.to_string(), CAPABILITY_BATCH.to_string()]
    );
}

#[test]
fn dense_response_round_trips_through_bincode() {
    let response = EmbedResponse {
        request_id: "req-1".to_string(),
        outputs: vec![EmbedOutput {
            job_id: 7,
            result: Ok(vec![0.25; DEFAULT_EMBEDDING_DIMENSION]),
        }],
    };

    let bytes = bincode::serialize(&response).unwrap();
    let decoded: EmbedResponse = bincode::deserialize(&bytes).unwrap();
    assert_eq!(decoded, response);
}
```

In `crates/note-embedding/src/stub.rs`, change the tests to the desired direct
vector API:

```rust
#[tokio::test]
async fn dense_vector_is_1024_dimensional() {
    let dense = StubEmbedder.embed("hello world").await.unwrap();
    assert_eq!(dense.len(), 1024);
}

#[tokio::test]
async fn same_input_produces_same_output() {
    let first = StubEmbedder.embed("hello world").await.unwrap();
    let second = StubEmbedder.embed("hello world").await.unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn different_input_produces_different_output() {
    let first = StubEmbedder.embed("hello").await.unwrap();
    let second = StubEmbedder.embed("world").await.unwrap();
    assert_ne!(first, second);
}

#[tokio::test]
async fn batch_returns_one_dense_vector_per_input() {
    let inputs = vec!["first".to_string(), "second".to_string()];
    let outputs = StubEmbedder.embed_batch(&inputs).await.unwrap();
    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().all(|vector| vector.len() == 1024));
}
```

Delete the `sparse_vector_is_nonempty` test.

In `crates/note-embedding/src/worker.rs`, add:

```rust
#[test]
fn response_size_counts_dense_payload_only() {
    let response = EmbedResponse {
        request_id: "req".to_string(),
        outputs: vec![crate::rpc::EmbedOutput {
            job_id: 1,
            result: Ok(vec![0.0; 4]),
        }],
    };

    assert_eq!(
        estimate_response_bytes(&response),
        "req".len() + 4 * std::mem::size_of::<f32>() + 32
    );
}
```

In `crates/note-server/tests/embedding_worker_process_test.rs`, change the
child-process assertions to:

```rust
assert_eq!(
    handshake.model.capabilities,
    vec!["dense".to_string(), "batch".to_string()]
);
let dense = response.outputs[0].result.as_ref().unwrap();
assert_eq!(dense.len(), 1024);
```

Change the restart test result to:

```rust
let dense = embedder
    .embed("second request after restart")
    .await
    .unwrap();
assert_eq!(dense.len(), 1024);
```

- [ ] **Step 2: Verify the tests fail against the tuple/sparse API**

Run:

```sh
cargo test -p note-embedding
```

Expected: compilation fails because the current API still returns
`(DenseVector, SparseVector)` and RPC results still use `EmbeddingVectors`.

- [ ] **Step 3: Replace the embedding trait with a dense-only contract**

Replace `crates/note-embedding/src/embedder.rs` with:

```rust
pub type DenseVector = Vec<f32>; // 1024-d, L2-normalized

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector>;

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<DenseVector>> {
        let mut outputs = Vec::with_capacity(texts.len());
        for text in texts {
            outputs.push(self.embed(text).await?);
        }
        Ok(outputs)
    }
}
```

- [ ] **Step 4: Make local, bounded, and ONNX embedders return dense vectors**

In `crates/note-embedding/src/stub.rs`, retain the deterministic normalized
vector generation and end `embed` with:

```rust
Ok(dense)
```

Delete `SparseVector`, word-token hashing, and sparse fallback code.

In `crates/note-embedding/src/bounded.rs`, use:

```rust
use crate::embedder::{DenseVector, Embedder};

#[async_trait::async_trait]
impl<E: Embedder + 'static> Embedder for BoundedEmbedder<E> {
    async fn embed(&self, text: &str) -> anyhow::Result<DenseVector> {
        let _permit = self.semaphore.acquire().await?;
        self.inner.embed(text).await
    }
}
```

Make `CountingEmbedder::embed` return:

```rust
Ok(vec![0.0; 1024])
```

In `crates/note-embedding/src/ort_embedder.rs`, use the signature:

```rust
async fn embed(&self, text: &str) -> anyhow::Result<DenseVector>
```

Make the blocking closure return `anyhow::Result<DenseVector>`, move `ids`
directly into `input_ids`, and return only:

```rust
let (_dense_shape, dense): (&_, &[f32]) =
    outputs["dense_vecs"].try_extract_tensor::<f32>()?;
let dense_vec = dense
    .get(..1024)
    .ok_or_else(|| anyhow::anyhow!("dense_vecs output too short: {}", dense.len()))?
    .to_vec();
Ok(dense_vec)
```

Delete all `sparse_vecs`, `HashMap`, token filtering, and sparse weight code.

- [ ] **Step 5: Make the worker protocol dense-only and bump it to version 2**

In `crates/note-embedding/src/rpc.rs`:

```rust
use crate::DenseVector;

pub const IPC_PROTOCOL_VERSION: u16 = 2;
pub const CAPABILITY_DENSE: &str = "dense";
pub const CAPABILITY_BATCH: &str = "batch";

pub fn default_capabilities() -> Vec<String> {
    [CAPABILITY_DENSE, CAPABILITY_BATCH]
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedOutput {
    pub job_id: i64,
    pub result: Result<DenseVector, RpcError>,
}
```

Delete `CAPABILITY_SPARSE`, `EmbeddingVectors`, its constructor, and
`into_parts`.

In `crates/note-embedding/src/worker.rs`, build outputs with:

```rust
.map(|(input, dense)| crate::rpc::EmbedOutput {
    job_id: input.job_id,
    result: Ok(dense),
})
```

Replace the successful response-size arm with:

```rust
Ok(dense) => dense.len() * std::mem::size_of::<f32>() + 32,
```

In `crates/note-embedding/src/process.rs`, remove `SparseVector`, change the
trait signature to `anyhow::Result<DenseVector>`, and return:

```rust
match output.result {
    Ok(dense) => Ok(dense),
    Err(error) => Err(anyhow::anyhow!("embedding worker item error: {error}")),
}
```

- [ ] **Step 6: Update embedding-job persistence to store dense data only**

In `crates/note-pipelines/src/embedding_queue.rs`, import only
`note_embedding::DenseVector`. Change job processing to:

```rust
match ctx.embedder.embed(&job.content).await {
    Ok(dense) => complete_embedding_job(ctx, job, dense).await.map(Some),
    Err(error) => fail_embedding_job(ctx, job, &error.to_string())
        .await
        .map(Some),
}
```

Use this completion signature:

```rust
async fn complete_embedding_job(
    ctx: &Context,
    job: EmbeddingJob,
    dense: DenseVector,
) -> anyhow::Result<ProcessedEmbeddingJob>
```

Within the current-chunk branch, retain `clear_note_chunk_derived`,
`insert_chunk_embedding`, and `mark_note_chunk_status`, but delete sparse
weight conversion and `insert_chunk_sparse_weights`.

Make `PausingEmbedder::embed` return:

```rust
self.started.notify_one();
self.release.notified().await;
Ok(vec![0.1; 1024])
```

Delete the stale-result test's `sparse_postings_query` assertion.

In `crates/note-pipelines/tests/save_note_test.rs`, rename the queue test to
`embedding_worker_populates_dense_recall_table_from_queue` and use:

```rust
let dense = ctx.embedder.embed("Some content").await.unwrap();
let session = backend.session().await.unwrap();
assert!(session.chunk_embedding_exists(&note.id, 0).await.unwrap());
assert!(session
    .dense_search(&dense, 10)
    .await
    .unwrap()
    .contains(&note.id));
assert!(session
    .claim_pending_embedding_jobs(10, 1)
    .await
    .unwrap()
    .is_empty());
```

- [ ] **Step 7: Keep search compiling as a temporary dense-only pipeline**

Task 4 adds title FTS and weighted fusion. Until then, change
`crates/note-pipelines/src/search_notes.rs` to:

```rust
let dense = ctx.embedder.embed(query).await?;
```

Delete sparse token extraction and `sparse_postings_query`. After obtaining
`dense_ranking`, use:

```rust
let fused = rrf_fuse(&[dense_ranking], RRF_K);
```

Keep label filtering, note hydration, and the existing result limit unchanged.

- [ ] **Step 8: Verify dense-only embedding and all compile consumers**

Run:

```sh
cargo fmt --all
cargo test -p note-embedding
cargo test -p note-pipelines
cargo test -p note-server --test embedding_worker_process_test
cargo test --workspace
```

Expected: all tests pass. Sparse storage APIs still exist temporarily, but no
embedding or pipeline caller produces or consumes sparse vectors.

- [ ] **Step 9: Commit the dense-only boundary**

```sh
git add crates/note-embedding crates/note-pipelines/src/embedding_queue.rs crates/note-pipelines/src/search_notes.rs crates/note-pipelines/tests/save_note_test.rs crates/note-server/tests/embedding_worker_process_test.rs
git commit -m "refactor(embedding): make retrieval vectors dense only"
```

---

### Task 3: Add Turso title FTS and remove sparse storage

**Files:**

- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/schema.sql`
- Modify: `crates/note-storage-turso/src/lib.rs`
- Modify: `crates/note-storage-turso/src/preflight.rs`
- Modify: `crates/note-storage-turso/src/connection.rs`
- Modify: `crates/note-storage-turso/src/retrieval.rs`
- Modify: `crates/note-storage-turso/src/notes.rs`
- Modify: `crates/note-storage-turso/tests/support/mod.rs`
- Modify: `crates/note-storage-turso/tests/retrieval_test.rs`
- Modify: `crates/note-storage-turso/tests/schema_test.rs`
- Modify: `crates/note-storage-turso/tests/transactions_test.rs`

- [ ] **Step 1: Add failing storage tests for title FTS**

In `crates/note-storage-turso/tests/support/mod.rs`, add:

```rust
#[allow(dead_code)]
pub async fn insert_named_note(
    session: &note_storage_turso::TursoSession,
    id: &str,
    title: &str,
    content: &str,
) {
    session
        .insert_note(NewNote {
            id,
            title,
            content,
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}
```

Import it in `crates/note-storage-turso/tests/retrieval_test.rs` and add:

```rust
#[tokio::test]
async fn title_search_matches_titles_but_not_body_text() {
    let fixture = fixture().await;
    insert_named_note(
        &fixture.session,
        "title-hit",
        "Rust ownership guide",
        "unrelated body",
    )
    .await;
    insert_named_note(
        &fixture.session,
        "body-only",
        "Other guide",
        "Rust ownership body",
    )
    .await;

    assert_eq!(
        fixture.session.title_search("Rust", 10).await.unwrap(),
        vec!["title-hit"]
    );
    assert!(fixture
        .session
        .title_search("unrelated", 10)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .session
        .title_search("Rust", 0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn title_search_tracks_updates_deletion_and_restoration() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "note", "Old heading", "body").await;
    fixture
        .session
        .update_note(note_storage::NoteUpdate {
            id: "note",
            title: "New heading",
            content: "body",
            attachments: &[],
            updated_at: 2,
            note_revision: 2,
        })
        .await
        .unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert_eq!(
        observer.title_search("New", 10).await.unwrap(),
        vec!["note"]
    );
    assert!(observer
        .title_search("Old", 10)
        .await
        .unwrap()
        .is_empty());
    observer.soft_delete_note("note", 3).await.unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert!(observer
        .title_search("New", 10)
        .await
        .unwrap()
        .is_empty());
    observer.restore_note("note", 3).await.unwrap();

    let observer = fixture.storage.connect().await.unwrap();
    assert_eq!(
        observer.title_search("New", 10).await.unwrap(),
        vec!["note"]
    );
}

#[tokio::test]
async fn equal_title_scores_use_note_id_order() {
    let fixture = fixture().await;
    insert_named_note(&fixture.session, "b", "Shared heading", "body").await;
    insert_named_note(&fixture.session, "a", "Shared heading", "body").await;

    assert_eq!(
        fixture.session.title_search("Shared", 2).await.unwrap(),
        vec!["a", "b"]
    );
}
```

- [ ] **Step 2: Change schema tests to require version 2, title FTS, and no sparse table**

In `crates/note-storage-turso/tests/schema_test.rs`:

```rust
const SCHEMA_VERSION: u32 = 2;
```

Change the future-version mutation to:

```rust
before[60..64].copy_from_slice(&(SCHEMA_VERSION + 1).to_be_bytes());
```

Add a v1 rejection test:

```rust
#[tokio::test]
async fn schema_v1_is_rejected_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    drop(TursoStorage::open(&path).await.unwrap());
    let mut before = std::fs::read(&path).unwrap();
    before[60..64].copy_from_slice(&1_u32.to_be_bytes());
    std::fs::write(&path, &before).unwrap();

    let error = match TursoStorage::open(&path).await {
        Ok(_) => panic!("schema v1 was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), StorageErrorKind::UnsupportedSchema);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
```

In the logical-schema test:

```rust
assert!(indexes.iter().any(|index| index == "idx_notes_title_fts"));
assert!(!tables.iter().any(|table| table == "note_chunk_sparse"));
assert!(!indexes
    .iter()
    .any(|index| index == "idx_note_chunk_sparse_token"));
```

Remove `note_chunk_sparse` and `idx_note_chunk_sparse_token` from the expected
object arrays. Enable index methods on the direct inspection builder:

```rust
let database = turso::Builder::new_local(path.to_str().unwrap())
    .experimental_index_method(true)
    .build()
    .await
    .unwrap();
```

- [ ] **Step 3: Run the new tests and verify the red state**

Run:

```sh
cargo test -p note-storage-turso --test retrieval_test title_search
cargo test -p note-storage-turso --test schema_test
```

Expected: compilation fails because `title_search` is absent, and schema
assertions fail because the database is still schema v1 with sparse tables.

- [ ] **Step 4: Replace sparse storage methods with backend-neutral title search**

In `crates/note-storage/src/repositories.rs`, make
`RetrievalRepository` exactly:

```rust
#[async_trait::async_trait]
pub trait RetrievalRepository: Send + Sync {
    async fn insert_chunk_embedding(
        &self,
        note_id: &str,
        chunk_idx: i64,
        embedding: &[f32],
    ) -> StorageResult<()>;
    async fn dense_search(&self, query: &[f32], limit: usize) -> StorageResult<Vec<String>>;
    async fn title_search(&self, query: &str, limit: usize) -> StorageResult<Vec<String>>;
}
```

In `crates/note-storage-turso/src/retrieval.rs`, delete
`insert_chunk_sparse_weights` and `sparse_postings_query`, then add:

```rust
async fn title_search(&self, query: &str, limit: usize) -> StorageResult<Vec<String>> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let limit = checked_limit(limit, "title search")?;
    let mut rows = self
        .connection
        .query(
            "SELECT id, fts_score(title, ?1) AS score
             FROM notes
             WHERE fts_match(title, ?1) AND deleted_at IS NULL
             ORDER BY score DESC, id ASC
             LIMIT ?2",
            turso::params![query, limit],
        )
        .await
        .map_err(|error| map_turso_error("query title search", error))?;
    let mut ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| map_turso_error("read title search", error))?
    {
        ids.push(
            row.get::<String>(0)
                .map_err(|error| map_turso_error("decode title search note id", error))?,
        );
    }
    Ok(ids)
}
```

- [ ] **Step 5: Enable Turso FTS and create schema v2**

In `crates/note-storage-turso/src/lib.rs`, build the database with:

```rust
let database = turso::Builder::new_local(path_string)
    .with_io_impl(io.clone())
    .experimental_index_method(true)
    .build()
    .await
    .map_err(|error| map_turso_error("open local database", error))?;
```

In `crates/note-storage-turso/schema.sql`, add immediately after `notes`:

```sql
CREATE INDEX IF NOT EXISTS idx_notes_title_fts
ON notes USING fts (title);
```

Delete the complete `note_chunk_sparse` table and token-index definitions.

In `crates/note-storage-turso/src/preflight.rs`:

```rust
pub(crate) const SCHEMA_VERSION: u32 = 2;
```

In `crates/note-storage-turso/src/connection.rs`, replace the hard-coded
version write with:

```rust
let user_version_sql = format!("PRAGMA user_version = {SCHEMA_VERSION}");
self.connection.execute(&user_version_sql, ()).await?;
```

There is intentionally no v1-to-v2 migration.

- [ ] **Step 6: Remove sparse cleanup and tests**

In `crates/note-storage-turso/src/notes.rs`:

- `clear_note_search_data` deletes only dense embeddings, jobs, and chunks.
- `clear_note_chunk_derived` executes only:

```rust
"DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx = ?2"
```

- `clear_note_chunks_from_derived` executes only:

```rust
"DELETE FROM note_chunk_embeddings WHERE note_id = ?1 AND chunk_idx >= ?2"
```

In `crates/note-storage-turso/tests/retrieval_test.rs`, delete all
`sparse_*` tests and sparse setup/assertions from:

- `empty_inputs_and_zero_limits_return_no_results`
- `deleting_a_note_cascades_chunks_jobs_and_retrieval`

Keep their dense and embedding-job assertions.

In `crates/note-storage-turso/tests/transactions_test.rs`, delete
`insert_chunk_sparse_weights` and remove `note_chunk_sparse` from the rollback
table-count array. Add `.experimental_index_method(true)` to the direct Turso
builder used for verification.

- [ ] **Step 7: Verify schema, FTS behavior, and the workspace**

Run:

```sh
cargo fmt --all
cargo test -p note-storage-turso
cargo test --workspace
```

Expected: schema v2 tests, title FTS tests, exact dense retrieval tests, and all
workspace tests pass.

- [ ] **Step 8: Commit the storage clean break**

```sh
git add crates/note-storage crates/note-storage-turso
git commit -m "feat(storage): add Turso title FTS retrieval"
```

---

### Task 4: Fuse title FTS and dense content retrieval at 3:1

**Files:**

- Modify: `crates/note-core/src/fusion.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/tests/search_notes_test.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Add failing pipeline tests for title-only recall and title priority**

Append to `crates/note-pipelines/tests/search_notes_test.rs`:

```rust
#[tokio::test]
async fn title_only_match_is_retrieved_without_body_embeddings() {
    let (ctx, _backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Quasar handbook".into(),
            content: "Unrelated body text".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let results = search_notes(&ctx, "Quasar", 10).await.unwrap();

    assert_eq!(results[0].note.id, note.id);
}

#[tokio::test]
async fn rank_one_title_match_outranks_rank_one_content_match() {
    let (ctx, _backend, _dir) = test_context().await;
    let title_match = save_note(
        &ctx,
        SaveNoteInput {
            title: "Needle handbook".into(),
            content: "Alpha unrelated body".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    save_note(
        &ctx,
        SaveNoteInput {
            title: "Other handbook".into(),
            content: "Needle".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();
    drain_embedding_jobs(&ctx, 10).await.unwrap();

    let results = search_notes(&ctx, "Needle", 1).await.unwrap();

    assert_eq!(results[0].note.id, title_match.id);
}
```

Change `search_filters_results_by_label` so both notes match by title and body
content is unrelated:

```rust
title: "Shared Rust title".into(),
content: "Alpha body".into(),
```

and:

```rust
title: "Shared Ops title".into(),
content: "Beta body".into(),
```

Search for `"Shared"` and keep `Some("topic=rust".into())`. The assertion
remains that only the Rust-labeled note is returned.

- [ ] **Step 2: Add a failing HTTP priority test**

In the test module in `crates/note-server/src/notes_api.rs`, add:

```rust
#[tokio::test]
async fn search_prefers_a_title_match_over_an_exact_content_match() {
    let (app, ctx, _dir) = test_app().await;
    app.clone()
        .oneshot(post(
            "/api/notes",
            r#"{"title":"Needle handbook","content":"Alpha unrelated body","labels":[]}"#,
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post(
            "/api/notes",
            r#"{"title":"Other handbook","content":"Needle","labels":[]}"#,
        ))
        .await
        .unwrap();
    note_pipelines::drain_embedding_jobs(&ctx, 10)
        .await
        .unwrap();

    let response = app
        .oneshot(post(
            "/api/notes/search",
            r#"{"query":"Needle","limit":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let results: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(results[0]["title"], "Needle handbook");
}
```

- [ ] **Step 3: Verify the new tests fail against dense-only search**

Run:

```sh
cargo test -p note-pipelines --test search_notes_test
cargo test -p note-server search_prefers_a_title_match_over_an_exact_content_match
```

Expected: the title-only test returns no results, and the priority tests return
the exact content match because the title FTS ranking is not yet fused.

- [ ] **Step 4: Implement two-channel weighted retrieval**

In `crates/note-pipelines/src/search_notes.rs`, import
`weighted_rrf_fuse` and define:

```rust
const RRF_K: f32 = 60.0;
const TITLE_RRF_WEIGHT: f32 = 3.0;
const CONTENT_RRF_WEIGHT: f32 = 1.0;
```

Keep the existing limit guard, dense query embedding, label-selector
resolution, and candidate-limit calculation. Replace retrieval/fusion with:

```rust
let title_ranking = session.title_search(query, retrieval_limit).await?;
let dense_ranking = session.dense_search(&dense, retrieval_limit).await?;
let fused = weighted_rrf_fuse(
    &[
        (TITLE_RRF_WEIGHT, title_ranking.as_slice()),
        (CONTENT_RRF_WEIGHT, dense_ranking.as_slice()),
    ],
    RRF_K,
);
```

Keep post-retrieval label filtering and active-note hydration unchanged. Update
the stale-index comment to refer to title/dense retrieval rows rather than
dense/sparse rows.

- [ ] **Step 5: Remove the old unweighted fusion API**

In `crates/note-core/src/fusion.rs`, delete `rrf_fuse` and its unweighted
tests:

- `single_ranking_uses_1_based_rank`
- `combines_scores_across_rankings`
- `empty_rankings_produce_empty_result`
- `result_is_sorted_descending_by_score`

Add the weighted empty-input test:

```rust
#[test]
fn empty_weighted_rankings_produce_empty_result() {
    assert!(weighted_rrf_fuse(&[], 60.0).is_empty());
}
```

- [ ] **Step 6: Correct MCP search documentation without changing its contract**

In `crates/note-mcp/src/stdio.rs`, replace:

```rust
/// Semantic (hybrid dense+sparse) search over saved notes.
```

with:

```rust
/// Weighted title-FTS and dense-content retrieval over saved notes.
```

Do not rename `semantic_search`, its request fields, or its response schema.

- [ ] **Step 7: Verify ranking, labels, and transport compatibility**

Run:

```sh
cargo fmt --all
cargo test -p note-core fusion::tests
cargo test -p note-pipelines --test search_notes_test
cargo test -p note-mcp --test tools_test
cargo test -p note-server search_
cargo test --workspace
```

Expected: title-only recall, 3:1 title priority, label filtering, HTTP search,
MCP search, and the full workspace pass.

- [ ] **Step 8: Commit two-channel retrieval**

```sh
git add crates/note-core/src/fusion.rs crates/note-pipelines/src/search_notes.rs crates/note-pipelines/tests/search_notes_test.rs crates/note-server/src/notes_api.rs crates/note-mcp/src/stdio.rs
git commit -m "feat(retrieval): weight title FTS above dense content"
```

---

### Task 5: Change Notes UI copy from search to retrieval

**Files:**

- Modify: `crates/note-frontend/src/pages/notes.rs`
- Test: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Add a failing unit test for the approved visible copy**

Append a test module to `crates/note-frontend/src/pages/notes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_copy_names_title_and_content() {
        assert_eq!(RETRIEVAL_PLACEHOLDER, "Retrieve by title or content");
        assert_eq!(RETRIEVE_BUTTON_LABEL, "Retrieve");
    }
}
```

- [ ] **Step 2: Run the test and verify the red state**

Run:

```sh
(cd crates/note-frontend && cargo test retrieval_copy_names_title_and_content)
```

Expected: compilation fails because the two copy constants do not exist.

- [ ] **Step 3: Define and render the retrieval copy**

Near the Notes page constants, add:

```rust
const RETRIEVAL_PLACEHOLDER: &str = "Retrieve by title or content";
const RETRIEVE_BUTTON_LABEL: &str = "Retrieve";
```

Use them in the form:

```rust
<input
    class="input input-primary"
    type="text"
    placeholder={RETRIEVAL_PLACEHOLDER}
    value={(*query).clone()}
    oninput={on_query_input}
/>
<Button variant={Some("primary".to_string())}>{ RETRIEVE_BUTTON_LABEL }</Button>
```

Update the local component comments from “search bar” to “retrieval bar”.
Keep the `search` URL parameter, API function, result type, and CSS class names
unchanged.

- [ ] **Step 4: Verify native frontend logic and Wasm compilation**

Run:

```sh
(cd crates/note-frontend && cargo test)
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: frontend tests and Wasm checking pass with the new visible copy.

- [ ] **Step 5: Commit the Notes UI terminology**

```sh
git add crates/note-frontend/src/pages/notes.rs
git commit -m "feat(frontend): label note search as retrieval"
```

---

### Task 6: Update current architecture documentation and run final gates

**Files:**

- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Update README architecture and retrieval descriptions**

In `README.md`:

- Keep “hybrid retrieval” but define its two channels as title FTS and dense
  body vectors.
- Change the storage line to:

```text
Rust Turso Database: notes + labels, title FTS/BM25 + exact dense cosine
```

- Change the embedding worker line to:

```text
embedding worker — embed queued body chunks → atomically persist dense vectors
```

- Change the search pipeline line to:

```text
search_notes — title FTS + exact dense content → weighted RRF → hydrate top-k
```

- Replace “Exact hybrid retrieval” with a description of Turso title FTS
  weighted `3.0`, exact dense body retrieval weighted `1.0`, and deterministic
  weighted RRF.

- [ ] **Step 2: Bring the current design document in line with schema v2**

In `docs/design.md`:

- In the topology diagram, describe BGE-M3 dense output only and Turso title
  FTS plus exact dense cosine.
- In the schema section, add:

```sql
CREATE INDEX idx_notes_title_fts ON notes USING fts (title);
```

- Delete the `note_chunk_sparse` table and token index.
- State that the embedding worker persists only the dense body-chunk vector.
- Change the embedding output description to the 1024-dimensional dense head.
- Replace the dense+sparse retrieval section with title FTS (`3.0`) plus exact
  dense content (`1.0`) weighted RRF.
- Change `search_notes` to embed the query once for dense content retrieval,
  request title FTS IDs, and fuse the two ranked lists.
- Remove the sparse-threshold open decision.

- [ ] **Step 3: Prove sparse retrieval is absent from production code**

Run:

```sh
rg -n 'SparseVector|CAPABILITY_SPARSE|sparse_vecs|EmbeddingVectors|vectors\.sparse|insert_chunk_sparse_weights|sparse_postings_query|note_chunk_sparse' crates README.md docs/design.md
```

Expected: no matches.

Historical specs and plans under `docs/superpowers/` may still describe the
old architecture and are not rewritten.

- [ ] **Step 4: Run formatting and complete native verification**

Run:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
```

Expected: all native formatting, checks, and tests pass.

- [ ] **Step 5: Run complete frontend verification**

Run:

```sh
(cd crates/note-frontend && cargo test)
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: frontend native tests and Wasm checking pass.

- [ ] **Step 6: Verify the final diff**

Run:

```sh
git diff --check
git status --short --branch
git log --oneline -n 8
```

Expected:

- `git diff --check` has no output.
- Only intended retrieval, embedding, storage, UI, and current-doc files are
  changed.
- The feature commits are visible in recent history.

- [ ] **Step 7: Commit current documentation**

```sh
git add README.md docs/design.md
git commit -m "docs(retrieval): document title FTS and dense vectors"
```

- [ ] **Step 8: Re-run the clean-tree finish check**

Run:

```sh
git status --short --branch
```

Expected: no modified or untracked files remain in the feature worktree.
