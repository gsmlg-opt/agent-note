# Note View Soft Delete and Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a confirmed soft-delete action to Show while preserving the originating Notes query through Show, Edit, return actions, saves, and deletion.

**Architecture:** Promote the existing Notes query payload to a shared typed route-query struct. Notes attaches the normalized query to note links; Show and Edit read it as optional context, forward it through links, and use it for successful navigation. Show owns the delete modal and calls the existing soft-delete API.

**Tech Stack:** Rust 2021, Yew 0.23, yew-router 0.20, serde, existing frontend API and modal components.

---

## File Structure

- Modify `crates/note-frontend/src/routes.rs`: own the shared serializable/deserializable Notes query type.
- Modify `crates/note-frontend/src/pages/notes.rs`: construct and attach the typed query to title, View, Edit, and search-result links.
- Modify `crates/note-frontend/src/pages/note_show.rs`: read/forward query context and add the confirmed soft-delete flow.
- Modify `crates/note-frontend/src/pages/note_edit.rs`: add Back to note and preserve query context after save.
- No CSS, shared editor, API, backend, or storage changes.

### Task 1: Shared Notes return-query contract

**Files:**
- Modify: `crates/note-frontend/src/routes.rs`
- Modify: `crates/note-frontend/src/pages/notes.rs`
- Test: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Write a failing test against the shared route-query type**

Import `NotesQueryParams` from `crate::routes` in the Notes tests and assert the existing URL state converts without dropping fields:

```rust
#[test]
fn notes_route_query_preserves_list_context() {
    let state = NotesUrlState {
        current: 3,
        page_size: 30,
        search: "release notes".to_string(),
        labels: vec![LabelFilter {
            key: "status".to_string(),
            operator: "=".to_string(),
            value: "draft".to_string(),
        }],
    };

    assert_eq!(
        notes_query_params(&state),
        NotesQueryParams {
            current: 3,
            page_size: 30,
            search: Some("release notes".to_string()),
            labels: Some("status=draft".to_string()),
        }
    );
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run `cd crates/note-frontend && cargo test notes_route_query_preserves_list_context`.

Expected: compilation fails because `crate::routes::NotesQueryParams` does not exist.

- [ ] **Step 3: Move the query payload to the route module**

Add to `routes.rs`:

```rust
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct NotesQueryParams {
    pub current: usize,
    pub page_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
}
```

Remove the private `NotesQueryParams` from `notes.rs` and import the shared type with `use crate::routes::{NotesQueryParams, Route};`.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run `cd crates/note-frontend && cargo test notes_route_query_preserves_list_context`.

Expected: the matching test passes.

### Task 2: Preserve query context from the Notes page

**Files:**
- Modify: `crates/note-frontend/src/pages/notes.rs`

- [ ] **Step 1: Construct one normalized query for the rendered list**

After `url_state` is parsed, add:

```rust
let notes_query = notes_query_params(&url_state);
```

Pass `&notes_query` into `list_view`, `note_table`, and `search_results_view`, extending their signatures with `notes_query: &NotesQueryParams`.

- [ ] **Step 2: Attach the typed query to every Notes-origin note link**

Use the explicit query generic for the table title, View, Edit, and search-result title links:

```rust
<Link<Route, NotesQueryParams>
    to={Route::NoteShow { id: id.clone() }}
    query={Some(notes_query.clone())}
    classes={classes!("note-title-link")}
