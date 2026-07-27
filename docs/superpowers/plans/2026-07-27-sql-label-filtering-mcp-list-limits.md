# SQL Label Filtering and MCP List Limits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound MCP note listing to the REST pagination policy and make label selectors constrain database list pages and semantic-retrieval candidates before pagination or ranking.

**Architecture:** Keep `note-core` as the authoritative selector matcher by resolving each selector against distinct stored values, then express the accepted raw values as backend-specific SQL `EXISTS` predicates. Add a lightweight matching-note-ID repository operation and pass those IDs into title and dense retrieval before their ranking limits. Share list pagination normalization in `note-pipelines` so REST and MCP cannot drift.

**Tech Stack:** Rust 2021, async traits, Tokio, Turso 0.7 with `json_each`, PostgreSQL with SQLx `QueryBuilder` and text arrays, pgvector, RMCP/Schemars, Axum/Utoipa.

---

## File Map

- Create `crates/note-storage/src/label_filter.rs`: shared resolved-selector representation and exact value resolver.
- Modify `crates/note-storage/src/lib.rs`: export the resolved-selector module.
- Modify `crates/note-storage/src/repositories.rs`: add matching-note-ID lookup and optional allowed IDs to retrieval.
- Modify `crates/note-pipelines/src/list_notes.rs`: own shared list pagination constants and normalization.
- Modify `crates/note-server/src/notes_api.rs`: consume the shared pagination policy.
- Modify `crates/note-mcp/src/stdio.rs`: normalize optional MCP pagination at the transport boundary and retain the optional input schema.
- Modify `crates/note-storage-turso/src/notes.rs`: resolve selector values and apply SQL predicates before list pagination and counting.
- Modify `crates/note-storage-pg/src/notes.rs`: implement the equivalent PostgreSQL SQL-first list/count path.
- Modify `crates/note-storage-turso/src/retrieval.rs`: apply optional allowed IDs before vector and title ranking limits.
- Modify `crates/note-storage-pg/src/retrieval.rs`: apply optional allowed IDs before vector ranking.
- Modify `crates/note-storage-pg/src/title_search.sql`: apply optional allowed IDs before title ranking and `LIMIT`.
- Modify `crates/note-pipelines/src/search_notes.rs`: resolve matching IDs before embedding and pass them into both retrievers.
- Modify `crates/note-storage-contract-tests/src/notes.rs`: prove filtered pagination, counts, matching IDs, all operators, and all value types across backends.
- Modify `crates/note-storage-contract-tests/src/retrieval.rs`: prove allowed IDs constrain title and dense candidates before top-k.
- Modify `crates/note-storage-contract-tests/src/embedding.rs`: adapt the direct dense-retrieval call.
- Modify `crates/note-storage-turso/tests/notes_test.rs`: cover Turso JSON value-set filtering and SQL predicate placement.
- Modify `crates/note-storage-turso/tests/retrieval_test.rs`: cover Turso constrained retrieval and adapt direct callers.
- Modify `crates/note-storage-pg/tests/repositories_test.rs`: cover PostgreSQL array filtering and SQL predicate placement.
- Modify `crates/note-storage-pg/tests/retrieval_test.rs`: cover PostgreSQL constrained retrieval, adapt direct callers, and retain the GIN plan check.
- Modify `crates/note-pipelines/tests/search_notes_test.rs`: prove a no-match selector returns before embedding and broad filtered search is not crowded out.
- Modify `crates/note-pipelines/tests/support/events.rs`: forward the expanded repository contracts.
- Modify `crates/note-pipelines/tests/save_note_test.rs`: adapt the direct dense-retrieval call.
- Modify `crates/note-pipelines/src/embedding_queue.rs`: adapt the direct dense-retrieval call.
- Modify `docs/design.md`: replace the obsolete post-ranking label-filter flow and remove the resolved open decision.

## Execution Preflight

- [ ] Confirm the implementation checkout has no unrelated changes:

```bash
git status --short --branch
```

Expected: a clean branch containing the committed design and implementation plan.

- [ ] Run the current focused baseline before adding red tests:

```bash
cargo test -p note-storage -p note-storage-turso -p note-pipelines -p note-mcp -p note-server
cargo test -p note-storage-pg
```

Expected: native focused suites pass. PostgreSQL tests either pass against `TEST_DATABASE_URL` or
print their explicit skip notice; record which occurred before implementation.

### Task 1: Share REST and MCP List Pagination

**Files:**
- Modify: `crates/note-pipelines/src/list_notes.rs`
- Modify: `crates/note-server/src/notes_api.rs`
- Modify: `crates/note-mcp/src/stdio.rs`

- [ ] **Step 1: Add the failing shared pagination-policy test**

Append this test module to `crates/note-pipelines/src/list_notes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_pagination_normalizes_defaults_bounds_and_offsets() {
        assert_eq!(normalized_list_limit(None), 10);
        assert_eq!(normalized_list_limit(Some(0)), 0);
        assert_eq!(normalized_list_limit(Some(25)), 25);
        assert_eq!(normalized_list_limit(Some(-1)), 0);
        assert_eq!(normalized_list_limit(Some(2_000)), 1_000);

        assert_eq!(normalized_list_offset(None), 0);
        assert_eq!(normalized_list_offset(Some(-1)), 0);
        assert_eq!(normalized_list_offset(Some(25)), 25);
    }
}
```

- [ ] **Step 2: Verify the shared policy test fails**

Run:

```bash
cargo test -p note-pipelines list_notes::tests::list_pagination_normalizes_defaults_bounds_and_offsets -- --exact
```

Expected: compilation fails because `normalized_list_limit` and `normalized_list_offset` do not exist.

- [ ] **Step 3: Implement the shared pagination policy**

Add this immediately after the imports in `crates/note-pipelines/src/list_notes.rs`:

```rust
pub const DEFAULT_LIST_LIMIT: i64 = 10;
pub const MAX_LIST_LIMIT: i64 = 1_000;

pub fn normalized_list_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(0, MAX_LIST_LIMIT)
}

pub fn normalized_list_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}
```

