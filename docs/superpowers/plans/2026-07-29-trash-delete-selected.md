# Trash Delete Selected Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a confirmed, best-effort `Delete selected` action to the existing Trash multi-selection toolbar.

**Architecture:** Keep the change inside `TrashPage` and reuse the existing single-note permanent-delete API sequentially. Pure helpers create the visible ordered snapshot, reconcile selection after partial success, and format the partial-failure alert. A `SelectionState` reducer owns selection changes so async restore and delete completions dispatch against the latest state; the component adds a batch delete target and preserves the current single-delete flow.

**Tech Stack:** Rust 2021, Yew 0.23, `web-sys`, existing `Modal` and DuskMoon button styles.

---

## File Structure

- Modify `crates/note-frontend/src/pages/trash.rs`: add pure batch helpers and tests, extend delete modal state, wire the toolbar action, and run sequential API calls.
- No backend, API-client, route, dependency, or CSS files change.

### Task 1: Define and Test Batch Selection Outcomes

**Files:**
- Modify: `crates/note-frontend/src/pages/trash.rs`
- Test: `crates/note-frontend/src/pages/trash.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write failing tests for ordered snapshots and best-effort outcomes**

Append this test module to `trash.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{
        batch_delete_failure_message, reconcile_delete_selection, selected_note_ids,
    };
    use crate::state::DeletedNoteSummary;
    use std::collections::HashSet;

    fn note(id: &str) -> DeletedNoteSummary {
        DeletedNoteSummary {
            id: id.to_string(),
            title: format!("Note {id}"),
            labels: vec![],
            created_at: 0,
            updated_at: 0,
            deleted_at: 0,
        }
    }

    #[test]
    fn selected_note_ids_follow_visible_trash_order() {
        let notes = vec![note("third"), note("second"), note("first")];
        let selected = HashSet::from(["first".to_string(), "third".to_string()]);

        assert_eq!(
            selected_note_ids(&notes, &selected),
            vec!["third".to_string(), "first".to_string()]
        );
    }

    #[test]
    fn batch_delete_keeps_failed_and_unattempted_ids_selected() {
        let selected = HashSet::from([
            "deleted".to_string(),
            "failed".to_string(),
            "new-selection".to_string(),
        ]);
        let attempted = vec!["deleted".to_string(), "failed".to_string()];
        let failed = HashSet::from(["failed".to_string()]);

        assert_eq!(
            reconcile_delete_selection(&selected, &attempted, &failed),
            HashSet::from(["failed".to_string(), "new-selection".to_string()])
        );
    }

    #[test]
    fn batch_delete_reports_partial_failure_counts() {
        assert_eq!(batch_delete_failure_message(5, 0), None);
        assert_eq!(
            batch_delete_failure_message(5, 2),
            Some(
                "Deleted 3 of 5 selected notes; 2 failed. Failed notes still in Trash remain selected."
                    .to_string(),
            )
        );
    }
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::trash::tests
```

Expected: compilation fails because `selected_note_ids`,
`reconcile_delete_selection`, and `batch_delete_failure_message` do not exist.

- [ ] **Step 3: Implement the minimal pure helpers**

Add these functions above `TrashPage`:

```rust
fn selected_note_ids(
    notes: &[DeletedNoteSummary],
    selected: &HashSet<String>,
) -> Vec<String> {
    notes
        .iter()
        .filter(|note| selected.contains(&note.id))
        .map(|note| note.id.clone())
        .collect()
}

fn reconcile_delete_selection(
    selected: &HashSet<String>,
    attempted: &[String],
    failed: &HashSet<String>,
) -> HashSet<String> {
    let attempted = attempted
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    selected
        .iter()
        .filter(|id| !attempted.contains(id.as_str()) || failed.contains(*id))
        .cloned()
        .collect()
}

