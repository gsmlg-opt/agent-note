# Exact Category Label Values Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Accept ordered category entries as either `key` or `key=value`, group pinned values for one key into one dashboard panel, and retain exact zero-count chips.

**Architecture:** Keep `SystemConfig.category_labels: Vec<String>` for storage and API compatibility. Parse each expression at the first equals sign in `note-core`, aggregate each parsed key once in the pipeline, and keep the dashboard DTO unchanged. Replace the System page immediate-add selector with a catalog-backed key, All/Exact mode, free-form value, and explicit Add action.

**Tech Stack:** Rust 2021, serde, Turso/SQLite, Axum/utoipa, Yew 0.23, Wasm.

---

### Task 1: Parse and validate category expressions

**Files:**
- Modify: `crates/note-core/src/system_config.rs`

- [ ] **Step 1: Write failing tests**

~~~rust
#[test]
fn parses_category_label_expressions_at_the_first_equals_sign() {
    assert_eq!(
        parse_category_label_expression("project"),
        CategoryLabelExpression { key: "project", value: None }
    );
    assert_eq!(
        parse_category_label_expression("project=a=b"),
        CategoryLabelExpression { key: "project", value: Some("a=b") }
    );
    assert_eq!(parse_category_label_expression("project=").value, Some(""));
    assert_eq!(
        parse_category_label_expression("project= padded & % + 值 ").value,
        Some(" padded & % + 值 ")
    );
}

#[test]
fn accepts_multiple_exact_values_for_one_key() {
    let config = SystemConfig {
        category_labels: vec![
            "project=yellow-dog".into(),
            "team".into(),
            "project=sigma".into(),
        ],
        ..SystemConfig::default()
    };
    assert_eq!(validate_system_config(&config), Ok(()));
}

#[test]
fn rejects_duplicate_and_mixed_category_entries() {
    for (entries, expected) in [
        (
            vec!["project=sigma".into(), "project=sigma".into()],
            SystemConfigValidationError::DuplicateCategoryLabelEntry {
                entry: "project=sigma".into(),
            },
        ),
        (
            vec!["project".into(), "project=sigma".into()],
            SystemConfigValidationError::MixedCategoryLabelModes {
                key: "project".into(),
            },
        ),
    ] {
        let config = SystemConfig {
            category_labels: entries,
            ..SystemConfig::default()
        };
        assert_eq!(validate_system_config(&config), Err(expected));
    }
}
~~~

Also add this whitespace assertion:

~~~rust
let valid = SystemConfig {
    category_labels: vec!["project= padded ".into()],
    ..SystemConfig::default()
};
let invalid = SystemConfig {
    category_labels: vec![" project=value".into()],
    ..SystemConfig::default()
};
assert_eq!(validate_system_config(&valid), Ok(()));
assert_eq!(
    validate_system_config(&invalid),
    Err(SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index: 0 })
);
~~~

- [ ] **Step 2: Verify the tests fail**

~~~bash
cargo test -p note-core system_config
~~~

Expected: compile errors for the missing parser and validation variants.

- [ ] **Step 3: Add the parser and errors**

~~~rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryLabelExpression<'a> {
    pub key: &'a str,
    pub value: Option<&'a str>,
}

pub fn parse_category_label_expression(expression: &str) -> CategoryLabelExpression<'_> {
    match expression.split_once('=') {
        Some((key, value)) => CategoryLabelExpression { key, value: Some(value) },
        None => CategoryLabelExpression { key: expression, value: None },
    }
}
~~~

Replace `DuplicateCategoryLabel` with:

~~~rust
DuplicateCategoryLabelEntry { entry: String },
MixedCategoryLabelModes { key: String },
~~~

Their messages are `category label {entry} is configured more than once` and `category label {key} cannot configure all values and exact values together`.

- [ ] **Step 4: Validate parsed keys and modes**

Import `HashMap`. Use `HashMap<&str, Option<HashSet<&str>>>`, where `None` means All and `Some(values)` means Exact. For every configured expression:

~~~rust
let parsed = parse_category_label_expression(expression);
let trimmed_key = parsed.key.trim();
if trimmed_key.is_empty() {
    return Err(SystemConfigValidationError::EmptyCategoryLabel { index });
}
if trimmed_key != parsed.key {
    return Err(SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index });
}
~~~

Insert the first mode and handle later entries without trimming the value:

~~~rust
match categories.entry(parsed.key) {
    std::collections::hash_map::Entry::Vacant(entry) => {
        entry.insert(parsed.value.map(|value| HashSet::from([value])));
    }
    std::collections::hash_map::Entry::Occupied(mut entry) => {
        match (entry.get_mut(), parsed.value) {
            (None, None) => {
                return Err(SystemConfigValidationError::DuplicateCategoryLabelEntry {
                    entry: expression.clone(),
                });
            }
            (None, Some(_)) | (Some(_), None) => {
                return Err(SystemConfigValidationError::MixedCategoryLabelModes {
                    key: parsed.key.to_string(),
                });
            }
            (Some(values), Some(value)) if !values.insert(value) => {
                return Err(SystemConfigValidationError::DuplicateCategoryLabelEntry {
                    entry: expression.clone(),
                });
            }
            (Some(_), Some(_)) => {}
        }
    }
}
~~~

- [ ] **Step 5: Verify and commit**

~~~bash
cargo fmt --all
cargo test -p note-core system_config
git add crates/note-core/src/system_config.rs
git commit -m "feat(config): parse exact category entries"
~~~

Expected: all focused core tests pass.

### Task 2: Build grouped pinned summaries

**Files:**
- Modify: `crates/note-pipelines/src/system.rs`
- Modify: `crates/note-pipelines/src/label_keys.rs`
- Modify: `crates/note-pipelines/tests/system_test.rs`

- [ ] **Step 1: Write a failing grouped-summary test**

Create active notes with `project=yellow-dog` twice, `project=sigma` once, and `team=platform` once. Configure:

~~~rust
category_labels: vec![
    "project=sigma".into(),
    "team".into(),
    "project=missing".into(),
    "project=yellow-dog".into(),
],
~~~

Assert:

~~~rust
vec![
    CategoryLabelSummary {
        key: "project".into(),
        description: "Project".into(),
        values: vec![
            CategoryLabelValueSummary { value: "sigma".into(), count: 1 },
            CategoryLabelValueSummary { value: "missing".into(), count: 0 },
            CategoryLabelValueSummary { value: "yellow-dog".into(), count: 2 },
        ],
    },
    CategoryLabelSummary {
        key: "team".into(),
        description: "Team".into(),
        values: vec![
            CategoryLabelValueSummary { value: "platform".into(), count: 1 },
        ],
    },
]
~~~

Add a second test proving `project=yellow-dog` validates catalog key `project` and prevents deleting `project`.

- [ ] **Step 2: Verify the tests fail**

~~~bash
cargo test -p note-pipelines --test system_test exact_
~~~

Expected: the pipeline reports `project=...` as an unknown key.

- [ ] **Step 3: Group expressions by first-seen key**

Add:

~~~rust
enum CategorySelection {
    All,
    Exact(Vec<String>),
}

struct CategoryRequest {
    key: String,
    selection: CategorySelection,
}
~~~

Implement `category_requests` by recording the first index for each parsed key and appending later exact values:

~~~rust
fn category_requests(entries: &[String]) -> Vec<CategoryRequest> {
    let mut indexes = HashMap::<String, usize>::new();
    let mut requests = Vec::<CategoryRequest>::new();
    for entry in entries {
        let parsed = parse_category_label_expression(entry);
        if let Some(index) = indexes.get(parsed.key).copied() {
            if let CategorySelection::Exact(values) = &mut requests[index].selection {
                values.push(parsed.value.expect("configuration already validated").to_string());
            }
            continue;
        }
        indexes.insert(parsed.key.to_string(), requests.len());
        requests.push(CategoryRequest {
            key: parsed.key.to_string(),
            selection: match parsed.value {
                Some(value) => CategorySelection::Exact(vec![value.to_string()]),
                None => CategorySelection::All,
            },
        });
    }
    requests
}
~~~

Configuration is validated before this helper runs, so mixed modes and duplicates are unreachable.

- [ ] **Step 4: Aggregate unique keys and map selections**

Use grouped keys for catalog validation and `label_value_counts`. Keep storage-returned ordering for `All`. For `Exact`, map configured values in their configured order:

~~~rust
CategorySelection::Exact(configured) => configured
    .into_iter()
    .map(|value| CategoryLabelValueSummary {
        count: available
            .iter()
            .find(|summary| summary.value == value)
            .map_or(0, |summary| summary.count),
        value,
    })
    .collect(),
~~~

Create one `CategoryLabelSummary` per grouped request and remove its description by parsed key.

- [ ] **Step 5: Protect parsed keys from deletion**

In `label_keys.rs` import the parser and use:

~~~rust
.any(|entry| parse_category_label_expression(entry).key == key)
~~~

- [ ] **Step 6: Verify and commit**

Update the existing duplicate stored-config assertion for the renamed error, then run:

~~~bash
cargo fmt --all
cargo test -p note-pipelines --test system_test
git add crates/note-pipelines/src/system.rs crates/note-pipelines/src/label_keys.rs crates/note-pipelines/tests/system_test.rs
git commit -m "feat(dashboard): group pinned category values"
~~~