The module is already publicly re-exported from `crates/note-pipelines/src/lib.rs`.

- [ ] **Step 4: Switch REST to the shared helpers**

In `crates/note-server/src/notes_api.rs`, import:

```rust
normalized_list_limit, normalized_list_offset,
```

Remove the local `DEFAULT_LIST_LIMIT`, `MAX_LIST_LIMIT`, `normalized_limit`, and
`normalized_offset`. Construct `ListNotesParams` with:

```rust
ListNotesParams {
    limit: Some(normalized_list_limit(req.limit)),
    offset: Some(normalized_list_offset(req.offset)),
    label: req.label,
}
```

Remove the now-redundant `list_notes_clamps_limit_to_one_thousand` unit test; the pipeline test owns
that policy. Keep the endpoint test `list_notes_defaults_to_ten_and_counts_total`.

- [ ] **Step 5: Add failing MCP handler pagination tests**

In the existing `stdio::tests` module, add focused helpers that seed notes through the test
context's storage session, then add:

```rust
async fn seed_summary_notes(backend: &dyn note_storage::StorageBackend, count: usize) {
    let session = backend.session().await.unwrap();
    for index in 0..count {
        let id = format!("mcp-list-{index:04}");
        let timestamp = i64::try_from(index).unwrap();
        session
            .insert_note(note_storage::NewNote {
                id: &id,
                title: &id,
                content: "summary-only body",
                attachments: &[],
                created_at: timestamp,
                updated_at: timestamp,
                note_revision: 1,
                deleted_at: None,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn list_notes_defaults_to_ten() {
    let (ctx, backend, _dir) = test_context().await;
    seed_summary_notes(backend.as_ref(), 12).await;
    let server = NoteMcpServer::new(Arc::new(ctx));

    let response = server
        .list_notes(Parameters(ListNotesRequest {
            limit: None,
            offset: None,
            label: None,
        }))
        .await
        .unwrap()
        .0;

    assert_eq!(response.notes.len(), 10);
}

#[tokio::test]
async fn list_notes_honors_zero_limit_and_explicit_offset() {
    let (ctx, backend, _dir) = test_context().await;
    seed_summary_notes(backend.as_ref(), 4).await;
    let server = NoteMcpServer::new(Arc::new(ctx));

    let empty = server
        .list_notes(Parameters(ListNotesRequest {
            limit: Some(0),
            offset: None,
            label: None,
        }))
        .await
        .unwrap()
        .0;
    assert!(empty.notes.is_empty());

    let page = server
        .list_notes(Parameters(ListNotesRequest {
            limit: Some(2),
            offset: Some(1),
            label: None,
        }))
        .await
        .unwrap()
        .0;
    assert_eq!(page.notes.len(), 2);
}

#[tokio::test]
async fn list_notes_clamps_limit_to_one_thousand() {
    let (ctx, backend, _dir) = test_context().await;
    seed_summary_notes(backend.as_ref(), 1_001).await;
    let server = NoteMcpServer::new(Arc::new(ctx));

    let response = server
        .list_notes(Parameters(ListNotesRequest {
            limit: Some(u32::MAX),
            offset: None,
            label: None,
        }))
        .await
        .unwrap()
        .0;

    assert_eq!(response.notes.len(), 1_000);
}
```

`seed_summary_notes` must insert deterministic `NewNote` values directly through one storage
session so this test does not create attachment files or embedding jobs.

- [ ] **Step 6: Verify MCP still lists without a bound**

Run:

```bash
cargo test -p note-mcp stdio::tests::list_notes_defaults_to_ten -- --exact
```

Expected: FAIL because the handler still passes `None` to `list_notes_tool` and returns all 12 notes.

- [ ] **Step 7: Normalize MCP pagination at the transport boundary**

Import the shared helpers into `crates/note-mcp/src/stdio.rs` and replace the handler arguments with:

```rust
let notes = list_notes_tool(
    &self.ctx,
    Some(normalized_list_limit(request.limit.map(i64::from))),
    Some(normalized_list_offset(request.offset.map(i64::from))),
    request.label,
)
.await
.map_err(to_error_data)?;
```

Update the `ListNotesRequest.limit` documentation to state that omission returns 10 notes and the
maximum is 1,000. Keep `limit` and `offset` as `Option<u32>` so the generated schema continues to
have no required fields. Do not normalize inside `list_notes_tool`; internal callers may still
request unbounded storage behavior.

- [ ] **Step 8: Run the pagination slice**

Run:

```bash
cargo test -p note-pipelines list_notes::tests::list_pagination_normalizes_defaults_bounds_and_offsets -- --exact
cargo test -p note-server notes_api::tests::list_notes_defaults_to_ten_and_counts_total -- --exact
cargo test -p note-mcp stdio::tests::list_notes_defaults_to_ten -- --exact
cargo test -p note-mcp stdio::tests::list_notes_honors_zero_limit_and_explicit_offset -- --exact
cargo test -p note-mcp stdio::tests::list_notes_clamps_limit_to_one_thousand -- --exact
cargo test -p note-mcp stdio::tests::advertised_schemas_are_bounded_and_keep_nullable_filters -- --exact
```

Expected: all six commands pass.

- [ ] **Step 9: Commit the pagination slice**

```bash
git add crates/note-pipelines/src/list_notes.rs crates/note-server/src/notes_api.rs crates/note-mcp/src/stdio.rs
git commit -m "fix(mcp): bound note list pagination"
```

### Task 2: Resolve Selectors with the Existing Core Matcher

**Files:**
- Create: `crates/note-storage/src/label_filter.rs`
- Modify: `crates/note-storage/src/lib.rs`

- [ ] **Step 1: Write failing resolver tests**

Create `crates/note-storage/src/label_filter.rs` with the imports and tests first:

```rust
use note_core::{label_matches_selector, Label, LabelSelector};

#[cfg(test)]
mod tests {
    use super::*;
    use note_core::{parse_label_selectors, LabelValueType};

    fn label(key: &str, value: &str, value_type: LabelValueType) -> Label {
        Label {
            key: key.into(),
            value: value.into(),
            description: String::new(),
            value_type,
        }
    }

    #[test]
    fn resolves_typed_regex_and_presence_selectors_to_raw_values() {
        let selectors =
            parse_label_selectors("status~=^RE.*Y$&version>=1.10.0&archived");
        let labels = vec![
            label("status", "ready", LabelValueType::Text),
            label("status", "blocked", LabelValueType::Text),
            label("version", "1.2.0", LabelValueType::Version),
            label("version", "1.10.0", LabelValueType::Version),
            label("archived", "yes", LabelValueType::Text),
        ];

        assert_eq!(
            resolve_label_selectors(&selectors, &labels),
            Some(vec![
                ResolvedLabelSelector {
                    key: "status".into(),
                    values: Some(vec!["ready".into()]),
                },
                ResolvedLabelSelector {
                    key: "version".into(),
                    values: Some(vec!["1.10.0".into()]),
                },
                ResolvedLabelSelector {
                    key: "archived".into(),
                    values: None,
                },
            ])
        );
    }

    #[test]
    fn returns_none_when_any_value_selector_has_no_match() {
        let labels = vec![label("status", "ready", LabelValueType::Text)];

        assert_eq!(
            resolve_label_selectors(
                &parse_label_selectors("status~=["),
                &labels,
            ),
            None
        );
        assert_eq!(
            resolve_label_selectors(
                &parse_label_selectors("missing=value"),
                &labels,
            ),
            None
        );
        assert_eq!(
            resolve_label_selectors(
                &parse_label_selectors("missing"),
                &labels,
            ),
            None
        );
    }

    #[test]
    fn preserves_separate_same_key_selectors_and_deduplicates_values() {
        let labels = vec![
            label("version", "1.10.0", LabelValueType::Version),
            label("version", "1.10.0", LabelValueType::Version),
            label("version", "2.0.0", LabelValueType::Version),
        ];
        let resolved = resolve_label_selectors(
            &parse_label_selectors("version>=1.10.0&version<2.0.0"),
            &labels,
        )
        .unwrap();

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].values, Some(vec!["1.10.0".into(), "2.0.0".into()]));
        assert_eq!(resolved[1].values, Some(vec!["1.10.0".into()]));
    }
}
```

- [ ] **Step 2: Verify the resolver tests fail**

Run:

```bash
cargo test -p note-storage label_filter::tests
```

Expected: compilation fails because `ResolvedLabelSelector` and `resolve_label_selectors` are not
defined.

- [ ] **Step 3: Implement the pure resolver**

Add this above the tests:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLabelSelector {
    pub key: String,
    /// `None` means bare-key presence. `Some` is always nonempty.
    pub values: Option<Vec<String>>,
}

pub fn resolve_label_selectors(
    selectors: &[LabelSelector],
    distinct_labels: &[Label],
) -> Option<Vec<ResolvedLabelSelector>> {
    let mut resolved = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let values = if selector.value.is_none() {
            if !distinct_labels
                .iter()
                .any(|label| label.key == selector.key)
            {
                return None;
            }
            None
        } else {
            let mut values = distinct_labels
                .iter()
                .filter(|label| label_matches_selector(label, selector))
                .map(|label| label.value.clone())
                .collect::<Vec<_>>();
            values.sort();
            values.dedup();
            if values.is_empty() {
                return None;
            }
            Some(values)
        };
        resolved.push(ResolvedLabelSelector {
            key: selector.key.clone(),
            values,
        });
    }
    Some(resolved)
}
```

Export the module from `crates/note-storage/src/lib.rs`:

```rust
pub mod label_filter;
pub use label_filter::*;
```

- [ ] **Step 4: Run and commit the resolver**

Run:

```bash
cargo test -p note-storage label_filter::tests
cargo test -p note-storage
```

Expected: both commands pass.

Commit:

```bash
git add crates/note-storage/src/label_filter.rs crates/note-storage/src/lib.rs
git commit -m "feat(storage): resolve label selector values"
```

### Task 3: Push Turso List and Count Filters into SQL

**Files:**
- Modify: `crates/note-storage-turso/src/notes.rs`
- Test: `crates/note-storage-turso/src/notes.rs`
- Test: `crates/note-storage-turso/tests/notes_test.rs`

- [ ] **Step 1: Add failing SQL-construction and filtered-page tests**

Add a private test module to `crates/note-storage-turso/src/notes.rs` that constructs two
`ResolvedLabelSelector` values, appends them to an active-note base query, then asserts:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_predicates_precede_order_and_limit() {
        let mut sql =
            "SELECT n.id FROM notes n WHERE n.deleted_at IS NULL".to_string();
        let mut params = Vec::new();
        let resolved = vec![
            ResolvedLabelSelector {
                key: "status".into(),
                values: Some(vec!["ready".into()]),
            },
            ResolvedLabelSelector {
                key: "archived".into(),
                values: None,
            },
        ];
        push_label_predicates(&mut sql, &mut params, "n", &resolved).unwrap();
        sql.push_str(" ORDER BY n.created_at DESC, n.id ASC LIMIT ? OFFSET ?");

        assert_eq!(sql.matches("EXISTS").count(), 2);
        assert!(sql.contains("json_each"));
        assert!(sql.find("EXISTS").unwrap() < sql.find("ORDER BY").unwrap());
        assert!(sql.find("ORDER BY").unwrap() < sql.find("LIMIT").unwrap());
    }
}
```

In `crates/note-storage-turso/tests/notes_test.rs`, add
`filtered_limit_is_applied_after_label_predicates`. Insert five newer `status=blocked` notes and one
older `status=ready` note, then assert:

```rust
let selectors = parse_label_selectors("status=ready");
let page = fixture
    .session
    .list_note_summaries(&selectors, Some(1), Some(0))
    .await
    .unwrap();
assert_eq!(page.iter().map(|note| note.id.as_str()).collect::<Vec<_>>(), vec!["ready"]);
assert_eq!(fixture.session.count_notes(&selectors).await.unwrap(), 1);
```

