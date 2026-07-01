# Note App Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the full note-taking app described in `docs/design.md` — a local-inference hybrid-search notes system with a Rust workspace (Axum REST + MCP server, BGE-M3 embedding via ONNX Runtime, libsql storage) and a Yew/Wasm frontend — as a 7-crate workspace built in 8 vertical, independently-testable phases.

**Architecture:** Pure core / effectful shell. `note-core` holds zero-I/O types and logic (validation, RRF fusion). `note-storage` and `note-embedding` are the two effect boundaries (DB, inference). `note-pipelines` composes them into the `save_note`/`search_notes` contracts that both `note-mcp` and `note-server`'s REST layer call — no business logic duplicated per transport. `note-frontend` is a separate Wasm target talking to `note-server` over HTTP only.

**Tech Stack:** Rust (Axum, `ort`/ONNX Runtime, `libsql`, `rmcp` for MCP), Yew + Trunk (Wasm frontend), yew-duskmoon-ui component primitives.

**Reference documents:**
- `docs/design.md` — product/pipeline contracts, storage schema, retrieval design (source of truth for *what*)
- `docs/superpowers/specs/2026-07-01-note-app-architecture-design.md` — crate boundaries, directory layout, phased build order (source of truth for *how it's structured*)

---

## Note on External Crate APIs

`ort` (ONNX Runtime bindings), `libsql`, `rmcp` (MCP SDK), and `yew-duskmoon-ui` all evolve quickly and this plan cannot guarantee the exact method signatures current at implementation time are the ones shown below. Tasks touching these crates are marked **⚠️ VERIFY API** — before writing the implementation, check the crate's current docs (`docs.rs/<crate>` via WebFetch, or `cargo doc --open` locally) and adjust the sketched code to match. The *behavior/contract* described (inputs, outputs, error conditions) is authoritative; the *exact Rust syntax* calling the external crate is a best-effort sketch. Always add dependencies with `cargo add <crate>` (not hand-typed `Cargo.toml` version numbers) so Cargo resolves whatever is actually current and compatible.

---

## Phase 1: Workspace Scaffold

### Task 1: Create workspace root and crate skeletons

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `.gitignore`
- Create: `README.md`
- Create: `crates/note-core/Cargo.toml`, `crates/note-core/src/lib.rs`
- Create: `crates/note-storage/Cargo.toml`, `crates/note-storage/src/lib.rs`
- Create: `crates/note-embedding/Cargo.toml`, `crates/note-embedding/src/lib.rs`
- Create: `crates/note-pipelines/Cargo.toml`, `crates/note-pipelines/src/lib.rs`
- Create: `crates/note-mcp/Cargo.toml`, `crates/note-mcp/src/lib.rs`
- Create: `crates/note-server/Cargo.toml`, `crates/note-server/src/main.rs`
- Create: `crates/note-frontend/Cargo.toml`, `crates/note-frontend/src/main.rs`, `crates/note-frontend/index.html`, `crates/note-frontend/Trunk.toml`
- Create: `models/.gitkeep`

- [ ] **Step 1: Create the directory skeleton**

```bash
mkdir -p crates/{note-core,note-storage,note-embedding,note-pipelines,note-mcp,note-server,note-frontend}/src
mkdir -p models
touch models/.gitkeep
```

- [ ] **Step 2: Write the workspace root `Cargo.toml`**

`note-frontend` is deliberately **excluded** from the workspace: it compiles to `wasm32-unknown-unknown` via Trunk and pulls in `wasm-bindgen`/`web-sys`, which fail to *link* (not just run) as a native binary. Keeping it as a separate, non-member crate avoids `cargo build --workspace` breaking on the native host.

```toml
[workspace]
resolver = "2"
members = [
    "crates/note-core",
    "crates/note-storage",
    "crates/note-embedding",
    "crates/note-pipelines",
    "crates/note-mcp",
    "crates/note-server",
]
exclude = ["crates/note-frontend"]

[workspace.package]
edition = "2021"
```

- [ ] **Step 3: Write `.gitignore`**

```
/target
/crates/note-frontend/target
/crates/note-frontend/dist
/models/*.onnx
!/models/.gitkeep
*.db
*.db-journal
```

- [ ] **Step 4: Write `README.md` with the manual model download step**

```markdown
# agent-note

Local-inference hybrid-search notes app. See `docs/design.md` for the full design.

## Setup

1. Install Rust (stable), [Trunk](https://trunkrs.dev/) (`cargo install trunk`), and the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`).
2. Download the BGE-M3 int8-quantized ONNX model and place it at `models/bge-m3-int8.onnx`.
   The rest of the pipeline (`note-storage`, `note-pipelines`, `note-mcp`, `note-server`) works
   without this file, using a stub embedder — only the real `OrtEmbedder` needs it.
3. `cargo build --workspace` builds everything except the frontend.
4. `cd crates/note-frontend && trunk serve` runs the frontend dev server.
```

- [ ] **Step 5: Create each native crate's `Cargo.toml` (library crates)**

For `note-core`, `note-storage`, `note-embedding`, `note-pipelines`, `note-mcp`:

```toml
[package]
name = "note-core"   # replace per crate
version = "0.1.0"
edition.workspace = true
```

(Empty `src/lib.rs` for now, just `// intentionally empty`.)

- [ ] **Step 6: Create `note-server`'s `Cargo.toml` and stub `main.rs`**

```toml
[package]
name = "note-server"
version = "0.1.0"
edition.workspace = true

[dependencies]
note-pipelines = { path = "../note-pipelines" }
note-mcp = { path = "../note-mcp" }
```

```rust
// crates/note-server/src/main.rs
fn main() {
    println!("note-server stub");
}
```

- [ ] **Step 7: Create `note-frontend`'s standalone `Cargo.toml`, `main.rs`, `index.html`, `Trunk.toml`**

```toml
# crates/note-frontend/Cargo.toml
[package]
name = "note-frontend"
version = "0.1.0"
edition = "2021"

[dependencies]
```

```rust
// crates/note-frontend/src/main.rs
fn main() {
    println!("note-frontend stub");
}
```

```html
<!-- crates/note-frontend/index.html -->
<!DOCTYPE html>
<html>
<head><title>Notes</title></head>
<body></body>
</html>
```

```toml
# crates/note-frontend/Trunk.toml
[build]
target = "index.html"
```

- [ ] **Step 8: Verify the native workspace builds**

Run: `cargo build --workspace`
Expected: builds successfully, 5 stub library crates + `note-server` binary, no errors.

- [ ] **Step 9: Verify `note-frontend` builds standalone (native check, not full wasm build yet)**

Run: `cd crates/note-frontend && cargo check`
Expected: PASS (empty stub, no deps yet, so this just confirms the crate is well-formed).

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml .gitignore README.md crates/ models/.gitkeep
git commit -m "Scaffold 7-crate workspace"
```

---

## Phase 2: `note-core` — Pure Types, Validation, RRF Fusion

### Task 2: Core types

**Files:**
- Create: `crates/note-core/src/types.rs`
- Modify: `crates/note-core/src/lib.rs`

- [ ] **Step 1: Write `types.rs`**

```rust
// crates/note-core/src/types.rs

pub type NoteId = String;

#[derive(Debug, Clone, PartialEq)]
pub struct LabelKey {
    pub key: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub key: String,
    pub value: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: NoteId,
    pub title: String,
    pub content: String,
    pub labels: Vec<Label>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub note: Note,
    pub score: f32, // fused RRF score — NOT raw cosine similarity, label it as such in the UI (design.md §7)
}
```

- [ ] **Step 2: Wire up `lib.rs`**

```rust
// crates/note-core/src/lib.rs
pub mod types;
pub use types::*;
```

- [ ] **Step 3: Run a quick compile check**

Run: `cargo check -p note-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/note-core/src/types.rs crates/note-core/src/lib.rs
git commit -m "Add note-core pure types"
```

### Task 3: Validation

**Files:**
- Create: `crates/note-core/src/validation.rs`
- Modify: `crates/note-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/note-core/src/validation.rs

#[derive(Debug, PartialEq)]
pub enum ValidationError {
    EmptyTitle,
    EmptyContent,
    UnknownLabelKey(String),
}

pub struct NoteInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>, // (key, value) pairs supplied by the caller
}

