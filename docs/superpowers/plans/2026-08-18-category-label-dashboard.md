# Category Label Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let administrators configure existing label keys as dashboard categories, show every distinct category value with its active-note count, and open Notes with the selected exact filter.

**Architecture:** Persist ordered category keys in `SystemConfig`, validate them against the label catalog in the pipeline, and aggregate value counts in each storage backend. The existing dashboard endpoint maps pipeline summaries into API DTOs and the Yew Home page renders URL-backed filter chips, while the System page edits the same configuration.

**Tech Stack:** Rust 2021, async storage repository traits, Turso/SQLite, PostgreSQL/sqlx, Axum/utoipa, Yew 0.23, yew-router 0.20, DuskMoon CSS classes.

---

### Task 1: Add the category-label configuration model and structural validation

**Files:**
- Modify: `crates/note-core/src/system_config.rs`
- Modify: `crates/note-storage-contract-tests/src/settings.rs`
- Modify: `crates/note-storage-turso/tests/settings_test.rs`
- Modify: `crates/note-storage-pg/tests/repositories_test.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-mcp/tests/tools_test.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/openapi.rs`

- [ ] **Step 1: Write failing core tests for defaults and category-key shape**

Add these tests to `crates/note-core/src/system_config.rs`:

```rust
#[test]
fn category_labels_default_to_empty() {
    assert!(SystemConfig::default().category_labels.is_empty());
}

#[test]
fn rejects_invalid_category_label_shapes() {
    for (labels, expected) in [
        (
            vec!["".to_string()],
            SystemConfigValidationError::EmptyCategoryLabel { index: 0 },
        ),
        (
            vec![" project ".to_string()],
            SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index: 0 },
        ),
        (
            vec!["project".to_string(), "project".to_string()],
            SystemConfigValidationError::DuplicateCategoryLabel {
                key: "project".to_string(),
            },
        ),
    ] {
        let config = SystemConfig {
            category_labels: labels,
            ..SystemConfig::default()
        };
        assert_eq!(validate_system_config(&config), Err(expected));
    }
}
```

- [ ] **Step 2: Run the focused test and verify the red state**

Run:

```bash
cargo test -p note-core system_config
```

Expected: compilation fails because `SystemConfig::category_labels` and the three validation variants do not exist.

- [ ] **Step 3: Add the field and structural error variants**

