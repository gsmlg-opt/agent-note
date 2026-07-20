# Label String-Match Operators Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add case-insensitive `^=` starts-with, `$=` ends-with, and `~=` regular-expression operators to note label filters across core matching, storage/search behavior, the web UI, and public API descriptions.

**Architecture:** Extend the shared `note-core` selector parser and matcher so both storage adapters and every pipeline inherit the behavior without adapter-specific code. Keep frontend parsing and operator presentation aligned with the core syntax, while leaving the existing generic selector serializer unchanged. Invalid regexes remain non-fatal and simply do not match.

**Tech Stack:** Rust 2021, `regex`, Tokio tests, Turso and PostgreSQL storage contracts, Yew/Wasm, Utoipa OpenAPI, RMCP/Schemars MCP schemas.

---

## File Map

- Modify `crates/note-core/Cargo.toml`: add the direct `regex` dependency.
- Modify `Cargo.lock`: record `note-core`'s dependency after Cargo resolves the manifest.
- Modify `crates/note-core/src/types.rs`: define, parse, serialize, and evaluate the new operators.
- Modify `crates/note-storage-contract-tests/src/notes.rs`: prove both storage adapters honor the operators for list, summary, and count paths.
- Modify `crates/note-pipelines/tests/search_notes_test.rs`: prove semantic retrieval applies regex label filtering.
- Modify `crates/note-frontend/src/pages/notes.rs`: parse URL state and expose the operators in the filter builder.
- Reuse `crates/note-frontend/src/api.rs`: exercise its existing generic selector serializer from the notes-page round-trip test; no production change is required.
- Modify `crates/note-server/src/notes_api.rs`: document the selector syntax in OpenAPI and test the generated descriptions.
- Modify `crates/note-mcp/src/stdio.rs`: document the selector syntax in MCP input schemas and test the generated descriptions.

### Task 1: Establish Failing Cross-Layer Tests

**Files:**
- Test: `crates/note-core/src/types.rs`
- Test: `crates/note-storage-contract-tests/src/notes.rs`
- Test: `crates/note-pipelines/tests/search_notes_test.rs`
- Test: `crates/note-frontend/src/pages/notes.rs`
- Test: `crates/note-server/src/notes_api.rs`
- Test: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Add core parser and matcher tests**

Append focused tests to the existing `types.rs` test module:

```rust
#[test]
fn parses_string_match_selectors() {
    assert_eq!(
        parse_label_selectors("name^=Agent&name$=NOTE&name~=^agent-.+$"),
        vec![
            LabelSelector {
                key: "name".to_string(),
                value: Some("Agent".to_string()),
                operator: LabelOperator::StartsWith,
            },
            LabelSelector {
                key: "name".to_string(),
                value: Some("NOTE".to_string()),
                operator: LabelOperator::EndsWith,
            },
            LabelSelector {
                key: "name".to_string(),
                value: Some("^agent-.+$".to_string()),
                operator: LabelOperator::Regex,
            },
        ]
    );
}

#[test]
fn string_match_operators_are_case_insensitive_for_all_value_types() {
    assert!(compare_label_values(
        LabelValueType::Text,
        "Agent-Note",
        LabelOperator::StartsWith,
        "agent"
    ));
    assert!(compare_label_values(
        LabelValueType::Text,
        "Agent-Note",
        LabelOperator::EndsWith,
        "NOTE"
    ));
    assert!(compare_label_values(
        LabelValueType::Text,
        "Agent-Note",
        LabelOperator::Regex,
        "^agent-.+"
    ));
    assert!(compare_label_values(
        LabelValueType::Number,
        "10",
        LabelOperator::StartsWith,
        "1"
    ));
}

#[test]
fn invalid_label_regex_does_not_match() {
    assert!(!compare_label_values(
        LabelValueType::Text,
        "Agent-Note",
        LabelOperator::Regex,
        "["
    ));
}
```

- [ ] **Step 2: Verify the core tests fail for the missing variants**

Run:

```bash
cargo test -p note-core string_match
```

Expected: compilation fails because `StartsWith`, `EndsWith`, and `Regex` do not yet exist.

- [ ] **Step 3: Add shared storage contract assertions**

Immediately after the existing typed selector pagination assertions in
`crates/note-storage-contract-tests/src/notes.rs`, add:

```rust
for selector in [
    "contract-notes-status^=RE",
    "contract-notes-status$=DY",
    "contract-notes-status~=^R.*Y$",
] {
    let selectors = parse_label_selectors(selector);
    assert_eq!(session.count_notes(&selectors).await.unwrap(), 5);
    assert_eq!(
        session
            .list_notes(&selectors, None, None)
            .await
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        session
            .list_note_summaries(&selectors, None, None)
            .await
            .unwrap()
            .len(),
        5
    );
}

let selectors = parse_label_selectors("contract-notes-priority^=1");
assert_eq!(session.count_notes(&selectors).await.unwrap(), 2);

let selectors = parse_label_selectors("contract-notes-status~=[");
assert_eq!(session.count_notes(&selectors).await.unwrap(), 0);
```