pub fn validate_note_input(
    input: &NoteInput,
    known_keys: &[String],
) -> Result<(), ValidationError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(title: &str, content: &str, labels: &[(&str, &str)]) -> NoteInput {
        NoteInput {
            title: title.to_string(),
            content: content.to_string(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn rejects_empty_title() {
        let result = validate_note_input(&input("", "content", &[]), &[]);
        assert_eq!(result, Err(ValidationError::EmptyTitle));
    }

    #[test]
    fn rejects_empty_content() {
        let result = validate_note_input(&input("title", "", &[]), &[]);
        assert_eq!(result, Err(ValidationError::EmptyContent));
    }

    #[test]
    fn rejects_unknown_label_key() {
        let result = validate_note_input(
            &input("title", "content", &[("status", "done")]),
            &[], // no known keys registered
        );
        assert_eq!(result, Err(ValidationError::UnknownLabelKey("status".to_string())));
    }

    #[test]
    fn accepts_valid_input_with_known_label() {
        let result = validate_note_input(
            &input("title", "content", &[("status", "done")]),
            &["status".to_string()],
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn accepts_valid_input_with_no_labels() {
        let result = validate_note_input(&input("title", "content", &[]), &[]);
        assert_eq!(result, Ok(()));
    }
}
```

- [ ] **Step 2: Wire into `lib.rs` and run to verify failure**

```rust
// crates/note-core/src/lib.rs (add)
pub mod validation;
pub use validation::*;
```

Run: `cargo test -p note-core`
Expected: FAIL (compile error from `todo!()` panicking at runtime — tests reach the `todo!()` and panic)

- [ ] **Step 3: Implement `validate_note_input`**

```rust
pub fn validate_note_input(
    input: &NoteInput,
    known_keys: &[String],
) -> Result<(), ValidationError> {
    if input.title.trim().is_empty() {
        return Err(ValidationError::EmptyTitle);
    }
    if input.content.trim().is_empty() {
        return Err(ValidationError::EmptyContent);
    }
    for (key, _) in &input.labels {
        if !known_keys.iter().any(|k| k == key) {
            return Err(ValidationError::UnknownLabelKey(key.clone()));
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-core`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/note-core/src/validation.rs crates/note-core/src/lib.rs
git commit -m "Add note-core input validation"
```

### Task 4: RRF fusion

**Files:**
- Create: `crates/note-core/src/fusion.rs`
- Modify: `crates/note-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/note-core/src/fusion.rs
use crate::types::NoteId;
use std::collections::HashMap;

/// Reciprocal Rank Fusion (docs/design.md §5): RRF(note) = Σ over retrievers r: 1 / (k + rank_r(note))
/// `rankings` is one ranked candidate list per retriever (e.g. dense, sparse); rank is 1-based position.
pub fn rrf_fuse(rankings: &[Vec<NoteId>], k: f32) -> Vec<(NoteId, f32)> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_ranking_uses_1_based_rank() {
        let ranking = vec!["a".to_string(), "b".to_string()];
        let result = rrf_fuse(&[ranking], 60.0);
        let get = |id: &str| result.iter().find(|(nid, _)| nid == id).unwrap().1;
        assert!((get("a") - 1.0 / 61.0).abs() < 1e-6);
        assert!((get("b") - 1.0 / 62.0).abs() < 1e-6);
    }

    #[test]
    fn combines_scores_across_rankings() {
        let dense = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let sparse = vec!["b".to_string(), "a".to_string(), "d".to_string()];
        let result = rrf_fuse(&[dense, sparse], 60.0);
        let get = |id: &str| result.iter().find(|(nid, _)| nid == id).unwrap().1;

        assert!((get("a") - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-6); // rank1 dense + rank2 sparse
        assert!((get("b") - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-6); // rank2 dense + rank1 sparse
        assert!((get("c") - (1.0 / 63.0)).abs() < 1e-6);              // rank3 dense only
        assert!((get("d") - (1.0 / 63.0)).abs() < 1e-6);              // rank3 sparse only
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn empty_rankings_produce_empty_result() {
        let result = rrf_fuse(&[], 60.0);
        assert!(result.is_empty());
    }

    #[test]
    fn result_is_sorted_descending_by_score() {
        let dense = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = rrf_fuse(&[dense], 60.0);
        for pair in result.windows(2) {
            assert!(pair[0].1 >= pair[1].1);
        }
    }
}
```

- [ ] **Step 2: Wire into `lib.rs` and run to verify failure**

```rust
// crates/note-core/src/lib.rs (add)
pub mod fusion;
pub use fusion::*;
```

Run: `cargo test -p note-core`
Expected: FAIL (`todo!()` panics)

- [ ] **Step 3: Implement `rrf_fuse`**

```rust
pub fn rrf_fuse(rankings: &[Vec<NoteId>], k: f32) -> Vec<(NoteId, f32)> {
    let mut scores: HashMap<NoteId, f32> = HashMap::new();
    for ranking in rankings {
        for (idx, note_id) in ranking.iter().enumerate() {
            let rank = (idx + 1) as f32;
            *scores.entry(note_id.clone()).or_insert(0.0) += 1.0 / (k + rank);
        }
    }
    let mut fused: Vec<(NoteId, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    fused
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-core`
Expected: PASS (9 tests total)

- [ ] **Step 5: Commit**

```bash
git add crates/note-core/src/fusion.rs crates/note-core/src/lib.rs
git commit -m "Add RRF fusion to note-core"
```

---

## Phase 3: `note-storage` — Schema & Data Access

### Task 5: Schema file

**Files:**
- Create: `crates/note-storage/schema.sql`

- [ ] **Step 1: Write `schema.sql` exactly matching docs/design.md §3**

```sql
CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE label_keys (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL
);

CREATE TABLE note_labels (
    note_id      TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id INTEGER NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value        TEXT NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);
CREATE INDEX idx_note_labels_key_value ON note_labels(label_key_id, value);

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

- [ ] **Step 2: Commit**

```bash
git add crates/note-storage/schema.sql
git commit -m "Add note-storage schema"
```

### Task 6: Database connection + schema init ⚠️ VERIFY API

**Files:**
- Create: `crates/note-storage/src/db.rs`
- Modify: `crates/note-storage/src/lib.rs`, `crates/note-storage/Cargo.toml`

Before writing this task, check `docs.rs/libsql` (WebFetch) for the current `Builder`/`Connection`/`execute`/`query` API — the sketch below reflects the general shape (local file DB, async `execute`/`query`) but exact method names may have shifted.

- [ ] **Step 1: Add dependencies**

```bash
cargo add libsql --package note-storage
cargo add tokio --package note-storage --features full
cargo add note-core --package note-storage --path ../note-core
```

- [ ] **Step 2: Write `db.rs` — open a local DB file and apply `schema.sql`**

```rust
// crates/note-storage/src/db.rs
use libsql::{Builder, Connection, Database};

const SCHEMA: &str = include_str!("../schema.sql");

pub struct Storage {
    pub db: Database,
}

impl Storage {
    pub async fn open_local(path: &str) -> anyhow::Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        Self::apply_schema(&conn).await?;
        Ok(Self { db })
    }

    async fn apply_schema(conn: &Connection) -> anyhow::Result<()> {
        for statement in SCHEMA.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            conn.execute(statement, ()).await?;
        }
        Ok(())
    }

    pub fn connect(&self) -> anyhow::Result<Connection> {
        Ok(self.db.connect()?)
    }
}
```

- [ ] **Step 3: Add `anyhow` and wire `lib.rs`**

```bash
cargo add anyhow --package note-storage
```

```rust
// crates/note-storage/src/lib.rs
pub mod db;
pub use db::*;
```

- [ ] **Step 4: Write an integration test that opens a temp DB and confirms tables exist**

```rust
// crates/note-storage/tests/schema_test.rs
use note_storage::Storage;

#[tokio::test]
async fn schema_applies_cleanly_to_a_fresh_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let storage = Storage::open_local(path.to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    let mut rows = conn
        .query("SELECT name FROM sqlite_master WHERE type='table'", ())
        .await
        .unwrap();
    let mut tables = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(0).unwrap();
        tables.push(name);
    }
    for expected in ["notes", "label_keys", "note_labels", "notes_embeddings", "notes_sparse_weights"] {
        assert!(tables.contains(&expected.to_string()), "missing table {expected}");
    }
}
```

```bash
cargo add tempfile --package note-storage --dev
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p note-storage`
Expected: PASS. If the `libsql` API sketch above doesn't compile, adjust `db.rs` to match the actual crate API (per the ⚠️ VERIFY API note) and re-run until green.

- [ ] **Step 6: Commit**

```bash
git add crates/note-storage/src/db.rs crates/note-storage/src/lib.rs crates/note-storage/Cargo.toml crates/note-storage/tests/schema_test.rs
git commit -m "Add note-storage DB connection and schema init"
```

### Task 7: Notes CRUD ⚠️ VERIFY API

**Files:**
- Create: `crates/note-storage/src/notes.rs`
- Modify: `crates/note-storage/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/note-storage/tests/notes_test.rs
use note_storage::{Storage, insert_note, get_note};

#[tokio::test]
async fn insert_then_get_returns_the_same_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000).await.unwrap();
    let fetched = get_note(&conn, "note-1").await.unwrap().expect("note should exist");

    assert_eq!(fetched.title, "Title");
    assert_eq!(fetched.content, "Content");
}

#[tokio::test]
async fn get_returns_none_for_missing_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    let fetched = get_note(&conn, "does-not-exist").await.unwrap();
    assert!(fetched.is_none());
}
```

Run: `cargo test -p note-storage notes_test`
Expected: FAIL (functions don't exist yet)

- [ ] **Step 2: Implement `notes.rs`**

```rust
// crates/note-storage/src/notes.rs
use libsql::Connection;
use note_core::{Note, NoteId};

pub async fn insert_note(
    conn: &Connection,
    id: &str,
    title: &str,
    content: &str,
    created_at: i64,
    updated_at: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO notes (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id, title, content, created_at, updated_at],
    )
    .await?;
    Ok(())
}