Also add a same-key case using `version>=1.10.0&version<2.0.0` and an invalid regex case returning
an empty page and zero count.

- [ ] **Step 2: Verify the Turso SQL helper is missing**

Run:

```bash
cargo test -p note-storage-turso notes::tests::resolved_predicates_precede_order_and_limit -- --exact
```

Expected: compilation fails because the resolved-selector SQL helper does not exist.

- [ ] **Step 3: Batch-load distinct Turso label values**

Import `resolve_label_selectors` and `ResolvedLabelSelector`. Add:

```rust
impl TursoSession {
    async fn resolved_label_selectors(
        &self,
        selectors: &[LabelSelector],
    ) -> StorageResult<Option<Vec<ResolvedLabelSelector>>> {
        let mut keys = selectors
            .iter()
            .map(|selector| selector.key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return Ok(resolve_label_selectors(selectors, &[]));
        }

        let keys_json = serde_json::to_string(&keys).map_err(|error| {
            StorageError::with_source(
                StorageErrorKind::Operation,
                "serialize label selector keys",
                error,
            )
        })?;
        let mut rows = self
            .connection
            .query(
                "SELECT DISTINCT lk.key, nl.value, lk.description, lk.value_type
                 FROM label_keys lk
                 JOIN note_labels nl ON nl.label_key_id = lk.id
                 JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
                 WHERE lk.key IN (SELECT value FROM json_each(?1))",
                turso::params![keys_json],
            )
            .await
            .map_err(|error| map_turso_error("resolve label selector values", error))?;
        let mut labels = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_turso_error("read label selector values", error))?
        {
            let value_type = row
                .get::<String>(3)
                .map_err(|error| map_turso_error("decode label selector value type", error))?
                .parse()
                .map_err(|error| StorageError::new(StorageErrorKind::Operation, error))?;
            labels.push(note_core::Label {
                key: row.get(0).map_err(|error| map_turso_error("decode label key", error))?,
                value: row.get(1).map_err(|error| map_turso_error("decode label value", error))?,
                description: row
                    .get(2)
                    .map_err(|error| map_turso_error("decode label description", error))?,
                value_type,
            });
        }
        Ok(resolve_label_selectors(selectors, &labels))
    }
}
```

- [ ] **Step 4: Add the Turso predicate builder**

Add a helper that appends one correlated `EXISTS` clause per resolved selector and numbers binds
from `params.len() + 1`:

```rust
fn push_label_predicates(
    sql: &mut String,
    params: &mut Vec<turso::Value>,
    note_alias: &str,
    selectors: &[ResolvedLabelSelector],
) -> StorageResult<()> {
    for selector in selectors {
        params.push(selector.key.clone().into());
        let key_param = params.len();
        sql.push_str(&format!(
            " AND EXISTS (
                 SELECT 1
                 FROM note_labels filtered_nl
                 JOIN label_keys filtered_lk ON filtered_lk.id = filtered_nl.label_key_id
                 WHERE filtered_nl.note_id = {note_alias}.id
                   AND filtered_lk.key = ?{key_param}"
        ));
        if let Some(values) = &selector.values {
            let values_json = serde_json::to_string(values).map_err(|error| {
                StorageError::with_source(
                    StorageErrorKind::Operation,
                    "serialize resolved label values",
                    error,
                )
            })?;
            params.push(values_json.into());
            let values_param = params.len();
            sql.push_str(&format!(
                " AND filtered_nl.value IN (
                     SELECT value FROM json_each(?{values_param})
                 )"
            ));
        }
        sql.push(')');
    }
    Ok(())
}
```

- [ ] **Step 5: Replace Rust post-filtering with SQL-first pages and counts**

For `list_notes` and `list_note_summaries`:

1. Keep the current empty-selector fast path.
2. Call `resolved_label_selectors`.
3. Return `Vec::new()` when it returns `None`.
4. Add predicates immediately after `WHERE n.deleted_at IS NULL`.
5. Append `ORDER BY n.created_at DESC, n.id ASC`.
6. Append `LIMIT` and `OFFSET` after predicates for both filtered and unfiltered pages.
7. Decode and hydrate labels only for returned rows.

For `count_notes`, use:

```rust
let mut sql = "SELECT COUNT(*) FROM notes n WHERE n.deleted_at IS NULL".to_string();
let mut params = Vec::new();
push_label_predicates(&mut sql, &mut params, "n", &resolved)?;
```

Decode the single nonnegative count. Remove `note_matches_selectors`,
`summary_matches_selectors`, and `paginate` after all callers are gone.

- [ ] **Step 6: Run and commit the Turso slice**

Run:

```bash
cargo test -p note-storage-turso --test notes_test filtered_limit_is_applied_after_label_predicates -- --exact
cargo test -p note-storage-turso notes::tests::resolved_predicates_precede_order_and_limit -- --exact
cargo test -p note-storage-turso
```

Expected: all commands pass.

Commit:

```bash
git add crates/note-storage-turso/src/notes.rs crates/note-storage-turso/tests/notes_test.rs
git commit -m "perf(turso): filter labeled note pages in SQL"
```

### Task 4: Push PostgreSQL List and Count Filters into SQL

**Files:**
- Modify: `crates/note-storage-pg/src/notes.rs`
- Test: `crates/note-storage-pg/src/notes.rs`
- Test: `crates/note-storage-pg/tests/repositories_test.rs`

- [ ] **Step 1: Add failing PostgreSQL predicate and paging tests**

