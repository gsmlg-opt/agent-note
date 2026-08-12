# Click a Note Label to Filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a Notes-table label click immediately apply its exact key/value as an equality filter.

**Architecture:** Keep the URL as the Notes page source of truth. A pure helper computes an idempotent next filter list, replacing equality filters for the clicked single-valued key while preserving every other predicate; a callback passes that list through the existing URL replacement and loading effect.

**Tech Stack:** Rust 2021, Yew, yew-router, yew-duskmoon, CSS

---

## File Structure

- Modify `crates/note-frontend/src/pages/notes.rs` for the tested filter transition and click wiring.
- Modify `crates/note-frontend/app.css` for the pointer affordance.

### Task 1: Define the Quick-Filter Transition

**Files:**
- Modify and test: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Write failing unit tests**

Add tests proving that `quick_label_filters` appends a new equality filter, returns an exact active filter unchanged, replaces all conflicting equality filters for the same key, and preserves other-key and same-key non-equality predicates.

```rust
#[test]
fn quick_label_filter_appends_a_new_equality_filter() {
    assert_eq!(
        quick_label_filters(&[], "status", "draft"),
        vec![LabelFilter {
            key: "status".to_string(),
            operator: "=".to_string(),
            value: "draft".to_string(),
        }]
    );
}

#[test]
fn quick_label_filter_is_idempotent_for_an_exact_filter() {
    let filters = vec![LabelFilter {
        key: "status".to_string(),
        operator: "=".to_string(),
        value: "draft".to_string(),
    }];
    assert_eq!(quick_label_filters(&filters, "status", "draft"), filters);
}

#[test]
fn quick_label_filter_replaces_equalities_and_preserves_other_predicates() {
    let filters = vec![
        LabelFilter { key: "status".into(), operator: "=".into(), value: "draft".into() },
        LabelFilter { key: "status".into(), operator: "!=".into(), value: "archived".into() },
        LabelFilter { key: "project".into(), operator: "=".into(), value: "agent-note".into() },
        LabelFilter { key: "status".into(), operator: "=".into(), value: "review".into() },
    ];
    assert_eq!(
        quick_label_filters(&filters, "status", "published"),
        vec![
            LabelFilter { key: "status".into(), operator: "=".into(), value: "published".into() },
            filters[1].clone(),
            filters[2].clone(),
        ]
    );
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run `cargo test --manifest-path crates/note-frontend/Cargo.toml quick_label_filter`.

Expected: compilation fails because `quick_label_filters` does not exist.

- [ ] **Step 3: Implement the minimal pure helper**

```rust
fn quick_label_filters(filters: &[LabelFilter], key: &str, value: &str) -> Vec<LabelFilter> {
    let clicked = LabelFilter {
        key: key.to_string(),
        operator: "=".to_string(),
        value: value.to_string(),
    };
    if filters.contains(&clicked) {
        return filters.to_vec();
    }

    let mut inserted = false;
    let mut next = Vec::with_capacity(filters.len() + 1);
    for filter in filters {
        if filter.key == key && filter.operator == "=" {
            if !inserted {
                next.push(clicked.clone());
                inserted = true;
            }
        } else {
            next.push(filter.clone());
        }
    }
    if !inserted {
        next.push(clicked);
    }
    next
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run `cargo test --manifest-path crates/note-frontend/Cargo.toml quick_label_filter`.

Expected: all three quick-label-filter tests pass.

### Task 2: Wire Table Labels to the URL-Backed Filter Flow

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Add the Notes-page callback**

Create this callback beside `on_add_filter`:

```rust
let on_quick_add_filter = {
    let label_filters = label_filters.clone();
    let query = query.clone();
    let page_size = page_size.clone();
    let replace_notes_url = replace_notes_url.clone();
    Callback::from(move |(key, value): (String, String)| {
        let filters = quick_label_filters(&label_filters, &key, &value);
        if filters == *label_filters {
            return;
        }
        label_filters.set(filters.clone());
        replace_notes_url.emit(NotesUrlState {
            current: 1,
            page_size: *page_size,
            search: (*query).trim().to_string(),
            labels: filters,
        });
    })
};
```

- [ ] **Step 2: Pass the callback to every table label chip**

Add a `Callback<(String, String)>` parameter to `list_view`, `note_table`, and `label_chip`. Pass it from the non-search Notes view and clone it for each label. The dependency Popover cannot accept a consumer action handler or action-specific ARIA, so track `duskmoon-dev/yew-duskmoon-ui#10` and render equivalent local hover/focus markup around a native, action-labeled button:

```rust
let title = format!("Filter by {label}");
let on_click = {
    let key = key.to_string();
    let value = value.to_string();
    Callback::from(move |_: MouseEvent| {
        on_quick_add_filter.emit((key.clone(), value.clone()));
    })
};

html! {
    <div class="popover popover-primary popover-bottom popover-hover note-label-popover note-label-filter-action">
        <button
            type="button"
            class="chip chip-primary note-label-chip"
            aria-label={title.clone()}
            title={title}
            onclick={on_click}
        >
            { label }
        </button>
        <div class="popover-content" role="tooltip">
            <dl class="note-label-popover-body">
                <div class="note-label-popover-row">
                    <dt>{ "name" }</dt>
                    <dd>{ key.to_string() }</dd>
                </div>
                <div class="note-label-popover-row">
                    <dt>{ "value" }</dt>
                    <dd>{ value.to_string() }</dd>
                </div>
            </dl>
            <span class="popover-arrow" aria-hidden="true"></span>
        </div>
    </div>
}
```

- [ ] **Step 3: Show pointer affordance without modifying vendored CSS**

Add an app-owned `.note-label-filter-action` rule that restores the dependency component's visible, relative inline-block trigger container styles, then change `cursor: default;` to `cursor: pointer;` in the existing `.note-label-chip` rule. Do not modify vendored DuskMoon CSS.

- [ ] **Step 4: Format and verify the scoped change**

Run:

```bash
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
git diff --check
```

Expected: formatting, all frontend tests, the Wasm check, and whitespace validation pass.

- [ ] **Step 5: Commit the implementation**

```bash
git add crates/note-frontend/src/pages/notes.rs crates/note-frontend/app.css docs/superpowers/plans/2026-08-13-click-label-to-filter.md
git commit -m "feat(frontend): filter notes by clicked labels"
```