pub async fn get_note(conn: &Connection, id: &str) -> anyhow::Result<Option<Note>> {
    let mut rows = conn
        .query(
            "SELECT id, title, content, created_at, updated_at FROM notes WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        Ok(Some(Note {
            id: row.get::<String>(0)?,
            title: row.get::<String>(1)?,
            content: row.get::<String>(2)?,
            labels: vec![], // labels hydrated separately — see Task 9
            created_at: row.get::<i64>(3)?,
            updated_at: row.get::<i64>(4)?,
        }))
    } else {
        Ok(None)
    }
}
```

- [ ] **Step 3: Wire into `lib.rs`**

```rust
// crates/note-storage/src/lib.rs (add)
pub mod notes;
pub use notes::*;
```

Also add `note-core` as a dependency if not already present from Task 6.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-storage`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/note-storage/src/notes.rs crates/note-storage/src/lib.rs crates/note-storage/tests/notes_test.rs
git commit -m "Add notes CRUD to note-storage"
```

### Task 8: Label key catalog CRUD ⚠️ VERIFY API

**Files:**
- Create: `crates/note-storage/src/label_keys.rs`
- Modify: `crates/note-storage/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/note-storage/tests/label_keys_test.rs
use note_storage::{Storage, insert_label_key, list_label_keys};

#[tokio::test]
async fn insert_then_list_returns_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_label_key(&conn, "status", "Workflow status of the note").await.unwrap();
    let keys = list_label_keys(&conn).await.unwrap();

    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
    assert_eq!(keys[0].description, "Workflow status of the note");
}

#[tokio::test]
async fn duplicate_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_label_key(&conn, "status", "first").await.unwrap();
    let result = insert_label_key(&conn, "status", "second").await;
    assert!(result.is_err());
}
```

Run: `cargo test -p note-storage label_keys_test`
Expected: FAIL (functions don't exist)

- [ ] **Step 2: Implement `label_keys.rs`**

```rust
// crates/note-storage/src/label_keys.rs
use libsql::Connection;
use note_core::LabelKey;

pub async fn insert_label_key(conn: &Connection, key: &str, description: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO label_keys (key, description) VALUES (?1, ?2)",
        libsql::params![key, description],
    )
    .await?; // UNIQUE constraint on `key` makes duplicates error out naturally
    Ok(())
}

pub async fn list_label_keys(conn: &Connection) -> anyhow::Result<Vec<LabelKey>> {
    let mut rows = conn.query("SELECT key, description FROM label_keys ORDER BY key", ()).await?;
    let mut keys = vec![];
    while let Some(row) = rows.next().await? {
        keys.push(LabelKey {
            key: row.get::<String>(0)?,
            description: row.get::<String>(1)?,
        });
    }
    Ok(keys)
}
```

- [ ] **Step 3: Wire into `lib.rs`, run tests, commit**

```rust
// crates/note-storage/src/lib.rs (add)
pub mod label_keys;
pub use label_keys::*;
```

Run: `cargo test -p note-storage`
Expected: PASS

```bash
git add crates/note-storage/src/label_keys.rs crates/note-storage/src/lib.rs crates/note-storage/tests/label_keys_test.rs
git commit -m "Add label-key catalog CRUD to note-storage"
```

### Task 9: Note-label attachment + hydration ⚠️ VERIFY API

**Files:**
- Create: `crates/note-storage/src/note_labels.rs`
- Modify: `crates/note-storage/src/lib.rs`, `crates/note-storage/src/notes.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/note-storage/tests/note_labels_test.rs
use note_storage::{Storage, insert_note, insert_label_key, attach_label, labels_for_note};

#[tokio::test]
async fn attach_and_fetch_labels_for_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000).await.unwrap();
    insert_label_key(&conn, "status", "Workflow status").await.unwrap();
    attach_label(&conn, "note-1", "status", "done").await.unwrap();

    let labels = labels_for_note(&conn, "note-1").await.unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].key, "status");
    assert_eq!(labels[0].value, "done");
    assert_eq!(labels[0].description, "Workflow status");
}

#[tokio::test]
async fn attaching_unknown_key_fails() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "Title", "Content", 1000, 1000).await.unwrap();
    let result = attach_label(&conn, "note-1", "does-not-exist", "value").await;
    assert!(result.is_err());
}
```

Run: `cargo test -p note-storage note_labels_test`
Expected: FAIL

- [ ] **Step 2: Implement `note_labels.rs`**

```rust
// crates/note-storage/src/note_labels.rs
use libsql::Connection;
use note_core::Label;

pub async fn attach_label(conn: &Connection, note_id: &str, key: &str, value: &str) -> anyhow::Result<()> {
    let label_key_id: i64 = {
        let mut rows = conn.query("SELECT id FROM label_keys WHERE key = ?1", libsql::params![key]).await?;
        let row = rows.next().await?.ok_or_else(|| anyhow::anyhow!("unknown label key: {key}"))?;
        row.get::<i64>(0)?
    };
    conn.execute(
        "INSERT INTO note_labels (note_id, label_key_id, value) VALUES (?1, ?2, ?3)",
        libsql::params![note_id, label_key_id, value],
    )
    .await?;
    Ok(())
}

pub async fn labels_for_note(conn: &Connection, note_id: &str) -> anyhow::Result<Vec<Label>> {
    let mut rows = conn
        .query(
            "SELECT lk.key, nl.value, lk.description
             FROM note_labels nl JOIN label_keys lk ON lk.id = nl.label_key_id
             WHERE nl.note_id = ?1",
            libsql::params![note_id],
        )
        .await?;
    let mut labels = vec![];
    while let Some(row) = rows.next().await? {
        labels.push(Label {
            key: row.get::<String>(0)?,
            value: row.get::<String>(1)?,
            description: row.get::<String>(2)?,
        });
    }
    Ok(labels)
}
```

- [ ] **Step 3: Update `get_note` (Task 7) to hydrate labels instead of leaving `vec![]`**

```rust
// crates/note-storage/src/notes.rs — replace the `labels: vec![]` line in get_note with:
labels: labels_for_note(conn, id).await?,
```

(This requires `get_note` to no longer be a single-query function — restructure it to call `labels_for_note` after fetching the row, before returning.)

- [ ] **Step 4: Wire into `lib.rs`, run all note-storage tests, commit**

```rust
// crates/note-storage/src/lib.rs (add)
pub mod note_labels;
pub use note_labels::*;
```

Run: `cargo test -p note-storage`
Expected: PASS (all note-storage tests green, including the updated `notes_test.rs` since `get_note` now hydrates labels)

```bash
git add crates/note-storage/src/note_labels.rs crates/note-storage/src/notes.rs crates/note-storage/src/lib.rs crates/note-storage/tests/note_labels_test.rs
git commit -m "Attach and hydrate note labels in note-storage"
```

### Task 10: Dense ANN query and sparse postings query ⚠️ VERIFY API

**Files:**
- Create: `crates/note-storage/src/retrieval.rs`
- Modify: `crates/note-storage/src/lib.rs`

This task is the highest-risk one in `note-storage` for API drift: `libsql_vector_idx` / vector search syntax is a newer libsql feature and its exact SQL function names (`vector_top_k`, `vector_distance_cos`, etc.) should be checked against current libsql docs before writing the query strings below.

- [ ] **Step 1: Write the failing test for sparse postings query (simpler, standard SQL — do this first)**

```rust
// crates/note-storage/tests/retrieval_test.rs
use note_storage::{Storage, insert_note, insert_sparse_weights, sparse_postings_query};

#[tokio::test]
async fn sparse_query_ranks_by_matching_token_weight() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000).await.unwrap();
    insert_note(&conn, "note-2", "B", "content", 1000, 1000).await.unwrap();
    insert_sparse_weights(&conn, "note-1", &[(42, 0.9)]).await.unwrap();
    insert_sparse_weights(&conn, "note-2", &[(42, 0.3)]).await.unwrap();

    let ranked = sparse_postings_query(&conn, &[42], 10).await.unwrap();
    assert_eq!(ranked, vec!["note-1".to_string(), "note-2".to_string()]);
}
```

Run: `cargo test -p note-storage retrieval_test`
Expected: FAIL

- [ ] **Step 2: Implement `insert_sparse_weights` and `sparse_postings_query`**

```rust
// crates/note-storage/src/retrieval.rs
use libsql::Connection;

pub async fn insert_sparse_weights(conn: &Connection, note_id: &str, weights: &[(i64, f64)]) -> anyhow::Result<()> {
    for (token_id, weight) in weights {
        conn.execute(
            "INSERT INTO notes_sparse_weights (note_id, token_id, weight) VALUES (?1, ?2, ?3)",
            libsql::params![note_id, *token_id, *weight],
        )
        .await?;
    }
    Ok(())
}

/// Ranks notes by summed weight over the given query token ids. Descending by summed weight.
pub async fn sparse_postings_query(conn: &Connection, token_ids: &[i64], limit: usize) -> anyhow::Result<Vec<String>> {
    let placeholders: Vec<String> = (1..=token_ids.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT note_id, SUM(weight) as total FROM notes_sparse_weights
         WHERE token_id IN ({}) GROUP BY note_id ORDER BY total DESC LIMIT {}",
        placeholders.join(","), limit
    );
    let params: Vec<libsql::Value> = token_ids.iter().map(|t| (*t).into()).collect();
    let mut rows = conn.query(&sql, params).await?;
    let mut ids = vec![];
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}
```

- [ ] **Step 3: Run to verify `sparse_postings_query` passes, then write the dense ANN query test**

Run: `cargo test -p note-storage retrieval_test::sparse_query_ranks_by_matching_token_weight`
Expected: PASS

```rust
// append to crates/note-storage/tests/retrieval_test.rs
use note_storage::{insert_embedding, dense_ann_query};