Add a private `notes.rs` unit test that builds a query with two resolved selectors and asserts:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_predicates_precede_order_and_limit() {
        let resolved = vec![
            ResolvedLabelSelector {
                key: "status".into(),
                values: Some(vec!["ready".into()]),
            },
            ResolvedLabelSelector {
                key: "archived".into(),
                values: None,
            },
        ];
        let mut builder: QueryBuilder<'_, Postgres> =
            QueryBuilder::new("SELECT n.id FROM notes n WHERE n.deleted_at IS NULL");
        push_label_predicates(&mut builder, "n", &resolved);
        builder.push(" ORDER BY n.created_at DESC, n.id ASC LIMIT ").push_bind(10_i64);
        let sql = builder.sql();

        assert_eq!(sql.matches("EXISTS").count(), 2);
        assert!(sql.contains("= ANY("));
        assert!(sql.find("EXISTS").unwrap() < sql.find("ORDER BY").unwrap());
        assert!(sql.find("ORDER BY").unwrap() < sql.find("LIMIT").unwrap());
    }
}
```

In `repositories_test.rs`, add `filtered_pages_and_counts_use_resolved_value_arrays`. Provision the
normal isolated PostgreSQL database, insert five newer blocked notes plus one older ready note, and
assert a `limit=1` `status=ready` page returns the older note and count returns one. Add the same-key
version selector and invalid-regex assertions used by the Turso focused test.

- [ ] **Step 2: Verify the PostgreSQL helper is missing**

Run:

```bash
cargo test -p note-storage-pg notes::tests::resolved_predicates_precede_order_and_limit -- --exact
```

Expected: compilation fails because the PostgreSQL predicate builder does not exist.

- [ ] **Step 3: Resolve PostgreSQL selector values in one query**

Import `note_storage::{resolve_label_selectors, ResolvedLabelSelector}` and
`sqlx::{Postgres, QueryBuilder}`. Add `PgSession::resolved_label_selectors`; collect, sort, and
deduplicate every selector key, then use:

```rust
let rows = sqlx::query_as::<_, (String, String, String, String)>(
    "SELECT DISTINCT lk.key, nl.value, lk.description, lk.value_type
     FROM label_keys lk
     JOIN note_labels nl ON nl.label_key_id = lk.id
     JOIN notes n ON n.id = nl.note_id AND n.deleted_at IS NULL
     WHERE lk.key = ANY($1)",
)
.bind(keys)
.fetch_all(&mut *connection)
.await
.map_err(|error| map_sqlx_error("resolve label selector values", error))?;
```

Convert each row into `note_core::Label`, parse `value_type` with the existing storage error
category, then call `resolve_label_selectors`.

- [ ] **Step 4: Add the PostgreSQL predicate builder**

Use owned cloned binds so the builder does not borrow the resolved filter:

```rust
fn push_label_predicates(
    builder: &mut QueryBuilder<'_, Postgres>,
    note_alias: &str,
    selectors: &[ResolvedLabelSelector],
) {
    for selector in selectors {
        builder
            .push(" AND EXISTS (SELECT 1 FROM note_labels filtered_nl ")
            .push("JOIN label_keys filtered_lk ON filtered_lk.id = filtered_nl.label_key_id ")
            .push("WHERE filtered_nl.note_id = ")
            .push(note_alias)
            .push(".id AND filtered_lk.key = ")
            .push_bind(selector.key.clone());
        if let Some(values) = &selector.values {
            builder
                .push(" AND filtered_nl.value = ANY(")
                .push_bind(values.clone())
                .push(")");
        }
        builder.push(")");
    }
}
```

- [ ] **Step 5: Replace filtered streams with SQLx query-builder pages and count**

Build filtered note and summary queries with the existing lateral JSON label aggregation, append
the active-note predicate, call `push_label_predicates`, then append:

```rust
builder
    .push(" ORDER BY n.created_at DESC, n.id ASC LIMIT ")
    .push_bind(normalize_limit(limit))
    .push(" OFFSET ")
    .push_bind(offset.unwrap_or(0).max(0));
```

Execute with `build_query_as::<NoteRow>()` or `build_query_as::<SummaryRow>()`. Build count from
`SELECT COUNT(*)::bigint FROM notes n WHERE n.deleted_at IS NULL`, append the same predicates, and
decode one `i64`. Remove `stream_matching_notes`, `stream_matching_summaries`,
`count_matching_summaries`, the Rust match helpers, and their now-unused `TryStreamExt` import.

- [ ] **Step 6: Run and commit the PostgreSQL slice**

Run:

```bash
cargo test -p note-storage-pg notes::tests::resolved_predicates_precede_order_and_limit -- --exact
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test repositories_test filtered_pages_and_counts_use_resolved_value_arrays -- --exact
cargo test -p note-storage-pg
```

Expected: the unit test passes. Live PostgreSQL tests pass when `TEST_DATABASE_URL` is configured;
without it, the repository's explicit skip notice is environmental and must be reported.

Commit:

```bash
git add crates/note-storage-pg/src/notes.rs crates/note-storage-pg/tests/repositories_test.rs
git commit -m "perf(postgres): filter labeled note pages in SQL"
```

### Task 5: Add SQL Matching-Note-ID Lookup

**Files:**
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/notes.rs`
- Modify: `crates/note-storage-pg/src/notes.rs`
- Modify: `crates/note-storage-contract-tests/src/notes.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] **Step 1: Add the failing shared contract assertions**

After the existing typed-selector page assertions in
`crates/note-storage-contract-tests/src/notes.rs`, add:

```rust
assert_eq!(
    session
        .count_notes(&parse_label_selectors("contract-notes-status"))
        .await
        .unwrap(),
    5
);
assert_eq!(
    session
        .count_notes(&parse_label_selectors("contract-notes-status!=blocked"))
        .await
        .unwrap(),
    5
);

assert_eq!(
    session.matching_note_ids(&selectors).await.unwrap(),
    vec![
        "contract-notes-active",
        "contract-notes-middle",
        "contract-notes-newer",
        "contract-notes-numeric-ten",
    ]
);

let missing = parse_label_selectors("contract-notes-missing=value");
assert!(session.matching_note_ids(&missing).await.unwrap().is_empty());

let invalid = parse_label_selectors("contract-notes-status~=[");
assert!(session.matching_note_ids(&invalid).await.unwrap().is_empty());
```

Define six additional label keys and one note that carries:

```rust
for (key, value_type) in [
    ("contract-filter-text", LabelValueType::Text),
    ("contract-filter-number", LabelValueType::Number),
    ("contract-filter-version", LabelValueType::Version),
    ("contract-filter-date", LabelValueType::Date),
    ("contract-filter-datetime", LabelValueType::DateTime),
    ("contract-filter-time", LabelValueType::Time),
] {
    session
        .insert_label_key_with_type(key, key, value_type)
        .await
        .unwrap();
}
session
    .insert_note(NewNote {
        id: "contract-filter-all-types",
        title: "All typed filters",
        content: "All typed filters",
        attachments: &[],
        created_at: 50,
        updated_at: 50,
        note_revision: 1,
        deleted_at: None,
    })
    .await
    .unwrap();