Expected: all pipeline system tests pass, including existing bare-key ordering.

### Task 3: Cover REST, OpenAPI, and cache behavior

**Files:**
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/system_api.rs`
- Modify: `crates/note-server/src/labels_api.rs`
- Modify: `crates/note-server/src/notes_api.rs`

- [ ] **Step 1: Extend System API tests**

Change the ordered round-trip case to:

~~~json
{"category_labels":["status=ready","project=yellow-dog","project=sigma"],"duplicate_check":{"enabled":false,"rules":[]}}
~~~

Assert the same ordered array is returned. Change the unknown-key case to `missing=value` and continue expecting `unknown category label key: missing`. Add a case sending both `project` and `project=yellow-dog`; expect HTTP 400 with the mixed-mode message and verify persisted configuration is unchanged.

- [ ] **Step 2: Extend deletion coverage**

Change `configured_category_label_cannot_be_deleted` to configure `project=yellow-dog`. Keep its HTTP 400 and catalog-preservation assertions.

- [ ] **Step 3: Extend dashboard cache coverage**

Create `project=yellow-dog` and `project=sigma` notes. First configure bare `project` and assert both values. Then PUT:

~~~json
{"category_labels":["project=missing","project=yellow-dog"],"duplicate_check":{"enabled":false,"rules":[]}}
~~~

Without manual invalidation, fetch the dashboard and assert:

~~~rust
serde_json::json!([{
    "key": "project",
    "description": "",
    "values": [
        {"value": "missing", "count": 0},
        {"value": "yellow-dog", "count": 1}
    ]
}])
~~~

- [ ] **Step 4: Update OpenAPI field documentation**

Document that `key` selects all values, `key=value` selects one exact value, the first equals sign separates key and value, keys are catalog-backed, and values are byte-preserving.

- [ ] **Step 5: Verify and commit**

~~~bash
cargo fmt --all
cargo test -p note-server system_api::tests
cargo test -p note-server labels_api::tests::configured_category_label_cannot_be_deleted
cargo test -p note-server notes_api::tests::dashboard_categories_reflect_config_and_invalidate_cache
cargo test -p note-server openapi
git add crates/note-server/src/openapi.rs crates/note-server/src/system_api.rs crates/note-server/src/labels_api.rs crates/note-server/src/notes_api.rs
git commit -m "test(server): cover exact category configuration"
~~~

Expected: focused server tests pass and the dashboard DTO schema is unchanged.

### Task 4: Add the All/Exact System editor

**Files:**
- Modify: `crates/note-frontend/Cargo.toml`
- Modify: `crates/note-frontend/Cargo.lock`
- Modify: `crates/note-frontend/src/state.rs`
- Modify: `crates/note-frontend/src/pages/system.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Add parser access and failing tests**

Add:

~~~toml
note-core = { path = "../note-core" }
~~~

Extend frontend state serde coverage with `["project=yellow-dog", "project="]`.

Replace the current helper test with:

~~~rust
#[test]
fn all_values_replaces_exact_entries_at_the_first_position() {
    let mut entries = vec![
        "team".into(),
        "project=yellow-dog".into(),
        "status".into(),
        "project=sigma".into(),
    ];
    assert_eq!(
        add_category_label(&mut entries, "project", CategoryLabelMode::All, ""),
        Ok(())
    );
    assert_eq!(entries, ["team", "project", "status"]);
}

#[test]
fn exact_values_preserve_bytes_and_reject_redundancy() {
    let mut entries = vec!["project=yellow-dog".into()];
    assert_eq!(
        add_category_label(
            &mut entries,
            "project",
            CategoryLabelMode::Exact,
            " padded = 值 "
        ),
        Ok(())
    );
    assert_eq!(entries[1], "project= padded = 值 ");
    assert!(add_category_label(
        &mut entries,
        "project",
        CategoryLabelMode::Exact,
        "yellow-dog"
    ).is_err());
}
~~~

Also test that exact mode rejects a key with an existing bare entry and that removal deletes only the selected full expression.

- [ ] **Step 2: Verify frontend tests fail**

~~~bash
cargo test --manifest-path crates/note-frontend/Cargo.toml system::tests
~~~

Expected: compile errors for `CategoryLabelMode` and the new helper signature.

- [ ] **Step 3: Implement the editor helper**

Import `note_core::parse_category_label_expression` and add:

~~~rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CategoryLabelMode {
    All,
    Exact,
}