#[tokio::test]
async fn dense_query_returns_nearest_neighbors() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let conn = storage.connect().unwrap();

    insert_note(&conn, "note-1", "A", "content", 1000, 1000).await.unwrap();
    insert_note(&conn, "note-2", "B", "content", 1000, 1000).await.unwrap();

    let mut close = vec![1.0f32; 1024];
    close[0] = 1.01;
    let mut far = vec![0.0f32; 1024];
    far[0] = 1.0;

    insert_embedding(&conn, "note-1", &close).await.unwrap();
    insert_embedding(&conn, "note-2", &far).await.unwrap();

    let query = vec![1.0f32; 1024];
    let ranked = dense_ann_query(&conn, &query, 10).await.unwrap();
    assert_eq!(ranked[0], "note-1"); // closer to the query vector
}
```

Run: `cargo test -p note-storage retrieval_test::dense_query_returns_nearest_neighbors`
Expected: FAIL (`insert_embedding`/`dense_ann_query` don't exist)

- [ ] **Step 4: Implement `insert_embedding` and `dense_ann_query` — ⚠️ verify the vector SQL functions against current libsql docs before finalizing**

```rust
pub async fn insert_embedding(conn: &Connection, note_id: &str, embedding: &[f32]) -> anyhow::Result<()> {
    // libsql's F32_BLOB vector functions typically accept a JSON array string via vector32(); confirm exact
    // function name/signature against current libsql docs.
    let json = serde_json::to_string(embedding)?;
    conn.execute(
        "INSERT INTO notes_embeddings (note_id, embedding) VALUES (?1, vector32(?2))",
        libsql::params![note_id, json],
    )
    .await?;
    Ok(())
}

pub async fn dense_ann_query(conn: &Connection, query: &[f32], limit: usize) -> anyhow::Result<Vec<String>> {
    let json = serde_json::to_string(query)?;
    let sql = format!(
        "SELECT note_id FROM vector_top_k('idx_notes_embedding', vector32(?1), {})",
        limit
    );
    let mut rows = conn.query(&sql, libsql::params![json]).await?;
    let mut ids = vec![];
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}
```

```bash
cargo add serde_json --package note-storage
```

- [ ] **Step 5: Run tests, adjusting the vector SQL syntax against actual libsql docs if it doesn't compile/pass**

Run: `cargo test -p note-storage`
Expected: PASS

- [ ] **Step 6: Wire into `lib.rs` and commit**

```rust
// crates/note-storage/src/lib.rs (add)
pub mod retrieval;
pub use retrieval::*;
```

```bash
git add crates/note-storage/src/retrieval.rs crates/note-storage/src/lib.rs crates/note-storage/Cargo.toml crates/note-storage/tests/retrieval_test.rs
git commit -m "Add dense ANN and sparse postings queries to note-storage"
```

---

## Phase 4: `note-embedding` — Embedder Abstraction

### Task 11: `Embedder` trait + `StubEmbedder`

**Files:**
- Create: `crates/note-embedding/src/embedder.rs`
- Create: `crates/note-embedding/src/stub.rs`
- Modify: `crates/note-embedding/src/lib.rs`, `crates/note-embedding/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add async-trait --package note-embedding
```

- [ ] **Step 2: Write the trait**

```rust
// crates/note-embedding/src/embedder.rs
use std::collections::HashMap;

pub type DenseVector = Vec<f32>;      // 1024-d, L2-normalized
pub type SparseVector = HashMap<i64, f32>; // token_id -> weight, thresholded

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// Dense + sparse output from a single inference call (docs/design.md §4/§6 — must not be
    /// split into two separate calls/methods, since BGE-M3 produces both from one forward pass).
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)>;
}
```

```bash
cargo add anyhow --package note-embedding
```

- [ ] **Step 3: Write the failing test for `StubEmbedder`**

```rust
// crates/note-embedding/src/stub.rs
use crate::embedder::{DenseVector, Embedder, SparseVector};

pub struct StubEmbedder;

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dense_vector_is_1024_dimensional() {
        let (dense, _) = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(dense.len(), 1024);
    }

    #[tokio::test]
    async fn same_input_produces_same_output() {
        let (d1, s1) = StubEmbedder.embed("hello world").await.unwrap();
        let (d2, s2) = StubEmbedder.embed("hello world").await.unwrap();
        assert_eq!(d1, d2);
        assert_eq!(s1, s2);
    }

    #[tokio::test]
    async fn different_input_produces_different_output() {
        let (d1, _) = StubEmbedder.embed("hello").await.unwrap();
        let (d2, _) = StubEmbedder.embed("world").await.unwrap();
        assert_ne!(d1, d2);
    }

    #[tokio::test]
    async fn sparse_vector_is_nonempty() {
        let (_, sparse) = StubEmbedder.embed("hello world").await.unwrap();
        assert!(!sparse.is_empty());
    }
}
```

```bash
cargo add tokio --package note-embedding --features full --dev
```

Run: `cargo test -p note-embedding`
Expected: FAIL (`todo!()` panics)

- [ ] **Step 4: Implement `StubEmbedder` — deterministic hash-based, no model needed**

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let seed = hasher.finish();

        // Deterministic pseudo-random dense vector, seeded from the text hash.
        let mut dense = Vec::with_capacity(1024);
        let mut state = seed;
        for _ in 0..1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            dense.push(((state >> 33) as f32 / u32::MAX as f32) - 0.5);
        }
        let norm: f32 = dense.iter().map(|x| x * x).sum::<f32>().sqrt();
        for v in dense.iter_mut() {
            *v /= norm;
        }

        // A handful of deterministic "token" entries derived from word hashes.
        let mut sparse = SparseVector::new();
        for word in text.split_whitespace() {
            let mut wh = DefaultHasher::new();
            word.hash(&mut wh);
            let token_id = (wh.finish() % 100_000) as i64;
            sparse.insert(token_id, 1.0);
        }
        if sparse.is_empty() {
            sparse.insert(0, 1.0); // never return a fully empty sparse map
        }

        Ok((dense, sparse))
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p note-embedding`
Expected: PASS (4 tests)

- [ ] **Step 6: Wire into `lib.rs` and commit**

```rust
// crates/note-embedding/src/lib.rs
pub mod embedder;
pub mod stub;
pub use embedder::*;
pub use stub::*;
```

```bash
git add crates/note-embedding/src/embedder.rs crates/note-embedding/src/stub.rs crates/note-embedding/src/lib.rs crates/note-embedding/Cargo.toml
git commit -m "Add Embedder trait and StubEmbedder"
```

### Task 12: `OrtEmbedder` skeleton (deferred real wiring) ⚠️ VERIFY API

**Files:**
- Create: `crates/note-embedding/src/ort_embedder.rs`
- Modify: `crates/note-embedding/src/lib.rs`

Per `docs/design.md` and the architecture spec, real BGE-M3 ONNX weights are a manual download (`models/bge-m3-int8.onnx`), not fetched automatically. This task defines the real embedder's shape against the `Embedder` trait but does **not** require model weights to compile — actual inference correctness is verified manually once weights are placed, not by the automated test suite.

- [ ] **Step 1: Add the `ort` dependency**

```bash
cargo add ort --package note-embedding
```

- [ ] **Step 2: Write `OrtEmbedder` — ⚠️ check `docs.rs/ort` for the current Session/Value API before filling this in**

```rust
// crates/note-embedding/src/ort_embedder.rs
use crate::embedder::{DenseVector, Embedder, SparseVector};
use std::path::Path;

pub struct OrtEmbedder {
    // session: ort::Session, // populate once the current `ort` API is confirmed
}

impl OrtEmbedder {
    pub fn load(_model_path: &Path) -> anyhow::Result<Self> {
        // TODO: build an ort::Session from model_path once the exact ort API (session builder,
        // execution providers, int8 handling) is confirmed against current docs.rs/ort.
        // int8 quantization per docs/design.md §4 — do not attempt int4, see design.md rationale.
        anyhow::bail!("OrtEmbedder::load not yet implemented — see Task 12 in the implementation plan")
    }
}

#[async_trait::async_trait]
impl Embedder for OrtEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        // Inference must run via spawn_blocking, never inline on the async reactor (docs/design.md §4).
        // A single forward pass must produce both dense and sparse output (see Embedder trait doc comment).
        anyhow::bail!("OrtEmbedder::embed not yet implemented — see Task 12 in the implementation plan")
    }
}
```

- [ ] **Step 3: Verify it compiles (no functional test yet — no model weights available in this environment)**

Run: `cargo check -p note-embedding`
Expected: PASS

- [ ] **Step 4: Wire into `lib.rs` and commit**

```rust
// crates/note-embedding/src/lib.rs (add)
pub mod ort_embedder;
pub use ort_embedder::*;
```

```bash
git add crates/note-embedding/src/ort_embedder.rs crates/note-embedding/src/lib.rs crates/note-embedding/Cargo.toml
git commit -m "Add OrtEmbedder skeleton (real wiring deferred until model weights available)"
```

### Task 13: Backpressure wrapper (spawn_blocking + semaphore)

**Files:**
- Create: `crates/note-embedding/src/bounded.rs`
- Modify: `crates/note-embedding/src/lib.rs`, `crates/note-embedding/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add tokio --package note-embedding --features full
```

