# Note Search Minimum Score Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a persisted global minimum fused-search score, apply it to web/REST/MCP searches, and render web results as the Notes table with Score first.

**Architecture:** Extend the backward-compatible `SystemConfig` JSON with `search.minimum_score`, then enforce the inclusive threshold once in `note-pipelines::search_notes_filtered`. REST expands its summary DTO for the existing full-note metadata, MCP keeps its current response shape, and Yew reuses the Notes table behavior for scored rows.

**Tech Stack:** Rust 2021, Tokio, Serde, Axum/Utoipa, rmcp/Schemars, Turso/PostgreSQL storage contracts, Yew/Wasm, DuskMoon CSS.

---

## File Map

- `crates/note-core/src/system_config.rs`: owns the persisted search setting, default, and validation.
- `crates/note-storage-contract-tests/src/settings.rs`: proves both storage engines round-trip the new setting.
- `crates/note-storage-turso/tests/settings_test.rs`, `crates/note-storage-pg/tests/repositories_test.rs`: keep adapter-specific settings fixtures complete.
- `crates/note-pipelines/tests/system_test.rs`, `crates/note-pipelines/tests/save_note_test.rs`, `crates/note-mcp/tests/tools_test.rs`: keep explicit System configuration fixtures complete.
- `crates/note-pipelines/src/search_notes.rs`: owns global threshold enforcement.
- `crates/note-pipelines/tests/search_notes_test.rs`: proves inclusive threshold behavior at the shared boundary.
- `crates/note-server/src/openapi.rs`: mirrors the runtime System configuration schema.
- `crates/note-server/src/system_api.rs`: proves default/round-trip/validation HTTP behavior.
- `crates/note-server/src/notes_api.rs`: expands REST search summaries and proves REST inherits the threshold.
- `crates/note-mcp/src/stdio.rs`: proves MCP semantic search inherits the threshold without changing its public shape.
- `crates/note-frontend/src/state.rs`: mirrors System and search result payloads for Yew.
- `crates/note-frontend/src/api.rs`: decodes expanded REST search summaries.
- `crates/note-frontend/src/pages/system.rs`: edits the minimum score and uses a shared save action.
- `crates/note-frontend/src/pages/notes.rs`: renders scored results with Notes-table behavior.
- `crates/note-frontend/app.css`: sizes the score column and the expanded table.
- `README.md`, `docs/design.md`: document the global threshold and result presentation.

### Task 1: Persist and validate the search setting

**Files:**
- Modify: `crates/note-core/src/system_config.rs`
- Modify: `crates/note-storage-contract-tests/src/settings.rs`
- Modify: `crates/note-storage-turso/tests/settings_test.rs`
- Modify: `crates/note-storage-pg/tests/repositories_test.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`

- [ ] **Step 1: Write failing core tests for defaults, compatibility, and validation**

Add tests that use the intended public shape:

```rust
#[test]
fn search_minimum_score_defaults_to_point_zero_one() {
    assert_eq!(SystemConfig::default().search.minimum_score, 0.01);

    let config: SystemConfig = serde_json::from_str(
        r#"{"category_labels":[],"duplicate_check":{"enabled":false,"rules":[]}}"#,
    )
    .unwrap();
    assert_eq!(config.search.minimum_score, 0.01);
}

#[test]
fn validates_search_minimum_score() {
    for minimum_score in [0.0, 0.01, 1.0, f32::MAX] {
        let config = SystemConfig {
            search: SearchConfig { minimum_score },
            ..SystemConfig::default()
        };
        assert_eq!(validate_system_config(&config), Ok(()));
    }

    for minimum_score in [-0.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let config = SystemConfig {
            search: SearchConfig { minimum_score },
            ..SystemConfig::default()
        };
        assert_eq!(
            validate_system_config(&config),
            Err(SystemConfigValidationError::InvalidSearchMinimumScore)
        );
    }
}
```

- [ ] **Step 2: Run the core tests and verify RED**

Run: `cargo test -p note-core system_config::tests:: -- --nocapture`

Expected: compilation fails because `SystemConfig::search`, `SearchConfig`, and `InvalidSearchMinimumScore` do not exist.

- [ ] **Step 3: Implement the core setting and validation**

Add the following public configuration unit and remove `Eq` only from structs containing `f32`:

```rust
pub const DEFAULT_MINIMUM_SEARCH_SCORE: f32 = 0.01;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_minimum_search_score")]
    pub minimum_score: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            minimum_score: DEFAULT_MINIMUM_SEARCH_SCORE,
        }
    }
}

fn default_minimum_search_score() -> f32 {
    DEFAULT_MINIMUM_SEARCH_SCORE
}
```