>
```

Use `Route::NoteEdit` for the Edit action and `result-title-link` for results while keeping the same `query={Some(notes_query.clone())}` contract.

- [ ] **Step 3: Compile and run Notes tests**

Run `cd crates/note-frontend && cargo test pages::notes::tests && cargo check`.

Expected: all Notes tests pass and native frontend compilation succeeds.

### Task 3: Show-page query navigation and confirmed soft delete

**Files:**
- Modify: `crates/note-frontend/src/pages/note_show.rs`
- Test: `crates/note-frontend/src/pages/note_show.rs`

- [ ] **Step 1: Write the failing confirmation-copy test**

Add `delete_confirmation_message` to the test imports and add:

```rust
#[test]
fn delete_confirmation_explains_that_the_note_can_be_restored() {
    assert_eq!(
        delete_confirmation_message("Release notes"),
        "Move the note \u{201c}Release notes\u{201d} to Trash? You can restore it later."
    );
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run `cd crates/note-frontend && cargo test delete_confirmation_explains_that_the_note_can_be_restored`.

Expected: compilation fails because the helper does not exist.

- [ ] **Step 3: Add the confirmation helper and page state**

Add:

```rust
fn delete_confirmation_message(title: &str) -> String {
    format!("Move the note \u{201c}{title}\u{201d} to Trash? You can restore it later.")
}
```

Import `Modal` and `NotesQueryParams`, then initialize hooks at the top of `NoteShowPage`:

```rust
let navigator = use_navigator().expect("router navigator");
let notes_query = use_location()
    .and_then(|location| location.query::<NotesQueryParams>().ok());
let delete_open = use_state(|| false);
```

Reset `delete_open` to false in the existing ID-change effect.

- [ ] **Step 4: Preserve context in Show actions**

Replace the Back and Edit links with typed links:

```rust
<Link<Route, NotesQueryParams>
    to={Route::Notes}
    query={notes_query.clone()}
    classes={classes!("btn", "btn-ghost")}
>
    { "Back" }
</Link<Route, NotesQueryParams>>
```

Use the same query for `Route::NoteEdit { id: n.id.clone() }`.

- [ ] **Step 5: Add the delete action and conditional modal**

Add a `Delete note` button with `btn btn-error` that sets `delete_open` true. Build the existing `Modal` when a note is loaded and deletion is open. Cancel and `on_close` set it false. Confirm calls `api::delete_note(&id)`; on error set the existing page error and keep the modal open. On success close and navigate with:

```rust
if let Some(query) = notes_query {
    let _ = navigator.push_with_query(&Route::Notes, query);
} else {
    navigator.push(&Route::Notes);
}
```

Use title `Move note to Trash`, body `delete_confirmation_message(&note.title)`, a ghost Cancel button, and an error-styled `Move to Trash` confirm button.

- [ ] **Step 6: Run the focused test and Show compilation**

Run `cd crates/note-frontend && cargo test delete_confirmation_explains_that_the_note_can_be_restored && cargo check`.

Expected: the focused test passes and Show compiles.

### Task 4: Edit-page return navigation

**Files:**
- Modify: `crates/note-frontend/src/pages/note_edit.rs`

- [ ] **Step 1: Read optional Notes context once**

Import `NotesQueryParams` and add:

```rust
let notes_query = use_location()
    .and_then(|location| location.query::<NotesQueryParams>().ok());
```

- [ ] **Step 2: Preserve context after Save**

Capture `notes_query` in `on_submit`. Replace the success branch with:

```rust
Ok(()) => {
    let route = Route::NoteShow { id };
    if let Some(query) = notes_query {
        let _ = navigator.push_with_query(&route, query);
    } else {
        navigator.push(&route);
    }
}
```

- [ ] **Step 3: Add Back to note above the shared editor**

Before `NoteEditor`, render a page-local action so the shared editor API stays unchanged:

```rust
<div class="note-page-actions">
    <Link<Route, NotesQueryParams>
        to={Route::NoteShow { id: n.id.clone() }}
        query={notes_query.clone()}
        classes={classes!("btn", "btn-ghost")}
    >
        { "Back to note" }
    </Link<Route, NotesQueryParams>>
</div>
```

- [ ] **Step 4: Run native frontend tests and compilation**

Run `cd crates/note-frontend && cargo test && cargo check`.

Expected: all frontend unit tests pass and native compilation succeeds.

### Task 5: Final scoped verification and commit

**Files:**
- Verify all files listed above and this plan.

- [ ] **Step 1: Format and run all scoped checks**

Run:

```bash
cargo fmt --all -- --check
cd crates/note-frontend
cargo test
cargo check
cargo check --target wasm32-unknown-unknown
```

Expected: formatting succeeds; all frontend tests pass; native and Wasm checks exit without errors.

- [ ] **Step 2: Inspect scope and diff quality**

Run:

```bash
git diff --check
git diff -- crates/note-frontend/src/routes.rs crates/note-frontend/src/pages/notes.rs crates/note-frontend/src/pages/note_show.rs crates/note-frontend/src/pages/note_edit.rs docs/superpowers/plans/2026-07-22-note-view-soft-delete.md
git status --short --branch
```

Expected: only the plan and four scoped frontend Rust files are uncommitted.

- [ ] **Step 3: Commit the implementation**

Run:

```bash
git add crates/note-frontend/src/routes.rs crates/note-frontend/src/pages/notes.rs crates/note-frontend/src/pages/note_show.rs crates/note-frontend/src/pages/note_edit.rs docs/superpowers/plans/2026-07-22-note-view-soft-delete.md
git commit -m "feat(frontend): preserve note navigation context"
```

Expected: one focused Conventional Commit records the implementation and plan.