for (key, value) in [
    ("contract-filter-text", "alpha"),
    ("contract-filter-number", "2.5"),
    ("contract-filter-version", "1.10.0"),
    ("contract-filter-date", "2026-07-27"),
    ("contract-filter-datetime", "2026-07-27T12:30:00"),
    ("contract-filter-time", "12:30:00"),
] {
    session
        .attach_label("contract-filter-all-types", key, value)
        .await
        .unwrap();
}

let all_types = parse_label_selectors(
    "contract-filter-text=alpha\
     &contract-filter-number>=2.5\
     &contract-filter-version>=1.10.0\
     &contract-filter-date=2026-07-27\
     &contract-filter-datetime<2026-07-27T13:00:00\
     &contract-filter-time>=12:30",
);
assert_eq!(
    session.matching_note_ids(&all_types).await.unwrap(),
    vec!["contract-filter-all-types"]
);
assert_eq!(session.count_notes(&all_types).await.unwrap(), 1);
assert_eq!(
    session
        .list_note_summaries(&all_types, Some(1), Some(0))
        .await
        .unwrap()[0]
        .id,
    "contract-filter-all-types"
);
```

Add this empty-operand assertion and retain the existing prefix, suffix, regex, invalid-regex,
typed comparison, negative-limit, and unbounded-page cases:

```rust
assert_eq!(
    session
        .count_notes(&parse_label_selectors("contract-notes-status^="))
        .await
        .unwrap(),
    6
);
```

After soft-deleting `contract-notes-active`, assert the matching ID list no longer contains it.
Matching IDs are specified in ascending ID order so both adapters have deterministic results.

- [ ] **Step 2: Add the repository method and verify implementations are incomplete**

Add to `NotesRepository`:

```rust
async fn matching_note_ids(
    &self,
    selectors: &[note_core::LabelSelector],
) -> StorageResult<Vec<String>>;
```

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
```

Expected: compilation fails because both storage sessions and the pipeline event transaction are
missing the new trait method.

- [ ] **Step 3: Implement matching IDs in both adapters**

In each adapter:

1. Return all active IDs ordered by ID when selectors are empty.
2. Resolve nonempty selectors through the adapter helper from Tasks 3 and 4.
3. Return an empty vector when resolution returns `None`.
4. Build `SELECT n.id FROM notes n WHERE n.deleted_at IS NULL`.
5. Append the same SQL predicates used by list and count.
6. Append `ORDER BY n.id`.

The Turso implementation uses the existing `Vec<turso::Value>` builder. The PostgreSQL
implementation uses `QueryBuilder<Postgres>` and `build_query_scalar::<String>()`.

- [ ] **Step 4: Forward the new method in pipeline test storage**

Add this entry to the `NotesRepository` forwarding macro invocation:

```rust
fn matching_note_ids(selectors: &[LabelSelector]) -> Vec<String>;
```

- [ ] **Step 5: Run both shared contracts**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test contracts_test postgresql_satisfies_shared_storage_contracts
```

Expected: Turso passes. PostgreSQL passes with the configured live test database; otherwise record
the repository's skip notice.

- [ ] **Step 6: Commit the matching-ID contract**

```bash
git add crates/note-storage/src/repositories.rs crates/note-storage-turso/src/notes.rs crates/note-storage-pg/src/notes.rs crates/note-storage-contract-tests/src/notes.rs crates/note-pipelines/tests/support/events.rs
git commit -m "feat(storage): query matching note ids"
```

### Task 6: Constrain Title and Dense Retrieval Before Ranking

**Files:**
- Modify: `crates/note-storage/src/repositories.rs`
- Modify: `crates/note-storage-turso/src/retrieval.rs`
- Modify: `crates/note-storage-pg/src/retrieval.rs`
- Modify: `crates/note-storage-pg/src/title_search.sql`
- Modify: `crates/note-storage-contract-tests/src/retrieval.rs`
- Modify: `crates/note-storage-contract-tests/src/embedding.rs`
- Modify: `crates/note-storage-turso/tests/retrieval_test.rs`
- Modify: `crates/note-storage-pg/tests/retrieval_test.rs`
- Modify: `crates/note-pipelines/src/embedding_queue.rs`
- Modify: `crates/note-pipelines/tests/save_note_test.rs`
- Modify: `crates/note-pipelines/tests/support/events.rs`

- [ ] **Step 1: Add failing allowed-ID retrieval contracts**

In `crates/note-storage-contract-tests/src/retrieval.rs`, add two active notes where the excluded
note is the stronger candidate and use `limit=1`:

```rust
insert_note(
    session.as_ref(),
    "contract-retrieval-allowed",
    "Needle",
    "allowed body",
)
.await;
insert_note(
    session.as_ref(),
    "contract-retrieval-excluded",
    "Needle Needle Needle",
    "excluded body",
)
.await;
session
    .insert_chunk_embedding("contract-retrieval-allowed", 0, &unit(1))
    .await
    .unwrap();
session
    .insert_chunk_embedding("contract-retrieval-excluded", 0, &unit(0))
    .await
    .unwrap();

let allowed = vec!["contract-retrieval-allowed".to_string()];
assert_eq!(
    session
        .title_search("Needle", 1, Some(&allowed))
        .await
        .unwrap(),
    allowed
);
assert_eq!(
    session
        .dense_search(&unit(0), 1, Some(&allowed))
        .await
        .unwrap(),
    allowed
);
assert!(session
    .title_search("Needle", 1, Some(&[]))
    .await
    .unwrap()
    .is_empty());
assert!(session
    .dense_search(&unit(0), 1, Some(&[]))
    .await
    .unwrap()
    .is_empty());