Add `#[serde(default)] pub search: SearchConfig` to `SystemConfig`. Add the unit error variant
`InvalidSearchMinimumScore`, display it as `search minimum score must be finite and non-negative`,
and begin `validate_system_config` with:

```rust
if !config.search.minimum_score.is_finite() || config.search.minimum_score < 0.0 {
    return Err(SystemConfigValidationError::InvalidSearchMinimumScore);
}
```

- [ ] **Step 4: Run all core tests and verify GREEN**

Run: `cargo test -p note-core system_config -- --nocapture`

Expected: all `system_config` tests pass.

- [ ] **Step 5: Extend settings fixtures and the shared storage contract**

Import `SearchConfig` and construct the round-tripped config with:

```rust
search: SearchConfig { minimum_score: 0.025 },
```

Add `search: SearchConfig::default()` to every existing explicit `SystemConfig` literal in the
files listed for this task, except the shared storage contract which deliberately uses `0.025`.

- [ ] **Step 6: Verify both storage adapters**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-turso --test settings_test
cargo test -p note-storage-pg --test contracts_test
cargo test -p note-storage-pg --test repositories_test system_config
```

Expected: Turso passes; PostgreSQL passes when its test database is configured or reports the repository's established skip behavior.

- [ ] **Step 7: Commit the configuration slice**

```bash
git add crates/note-core/src/system_config.rs crates/note-storage-contract-tests/src/settings.rs \
  crates/note-storage-turso/tests/settings_test.rs crates/note-storage-pg/tests/repositories_test.rs \
  crates/note-pipelines/tests/system_test.rs crates/note-pipelines/tests/save_note_test.rs \
  crates/note-mcp/tests/tools_test.rs
git commit -m "feat(search): configure minimum result score"
```

### Task 2: Enforce the threshold in the shared search pipeline

**Files:**
- Modify: `crates/note-pipelines/tests/search_notes_test.rs`
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Write a failing inclusive-threshold pipeline test**

Create one title-only note, capture its fused score under a zero threshold, then prove equality is
retained and a value immediately above it rejects the hit:

```rust
#[tokio::test]
async fn search_applies_the_saved_minimum_score_inclusively() {
    let (ctx, backend, _dir) = test_context().await;
    let note = save_note(
        &ctx,
        SaveNoteInput {
            title: "Threshold needle".into(),
            content: "Unrelated body".into(),
            attachments: vec![],
            labels: vec![],
        },
    )
    .await
    .unwrap();

    let session = backend.session().await.unwrap();
    let mut config = session.get_system_config().await.unwrap();
    config.search.minimum_score = 0.0;
    session.set_system_config(&config).await.unwrap();
    let score = search_notes(&ctx, "Threshold needle", 10).await.unwrap()[0].score;

    config.search.minimum_score = score;
    session.set_system_config(&config).await.unwrap();
    assert_eq!(search_notes(&ctx, "Threshold needle", 10).await.unwrap()[0].note.id, note.id);

    config.search.minimum_score = f32::from_bits(score.to_bits() + 1);
    session.set_system_config(&config).await.unwrap();
    assert!(search_notes(&ctx, "Threshold needle", 10).await.unwrap().is_empty());
}
```

In the REST `search_returns_200_json_array` fixture and the existing MCP semantic-search fixture,
obtain a baseline score, save `score + one f32 ULP` into `SystemConfig.search.minimum_score`, and
assert the next transport response is empty. Both tests must configure storage and then call their
real transport handler/method rather than filtering returned JSON locally.

- [ ] **Step 2: Run all three tests and verify RED**

Run:

```bash
cargo test -p note-pipelines --test search_notes_test search_applies_the_saved_minimum_score_inclusively -- --nocapture
cargo test -p note-server notes_api::tests::search_uses_saved_minimum_score -- --nocapture
cargo test -p note-mcp semantic_search_uses_saved_minimum_score -- --nocapture
```

Expected: each final empty-result assertion fails because the shared pipeline still returns the
low-scoring result.

- [ ] **Step 3: Apply the setting once in `search_notes_filtered`**

After opening the retrieval session, read the config:

```rust
let minimum_score = session.get_system_config().await?.search.minimum_score;
```

At the start of the fused-result loop, exploit descending score order:

```rust
for (note_id, score) in fused {
    if score < minimum_score {
        break;
    }
    // existing allowed-ID check, hydration, and limit handling
}
```

- [ ] **Step 4: Verify pipeline and both transport paths**

Run:

```bash
cargo test -p note-pipelines --test search_notes_test -- --nocapture
cargo test -p note-server notes_api::tests::search_uses_saved_minimum_score -- --nocapture
cargo test -p note-mcp semantic_search_uses_saved_minimum_score -- --nocapture
```

Expected: all tests pass. REST and MCP delegate to the shared pipeline; neither contains duplicate
threshold filtering.

- [ ] **Step 5: Commit the shared behavior**

```bash
git add crates/note-pipelines/src/search_notes.rs crates/note-pipelines/tests/search_notes_test.rs \
  crates/note-server/src/notes_api.rs crates/note-mcp/src/stdio.rs