- [ ] **Step 2: Write the failing test — bounds concurrency to the semaphore size**

```rust
// crates/note-embedding/src/bounded.rs
use crate::embedder::{DenseVector, Embedder, SparseVector};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub struct BoundedEmbedder<E: Embedder + 'static> {
    inner: Arc<E>,
    semaphore: Arc<Semaphore>,
}

impl<E: Embedder + 'static> BoundedEmbedder<E> {
    pub fn new(inner: E, max_concurrent: usize) -> Self {
        todo!()
    }
}

#[async_trait::async_trait]
impl<E: Embedder + 'static> Embedder for BoundedEmbedder<E> {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct CountingEmbedder {
        in_flight: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok((vec![0.0; 1024], Default::default()))
        }
    }

    #[tokio::test]
    async fn limits_concurrent_calls_to_semaphore_size() {
        let max_observed = Arc::new(AtomicUsize::new(0));
        let inner = CountingEmbedder {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_observed: max_observed.clone(),
        };
        let bounded = Arc::new(BoundedEmbedder::new(inner, 2));

        let mut handles = vec![];
        for _ in 0..10 {
            let b = bounded.clone();
            handles.push(tokio::spawn(async move { b.embed("x").await.unwrap() }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert!(max_observed.load(Ordering::SeqCst) <= 2, "concurrency exceeded semaphore limit");
    }
}
```

Run: `cargo test -p note-embedding bounded`
Expected: FAIL (`todo!()` panics)

- [ ] **Step 3: Implement `BoundedEmbedder`**

```rust
impl<E: Embedder + 'static> BoundedEmbedder<E> {
    pub fn new(inner: E, max_concurrent: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }
}

#[async_trait::async_trait]
impl<E: Embedder + 'static> Embedder for BoundedEmbedder<E> {
    async fn embed(&self, text: &str) -> anyhow::Result<(DenseVector, SparseVector)> {
        let _permit = self.semaphore.acquire().await?;
        self.inner.embed(text).await
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-embedding`
Expected: PASS

- [ ] **Step 5: Wire into `lib.rs` and commit**

```rust
// crates/note-embedding/src/lib.rs (add)
pub mod bounded;
pub use bounded::*;
```

```bash
git add crates/note-embedding/src/bounded.rs crates/note-embedding/src/lib.rs crates/note-embedding/Cargo.toml
git commit -m "Add semaphore-bounded backpressure wrapper for Embedder"
```

Note: `spawn_blocking` itself is not exercised here because `StubEmbedder`/`CountingEmbedder` are already non-blocking async code. When `OrtEmbedder`'s real inference call is implemented (Task 12, post-model-download), wrap its blocking ONNX Runtime call in `tokio::task::spawn_blocking` inside `OrtEmbedder::embed` — the semaphore in `BoundedEmbedder` bounds how many such blocking calls run at once, per docs/design.md §4.

---

## Phase 5: `note-pipelines` — Shared Core

### Task 14: `Context` struct

**Files:**
- Create: `crates/note-pipelines/src/context.rs`
- Modify: `crates/note-pipelines/src/lib.rs`, `crates/note-pipelines/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add note-core --package note-pipelines --path ../note-core
cargo add note-storage --package note-pipelines --path ../note-storage
cargo add note-embedding --package note-pipelines --path ../note-embedding
cargo add anyhow --package note-pipelines
```

- [ ] **Step 2: Write `Context`**

```rust
// crates/note-pipelines/src/context.rs
use note_embedding::Embedder;
use note_storage::Storage;
use std::sync::Arc;

/// One struct holding a pooled DB handle and the shared embedder instance (docs/design.md §6).
/// Constructed once per process (Axum) or per stdio invocation — passed explicitly, no globals.
pub struct Context {
    pub storage: Arc<Storage>,
    pub embedder: Arc<dyn Embedder>,
}

impl Context {
    pub fn new(storage: Arc<Storage>, embedder: Arc<dyn Embedder>) -> Self {
        Self { storage, embedder }
    }
}
```

- [ ] **Step 3: Compile check, wire into `lib.rs`, commit**

```rust
// crates/note-pipelines/src/lib.rs
pub mod context;
pub use context::*;
```

Run: `cargo check -p note-pipelines`
Expected: PASS

```bash
git add crates/note-pipelines/src/context.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/Cargo.toml
git commit -m "Add note-pipelines Context struct"
```

### Task 15: `define_label_key` / `list_label_keys` pipelines

**Files:**
- Create: `crates/note-pipelines/src/label_keys.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/note-pipelines/tests/label_keys_test.rs
use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, list_label_keys, Context};
use note_storage::Storage;
use std::sync::Arc;

async fn test_context() -> Context {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    Context::new(Arc::new(storage), Arc::new(StubEmbedder))
}

#[tokio::test]
async fn define_then_list_roundtrips() {
    let ctx = test_context().await;
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();
    let keys = list_label_keys(&ctx).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "status");
}

#[tokio::test]
async fn empty_description_is_rejected() {
    let ctx = test_context().await;
    let result = define_label_key(&ctx, "status", "").await;
    assert!(result.is_err());
}
```

```bash
cargo add tempfile --package note-pipelines --dev
cargo add tokio --package note-pipelines --features full --dev
```

Run: `cargo test -p note-pipelines label_keys_test`
Expected: FAIL

- [ ] **Step 2: Implement**

```rust
// crates/note-pipelines/src/label_keys.rs
use crate::context::Context;
use note_core::LabelKey;

pub async fn define_label_key(ctx: &Context, key: &str, description: &str) -> anyhow::Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("label key must not be empty");
    }
    if description.trim().is_empty() {
        anyhow::bail!("label key description must not be empty");
    }
    let conn = ctx.storage.connect()?;
    note_storage::insert_label_key(&conn, key, description).await
}

pub async fn list_label_keys(ctx: &Context) -> anyhow::Result<Vec<LabelKey>> {
    let conn = ctx.storage.connect()?;
    note_storage::list_label_keys(&conn).await
}
```

- [ ] **Step 3: Run tests, wire into `lib.rs`, commit**

```rust
// crates/note-pipelines/src/lib.rs (add)
pub mod label_keys;
pub use label_keys::*;
```

Run: `cargo test -p note-pipelines`
Expected: PASS

```bash
git add crates/note-pipelines/src/label_keys.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/Cargo.toml crates/note-pipelines/tests/label_keys_test.rs
git commit -m "Add define_label_key/list_label_keys pipelines"
```

### Task 16: `save_note` pipeline

**Files:**
- Create: `crates/note-pipelines/src/save_note.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/note-pipelines/tests/save_note_test.rs
use note_embedding::StubEmbedder;
use note_pipelines::{define_label_key, save_note, Context, SaveNoteInput};
use note_storage::Storage;
use std::sync::Arc;

async fn test_context() -> Context {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    Context::new(Arc::new(storage), Arc::new(StubEmbedder))
}

#[tokio::test]
async fn saves_and_returns_a_persisted_note() {
    let ctx = test_context().await;
    define_label_key(&ctx, "status", "Workflow status").await.unwrap();

    let note = save_note(&ctx, SaveNoteInput {
        title: "My note".into(),
        content: "Some content".into(),
        labels: vec![("status".into(), "done".into())],
    }).await.unwrap();

    assert_eq!(note.title, "My note");
    assert_eq!(note.labels.len(), 1);
    assert_eq!(note.labels[0].key, "status");
    assert!(!note.id.is_empty());
}

#[tokio::test]
async fn unknown_label_key_rejects_the_whole_request() {
    let ctx = test_context().await;
    let result = save_note(&ctx, SaveNoteInput {
        title: "My note".into(),
        content: "Some content".into(),
        labels: vec![("nonexistent".into(), "value".into())],
    }).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn empty_title_is_rejected() {
    let ctx = test_context().await;
    let result = save_note(&ctx, SaveNoteInput {
        title: "".into(),
        content: "Some content".into(),
        labels: vec![],
    }).await;
    assert!(result.is_err());
}
```