```

Give the excluded note a title with repeated `Needle` terms and a vector equal to `unit(0)`. Give
the allowed note a single-term title and a farther normalized vector. Insert both embeddings before
the assertions.

- [ ] **Step 2: Expand the retrieval trait**

Replace both retrieval signatures with:

```rust
async fn dense_search(
    &self,
    query: &[f32],
    limit: usize,
    allowed_note_ids: Option<&[String]>,
) -> StorageResult<Vec<String>>;

async fn title_search(
    &self,
    query: &str,
    limit: usize,
    allowed_note_ids: Option<&[String]>,
) -> StorageResult<Vec<String>>;
```

Update the event forwarder with the same argument lists. Add trailing `None` to existing direct
calls in:

```text
crates/note-storage-contract-tests/src/retrieval.rs
crates/note-storage-contract-tests/src/embedding.rs
crates/note-storage-turso/tests/retrieval_test.rs
crates/note-storage-pg/tests/retrieval_test.rs
crates/note-pipelines/src/embedding_queue.rs
crates/note-pipelines/tests/save_note_test.rs
```

Do not change the new allowed-ID contract calls.

- [ ] **Step 3: Verify constrained retrieval fails to compile**

Run:

```bash
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
```

Expected: compilation fails until both adapter implementations accept the new argument.

- [ ] **Step 4: Implement Turso allowed-ID filtering**

Return an empty vector immediately for `Some([])`. Serialize nonempty allowed IDs once as JSON.
Keep the existing unfiltered queries for `None`.

For filtered dense retrieval, use:

```sql
SELECT e.note_id
FROM note_chunk_embeddings AS e
JOIN notes AS n ON n.id = e.note_id
WHERE n.deleted_at IS NULL
  AND e.note_id IN (SELECT value FROM json_each(?2))
GROUP BY e.note_id
ORDER BY MIN(vector_distance_cos(e.embedding, vector(?1))) ASC, e.note_id ASC
LIMIT ?3
```

Add a filtered title candidate constant:

```rust
const FILTERED_TITLE_FTS_CANDIDATE_SQL: &str =
    "SELECT id, fts_score(title, ?1) AS score
     FROM notes
     WHERE fts_match(title, ?1)
       AND id IN (SELECT value FROM json_each(?2))
     ORDER BY score DESC
     LIMIT ?3";
```

The allowed-ID predicate must be in this initial FTS query, before its fixed physical candidate
limit. Retain the later active-note check as a defensive consistency check.

- [ ] **Step 5: Implement PostgreSQL allowed-ID filtering**

Convert the borrowed slice once:

```rust
let allowed_note_ids = allowed_note_ids.map(|ids| ids.to_vec());
if allowed_note_ids.as_ref().is_some_and(Vec::is_empty) {
    return Ok(Vec::new());
}
```

Use this dense query and bind vector, optional text array, then limit:

```sql
SELECT embedding.note_id
FROM note_chunk_embeddings AS embedding
JOIN notes ON notes.id = embedding.note_id
WHERE notes.deleted_at IS NULL
  AND ($2::text[] IS NULL OR embedding.note_id = ANY($2))
GROUP BY embedding.note_id
ORDER BY MIN(embedding.embedding <=> $1) ASC, embedding.note_id ASC
LIMIT $3
```

Change `title_search.sql` to:

```sql
WITH query AS (
    SELECT to_tsquery('simple'::regconfig, $1) AS terms
)
SELECT notes.id
FROM notes
CROSS JOIN query
WHERE notes.deleted_at IS NULL
  AND ($2::text[] IS NULL OR notes.id = ANY($2))
  AND notes.title_fts @@ query.terms
ORDER BY ts_rank_cd(notes.title_fts, query.terms) DESC, notes.id ASC
LIMIT $3
```

Bind the normalized query, optional ID vector, and checked limit in that order. Update the
PostgreSQL `EXPLAIN` test to bind `Option::<Vec<String>>::None` between the query and limit while
retaining the GIN index assertion.

- [ ] **Step 6: Add adapter-focused constrained-retrieval regressions**

In each adapter's `retrieval_test.rs`, add
`allowed_ids_are_applied_before_title_and_dense_limits`. Insert the stronger excluded note and
weaker allowed note used by the shared contract, assert both filtered calls return the allowed ID
with `limit=1`, and assert the equivalent `None` calls return the excluded ID first.

- [ ] **Step 7: Run the retrieval slice and audit call sites**

Run:

```bash
cargo test -p note-storage-turso --test retrieval_test
cargo test -p note-storage-turso --test contracts_test turso_satisfies_shared_storage_contracts
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test retrieval_test
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg --test contracts_test postgresql_satisfies_shared_storage_contracts
rg -n 'dense_search\\(|title_search\\(' crates --glob '*.rs'
```

Expected: Turso tests pass, live PostgreSQL tests pass when configured, and every unfiltered call
shown by `rg` has a trailing `None`.

- [ ] **Step 8: Commit constrained retrieval**

```bash
git add crates/note-storage/src/repositories.rs crates/note-storage-turso/src/retrieval.rs crates/note-storage-pg/src/retrieval.rs crates/note-storage-pg/src/title_search.sql crates/note-storage-contract-tests/src/retrieval.rs crates/note-storage-contract-tests/src/embedding.rs crates/note-storage-turso/tests/retrieval_test.rs crates/note-storage-pg/tests/retrieval_test.rs crates/note-pipelines/src/embedding_queue.rs crates/note-pipelines/tests/save_note_test.rs crates/note-pipelines/tests/support/events.rs
git commit -m "feat(storage): constrain retrieval candidates"
```

### Task 7: Filter Search Before Embedding and Ranking

**Files:**
- Modify: `crates/note-pipelines/src/search_notes.rs`
- Modify: `crates/note-pipelines/tests/search_notes_test.rs`

- [ ] **Step 1: Add a failing pre-embedding empty-filter test**

Reuse `PanicEmbedder` and add:

```rust
#[tokio::test]
async fn empty_label_collection_returns_without_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn note_storage::StorageBackend> = Arc::new(
        note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
            .await
            .unwrap(),
    );
    let attachments: Arc<dyn note_attachments::AttachmentStore> = Arc::new(
        note_attachments::FilesystemAttachmentStore::new(dir.path().join("attachments")),
    );
    let ctx = note_pipelines::Context::new(storage, Arc::new(PanicEmbedder), attachments);

    let results =
        search_notes_filtered(&ctx, "needle", 10, Some("missing=value".into()))
            .await
            .unwrap();

    assert!(results.is_empty());
}
```

- [ ] **Step 2: Verify the old pipeline embeds first**

Run:

```bash
cargo test -p note-pipelines --test search_notes_test empty_label_collection_returns_without_embedding -- --exact
```

Expected: FAIL by panic because the current pipeline embeds before determining that no label matches.

- [ ] **Step 3: Add a crowd-out regression**

Add `filtered_search_cannot_be_crowded_out_before_title_limit`:

1. Save one `scope=allowed` note titled `Needle`.
2. Insert at least 129 `scope=excluded` notes titled `Needle Needle Needle`.
3. Do not drain embeddings, so the assertion isolates title retrieval.
4. Search for `Needle` with `limit=1` and `label=scope=allowed`.
5. Assert the allowed note is returned.

The old bounded post-filter path requests 128 title candidates and returns no result; the new
pre-ranking constraint returns the allowed note.

- [ ] **Step 4: Reorder and constrain the search pipeline**

Replace the allowed-set preparation with:

```rust
let session = ctx.storage().session().await?;
let selectors = label
    .as_deref()
    .map(parse_label_selectors)
    .unwrap_or_default();