git commit -m "fix(search): exclude results below configured score"
```

### Task 3: Publish the System setting and expanded REST results

**Files:**
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Modify: `crates/note-server/src/notes_api.rs`

- [ ] **Step 1: Write failing System API/OpenAPI assertions**

Extend the default/round-trip tests to assert:

```rust
assert_eq!(config["search"]["minimum_score"], 0.01);
```

PUT `{"search":{"minimum_score":0.025}}` alongside the existing settings, then assert GET returns
`0.025`. Add an OpenAPI parity assertion that `SearchConfigSchema.minimum_score` defaults to `0.01`
and has minimum `0.0`.

- [ ] **Step 2: Run the System server tests and verify RED**

Run: `cargo test -p note-server system_api::tests:: -- --nocapture`

Expected: schema parity/default assertions fail because the System OpenAPI schema lacks `search`.

- [ ] **Step 3: Add the System OpenAPI schema**

Add to `SystemConfigSchema`:

```rust
#[schema(required = false, default = json!({"minimum_score": 0.01}))]
pub search: SearchConfigSchema,
```

and define:

```rust
#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SearchConfigSchema {
    #[schema(required = false, default = 0.01, minimum = 0.0)]
    pub minimum_score: f32,
}
```

Update the runtime/schema property-key parity test for the new nested object.

- [ ] **Step 4: Write failing REST search response metadata assertions**

In `search_returns_200_json_array`, assert the seeded hit contains `labels`, numeric `created_at`,
numeric `updated_at`, and `score`. Keep the global threshold integration test added in Task 2.

- [ ] **Step 5: Run REST tests and verify RED**

Run: `cargo test -p note-server notes_api::tests::search_ -- --nocapture`

Expected: metadata assertions fail because `SearchResultDto` omits labels and timestamps.

- [ ] **Step 6: Expand the REST DTO and mapper**

Use this DTO shape:

```rust
#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchResultDto {
    pub id: String,
    pub title: String,
    pub revision: i64,
    pub score: f32,
    #[schema(schema_with = crate::openapi::label_pairs_schema)]
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}
```

Move all fields from `r.note` in the mapper, and extend the OpenAPI required-field assertion for
`SearchResultDto`.

- [ ] **Step 7: Verify and commit server contracts**

Run:

```bash
cargo test -p note-server system_api::tests:: -- --nocapture
cargo test -p note-server notes_api::tests::search_ -- --nocapture
cargo test -p note-server openapi::tests::system_schema_fields_match_runtime_serialization -- --nocapture
```

Expected: all selected tests pass.

```bash
git add crates/note-server/src/openapi.rs crates/note-server/src/system_api.rs crates/note-server/src/notes_api.rs
git commit -m "feat(server): expose scored search summaries"
```

### Task 4: Add the System-page search editor

**Files:**
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/pages/system.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing frontend state tests**

Add `SearchConfig` to the frontend model and first write tests expecting old payloads to use `0.01`
and explicit payloads to preserve `0.025`:

```rust
#[test]
fn system_config_deserializes_search_score_with_backward_compatible_default() {
    let omitted: SystemConfig = serde_json::from_str(r#"{"duplicate_check":{}}"#).unwrap();
    let configured: SystemConfig = serde_json::from_str(
        r#"{"search":{"minimum_score":0.025},"duplicate_check":{}}"#,
    )
    .unwrap();
    assert_eq!(omitted.search.minimum_score, 0.01);
    assert_eq!(configured.search.minimum_score, 0.025);
}
```

- [ ] **Step 2: Run the frontend state test and verify RED**

Run: `cargo test --manifest-path crates/note-frontend/Cargo.toml system_config_deserializes_search_score -- --nocapture`

Expected: compilation fails because frontend `SystemConfig` has no search field.

- [ ] **Step 3: Implement the frontend model and input**

Mirror the backend type with `PartialEq` (not `Eq`) and the same custom default. Add an
`on_minimum_score` input callback that parses `HtmlInputElement::value()` as `f32`, updates
`next.search.minimum_score`, and clears the saved flag only on successful parsing.

Render before Duplicate note check:

```rust
<section class="system-section" aria-labelledby="note-search-title">
    <div class="system-section-head">
        <div>
            <h3 id="note-search-title">{ "Note search" }</h3>
            <p>{ "Hide results below this fused weighted-RRF score." }</p>
        </div>
    </div>
    <label class="field system-score-field">
        <span>{ "Minimum score" }</span>
        <input type="number" class="input" min="0" step="0.001"
            value={current.search.minimum_score.to_string()}
            disabled={*saving} onchange={on_minimum_score} />
    </label>
</section>
```

Move the existing Save settings button to one common action row after all editable configuration
sections. Add `.system-score-field { max-width: 18rem; }`.

- [ ] **Step 4: Verify and commit the System UI**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml state::tests:: -- --nocapture
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: tests and Wasm checking pass.

```bash
git add crates/note-frontend/src/state.rs crates/note-frontend/src/pages/system.rs crates/note-frontend/app.css
git commit -m "feat(frontend): edit minimum search score"
```

### Task 5: Render search results as the scored Notes table

**Files:**
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/api.rs`
- Modify: `crates/note-frontend/src/pages/notes.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing API decoding and table-structure tests**

Expand the existing `SearchResultDto` fixture to require:

```rust
"labels": [["project", "agent-note"]],
"created_at": 1_700_000_000,
"updated_at": 1_700_000_100
```

Assert mapping preserves all three fields. Add a Yew VNode test for a one-row scored table and
assert header text order is `Score`, `Title`, `Labels`, `Created`, `Updated`, `Actions`.

- [ ] **Step 2: Run the focused frontend tests and verify RED**

Run: `cargo test --manifest-path crates/note-frontend/Cargo.toml search_result -- --nocapture`

Expected: DTO/model compilation or assertions fail because metadata is not decoded and results are
still rendered as cards.

- [ ] **Step 3: Expand frontend search result types**

Add to both private `SearchResultDto` and public `SearchResultSummary`:

```rust
pub labels: Vec<(String, String)>,
pub created_at: i64,
pub updated_at: i64,
```

Map these fields unchanged in `search_filtered`, retaining the finite-score guard.

- [ ] **Step 4: Replace cards with a scored table using existing row behavior**

Change `search_results_view` to accept `delete_target` and `on_quick_add_filter`. Render
`<table class="table note-table search-result-table">` with Score first, then copy the existing
title/labels/timestamps/actions cells exactly. The only additional cell is:

```rust
<td class="col-score">{ format!("{:.4}", result.score) }</td>
```

Keep pagination after the table and preserve the existing `NotesQueryParams` on title, view, and
edit links. Use the existing delete-target tuple so search deletion follows the normal confirmation
flow.

- [ ] **Step 5: Add restrained table sizing**

```css
.search-result-table {
    min-width: 66rem;
}

.col-score {
    width: 6rem;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
}
```

Remove card-only `.results`, `.result`, and `.result-score` CSS only if `rg` proves they have no
other consumers.

- [ ] **Step 6: Verify and commit the search table**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: all frontend tests and the Wasm target check pass.

```bash
git add crates/note-frontend/src/state.rs crates/note-frontend/src/api.rs crates/note-frontend/src/pages/notes.rs crates/note-frontend/app.css
git commit -m "feat(frontend): show scored note search table"
```

### Task 6: Update canonical docs and run scoped verification

**Files:**
- Modify: `README.md`
- Modify: `docs/design.md`

- [ ] **Step 1: Document the global threshold and response fields**

Update both hybrid-search descriptions to state that fused results below the persisted System
`search.minimum_score` are removed before hydration/limit, the default is `0.01`, and equality is
included. Update the REST response paragraph to list labels and timestamps and the frontend
paragraph to describe `Score | Title | Labels | Created | Updated | Actions`.

- [ ] **Step 2: Run the complete affected verification slice**

```bash
cargo test -p note-core system_config -- --nocapture
cargo test -p note-pipelines --test search_notes_test -- --nocapture
cargo test -p note-server system_api::tests:: -- --nocapture
cargo test -p note-server notes_api::tests::search_ -- --nocapture
cargo test -p note-mcp semantic_search -- --nocapture
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cargo fmt --all -- --check
git diff --check
```

Expected: all selected tests/checks pass. PostgreSQL live coverage must be reported as skipped when
`TEST_DATABASE_URL` is absent rather than claimed as executed.

- [ ] **Step 3: Confirm only scoped files changed and commit docs**

Run: `git status --short && git diff --stat HEAD~6..HEAD`

Expected: the two pre-existing untracked docs remain untouched; only files named in this plan and
the design/plan documents are changed by this feature.

```bash
git add README.md docs/design.md
git commit -m "docs(search): describe minimum score filtering"
```

- [ ] **Step 4: Stop at local completion**

Do not push, merge, publish an image, restart services, deploy, or begin unrelated cleanup. Report
the local commits, exact verification commands, any skipped PostgreSQL coverage, and preserved
untracked files.