Run: `cargo test -p note-pipelines save_note_test`
Expected: FAIL (types/function don't exist)

- [ ] **Step 2: Implement `save_note`**

```rust
// crates/note-pipelines/src/save_note.rs
use crate::context::Context;
use note_core::{validate_note_input, Label, Note, NoteInput};

pub struct SaveNoteInput {
    pub title: String,
    pub content: String,
    pub labels: Vec<(String, String)>,
}

pub async fn save_note(ctx: &Context, input: SaveNoteInput) -> anyhow::Result<Note> {
    let conn = ctx.storage.connect()?;

    let known_keys: Vec<String> = note_storage::list_label_keys(&conn)
        .await?
        .into_iter()
        .map(|k| k.key)
        .collect();

    validate_note_input(
        &NoteInput {
            title: input.title.clone(),
            content: input.content.clone(),
            labels: input.labels.clone(),
        },
        &known_keys,
    )
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let (dense, sparse) = ctx.embedder.embed(&input.content).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    // Atomic write across notes, notes_embeddings, notes_sparse_weights (docs/design.md §3).
    // NOTE: this sketch issues sequential statements on one connection; wrap in an explicit
    // transaction (conn.transaction()) once the exact libsql transaction API is confirmed —
    // a partial write here would silently degrade the note to dense-only recall, which
    // docs/design.md §3 calls a correctness bug, not a soft failure.
    note_storage::insert_note(&conn, &id, &input.title, &input.content, now, now).await?;
    note_storage::insert_embedding(&conn, &id, &dense).await?;
    let weights: Vec<(i64, f64)> = sparse.into_iter().map(|(k, v)| (k, v as f64)).collect();
    note_storage::insert_sparse_weights(&conn, &id, &weights).await?;

    let mut resolved_labels: Vec<Label> = vec![];
    for (key, value) in &input.labels {
        note_storage::attach_label(&conn, &id, key, value).await?;
    }
    resolved_labels = note_storage::labels_for_note(&conn, &id).await?;

    Ok(Note {
        id,
        title: input.title,
        content: input.content,
        labels: resolved_labels,
        created_at: now,
        updated_at: now,
    })
}
```

```bash
cargo add uuid --package note-pipelines --features v4
cargo add chrono --package note-pipelines
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p note-pipelines`
Expected: PASS

- [ ] **Step 4: Wire into `lib.rs` and commit**

```rust
// crates/note-pipelines/src/lib.rs (add)
pub mod save_note;
pub use save_note::*;
```

```bash
git add crates/note-pipelines/src/save_note.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/Cargo.toml crates/note-pipelines/tests/save_note_test.rs
git commit -m "Add save_note pipeline"
```

**Follow-up before moving on:** the atomic-transaction TODO above is a known gap — either resolve it now by wrapping the three inserts in a real libsql transaction (⚠️ verify the transaction API against current docs.rs/libsql), or open a tracked follow-up before considering Phase 5 done, since docs/design.md explicitly calls partial writes here a correctness bug.

### Task 17: `search_notes` pipeline

**Files:**
- Create: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/note-pipelines/tests/search_notes_test.rs
use note_embedding::StubEmbedder;
use note_pipelines::{save_note, search_notes, Context, SaveNoteInput};
use note_storage::Storage;
use std::sync::Arc;

async fn test_context() -> Context {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    Context::new(Arc::new(storage), Arc::new(StubEmbedder))
}

#[tokio::test]
async fn search_returns_saved_notes_with_fused_scores() {
    let ctx = test_context().await;
    save_note(&ctx, SaveNoteInput {
        title: "Rust ownership".into(),
        content: "Ownership and borrowing in Rust".into(),
        labels: vec![],
    }).await.unwrap();
    save_note(&ctx, SaveNoteInput {
        title: "Grocery list".into(),
        content: "Milk eggs bread".into(),
        labels: vec![],
    }).await.unwrap();

    let results = search_notes(&ctx, "Rust ownership", 10).await.unwrap();

    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.note.title == "Rust ownership"));
    for r in &results {
        assert!(r.score > 0.0);
    }
}
```

Run: `cargo test -p note-pipelines search_notes_test`
Expected: FAIL

- [ ] **Step 2: Implement `search_notes`**

```rust
// crates/note-pipelines/src/search_notes.rs
use crate::context::Context;
use note_core::{rrf_fuse, SearchResult};

const RRF_K: f32 = 60.0; // docs/design.md §5 — tunable, revisit if fusion balance looks off