- [ ] **Step 4: Verify the Turso storage contract fails on the new syntax**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
```

Expected: FAIL because the current parser treats the operator marker as part of the key and returns
zero matches where five are expected.

- [ ] **Step 5: Change the semantic-search regression to exercise case-insensitive regex**

In `search_filters_results_by_label`, change the selector passed to
`search_notes_filtered`:

```rust
let results =
    search_notes_filtered(&ctx, "Shared", 10, Some("topic~=^RU.T$".into()))
        .await
        .unwrap();
```

Keep the existing assertions that include the Rust note and exclude the Ops note.

- [ ] **Step 6: Verify semantic search fails**

Run:

```bash
cargo test -p note-pipelines --test search_notes_test search_filters_results_by_label
```

Expected: FAIL because the Rust result is filtered out by the unrecognized operator.

- [ ] **Step 7: Add a frontend parse-and-serialize round-trip test**

Add this test to `crates/note-frontend/src/pages/notes.rs`:

```rust
#[test]
fn parses_and_serializes_string_match_filters() {
    let selector = "topic^=Rust&topic$=LANG&topic~=^ru.*t$";
    let filters = parse_label_filters(selector);

    assert_eq!(
        filters,
        vec![
            LabelFilter {
                key: "topic".into(),
                operator: "^=".into(),
                value: "Rust".into(),
            },
            LabelFilter {
                key: "topic".into(),
                operator: "$=".into(),
                value: "LANG".into(),
            },
            LabelFilter {
                key: "topic".into(),
                operator: "~=".into(),
                value: "^ru.*t$".into(),
            },
        ]
    );
    assert_eq!(
        api::label_filter_selector(&filters),
        Some(selector.to_string())
    );
}
```

This single red test covers both URL-state parsing and the already-generic API serializer.

- [ ] **Step 8: Verify the frontend test fails**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml parses_and_serializes_string_match_filters
```

Expected: FAIL because the parser currently produces keys ending in `^`, `$`, and `~`.

- [ ] **Step 9: Add failing REST schema-description coverage**

Add this test beside the existing OpenAPI query tests in `crates/note-server/src/notes_api.rs`:

```rust
#[test]
fn openapi_documents_label_filter_operators() {
    let document = note_openapi_document();
    let list = &document["paths"]["/api/notes"]["get"];
    let list_description = operation_parameter(list, "label")["description"]
        .as_str()
        .unwrap();
    let search_description =
        document["components"]["schemas"]["SearchQuery"]["properties"]["label"]["description"]
            .as_str()
            .unwrap();

    for description in [list_description, search_description] {
        for operator in ["^=", "$=", "~="] {
            assert!(description.contains(operator));
        }
        assert!(description.contains("case-insensitive"));
    }
}
```

- [ ] **Step 10: Add failing MCP schema-description coverage**

In `advertised_schemas_are_bounded_and_keep_nullable_filters`, after obtaining `list_input` and
`search_input`, add:

```rust
for input in [&list_input, &search_input] {
    let description = input["properties"]["label"]["description"]
        .as_str()
        .unwrap();
    for operator in ["^=", "$=", "~="] {
        assert!(description.contains(operator));
    }
    assert!(description.contains("case-insensitive"));
}
```

- [ ] **Step 11: Verify both schema tests fail**

Run:

```bash
cargo test -p note-server openapi_documents_label_filter_operators
cargo test -p note-mcp advertised_schemas_are_bounded_and_keep_nullable_filters
```

Expected: both FAIL because the current field descriptions omit the new operators.

### Task 2: Implement Shared Core Parsing and Matching

**Files:**
- Modify: `crates/note-core/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/note-core/src/types.rs`

- [ ] **Step 1: Add the regex dependency**

Add the dependency to `crates/note-core/Cargo.toml`:

```toml
regex = "1.12.4"
```

- [ ] **Step 2: Add and serialize the operator variants**

Extend `LabelOperator` and `LabelOperator::as_str`:

```rust
pub enum LabelOperator {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    StartsWith,
    EndsWith,
    Regex,
}
```

```rust
LabelOperator::StartsWith => "^=",
LabelOperator::EndsWith => "$=",
LabelOperator::Regex => "~=",
```

- [ ] **Step 3: Parse the new tokens before plain equality**

Extend the token table in `split_selector_term`:

```rust
for (token, operator) in [
    (">=", LabelOperator::Gte),
    ("<=", LabelOperator::Lte),
    ("!=", LabelOperator::NotEq),
    ("^=", LabelOperator::StartsWith),
    ("$=", LabelOperator::EndsWith),
    ("~=", LabelOperator::Regex),
    ("=", LabelOperator::Eq),
    (">", LabelOperator::Gt),
    ("<", LabelOperator::Lt),
] {
```

- [ ] **Step 4: Match stored strings before typed ordering conversion**

At the start of `compare_label_values`, dispatch the new operators:

```rust
match operator {
    LabelOperator::StartsWith => {
        return left.to_lowercase().starts_with(&right.to_lowercase());
    }
    LabelOperator::EndsWith => {
        return left.to_lowercase().ends_with(&right.to_lowercase());
    }
    LabelOperator::Regex => {
        let Ok(pattern) = regex::RegexBuilder::new(right)
            .case_insensitive(true)
            .build()
        else {
            return false;
        };
        return pattern.is_match(left);
    }
    _ => {}
}
```

Keep the existing typed comparison below this block. Make `compare_ordering` exhaustive without
changing its ordered-operator behavior:

```rust
LabelOperator::StartsWith | LabelOperator::EndsWith | LabelOperator::Regex => false,
```

- [ ] **Step 5: Run core tests and update the lockfile**

Run:

```bash
cargo test -p note-core
```

Expected: PASS, including parser, typed comparison, case-insensitive matching, and invalid-regex
tests. Cargo updates `Cargo.lock` so `note-core` lists `regex` as a direct dependency.

- [ ] **Step 6: Run the storage and pipeline regressions**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
cargo test -p note-pipelines --test search_notes_test search_filters_results_by_label
```

Expected: PASS.

- [ ] **Step 7: Commit the shared behavior**

```bash
git add Cargo.lock crates/note-core/Cargo.toml crates/note-core/src/types.rs \
  crates/note-storage-contract-tests/src/notes.rs \
  crates/note-pipelines/tests/search_notes_test.rs
git commit -m "feat(labels): add string match operators"
```

### Task 3: Expose Operators in the Frontend

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs`
- Reuse: `crates/note-frontend/src/api.rs`

- [ ] **Step 1: Keep parser and display order explicit**

Add constants near the existing notes-page constants:

```rust
const LABEL_FILTER_PARSER_OPERATORS: [&str; 9] =
    [">=", "<=", "!=", "^=", "$=", "~=", "=", ">", "<"];
const LABEL_FILTER_DISPLAY_OPERATORS: [&str; 9] =
    ["=", "!=", "^=", "$=", "~=", ">", ">=", "<", "<="];
```

Use the parser constant in `parse_label_filters`:

```rust
for operator in LABEL_FILTER_PARSER_OPERATORS {
```

Use the display constant in `label_filter_bar`:

```rust
{ for LABEL_FILTER_DISPLAY_OPERATORS.iter().map(|operator| {
```

- [ ] **Step 2: Run the frontend test**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml parses_and_serializes_string_match_filters
```

Expected: PASS.

- [ ] **Step 3: Run all frontend logic tests and Wasm checking**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: PASS.

- [ ] **Step 4: Commit the frontend surface**

```bash
git add crates/note-frontend/src/pages/notes.rs
git commit -m "feat(frontend): expose label match operators"
```

### Task 4: Document REST and MCP Selector Syntax

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Document REST query and search-body fields**

Replace the label field comments on `ListNotesQuery`, `CountNotesQuery`, and `SearchQuery` with:

```rust
/// Label selector with `&`-separated terms ANDed. Supports bare-key presence,
/// `=`, `!=`, `>`, `>=`, `<`, `<=`, case-insensitive `^=` starts-with,
/// case-insensitive `$=` ends-with, and case-insensitive `~=` regex matching.
```

- [ ] **Step 2: Document MCP list and semantic-search fields**

Replace the label field comments on `ListNotesRequest` and `SemanticSearchRequest` with the same
selector description:

```rust
/// Label selector with `&`-separated terms ANDed. Supports bare-key presence,
/// `=`, `!=`, `>`, `>=`, `<`, `<=`, case-insensitive `^=` starts-with,
/// case-insensitive `$=` ends-with, and case-insensitive `~=` regex matching.
```

- [ ] **Step 3: Run generated-schema tests**

Run:

```bash
cargo test -p note-server openapi_documents_label_filter_operators
cargo test -p note-mcp advertised_schemas_are_bounded_and_keep_nullable_filters
```

Expected: PASS.

- [ ] **Step 4: Commit public documentation**

```bash
git add crates/note-server/src/notes_api.rs crates/note-mcp/src/stdio.rs
git commit -m "docs(labels): document filter operators"
```

### Task 5: Run Final Scoped Verification

**Files:**
- Verify all files listed above.

- [ ] **Step 1: Check formatting**

Run:

```bash
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

Expected: both exit successfully with no formatting differences.

- [ ] **Step 2: Run affected native test suites**

Run:

```bash
cargo test -p note-core
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
cargo test -p note-storage-pg --test contracts_test postgresql_satisfies_shared_storage_contracts
cargo test -p note-pipelines --test search_notes_test
cargo test -p note-server notes_api::tests
cargo test -p note-mcp
```

Expected: all configured tests PASS. The PostgreSQL contract may report an intentional skip when
its provisioning environment is unavailable; record that output explicitly.

- [ ] **Step 3: Run frontend tests and compilation checks**

Run:

```bash
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --workspace --all-targets
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
```

Expected: PASS.

- [ ] **Step 4: Validate the final repository state**

Run:

```bash
git diff --check
git status --short --branch
git log -5 --oneline
```

Expected: no unstaged implementation changes, no whitespace errors, and the focused implementation
commits appear after the approved design commit.