Update the production types and validator in `crates/note-core/src/system_config.rs`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemConfig {
    #[serde(default)]
    pub category_labels: Vec<String>,
    #[serde(default)]
    pub duplicate_check: DuplicateCheckConfig,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SystemConfigValidationError {
    EmptyCategoryLabel { index: usize },
    CategoryLabelHasOuterWhitespace { index: usize },
    DuplicateCategoryLabel { key: String },
    TooManyRules { count: usize },
    EmptyRule { rule: usize },
    TooManyTerms { rule: usize, count: usize },
    EmptyTermKey { rule: usize, term: usize },
    TermKeyHasOuterWhitespace { rule: usize, term: usize },
    DuplicateTermKey { rule: usize, key: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum CategoryLabelConfigError {
    UnknownKey { key: String },
    ConfiguredKeyCannotBeDeleted { key: String },
}
```

Give `CategoryLabelConfigError` `Display` and `Error` implementations with these messages:

```rust
match self {
    Self::UnknownKey { key } => write!(f, "unknown category label key: {key}"),
    Self::ConfiguredKeyCannotBeDeleted { key } => write!(
        f,
        "label key {key} is configured as a category; remove it from System settings first"
    ),
}
```

At the start of `validate_system_config`, validate category keys before duplicate-check rules:

```rust
let mut category_keys = HashSet::new();
for (index, key) in config.category_labels.iter().enumerate() {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(SystemConfigValidationError::EmptyCategoryLabel { index });
    }
    if trimmed != key {
        return Err(SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index });
    }
    if !category_keys.insert(key.as_str()) {
        return Err(SystemConfigValidationError::DuplicateCategoryLabel { key: key.clone() });
    }
}
```

Add matching human-readable `Display` arms for the three structural variants.

- [ ] **Step 4: Update every explicit `SystemConfig` literal**

Add `category_labels: Vec::new(),` to legacy literals in the files listed for this task. Use non-empty values only in the new category-specific tests. Confirm no literal was missed:

```bash
rg -n "SystemConfig \{" crates
cargo check --workspace --all-targets
```

Expected: every literal supplies `category_labels`, and the workspace check reaches later tasks without a missing-field error.

- [ ] **Step 5: Run the focused core tests and verify green**

Run:

```bash
cargo test -p note-core system_config
```

Expected: all `system_config` tests pass.

- [ ] **Step 6: Commit the configuration model**

```bash
git add docs/superpowers/plans/2026-08-18-category-label-dashboard.md \
  crates/note-core/src/system_config.rs \
  crates/note-storage-contract-tests/src/settings.rs \
  crates/note-storage-turso/tests/settings_test.rs \
  crates/note-storage-pg/tests/repositories_test.rs \
  crates/note-pipelines/tests/system_test.rs \
  crates/note-pipelines/tests/save_note_test.rs \
  crates/note-mcp/tests/tools_test.rs \
  crates/note-server/src/notes_api.rs \
  crates/note-server/src/openapi.rs
git commit -m "feat(config): add category label settings"
```

### Task 2: Aggregate category values in both storage backends

**Files:**
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/labels.rs`
- Modify: `crates/note-storage-pg/src/labels.rs`
- Modify: `crates/note-storage-contract-tests/src/notes.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Add the failing shared storage contract assertion**

After the existing active-note `label_note_counts` assertions in `crates/note-storage-contract-tests/src/notes.rs`, add:

```rust
assert_eq!(
    session
        .label_value_counts(&["contract-notes-status".to_string()])
        .await
        .unwrap(),
    vec![
        LabelValueCount {
            key: "contract-notes-status".into(),
            value: "ready".into(),
            count: 4,
        },
        LabelValueCount {
            key: "contract-notes-status".into(),
            value: "blocked".into(),
            count: 1,
        },
    ]
);
assert!(session.label_value_counts(&[]).await.unwrap().is_empty());
```

Import `LabelValueCount` from `note_storage` in that file.

- [ ] **Step 2: Run the Turso contract and verify the red state**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
```

Expected: compilation fails because `LabelValueCount` and `label_value_counts` do not exist.

- [ ] **Step 3: Define the repository result and method**

In `crates/note-storage/src/repositories.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelValueCount {
    pub key: String,
    pub value: String,
    pub count: usize,
}
```

Extend `LabelRepository`:

```rust
async fn label_value_counts(&self, keys: &[String]) -> StorageResult<Vec<LabelValueCount>>;
```

Add the corresponding forwarding signature beside `label_note_counts` in all three adapter lists:

```rust
fn label_value_counts(keys: &[String]) -> Vec<note_storage::LabelValueCount>;
```

The adapter lists are in `crates/note-pipelines/tests/support/events.rs`, `crates/note-server/src/notes_api.rs`, and `crates/note-mcp/src/stdio.rs`.

- [ ] **Step 4: Implement the Turso aggregation**

In `crates/note-storage-turso/src/labels.rs`, return early for an empty key list, generate one positional placeholder per key, bind with `turso::params_from_iter`, and decode `(key, value, count)`:

```rust
async fn label_value_counts(&self, keys: &[String]) -> StorageResult<Vec<LabelValueCount>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let _operation_guard = self.operation_guard().await;
    let placeholders = (1..=keys.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT lk.key, nl.value, COUNT(n.id)
         FROM label_keys lk
         JOIN note_labels nl ON nl.label_key_id = lk.id
         JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
         WHERE lk.key IN ({placeholders})
         GROUP BY lk.key, nl.value
         ORDER BY lk.key, COUNT(n.id) DESC, nl.value"
    );
    let params = keys.iter().cloned().map(turso::Value::from);
    let mut rows = self.connection
        .query(&sql, turso::params_from_iter(params))
        .await
        .map_err(|error| map_turso_error("query label value counts", error))?;
    let mut counts = Vec::new();
    while let Some(row) = rows.next().await
        .map_err(|error| map_turso_error("read label value counts", error))?
    {
        counts.push(LabelValueCount {
            key: row.get(0).map_err(|error| map_turso_error("decode counted label key", error))?,
            value: row.get(1).map_err(|error| map_turso_error("decode counted label value", error))?,
            count: row.get::<i64>(2)
                .map_err(|error| map_turso_error("decode label value count", error))?
                .max(0) as usize,
        });
    }
    Ok(counts)
}
```

- [ ] **Step 5: Implement the PostgreSQL aggregation**

In `crates/note-storage-pg/src/labels.rs`, use the established array-binding pattern:

```rust
async fn label_value_counts(&self, keys: &[String]) -> StorageResult<Vec<LabelValueCount>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut connection = self.connection().await?;
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT lk.key, nl.value, COUNT(n.id)::bigint
         FROM label_keys lk
         JOIN note_labels nl ON nl.label_key_id = lk.id
         JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
         WHERE lk.key = ANY($1)
         GROUP BY lk.key, nl.value
         ORDER BY lk.key, COUNT(n.id) DESC, nl.value",
    )
    .bind(keys.to_vec())
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| map_sqlx_error("query label value counts", error))?;
    Ok(rows
        .into_iter()
        .map(|(key, value, count)| LabelValueCount {
            key,
            value,
            count: count.max(0) as usize,
        })
        .collect())
}
```

- [ ] **Step 6: Run both storage contracts**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-storage-pg --test contracts_test
```

Expected: Turso passes. PostgreSQL passes when its test database is configured; otherwise its existing harness reports the environment skip without claiming live PostgreSQL coverage.

- [ ] **Step 7: Commit the storage contract**

```bash
git add crates/note-storage/src/repositories.rs \
  crates/note-storage-turso/src/labels.rs \
  crates/note-storage-pg/src/labels.rs \
  crates/note-storage-contract-tests/src/notes.rs \
  crates/note-pipelines/tests/support/events.rs \
  crates/note-server/src/notes_api.rs \
  crates/note-mcp/src/stdio.rs
git commit -m "feat(storage): aggregate category label values"
```

### Task 3: Validate catalog references and build ordered category summaries

**Files:**
- Modify: `crates/note-pipelines/src/system.rs`
- Modify: `crates/note-pipelines/src/label_keys.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`

- [ ] **Step 1: Write failing pipeline tests for unknown keys and deletion protection**

Add tests to `crates/note-pipelines/tests/system_test.rs`:

```rust
#[tokio::test]
async fn category_labels_must_exist_and_configured_keys_cannot_be_deleted() {
    let (ctx, _backend, _dir) = test_context().await;
    let unknown = SystemConfig {
        category_labels: vec!["project".into()],
        ..SystemConfig::default()
    };
    let error = update_system_config(&ctx, &unknown).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<CategoryLabelConfigError>(),
        Some(&CategoryLabelConfigError::UnknownKey { key: "project".into() })
    );

    define_label_key(&ctx, "project", "Project").await.unwrap();
    update_system_config(&ctx, &unknown).await.unwrap();
    let error = delete_label_key(&ctx, "project").await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<CategoryLabelConfigError>(),
        Some(&CategoryLabelConfigError::ConfiguredKeyCannotBeDeleted {
            key: "project".into(),
        })
    );
}
```

Import `CategoryLabelConfigError`, `define_label_key`, and `delete_label_key`.

- [ ] **Step 2: Write the failing ordered-summary test**

Define `CategoryLabelSummary` and `CategoryLabelValueSummary` only through expected test values first. Use this setup to insert three active notes and one soft-deleted note:

```rust
let (ctx, backend, _dir) = test_context().await;
define_label_key(&ctx, "project", "Project").await.unwrap();
define_label_key(&ctx, "team", "Owning team").await.unwrap();
let session = backend.session().await.unwrap();
for (id, project, team) in [
    ("category-a", "yellow-dog", Some("platform")),
    ("category-b", "yellow-dog", None),
    ("category-c", "sigma", None),
    ("category-deleted", "archived-only", None),
] {
    session.insert_note(NewNote {
        id,
        title: id,
        content: "body",
        attachments: &[],
        created_at: 1,
        updated_at: 1,
        note_revision: 1,
        deleted_at: None,
    }).await.unwrap();
    session.attach_label(id, "project", project).await.unwrap();
    if let Some(team) = team {
        session.attach_label(id, "team", team).await.unwrap();
    }
}
session.soft_delete_note("category-deleted", 1, 2).await.unwrap();
update_system_config(&ctx, &SystemConfig {
    category_labels: vec!["team".into(), "project".into()],
    ..SystemConfig::default()
}).await.unwrap();
```

Import `NewNote`, `NotesRepository`, and `LabelRepository`, then assert:

```rust
assert_eq!(
    category_label_summaries(&ctx).await.unwrap(),
    vec![
        CategoryLabelSummary {
            key: "team".into(),
            description: "Owning team".into(),
            values: vec![CategoryLabelValueSummary {
                value: "platform".into(),
                count: 1,
            }],
        },
        CategoryLabelSummary {
            key: "project".into(),
            description: "Project".into(),
            values: vec![
                CategoryLabelValueSummary {
                    value: "yellow-dog".into(),
                    count: 2,
                },
                CategoryLabelValueSummary {
                    value: "sigma".into(),
                    count: 1,
                },
            ],
        },
    ]
);
```

Configure keys in `["team", "project"]` order to prove configured ordering is retained. The deleted note must carry a unique value and must not appear.

- [ ] **Step 3: Run the pipeline test and verify the red state**

Run:

```bash
cargo test -p note-pipelines --test system_test
```

Expected: failure because catalog validation, deletion protection, and category summaries are not implemented.

- [ ] **Step 4: Validate configured keys during update**

In `update_system_config`, reuse one session for catalog lookup and persistence:

```rust
pub async fn update_system_config(ctx: &Context, config: &SystemConfig) -> anyhow::Result<()> {
    validate_system_config(config).map_err(anyhow::Error::new)?;
    let session = ctx.storage().session().await?;
    let known = session
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label| label.key)
        .collect::<HashSet<_>>();
    if let Some(key) = config
        .category_labels
        .iter()
        .find(|key| !known.contains(key.as_str()))
    {
        return Err(anyhow::Error::new(CategoryLabelConfigError::UnknownKey {
            key: key.clone(),
        }));
    }
    Ok(session.set_system_config(config).await?)
}
```

- [ ] **Step 5: Protect configured keys during deletion**

In `delete_label_key`:

```rust
pub async fn delete_label_key(ctx: &Context, key: &str) -> anyhow::Result<()> {
    let session = ctx.storage().session().await?;
    let config = session.get_system_config().await?;
    if config.category_labels.iter().any(|configured| configured == key) {
        return Err(anyhow::Error::new(
            CategoryLabelConfigError::ConfiguredKeyCannotBeDeleted {
                key: key.to_string(),
            },
        ));
    }
    Ok(session.delete_label_key(key).await?)
}
```

- [ ] **Step 6: Add the ordered pipeline summary types and loader**

In `crates/note-pipelines/src/system.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryLabelValueSummary {
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryLabelSummary {
    pub key: String,
    pub description: String,
    pub values: Vec<CategoryLabelValueSummary>,
}

pub async fn category_label_summaries(
    ctx: &Context,
) -> anyhow::Result<Vec<CategoryLabelSummary>> {
    let session = ctx.storage().session().await?;
    let config = session.get_system_config().await?;
    if config.category_labels.is_empty() {
        return Ok(Vec::new());
    }
    let metadata = session
        .list_label_keys()
        .await?
        .into_iter()
        .map(|label| (label.key, label.description))
        .collect::<HashMap<_, _>>();
    let mut values = session
        .label_value_counts(&config.category_labels)
        .await?
        .into_iter()
        .fold(HashMap::<String, Vec<CategoryLabelValueSummary>>::new(), |mut map, row| {
            map.entry(row.key).or_default().push(CategoryLabelValueSummary {
                value: row.value,
                count: row.count,
            });
            map
        });
    Ok(config
        .category_labels
        .into_iter()
        .filter_map(|key| {
            metadata.get(&key).map(|description| CategoryLabelSummary {
                values: values.remove(&key).unwrap_or_default(),
                key,
                description: description.clone(),
            })
        })
        .collect())
}
```

Import `HashMap` and `HashSet`. Storage ordering supplies count-descending/value-ascending order inside each group; configuration iteration supplies category order.

- [ ] **Step 7: Run the pipeline tests and verify green**

Run:

```bash
cargo test -p note-pipelines --test system_test
```

Expected: all system pipeline tests pass.

- [ ] **Step 8: Commit the pipeline behavior**

```bash
git add crates/note-pipelines/src/system.rs \
  crates/note-pipelines/src/label_keys.rs \
  crates/note-pipelines/tests/system_test.rs
git commit -m "feat(pipelines): resolve configured categories"
```

### Task 4: Extend the dashboard and system HTTP contracts

**Files:**
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Modify: `crates/note-server/src/labels_api.rs`

- [ ] **Step 1: Write failing HTTP tests for category DTOs and cache invalidation**

Extend the server tests with a router that merges notes and system routes over the same `Arc<Context>`. The test sequence must:

```rust
async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn dashboard_categories_reflect_config_and_invalidate_cache() {
invalidate_dashboard_cache();
let app = notes_router::<Arc<Context>>()
    .merge(crate::system_api::system_router::<Arc<Context>>())
    .with_state(ctx.clone());

app.clone().oneshot(post(
    "/api/notes",
    r#"{"title":"Yellow Dog","content":"C","labels":[["project","yellow-dog"]]}"#,
)).await.unwrap();

let first = app.clone().oneshot(get("/api/dashboard")).await.unwrap();
let first_json = json_body(first).await;
assert_eq!(first_json["categories"], serde_json::json!([]));

let configured = app.clone().oneshot(put(
    "/api/system/config",
    r#"{"category_labels":["project"],"duplicate_check":{"enabled":false,"rules":[]}}"#,
)).await.unwrap();
assert_eq!(configured.status(), StatusCode::NO_CONTENT);

let second = app.oneshot(get("/api/dashboard")).await.unwrap();
let second_json = json_body(second).await;
assert_eq!(second_json["categories"][0]["key"], "project");
assert_eq!(second_json["categories"][0]["values"][0]["value"], "yellow-dog");
assert_eq!(second_json["categories"][0]["values"][0]["count"], 1);
invalidate_dashboard_cache();
}
```

This proves the configuration handler invalidates a dashboard response that was already cached.

- [ ] **Step 2: Write failing system and label validation HTTP tests**

Add assertions that:

```rust
let unknown = app.clone().oneshot(put(
    "/api/system/config",
    r#"{"category_labels":["missing"],"duplicate_check":{"enabled":false,"rules":[]}}"#,
)).await.unwrap();
assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
```

Add the deletion test in `labels_api.rs` using its existing request helpers:

```rust
#[tokio::test]
async fn configured_category_label_cannot_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> = Arc::new(
        TursoStorage::open(dir.path().join("t.db")).await.unwrap(),
    );
    let ctx = Arc::new(Context::new(
        storage,
        Arc::new(StubEmbedder),
        Arc::new(FilesystemAttachmentStore::new(dir.path().join("attachments"))),
    ));
    let app = labels_router::<Arc<Context>>()
        .merge(crate::system_api::system_router::<Arc<Context>>())
        .with_state(ctx);

    assert_eq!(
        app.clone().oneshot(post(
            "/api/labels",
            r#"{"key":"project","description":"Project"}"#,
        )).await.unwrap().status(),
        StatusCode::OK,
    );
    assert_eq!(
        app.clone().oneshot(put(
            "/api/system/config",
            r#"{"category_labels":["project"],"duplicate_check":{"enabled":false,"rules":[]}}"#,
        )).await.unwrap().status(),
        StatusCode::NO_CONTENT,
    );
    let response = app.oneshot(delete("/api/labels/project")).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body)
        .contains("remove it from System settings first"));
}
```

- [ ] **Step 3: Run focused server tests and verify red**

Run:

```bash
cargo test -p note-server system_api::tests
cargo test -p note-server labels_api::tests
cargo test -p note-server dashboard_categories_reflect_config_and_invalidate_cache
```

Expected: failures because the HTTP schemas, DTOs, status mapping, and cache invalidation are absent.

- [ ] **Step 4: Extend OpenAPI system configuration**

Add to `SystemConfigSchema` in `crates/note-server/src/openapi.rs`:

```rust
#[schema(required = false, default = json!([]))]
pub category_labels: Vec<String>,
```

Update `system_schema_fields_match_runtime_serialization` so the runtime value uses `category_labels: vec!["project".into()]` and assert the schema default is `[]`.

- [ ] **Step 5: Add dashboard category DTOs and pipeline mapping**

In `crates/note-server/src/notes_api.rs`:

```rust
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct DashboardCategoryValueDto {
    pub value: String,
    pub count: usize,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct DashboardCategoryDto {
    pub key: String,
    pub description: String,
    pub values: Vec<DashboardCategoryValueDto>,
}
```

Add `pub categories: Vec<DashboardCategoryDto>` to `DashboardDto`. In `load_dashboard`, call `category_label_summaries(ctx).await?` and map each pipeline summary without reordering it:

```rust
let categories = category_label_summaries(ctx)
    .await?
    .into_iter()
    .map(|category| DashboardCategoryDto {
        key: category.key,
        description: category.description,
        values: category.values.into_iter().map(|value| DashboardCategoryValueDto {
            value: value.value,
            count: value.count,
        }).collect(),
    })
    .collect();
```

Assign `categories` when constructing `DashboardDto`.

- [ ] **Step 6: Map validation errors and invalidate dashboard configuration cache**

In `system_api.rs`, treat both domain validation errors as HTTP 400:

```rust
let caller_fault = error.downcast_ref::<note_core::SystemConfigValidationError>().is_some()
    || error.downcast_ref::<note_core::CategoryLabelConfigError>().is_some();
```

After a successful `update_system_config`, call `crate::notes_api::invalidate_dashboard_cache()` before returning `NO_CONTENT`.

In `labels_api.rs`, change the delete operation to document HTTP 400 and map only `CategoryLabelConfigError` to 400; storage failures remain 500.

- [ ] **Step 7: Run focused server tests and verify green**

Run:

```bash
cargo test -p note-server system_api::tests
cargo test -p note-server labels_api::tests
cargo test -p note-server notes_api::tests::dashboard_uses_summary_data_and_invalidates_after_note_write
cargo test -p note-server openapi::tests::system_schema_fields_match_runtime_serialization
```

Expected: all focused HTTP and schema tests pass.

- [ ] **Step 8: Commit the HTTP contract**

```bash
git add crates/note-server/src/openapi.rs \
  crates/note-server/src/notes_api.rs \
  crates/note-server/src/system_api.rs \
  crates/note-server/src/labels_api.rs
git commit -m "feat(api): expose dashboard categories"
```

### Task 5: Mirror category contracts in the frontend

**Files:**
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/api.rs`
- Modify: `crates/note-frontend/src/routes.rs`
- Modify: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Write failing deserialization and default-page tests**

In `crates/note-frontend/src/api.rs`, add a test that deserializes:

```rust
let summary: DashboardSummary = serde_json::from_value(serde_json::json!({
    "note_count": 3,
    "embedded_note_count": 0,
    "embedding_note": null,
    "label_count": 1,
    "last_updated_at": null,
    "labels": [],
    "categories": [{
        "key": "project",
        "description": "Project",
        "values": [{"value": "yellow-dog", "count": 2}]
    }],
    "recent_updates": []
})).unwrap();
assert_eq!(summary.categories[0].values[0].count, 2);
```

In `routes.rs`, add:

```rust
#[test]
fn notes_default_page_size_is_supported() {
    assert!(matches!(DEFAULT_NOTES_PAGE_SIZE, 10 | 30 | 50 | 100 | 1000));
}
```

- [ ] **Step 2: Run the frontend tests and verify red**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: compilation fails because frontend category DTOs, `category_labels`, and `DEFAULT_NOTES_PAGE_SIZE` are absent.

- [ ] **Step 3: Add frontend configuration and dashboard DTOs**

Add `category_labels` to the frontend `SystemConfig` with `#[serde(default)]`:

```rust
pub struct SystemConfig {
    #[serde(default)]
    pub category_labels: Vec<String>,
    #[serde(default)]
    pub duplicate_check: DuplicateCheckConfig,
}
```

Add in `api.rs`:

```rust
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardCategoryValue {
    pub value: String,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DashboardCategory {
    pub key: String,
    pub description: String,
    pub values: Vec<DashboardCategoryValue>,
}
```

Add `pub categories: Vec<DashboardCategory>` to `DashboardSummary`.

- [ ] **Step 4: Centralize the Notes default page size**

Add in `routes.rs`:

```rust
pub const DEFAULT_NOTES_PAGE_SIZE: usize = 10;
```

Replace the private `DEFAULT_PAGE_SIZE` in `pages/notes.rs` with an import and use of `DEFAULT_NOTES_PAGE_SIZE` so dashboard links and Notes parsing cannot drift.

- [ ] **Step 5: Run frontend tests and verify green**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: the full native frontend test suite passes.

- [ ] **Step 6: Commit the frontend contracts**

```bash
git add crates/note-frontend/src/state.rs \
  crates/note-frontend/src/api.rs \
  crates/note-frontend/src/routes.rs \
  crates/note-frontend/src/pages/notes.rs
git commit -m "feat(frontend): add category dashboard models"
```

### Task 6: Add category-label controls to System settings

**Files:**
- Modify: `crates/note-frontend/src/pages/system.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing pure helper tests**

Add to `pages/system.rs` tests:

```rust
#[test]
fn category_selection_preserves_order_and_rejects_duplicates() {
    let mut keys = vec!["project".to_string()];
    assert!(add_category_label(&mut keys, "team"));
    assert!(!add_category_label(&mut keys, "project"));
    assert_eq!(keys, vec!["project", "team"]);
}

#[test]
fn category_selection_removes_only_the_requested_key() {
    let mut keys = vec!["project".to_string(), "team".to_string()];
    assert!(remove_category_label(&mut keys, "project"));
    assert_eq!(keys, vec!["team"]);
    assert!(!remove_category_label(&mut keys, "missing"));
}
```

- [ ] **Step 2: Run the focused frontend test and verify red**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::system::tests
```

Expected: failure because the category selection helpers do not exist.

- [ ] **Step 3: Implement the pure selection helpers**

```rust
fn add_category_label(keys: &mut Vec<String>, key: &str) -> bool {
    if key.is_empty() || keys.iter().any(|existing| existing == key) {
        return false;
    }
    keys.push(key.to_string());
    true
}

fn remove_category_label(keys: &mut Vec<String>, key: &str) -> bool {
    let original_len = keys.len();
    keys.retain(|existing| existing != key);
    keys.len() != original_len
}
```

- [ ] **Step 4: Render catalog-backed controls**

Import `HtmlSelectElement`. Add a Category labels section before duplicate-check rules. The selector lists only unselected catalog keys:

```rust
let on_add_category_label = {
    let config = config.clone();
    let saved = saved.clone();
    Callback::from(move |event: Event| {
        let select: HtmlSelectElement = event.target_unchecked_into();
        let key = select.value();
        if let Some(mut next) = (*config).clone() {
            if add_category_label(&mut next.category_labels, &key) {
                config.set(Some(next));
                saved.set(false);
            }
        }
    })
};

<select
    class="select category-label-select"
    aria-label="Add category label"
    value=""
    disabled={*saving || labels.iter().all(|label| current.category_labels.contains(&label.key))}
    onchange={on_add_category_label}
>
    <option value="" disabled=true>{ "Add category label" }</option>
    { for labels.iter()
        .filter(|label| !current.category_labels.contains(&label.key))
        .map(|label| html! {
            <option value={label.key.clone()}>{ label.key.clone() }</option>
        }) }
</select>
```

Render selected keys as removable native buttons so keyboard users can remove them:

```rust
<div class="category-label-selection">
    { for current.category_labels.iter().map(|key| html! {
        <button
            type="button"
            class="chip chip-clickable chip-primary category-label-selection-chip"
            aria-label={format!("Remove category label {key}")}
            onclick={{
                let key = key.clone();
                let config = config.clone();
                let saved = saved.clone();
                Callback::from(move |_| {
                    if let Some(mut next) = (*config).clone() {
                        if remove_category_label(&mut next.category_labels, &key) {
                            config.set(Some(next));
                            saved.set(false);
                        }
                    }
                })
            }}
            disabled={*saving}
        >
            <span>{ key.clone() }</span>
            <span aria-hidden="true">{ "×" }</span>
        </button>
    }) }
</div>
```

Both callbacks clone `SystemConfig`, call the pure helper on `next.category_labels`, update state only when the helper returns true, and set `saved` to false. Keep one shared **Save settings** action for category and duplicate-check edits.

- [ ] **Step 5: Add focused layout styles**

Add styles without touching vendored `duskmoon-core.css`:

```css
.category-label-controls {
    display: grid;
    gap: 0.75rem;
}

.category-label-select {
    width: min(24rem, 100%);
}

.category-label-selection {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
}

.category-label-selection-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
}
```

- [ ] **Step 6: Run focused tests and verify green**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::system::tests
```

Expected: all System page helper tests pass.

- [ ] **Step 7: Commit the System UI**

```bash
git add crates/note-frontend/src/pages/system.rs crates/note-frontend/app.css
git commit -m "feat(system): configure category labels"
```

### Task 7: Render dashboard category chips with exact Notes filters

**Files:**
- Modify: `crates/note-frontend/src/pages/dashboard.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing query and chip-copy tests**

Add to `pages/dashboard.rs` tests:

```rust
use yew_router::query::ToQuery;

#[test]
fn category_query_opens_page_one_with_an_exact_escaped_filter() {
    let query = category_notes_query("project", "yellow-dog & sigma");
    assert_eq!(query.current, 1);
    assert_eq!(query.page_size, DEFAULT_NOTES_PAGE_SIZE);
    assert_eq!(query.search, None);
    assert_eq!(query.labels.as_deref(), Some("project=yellow-dog & sigma"));

    let encoded = query.to_query().unwrap();
    assert!(encoded.contains("labels=project%3Dyellow-dog"));
    assert!(encoded.contains("%26"));
}

#[test]
fn category_chip_copy_includes_the_note_count() {
    assert_eq!(category_chip_text("yellow-dog", 1), "yellow-dog · 1 note");
    assert_eq!(category_chip_text("sigma", 7), "sigma · 7 notes");
}
```

- [ ] **Step 2: Run the focused dashboard tests and verify red**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::dashboard::tests
```

Expected: failure because `category_notes_query` and `category_chip_text` do not exist.

- [ ] **Step 3: Implement the query and copy helpers**

```rust
fn category_notes_query(key: &str, value: &str) -> NotesQueryParams {
    NotesQueryParams {
        current: 1,
        page_size: DEFAULT_NOTES_PAGE_SIZE,
        search: None,
        labels: api::label_filter_selector(&[LabelFilter {
            key: key.to_string(),
            operator: "=".to_string(),
            value: value.to_string(),
        }]),
    }
}

fn category_chip_text(value: &str, count: usize) -> String {
    let noun = if count == 1 { "note" } else { "notes" };
    format!("{value} · {count} {noun}")
}
```

- [ ] **Step 4: Render configured category panels before the existing grid**

Render nothing when `summary.categories` is empty. Otherwise add:

```rust
<div class="dashboard-categories" aria-label="Note categories">
    { for summary.categories.iter().map(|category| html! {
        <section class="dashboard-panel dashboard-category" key={category.key.clone()}>
            <div class="dashboard-panel-head">
                <div>
                    <h3>{ category.key.clone() }</h3>
                    if !category.description.is_empty() {
                        <p>{ category.description.clone() }</p>
                    }
                </div>
            </div>
            if category.values.is_empty() {
                <p class="empty compact">{ "No notes in this category." }</p>
            } else {
                <div class="dashboard-category-values">
                    { for category.values.iter().map(|value| html! {
                        <Link<Route, NotesQueryParams>
                            to={Route::Notes}
                            query={Some(category_notes_query(&category.key, &value.value))}
                            classes={classes!("chip", "chip-clickable", "chip-primary", "dashboard-category-chip")}
                        >
                            { category_chip_text(&value.value, value.count) }
                        </Link<Route, NotesQueryParams>>
                    }) }
                </div>
            }
        </section>
    }) }
</div>
```

Update the page hint to include category navigation while retaining inventory, embedding, labels, and recent updates.

- [ ] **Step 5: Style responsive category panels and chips**

```css
.dashboard-categories {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(18rem, 100%), 1fr));
    gap: 1rem;
}