let allowed_note_ids = if selectors.is_empty() {
    None
} else {
    let ids = session.matching_note_ids(&selectors).await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Some(ids)
};

let dense = ctx.embedder.embed(query).await?;
let retrieval_limit = if allowed_note_ids.is_some() {
    limit.min(MAX_RETRIEVAL_CANDIDATES)
} else {
    limit
        .saturating_mul(RETRIEVAL_OVERFETCH_FACTOR)
        .clamp(MIN_RETRIEVAL_CANDIDATES, MAX_RETRIEVAL_CANDIDATES)
};
let allowed_slice = allowed_note_ids.as_deref();
let title_ranking = session
    .title_search(query, retrieval_limit, allowed_slice)
    .await?;
let dense_ranking = session
    .dense_search(&dense, retrieval_limit, allowed_slice)
    .await?;
```

Build a `HashSet<&str>` from the optional vector only for the existing defensive final membership
check. Correctness must come from the constrained storage queries, not that final check.

- [ ] **Step 5: Run pipeline regressions**

Run:

```bash
cargo test -p note-pipelines --test search_notes_test empty_label_collection_returns_without_embedding -- --exact
cargo test -p note-pipelines --test search_notes_test filtered_search_cannot_be_crowded_out_before_title_limit -- --exact
cargo test -p note-pipelines --test search_notes_test search_filters_results_by_label -- --exact
cargo test -p note-pipelines --test search_notes_test fusion_overfetches_candidates_before_applying_requested_limit -- --exact
cargo test -p note-pipelines --test search_notes_test huge_limit_is_bounded_before_storage_retrieval -- --exact
```

Expected: all five tests pass. The unfiltered fusion and huge-limit tests prove the existing
unfiltered path remains unchanged.

- [ ] **Step 6: Commit the pipeline slice**

```bash
git add crates/note-pipelines/src/search_notes.rs crates/note-pipelines/tests/search_notes_test.rs
git commit -m "fix(search): filter labels before retrieval"
```

### Task 8: Update Architecture Documentation and Verify the Workspace

**Files:**
- Modify: `docs/design.md`

- [ ] **Step 1: Update the documented search flow**

Replace the `search_notes` pipeline paragraph with:

```markdown
**search_notes**: parse label selectors and resolve matching active note IDs in storage before
embedding; return immediately when the label collection is empty → embed the query once → constrain
both title FTS and exact dense retrieval to the matching IDs before their ranking limits → combine
the two ranked ID lists with weighted RRF (pure function, no I/O) → hydrate labels/metadata for
top-k → return `Vec<(Note, f32)>` where the score is the fused weighted-RRF score, not raw BM25 or
cosine. Selector operators are evaluated by the shared core matcher over distinct stored values;
the accepted raw values are then applied as SQL predicates before list pagination, counts, and
matching-ID selection.
```

Remove the open decision “Whether label filtering should move deeper into backend retrieval at
larger corpus sizes,” because this implementation resolves it.

- [ ] **Step 2: Run formatting and focused crate suites**

Run:

```bash
cargo fmt --all -- --check
cargo test -p note-storage
cargo test -p note-storage-turso
cargo test -p note-storage-contract-tests
cargo test -p note-pipelines
cargo test -p note-mcp
cargo test -p note-server
```

Expected: formatting and every package command pass.

- [ ] **Step 3: Run live PostgreSQL verification**

Run:

```bash
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p note-storage-pg
```

Expected: the complete PostgreSQL suite passes. If the configured database is unavailable, report
the environmental failure rather than claiming PostgreSQL coverage.

- [ ] **Step 4: Run workspace finish gates**

Run:

```bash
cargo test --workspace
cargo check --workspace --all-targets
cargo fmt --all -- --check
git diff --check
git status --short --branch
```

Expected: all commands pass; status contains only the intended implementation and documentation
changes relative to the plan commit history. No frontend command is required because no frontend
file or shared frontend type changes.

- [ ] **Step 5: Commit documentation**

```bash
git add docs/design.md
git commit -m "docs(search): describe SQL label filtering"
```

- [ ] **Step 6: Review the final commit range**

Run:

```bash
git log --oneline --decorate -10
git diff origin/main...HEAD --stat
git diff origin/main...HEAD --check
```

Expected: the range contains the approved design commit plus focused pagination, resolver, Turso,
PostgreSQL, matching-ID, retrieval, pipeline, and documentation commits; the diff has no whitespace
errors.