pub async fn search_notes(ctx: &Context, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
    let conn = ctx.storage.connect()?;
    let (dense, sparse) = ctx.embedder.embed(query).await?;

    let token_ids: Vec<i64> = sparse.keys().copied().collect();

    let (dense_ranking, sparse_ranking) = tokio::try_join!(
        note_storage::dense_ann_query(&conn, &dense, limit),
        note_storage::sparse_postings_query(&conn, &token_ids, limit),
    )?;

    let fused = rrf_fuse(&[dense_ranking, sparse_ranking], RRF_K);

    let mut results = vec![];
    for (note_id, score) in fused.into_iter().take(limit) {
        if let Some(note) = note_storage::get_note(&conn, &note_id).await? {
            results.push(SearchResult { note, score });
        }
    }
    Ok(results)
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p note-pipelines`
Expected: PASS

- [ ] **Step 4: Wire into `lib.rs` and commit**

```rust
// crates/note-pipelines/src/lib.rs (add)
pub mod search_notes;
pub use search_notes::*;
```

```bash
git add crates/note-pipelines/src/search_notes.rs crates/note-pipelines/src/lib.rs crates/note-pipelines/tests/search_notes_test.rs
git commit -m "Add search_notes pipeline with RRF fusion"
```

---

## Phase 6: `note-mcp` — MCP Tools & Transports ⚠️ VERIFY API

Before starting this phase, check `docs.rs/rmcp` (or whichever Rust MCP SDK is current — WebFetch the MCP Rust SDK's current repo/docs) for the current tool-definition and transport API. The tool *contracts* below (inputs/outputs, calling `note-pipelines` directly) are authoritative; the SDK plumbing is a sketch.

### Task 18: Tool definitions

**Files:**
- Create: `crates/note-mcp/src/tools.rs`
- Modify: `crates/note-mcp/src/lib.rs`, `crates/note-mcp/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add rmcp --package note-mcp
cargo add note-pipelines --package note-mcp --path ../note-pipelines
cargo add serde --package note-mcp --features derive
```

- [ ] **Step 2: Define tool input/output types and thin wrappers over the pipelines**

```rust
// crates/note-mcp/src/tools.rs
use note_pipelines::{save_note, search_notes, Context, SaveNoteInput as PipelineSaveNoteInput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SaveNoteToolInput {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
pub struct SaveNoteToolOutput {
    pub id: String,
}

pub async fn save_note_tool(ctx: &Context, input: SaveNoteToolInput) -> anyhow::Result<SaveNoteToolOutput> {
    let note = save_note(ctx, PipelineSaveNoteInput {
        title: input.title,
        content: input.content,
        labels: input.labels,
    }).await?;
    Ok(SaveNoteToolOutput { id: note.id })
}

#[derive(Debug, Deserialize)]
pub struct SemanticSearchToolInput {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct SemanticSearchToolResult {
    pub id: String,
    pub title: String,
    pub score: f32,
}

pub async fn semantic_search_tool(ctx: &Context, input: SemanticSearchToolInput) -> anyhow::Result<Vec<SemanticSearchToolResult>> {
    let results = search_notes(ctx, &input.query, input.limit).await?;
    Ok(results
        .into_iter()
        .map(|r| SemanticSearchToolResult { id: r.note.id, title: r.note.title, score: r.score })
        .collect())
}
```

- [ ] **Step 3: Write tests exercising the tool wrappers against `StubEmbedder`**

```rust
// crates/note-mcp/tests/tools_test.rs
use note_embedding::StubEmbedder;
use note_mcp::{save_note_tool, semantic_search_tool, SaveNoteToolInput, SemanticSearchToolInput};
use note_pipelines::Context;
use note_storage::Storage;
use std::sync::Arc;

async fn test_context() -> Context {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    Context::new(Arc::new(storage), Arc::new(StubEmbedder))
}

#[tokio::test]
async fn save_note_tool_returns_an_id() {
    let ctx = test_context().await;
    let result = save_note_tool(&ctx, SaveNoteToolInput {
        title: "Title".into(),
        content: "Content".into(),
        labels: vec![],
    }).await.unwrap();
    assert!(!result.id.is_empty());
}

#[tokio::test]
async fn semantic_search_tool_finds_saved_note() {
    let ctx = test_context().await;
    save_note_tool(&ctx, SaveNoteToolInput {
        title: "Findable".into(),
        content: "unique searchable content".into(),
        labels: vec![],
    }).await.unwrap();

    let results = semantic_search_tool(&ctx, SemanticSearchToolInput {
        query: "unique searchable content".into(),
        limit: 5,
    }).await.unwrap();

    assert!(results.iter().any(|r| r.title == "Findable"));
}
```

```bash
cargo add note-embedding --package note-mcp --path ../note-embedding --dev
cargo add note-storage --package note-mcp --path ../note-storage --dev
cargo add tempfile --package note-mcp --dev
cargo add tokio --package note-mcp --features full --dev
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p note-mcp tools_test`
Expected: PASS

- [ ] **Step 5: Wire into `lib.rs` and commit**

```rust
// crates/note-mcp/src/lib.rs
pub mod tools;
pub use tools::*;
```

```bash
git add crates/note-mcp/src/tools.rs crates/note-mcp/src/lib.rs crates/note-mcp/Cargo.toml crates/note-mcp/tests/tools_test.rs
git commit -m "Add MCP tool wrappers over note-pipelines"
```

### Task 19: stdio transport ⚠️ VERIFY API

**Files:**
- Create: `crates/note-mcp/src/stdio.rs`
- Modify: `crates/note-mcp/src/lib.rs`

- [ ] **Step 1: Check the current `rmcp` (or chosen SDK) stdio transport API via WebFetch/docs.rs**

- [ ] **Step 2: Implement a `run_stdio(ctx: Context)` entrypoint that registers `save_note_tool`/`semantic_search_tool` and serves JSON-RPC over stdin/stdout, logging to stderr**

```rust
// crates/note-mcp/src/stdio.rs
use crate::tools::{save_note_tool, semantic_search_tool};
use note_pipelines::Context;

/// Runs the MCP server over stdio (JSON-RPC on stdin/stdout, logs to stderr per docs/design.md §8).
/// Exact SDK wiring (tool registration macros/builders) depends on the current rmcp API — fill in
/// once confirmed; the two tools above are the only thing this transport exposes.
pub async fn run_stdio(_ctx: Context) -> anyhow::Result<()> {
    anyhow::bail!("stdio transport not yet wired — verify rmcp's stdio transport API first")
}
```

- [ ] **Step 3: Compile check, wire into `lib.rs`, commit as a checkpoint (functional wiring completed once SDK API confirmed)**

```rust
// crates/note-mcp/src/lib.rs (add)
pub mod stdio;
pub use stdio::*;
```

Run: `cargo check -p note-mcp`
Expected: PASS

```bash
git add crates/note-mcp/src/stdio.rs crates/note-mcp/src/lib.rs
git commit -m "Add note-mcp stdio transport skeleton"
```

### Task 20: Streamable HTTP transport ⚠️ VERIFY API

**Files:**
- Create: `crates/note-mcp/src/http.rs`
- Modify: `crates/note-mcp/src/lib.rs`

- [ ] **Step 1: Check the current `rmcp` Streamable HTTP transport API (single `/mcp` endpoint, POST+GET, optional per-response SSE upgrade — docs/design.md §8; do NOT implement the legacy two-endpoint HTTP+SSE transport)**

- [ ] **Step 2: Implement an Axum-compatible handler/router fragment exposing `/mcp`**

```rust
// crates/note-mcp/src/http.rs
use axum::Router;
use note_pipelines::Context;
use std::sync::Arc;

/// Builds the `/mcp` Streamable HTTP router (single endpoint, POST+GET, optional SSE upgrade
/// per-response for long calls — docs/design.md §8). Mounted by note-server (Phase 7).
pub fn mcp_router(_ctx: Arc<Context>) -> Router {
    todo!("wire rmcp's Streamable HTTP transport into an Axum Router once the API is confirmed")
}
```

```bash
cargo add axum --package note-mcp
```

- [ ] **Step 3: Compile check, wire into `lib.rs`, commit**

```rust
// crates/note-mcp/src/lib.rs (add)
pub mod http;
pub use http::*;
```

Run: `cargo check -p note-mcp`
Expected: PASS

```bash
git add crates/note-mcp/src/http.rs crates/note-mcp/src/lib.rs crates/note-mcp/Cargo.toml
git commit -m "Add note-mcp Streamable HTTP transport skeleton"
```

---

## Phase 7: `note-server` — Axum REST + Entrypoint Dispatch

### Task 21: `/api/notes` REST handlers

**Files:**
- Create: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add axum --package note-server
cargo add tokio --package note-server --features full
cargo add serde --package note-server --features derive
cargo add serde_json --package note-server
```

- [ ] **Step 2: Implement POST `/api/notes` (save) and GET `/api/notes/search` (search)**

```rust
// crates/note-server/src/notes_api.rs
use axum::{extract::State, routing::{get, post}, Json, Router};
use note_pipelines::{save_note, search_notes, Context, SaveNoteInput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SaveNoteRequest {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct SaveNoteResponse {
    pub id: String,
}

async fn save_note_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<SaveNoteRequest>,
) -> Result<Json<SaveNoteResponse>, (axum::http::StatusCode, String)> {
    let note = save_note(&ctx, SaveNoteInput { title: req.title, content: req.content, labels: req.labels })
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(SaveNoteResponse { id: note.id }))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct SearchResultDto {
    pub id: String,
    pub title: String,
    pub score: f32, // fused RRF score — label as such in any client UI, not "similarity" (docs/design.md §7)
}

async fn search_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<SearchQuery>,
) -> Result<Json<Vec<SearchResultDto>>, (axum::http::StatusCode, String)> {
    let results = search_notes(&ctx, &req.query, req.limit)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        results.into_iter().map(|r| SearchResultDto { id: r.note.id, title: r.note.title, score: r.score }).collect(),
    ))
}

pub fn notes_router() -> Router<Arc<Context>> {
    Router::new()
        .route("/api/notes", post(save_note_handler))
        .route("/api/notes/search", get(search_handler))
}
```

- [ ] **Step 2: Write an integration test using Axum's test utilities (or a plain function call test, since handlers are just async functions over `Context`)**

```rust
// crates/note-server/tests/notes_api_test.rs
use note_embedding::StubEmbedder;
use note_pipelines::{search_notes, save_note, Context, SaveNoteInput};
use note_storage::Storage;
use std::sync::Arc;

#[tokio::test]
async fn saved_note_is_findable_via_pipelines_directly() {
    // note-server's handlers are thin wrappers over note-pipelines (already tested in Phase 5);
    // this test confirms the wiring compiles and the same Context type flows through cleanly.
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_local(dir.path().join("test.db").to_str().unwrap()).await.unwrap();
    let ctx = Context::new(Arc::new(storage), Arc::new(StubEmbedder));

    save_note(&ctx, SaveNoteInput { title: "T".into(), content: "C unique".into(), labels: vec![] }).await.unwrap();
    let results = search_notes(&ctx, "C unique", 5).await.unwrap();
    assert!(!results.is_empty());
}
```

```bash
cargo add note-pipelines --package note-server --path ../note-pipelines --dev
cargo add note-embedding --package note-server --path ../note-embedding --dev
cargo add note-storage --package note-server --path ../note-storage --dev
cargo add tempfile --package note-server --dev
```

- [ ] **Step 3: Run tests, commit**

Run: `cargo test -p note-server`
Expected: PASS

```bash
git add crates/note-server/src/notes_api.rs crates/note-server/Cargo.toml crates/note-server/tests/notes_api_test.rs
git commit -m "Add /api/notes REST handlers"
```

### Task 22: `/api/labels` REST handlers

**Files:**
- Create: `crates/note-server/src/labels_api.rs`

- [ ] **Step 1: Implement POST `/api/labels` (define) and GET `/api/labels` (list) — REST/UI-only per docs/design.md §8, not exposed over MCP**

```rust
// crates/note-server/src/labels_api.rs
use axum::{extract::State, routing::{get, post}, Json, Router};
use note_core::LabelKey;
use note_pipelines::{define_label_key, list_label_keys, Context};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct DefineLabelKeyRequest {
    pub key: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct LabelKeyDto {
    pub key: String,
    pub description: String,
}

impl From<LabelKey> for LabelKeyDto {
    fn from(lk: LabelKey) -> Self {
        Self { key: lk.key, description: lk.description }
    }
}

async fn define_label_key_handler(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<DefineLabelKeyRequest>,
) -> Result<(), (axum::http::StatusCode, String)> {
    define_label_key(&ctx, &req.key, &req.description)
        .await
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e.to_string()))
}

async fn list_label_keys_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<LabelKeyDto>>, (axum::http::StatusCode, String)> {
    let keys = list_label_keys(&ctx)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub fn labels_router() -> Router<Arc<Context>> {
    Router::new().route("/api/labels", post(define_label_key_handler).get(list_label_keys_handler))
}
```

- [ ] **Step 2: Compile check and commit**

Run: `cargo check -p note-server`
Expected: PASS

```bash
git add crates/note-server/src/labels_api.rs
git commit -m "Add /api/labels REST handlers"
```

### Task 23: Entrypoint dispatch (stdio vs HTTP) and DB pool sizing

**Files:**
- Modify: `crates/note-server/src/main.rs`

- [ ] **Step 1: Wire `main.rs` to dispatch on a `--stdio` flag, constructing its own `Context` either way (docs/design.md §2 — same binary, never proxies through Axum)**

```rust
// crates/note-server/src/main.rs
mod labels_api;
mod notes_api;

use axum::Router;
use note_embedding::{BoundedEmbedder, StubEmbedder}; // swap StubEmbedder for OrtEmbedder once model weights are available
use note_pipelines::Context;
use note_storage::Storage;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let stdio_mode = std::env::args().any(|a| a == "--stdio");
    let db_path = std::env::var("NOTE_DB_PATH").unwrap_or_else(|_| "notes.db".to_string());

    if stdio_mode {
        // stdio is single-client — no connection pool needed (docs/design.md §9).
        let storage = Storage::open_local(&db_path).await?;
        let embedder = BoundedEmbedder::new(StubEmbedder, 1);
        let ctx = Context::new(Arc::new(storage), Arc::new(embedder));
        note_mcp::run_stdio(ctx).await?;
    } else {
        // Axum is the only process that needs connection pooling (docs/design.md §2).
        // TODO: size the pool deliberately once concurrency requirements are clearer (docs/design.md §9
        // open decision) — starting with a single shared Storage handle is a placeholder, not a final answer.
        let storage = Storage::open_local(&db_path).await?;
        let embedder = BoundedEmbedder::new(StubEmbedder, 4);
        let ctx = Arc::new(Context::new(Arc::new(storage), Arc::new(embedder)));

        let app: Router = Router::new()
            .merge(notes_api::notes_router())
            .merge(labels_api::labels_router())
            .merge(note_mcp::mcp_router(ctx.clone()))
            .with_state(ctx);

        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        axum::serve(listener, app).await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Verify the workspace still builds end to end**

Run: `cargo build --workspace`
Expected: PASS (note: `note_mcp::run_stdio`/`mcp_router` still return errors at runtime per Tasks 19–20's deferred SDK wiring — this step only confirms compilation)

- [ ] **Step 3: Commit**

```bash
git add crates/note-server/src/main.rs
git commit -m "Wire note-server entrypoint dispatch (stdio vs HTTP)"
```

---

## Phase 8: `note-frontend` — Yew MVU UI ⚠️ VERIFY yew-duskmoon-ui API

Before this phase, confirm the actual package name and component API for "yew-duskmoon-ui" (docs/design.md §7) via WebFetch/crates.io/docs.rs — this plan assumes it exists as described but that hasn't been independently verified.

### Task 24: `AppState` + reducer

**Files:**
- Create: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cd crates/note-frontend
cargo add yew --features csr
cd ../..
```

- [ ] **Step 2: Write the failing test for the reducer (pure function, testable without a browser)**

```rust
// crates/note-frontend/src/state.rs
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchResultSummary {
    pub id: String,
    pub title: String,
    pub score: f32, // fused RRF score, label as such (docs/design.md §7) — not "similarity"
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    pub notes: Vec<NoteSummary>,
    pub search_results: Vec<SearchResultSummary>,
    pub loading: bool,
    pub error: Option<String>,
}

pub enum Action {
    SearchStarted,
    SearchSucceeded(Vec<SearchResultSummary>),
    SearchFailed(String),
}

pub fn reduce(state: &AppState, action: Action) -> AppState {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_started_sets_loading_and_clears_error() {
        let state = AppState { error: Some("old error".into()), ..Default::default() };
        let next = reduce(&state, Action::SearchStarted);
        assert!(next.loading);
        assert_eq!(next.error, None);
    }

    #[test]
    fn search_succeeded_stores_results_and_clears_loading() {
        let state = AppState { loading: true, ..Default::default() };
        let results = vec![SearchResultSummary { id: "1".into(), title: "T".into(), score: 0.5 }];
        let next = reduce(&state, Action::SearchSucceeded(results.clone()));
        assert!(!next.loading);
        assert_eq!(next.search_results, results);
    }

    #[test]
    fn search_failed_stores_error_and_clears_loading() {
        let state = AppState { loading: true, ..Default::default() };
        let next = reduce(&state, Action::SearchFailed("boom".into()));
        assert!(!next.loading);
        assert_eq!(next.error, Some("boom".into()));
    }
}
```

Run: `cargo test -p note-frontend` (native test run of the pure reducer logic — no wasm target needed for this)
Expected: FAIL (`todo!()` panics)

- [ ] **Step 3: Implement `reduce`**

```rust
pub fn reduce(state: &AppState, action: Action) -> AppState {
    match action {
        Action::SearchStarted => AppState { loading: true, error: None, ..state.clone() },
        Action::SearchSucceeded(results) => AppState { loading: false, search_results: results, ..state.clone() },
        Action::SearchFailed(err) => AppState { loading: false, error: Some(err), ..state.clone() },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass, commit**

Run: `cargo test -p note-frontend`
Expected: PASS

```bash
git add crates/note-frontend/src/state.rs crates/note-frontend/Cargo.toml
git commit -m "Add note-frontend AppState and reducer"
```

### Task 25: `NoteEditor` and `VectorSearch` components ⚠️ VERIFY yew-duskmoon-ui API

**Files:**
- Create: `crates/note-frontend/src/components/note_editor.rs`
- Create: `crates/note-frontend/src/components/vector_search.rs`
- Modify: `crates/note-frontend/src/main.rs`

- [ ] **Step 1: Add the yew-duskmoon-ui dependency (⚠️ confirm exact package name first)**

```bash
cd crates/note-frontend
cargo add yew-duskmoon-ui   # confirm this is the real crate name before running
cd ../..
```

- [ ] **Step 2: Sketch `NoteEditor` — form emits a note on submit; label picker lists registered keys with descriptions (docs/design.md §7)**

```rust
// crates/note-frontend/src/components/note_editor.rs
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct NoteEditorProps {
    pub available_labels: Vec<(String, String)>, // (key, description) from list_label_keys
    pub on_submit: Callback<(String, String, Vec<(String, String)>)>, // (title, content, labels)
}

#[function_component(NoteEditor)]
pub fn note_editor(props: &NoteEditorProps) -> Html {
    // TODO: build the form using duskmoon-ui's Card/Input/TextArea/Tag primitives once the
    // exact component API is confirmed (docs/design.md §7). Tag renders each attached label as
    // key=value with the key's description as a tooltip/hint.
    html! { <div>{"NoteEditor placeholder"}</div> }
}
```

- [ ] **Step 3: Sketch `VectorSearch` — query input, renders fused-score results labeled as such**

```rust
// crates/note-frontend/src/components/vector_search.rs
use crate::state::SearchResultSummary;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct VectorSearchProps {
    pub results: Vec<SearchResultSummary>,
    pub loading: bool,
    pub on_query: Callback<String>,
}

#[function_component(VectorSearch)]
pub fn vector_search(props: &VectorSearchProps) -> Html {
    // TODO: build with duskmoon-ui primitives. Render each result's score labeled "fused score",
    // never "similarity" (docs/design.md §7 — it's a rank fusion, not a raw distance).
    html! { <div>{"VectorSearch placeholder"}</div> }
}
```

- [ ] **Step 4: Wire a minimal `main.rs` that renders both components with an empty reducer-backed state**

```rust
// crates/note-frontend/src/main.rs
mod components;
mod state;

use components::note_editor::NoteEditor;
use components::vector_search::VectorSearch;
use state::AppState;
use yew::prelude::*;

#[function_component(App)]
fn app() -> Html {
    let state = use_state(AppState::default);
    html! {
        <div>
            <NoteEditor available_labels={vec![]} on_submit={Callback::noop()} />
            <VectorSearch results={state.search_results.clone()} loading={state.loading} on_query={Callback::noop()} />
        </div>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
```

- [ ] **Step 5: Attempt a wasm build**

Run: `cd crates/note-frontend && trunk build`
Expected: PASS. If `yew-duskmoon-ui`'s real API differs from the placeholder sketch above, fix the component bodies against its actual docs before this passes — the placeholders exist so structure/wiring can be verified independently of that crate's exact API.

- [ ] **Step 6: Commit**

```bash
git add crates/note-frontend/src/components crates/note-frontend/src/main.rs crates/note-frontend/Cargo.toml
git commit -m "Add NoteEditor and VectorSearch component skeletons"
```

### Task 26: Wire frontend to `note-server` REST API

**Files:**
- Create: `crates/note-frontend/src/api.rs`
- Modify: `crates/note-frontend/src/main.rs`

- [ ] **Step 1: Add an HTTP client dependency**

```bash
cd crates/note-frontend
cargo add gloo-net
cd ../..
```

- [ ] **Step 2: Implement thin fetch wrappers matching note-server's REST DTOs (Task 21/22)**

```rust
// crates/note-frontend/src/api.rs
use crate::state::SearchResultSummary;
use gloo_net::http::Request;
use serde::Deserialize;

#[derive(Deserialize)]
struct SearchResultDto {
    id: String,
    title: String,
    score: f32,
}

pub async fn search(query: &str, limit: usize) -> Result<Vec<SearchResultSummary>, gloo_net::Error> {
    let body = serde_json::json!({ "query": query, "limit": limit });
    let dtos: Vec<SearchResultDto> = Request::get("/api/notes/search")
        .json(&body)?
        .send()
        .await?
        .json()
        .await?;
    Ok(dtos.into_iter().map(|d| SearchResultSummary { id: d.id, title: d.title, score: d.score }).collect())
}
```

```bash
cd crates/note-frontend
cargo add serde --features derive
cargo add serde_json
cd ../..
```

- [ ] **Step 3: Wire `main.rs`'s search callback to call `api::search` and dispatch through `reduce`**

```rust
// crates/note-frontend/src/main.rs — replace the on_query={Callback::noop()} wiring with:
let on_query = {
    let state = state.clone();
    Callback::from(move |query: String| {
        let state = state.clone();
        wasm_bindgen_futures::spawn_local(async move {
            state.set(state::reduce(&state, state::Action::SearchStarted));
            match api::search(&query, 10).await {
                Ok(results) => state.set(state::reduce(&state, state::Action::SearchSucceeded(results))),
                Err(e) => state.set(state::reduce(&state, state::Action::SearchFailed(e.to_string()))),
            }
        });
    })
};
```

```bash
cd crates/note-frontend
cargo add wasm-bindgen-futures
cd ../..
```

- [ ] **Step 4: Build and manually verify in a browser**

Run: `cd crates/note-frontend && trunk serve`
Manually open the served URL, confirm the page loads without console errors (searching won't return real results until `note-server` is also running with a reachable DB, but the page should render and the fetch should at least attempt the request).

- [ ] **Step 5: Commit**

```bash
git add crates/note-frontend/src/api.rs crates/note-frontend/src/main.rs crates/note-frontend/Cargo.toml
git commit -m "Wire note-frontend search to note-server REST API"
```

---

## Final Verification

- [ ] Run the full native test suite: `cargo test --workspace`
- [ ] Run the full native build: `cargo build --workspace`
- [ ] Run the frontend build: `cd crates/note-frontend && trunk build`
- [ ] Confirm no `todo!()`/`unimplemented!()` remain **except** the explicitly-deferred `OrtEmbedder` real inference body (Task 12) and the MCP transport wiring pending SDK API confirmation (Tasks 19–20) and the atomic-transaction follow-up flagged in Task 16 — everything else should be fully implemented and tested.