.dashboard-category .dashboard-panel-head p {
    margin: 0.2rem 0 0;
    color: var(--color-on-surface-variant, #46464f);
    font-size: 0.82rem;
}

.dashboard-category-values {
    display: flex;
    flex-wrap: wrap;
    gap: 0.55rem;
    padding: 0.9rem;
}

.dashboard-category-chip {
    text-decoration: none;
}
```

- [ ] **Step 6: Run dashboard and full frontend tests**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::dashboard::tests
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: focused dashboard tests and the full frontend native suite pass.

- [ ] **Step 7: Commit the Home UI**

```bash
git add crates/note-frontend/src/pages/dashboard.rs crates/note-frontend/app.css
git commit -m "feat(dashboard): link category value chips"
```

### Task 8: Run scoped end-to-end verification

**Files:**
- Verify: all files changed in Tasks 1-7

- [ ] **Step 1: Format the native workspace and frontend crate**

Run:

```bash
cargo fmt --all
cargo fmt --manifest-path crates/note-frontend/Cargo.toml
git diff --check
```

Expected: formatters complete and `git diff --check` emits no output.

- [ ] **Step 2: Run native feature coverage**

Run:

```bash
cargo test -p note-core system_config
cargo test -p note-storage-turso --test contracts_test
cargo test -p note-pipelines --test system_test
cargo test -p note-server
```

Expected: all commands exit 0 with no failed tests.

- [ ] **Step 3: Run the native workspace regression suite**

Run:

```bash
cargo test --workspace
cargo check --workspace --all-targets
```

Expected: both commands exit 0.

- [ ] **Step 4: Run frontend native and Wasm verification**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: all frontend tests pass and the Wasm check exits 0. Pre-existing warnings may be reported separately but are not feature failures.

- [ ] **Step 5: Verify PostgreSQL when the live test database is available**

Run:

```bash
cargo test -p note-storage-pg --test contracts_test
```

Expected: the contract passes against `TEST_DATABASE_URL`. If that variable is absent, report PostgreSQL live coverage as not run; do not describe it as passing.

- [ ] **Step 6: Review the final scope and status**

Run:

```bash
git status --short
git diff --stat HEAD~7..HEAD
git diff --check HEAD~7..HEAD
```

Expected: only category-dashboard implementation, tests, styles, spec, and plan are included. The pre-existing untracked `docs/agent-note-reliability-hardening-codex-plan.md` and `docs/emacs-org-guide.md` remain untouched.