fn batch_delete_failure_message(total: usize, failed: usize) -> Option<String> {
    (failed > 0).then(|| {
        format!(
            "Deleted {} of {total} selected notes; {failed} failed. \
             Failed notes still in Trash remain selected.",
            total.saturating_sub(failed)
        )
    })
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml pages::trash::tests
```

Expected: 3 Trash tests pass.

- [ ] **Step 5: Commit the tested helpers**

```sh
git add crates/note-frontend/src/pages/trash.rs
git commit -m "test(frontend): define Trash bulk delete outcomes"
```

### Task 2: Add the Batch Delete Target and Confirmation Flow

**Files:**
- Modify: `crates/note-frontend/src/pages/trash.rs`
- Test: `crates/note-frontend/src/pages/trash.rs`

- [ ] **Step 1: Write failing tests for delete-target copy and IDs**

Extend the test module:

```rust
use super::DeleteTarget;

#[test]
fn batch_delete_target_has_irreversible_count_copy() {
    let target = DeleteTarget::Batch {
        ids: vec!["one".to_string(), "two".to_string()],
    };

    assert_eq!(target.ids(), vec!["one".to_string(), "two".to_string()]);
    assert_eq!(
        target.modal_copy(),
        (
            "Delete selected notes permanently".to_string(),
            "Permanently delete 2 selected notes? This cannot be undone.".to_string(),
            "Delete 2 notes".to_string(),
        )
    );
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml \
  pages::trash::tests::batch_delete_target_has_irreversible_count_copy
```

Expected: compilation fails because `DeleteTarget` and its methods do not
exist.

- [ ] **Step 3: Replace the tuple target with a typed delete target**

Add beside `RestoreTarget`:

```rust
#[derive(Clone, PartialEq)]
enum DeleteTarget {
    Single { id: String, title: String },
    Batch { ids: Vec<String> },
}

impl DeleteTarget {
    fn ids(&self) -> Vec<String> {
        match self {
            Self::Single { id, .. } => vec![id.clone()],
            Self::Batch { ids } => ids.clone(),
        }
    }

    fn modal_copy(&self) -> (String, String, String) {
        match self {
            Self::Single { title, .. } => (
                "Delete note permanently".to_string(),
                format!(
                    "Permanently delete the note \u{201c}{title}\u{201d}? \
                     This cannot be undone."
                ),
                "Delete permanently".to_string(),
            ),
            Self::Batch { ids } => (
                "Delete selected notes permanently".to_string(),
                format!(
                    "Permanently delete {} selected notes? This cannot be undone.",
                    ids.len()
                ),
                format!("Delete {} notes", ids.len()),
            ),
        }
    }

    fn is_batch(&self) -> bool {
        matches!(self, Self::Batch { .. })
    }
}
```

Change the state declaration to:

```rust
let delete_target = use_state(|| None::<DeleteTarget>);
let batch_delete_error = use_state(|| None::<String>);
```

Change each row delete action to clear stale errors and construct:

```rust
// Insert these parameters after `delete_target` in `trash_table`.
error: &UseStateHandle<Option<String>>,
batch_delete_error: &UseStateHandle<Option<String>>,

let on_delete = {
    let delete_target = delete_target.clone();
    let error = error.clone();
    let batch_delete_error = batch_delete_error.clone();
    let id = note.id.clone();
    let title = note.title.clone();
    Callback::from(move |_| {
        error.set(None);
        batch_delete_error.set(None);
        delete_target.set(Some(DeleteTarget::Single {
            id: id.clone(),
            title: title.clone(),
        }))
    })
};
```

Pass `&error` and `&batch_delete_error` at the existing `trash_table(...)`
callsite in the same order as the new parameters.

- [ ] **Step 4: Add the toolbar callback and destructive button**

Create the callback beside `open_batch_restore`:

```rust
let open_batch_delete = {
    let notes = notes.clone();
    let selected = selected.clone();
    let delete_target = delete_target.clone();
    let error = error.clone();
    let batch_delete_error = batch_delete_error.clone();
    Callback::from(move |_| {
        let ids = selected_note_ids(&notes, &selected);
        if !ids.is_empty() {
            error.set(None);
            batch_delete_error.set(None);
            delete_target.set(Some(DeleteTarget::Batch { ids }));
        }
    })
};
```

Wrap the bulk actions in a `row-actions` div and add:

```rust
<button
    type="button"
    class="btn btn-error"
    disabled={selected.is_empty() || *restoring || *deleting}
    onclick={open_batch_delete}
>
    { icons::trash() }
    <span>{ "Delete selected" }</span>
</button>
```

Also change the Restore selected button to:

```rust
disabled={selected.is_empty() || *restoring || *deleting}
```

- [ ] **Step 5: Implement single and batch confirmation branches**

Build the modal from `DeleteTarget::modal_copy()`. Guard close and cancel with
`deleting`. On confirmation, snapshot `target.ids()` and `target.is_batch()`.
Use this complete callback so single failures retain the modal while batch
failures continue through every selected ID:

```rust
let ids = target.ids();
let is_batch = target.is_batch();
let (modal_title, message, confirm_label) = target.modal_copy();
let on_close = {
    let delete_target = delete_target.clone();
    let deleting = deleting.clone();
    Callback::from(move |_: ()| {
        if !*deleting {
            delete_target.set(None);
        }
    })
};
let on_cancel = {
    let delete_target = delete_target.clone();
    let deleting = deleting.clone();
    Callback::from(move |_| {
        if !*deleting {
            delete_target.set(None);
        }
    })
};
let on_confirm = {
    let delete_target = delete_target.clone();
    let deleting = deleting.clone();
    let selected = selected.clone();
    let error = error.clone();
    let batch_delete_error = batch_delete_error.clone();
    let refresh_tick = refresh_tick.clone();
    Callback::from(move |_| {
        if *deleting {
            return;
        }
        deleting.set(true);
        error.set(None);
        batch_delete_error.set(None);

        let delete_target = delete_target.clone();
        let deleting = deleting.clone();
        let selected = selected.clone();
        let error = error.clone();
        let batch_delete_error = batch_delete_error.clone();
        let refresh_tick = refresh_tick.clone();
        let ids = ids.clone();

        wasm_bindgen_futures::spawn_local(async move {
            if is_batch {
                let mut failed = HashSet::new();
                for id in &ids {
                    if api::permanently_delete_note(id).await.is_err() {
                        failed.insert(id.clone());
                    }
                }

                selected.set(reconcile_delete_selection(
                    &*selected,
                    &ids,
                    &failed,
                ));
                batch_delete_error
                    .set(batch_delete_failure_message(ids.len(), failed.len()));
                delete_target.set(None);
                refresh_tick.set((*refresh_tick).saturating_add(1));
            } else {
                let id = ids
                    .first()
                    .expect("single delete target has one id");
                match api::permanently_delete_note(id).await {
                    Ok(()) => {
                        delete_target.set(None);
                        refresh_tick.set((*refresh_tick).saturating_add(1));
                    }
                    Err(message) => error.set(Some(message)),
                }
            }
            deleting.set(false);
        });
    })
};
```

Render `batch_delete_error` through the existing alert location in addition to
the fetch/single-delete `error`. Manual Refresh and the start of either delete
flow clear the stale batch outcome.

Update Refresh to clear the previous batch result:

```rust
let on_refresh = {
    let refresh_tick = refresh_tick.clone();
    let batch_delete_error = batch_delete_error.clone();
    Callback::from(move |_| {
        batch_delete_error.set(None);
        refresh_tick.set((*refresh_tick).saturating_add(1));
    })
};
```

Render both possible error sources in the existing location:

```rust
if let Some(message) = &*batch_delete_error {
    <Alert variant={Some("error".to_string())}>
        <span>{ message.clone() }</span>
    </Alert>
}
if let Some(message) = &*error {
    <Alert variant={Some("error".to_string())}>
        <span>{ message.clone() }</span>
    </Alert>
}
```

Use the target-specific modal title, message, and confirm label:

```rust
<Modal title={modal_title} on_close={on_close}>
    <p>{ message }</p>
    <div class="app-modal-actions">
        <button type="button" class="btn btn-ghost" onclick={on_cancel} disabled={*deleting}>
            { "Cancel" }
        </button>
        <button type="button" class="btn btn-error" onclick={on_confirm} disabled={*deleting}>
            { if *deleting { "Deleting...".to_string() } else { confirm_label } }
        </button>
    </div>
</Modal>
```

- [ ] **Step 6: Run the frontend suite and verify GREEN**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: all frontend tests pass, including the new Trash tests.

- [ ] **Step 7: Commit the Trash interaction**

```sh
git add crates/note-frontend/src/pages/trash.rs
git commit -m "feat(frontend): delete selected Trash notes"
```

### Task 3: Apply Post-Implementation Quality Fixes

**Files:**
- Modify: `crates/note-frontend/src/pages/trash.rs`
- Test: `crates/note-frontend/src/pages/trash.rs`

- [ ] **Step 1: Write the failing latest-selection reducer regression**

Add a focused test that starts with the successful and failed attempted IDs,
then applies a later row-selection action before applying the batch result:

```rust
let state = Rc::new(SelectionState(HashSet::from([
    "deleted".to_string(),
    "failed".to_string(),
])));
let state = state.reduce(SelectionAction::Toggle {
    id: "new-selection".to_string(),
    selected: true,
});
let state = state.reduce(SelectionAction::ReconcileDelete {
    attempted: vec!["deleted".to_string(), "failed".to_string()],
    failed: HashSet::from(["failed".to_string()]),
});

assert_eq!(
    **state,
    HashSet::from(["failed".to_string(), "new-selection".to_string()])
);
```

Run the focused test and verify RED because `SelectionState` and
`SelectionAction` do not exist yet.

- [ ] **Step 2: Move every selection mutation to reducer actions**

Replace `use_state(HashSet<String>)` with `use_reducer(SelectionState::default)`
and implement actions for:

- `Replace` for select-all and clear;
- `RetainVisible` after a successful refresh;
- `Toggle` for a row checkbox;
- `Remove` after restore succeeds;
- `ReconcileDelete` with attempted and failed IDs after batch deletion.

The batch completion must dispatch `ReconcileDelete` without dereferencing a
selection handle captured by the confirmation render. Reducer dispatch then
uses the latest selection, preserving `new-selection` while removing
`deleted` and keeping `failed`.

Keep selection checkboxes enabled during restore and delete requests. Pass the
current `deleting` and `restoring` values into `trash_table`, disable both
per-row mutation buttons when either is true, and repeat that check at the
start of both row mutation callbacks.

- [ ] **Step 3: Verify the reducer fix and commit it**

Run the focused reducer test, the frontend suite, the Wasm check, formatting,
and `git diff --check`. Then commit:

```sh
git add crates/note-frontend/src/pages/trash.rs
git commit -m "fix(frontend): reconcile latest Trash selection"
```

- [ ] **Step 4: Write the failing singular-summary regression**

Add this assertion while keeping the existing `(5, 2)` expectation unchanged:

```rust
assert_eq!(
    batch_delete_failure_message(1, 1),
    Some(
        "Deleted 0 of 1 selected note; 1 failed. \
         Failed note still in Trash remains selected."
            .to_string()
    )
);
```

Run the focused test and verify RED against the old plural `notes` and `remain`
copy.

- [ ] **Step 5: Pluralize the summary, verify GREEN, and commit**

Choose the total noun from `total`, and the failed noun plus `remains`/`remain`
verb from `failed`. Do not change the modal title
`Delete selected notes permanently`. Run the focused test, full frontend suite,
Wasm check, formatting, and `git diff --check`, then commit:

```sh
git add crates/note-frontend/src/pages/trash.rs
git commit -m "fix(frontend): pluralize Trash delete summary"
```

### Task 4: Verify the Trash Feature

**Files:**
- Verify: `crates/note-frontend/src/pages/trash.rs`

- [ ] **Step 1: Check native frontend tests**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: exit 0 with no failed tests.

- [ ] **Step 2: Check the Wasm target**

Run:

```sh
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
```

Expected: exit 0.

- [ ] **Step 3: Check formatting and patch hygiene**

Run:

```sh
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
git status --short --branch
```

Expected: formatting and diff checks exit 0; status contains no uncommitted
files.