fn add_category_label(
    entries: &mut Vec<String>,
    key: &str,
    mode: CategoryLabelMode,
    value: &str,
) -> Result<(), &'static str> {
    if key.is_empty() {
        return Err("Choose a label name.");
    }
    match mode {
        CategoryLabelMode::All => {
            let first = entries.iter()
                .position(|entry| parse_category_label_expression(entry).key == key);
            let Some(first) = first else {
                entries.push(key.to_string());
                return Ok(());
            };
            if entries[first] == key {
                return Err("That all-values category is already configured.");
            }
            entries.retain(|entry| parse_category_label_expression(entry).key != key);
            entries.insert(first, key.to_string());
            Ok(())
        }
        CategoryLabelMode::Exact => {
            if entries.iter().any(|entry| {
                let parsed = parse_category_label_expression(entry);
                parsed.key == key && parsed.value.is_none()
            }) {
                return Err("Remove the all-values category before adding exact values.");
            }
            let expression = format!("{key}={value}");
            if entries.iter().any(|entry| entry == &expression) {
                return Err("That exact category value is already configured.");
            }
            entries.push(expression);
            Ok(())
        }
    }
}
~~~

Do not trim `value`. Keep removal based on the full expression.

- [ ] **Step 4: Replace immediate-add UI with drafts**

Add state:

~~~rust
let category_key_draft = use_state(String::new);
let category_mode_draft = use_state(|| CategoryLabelMode::All);
let category_value_draft = use_state(String::new);
let category_draft_error = use_state(|| None::<String>);
~~~

Replace the current immediate-add callback with separate key, mode, and value callbacks plus this Add callback:

~~~rust
let on_add_category_label = {
    let config = config.clone();
    let saved = saved.clone();
    let key = category_key_draft.clone();
    let mode = category_mode_draft.clone();
    let value = category_value_draft.clone();
    let draft_error = category_draft_error.clone();
    Callback::from(move |_| {
        let Some(mut next) = (*config).clone() else { return };
        match add_category_label(&mut next.category_labels, &key, *mode, &value) {
            Ok(()) => {
                config.set(Some(next));
                value.set(String::new());
                draft_error.set(None);
                saved.set(false);
            }
            Err(message) => draft_error.set(Some(message.to_string())),
        }
    })
};
~~~

Render labeled key and mode selects, an exact-value text input only in Exact mode, and an explicit Add button. Wire each input to its callback. Show helper errors with `role="alert"`.

Disable exact selection for a key with an existing bare entry. Allow All for a key with exact entries so the helper can replace them at their first position. Empty exact values remain valid. Chips display and remove the full expression.

- [ ] **Step 5: Add responsive styles**

~~~css
.category-label-editor {
    display: grid;
    grid-template-columns: minmax(10rem, 1fr) minmax(9rem, 0.7fr) minmax(12rem, 1fr) auto;
    align-items: end;
    gap: 0.75rem;
}

.category-label-editor .field {
    min-width: 0;
}

.category-label-editor .select,
.category-label-editor input {
    width: 100%;
    min-width: 0;
    box-sizing: border-box;
}

@media (max-width: 48rem) {
    .category-label-editor {
        grid-template-columns: 1fr;
        align-items: stretch;
    }
}
~~~

- [ ] **Step 6: Verify and commit**

~~~bash
cargo fmt --manifest-path crates/note-frontend/Cargo.toml
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
git add crates/note-frontend/Cargo.toml crates/note-frontend/Cargo.lock crates/note-frontend/src/state.rs crates/note-frontend/src/pages/system.rs crates/note-frontend/app.css
git commit -m "feat(frontend): configure exact category values"
~~~

Expected: frontend tests and Wasm checking pass. Existing Home exact-filter URL tests remain green without production changes.

### Task 5: Verify the delta

**Files:**
- Verify only.

- [ ] **Step 1: Run affected native tests**

~~~bash
cargo test -p note-core system_config
cargo test -p note-pipelines --test system_test
cargo test -p note-server system_api::tests
cargo test -p note-server labels_api::tests::configured_category_label_cannot_be_deleted
cargo test -p note-server notes_api::tests::dashboard_categories_reflect_config_and_invalidate_cache
cargo test -p note-server openapi
~~~

Expected: all selected native tests pass.

- [ ] **Step 2: Report PostgreSQL integration status**

~~~bash
if [[ -n "$TEST_DATABASE_URL" ]]; then
  cargo test -p note-storage-pg --test repositories_test label_value_counts
else
  echo "TEST_DATABASE_URL is unset; PostgreSQL integration coverage is unverified"
fi
~~~

Expected: PostgreSQL coverage passes when configured; otherwise report it as unverified.

- [ ] **Step 3: Run frontend, format, and diff checks**

~~~bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
git status --short
~~~

Expected: all checks pass, `git diff --check` is silent, and unrelated workspace files remain untouched.
