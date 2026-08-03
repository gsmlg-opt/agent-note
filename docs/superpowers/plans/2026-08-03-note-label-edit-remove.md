# Note Label Edit and Remove Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add accessible edit and remove controls for labels attached to a note in the shared web note editor.

**Architecture:** Keep label draft state in `NoteEditor` and add one optional editing index. Small pure helpers replace and remove indexed label pairs and reconcile the editing index; Yew callbacks use those helpers while the existing submit path continues sending the complete label vector.

**Tech Stack:** Rust 2021, Yew, yew-duskmoon, CSS, native Rust unit tests, Wasm compilation

---

## File Structure

- Modify `crates/note-frontend/src/components/note_editor.rs`: add tested label transitions, edit-mode state, callbacks, and accessible controls.
- Modify `crates/note-frontend/app.css`: keep label text and actions compact and wrapping.

No backend, API, routing, storage, or global label-definition files change.

### Task 1: Add test-driven label draft transitions

**Files:**
- Modify: `crates/note-frontend/src/components/note_editor.rs:651-699`
- Test: `crates/note-frontend/src/components/note_editor.rs:661-699`

- [ ] **Step 1: Write the failing transition tests**

Add these tests to the existing `tests` module:

```rust
#[test]
fn editing_a_draft_label_replaces_its_key_and_value_in_place() {
    let mut labels = vec![
        ("status".to_string(), "draft".to_string()),
        ("priority".to_string(), "1".to_string()),
    ];

    assert!(replace_label_at(
        &mut labels,
        0,
        "stage".to_string(),
        "review".to_string(),
    ));
    assert_eq!(
        labels,
        vec![
            ("stage".to_string(), "review".to_string()),
            ("priority".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn removing_a_draft_label_removes_only_the_target() {
    let mut labels = vec![
        ("status".to_string(), "draft".to_string()),
        ("priority".to_string(), "1".to_string()),
    ];

    assert!(remove_label_at(&mut labels, 0));
    assert_eq!(
        labels,
        vec![("priority".to_string(), "1".to_string())]
    );
}

#[test]
fn invalid_label_indices_leave_the_draft_unchanged() {
    let original = vec![("status".to_string(), "draft".to_string())];
    let mut labels = original.clone();

    assert!(!replace_label_at(
        &mut labels,
        2,
        "stage".to_string(),
        "review".to_string(),
    ));
    assert!(!remove_label_at(&mut labels, 2));
    assert_eq!(labels, original);
}

#[test]
fn removing_a_label_reconciles_the_current_edit_index() {
    assert_eq!(editing_index_after_removal(Some(2), 0), Some(1));
    assert_eq!(editing_index_after_removal(Some(1), 1), None);
    assert_eq!(editing_index_after_removal(Some(0), 2), Some(0));
    assert_eq!(editing_index_after_removal(None, 0), None);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml \
  components::note_editor::tests
```

Expected: compilation fails because `replace_label_at`, `remove_label_at`, and `editing_index_after_removal` do not exist.

- [ ] **Step 3: Implement the minimum pure transitions**

Add these helpers immediately before `label_value_input_type`:

```rust
fn replace_label_at(
    labels: &mut [(String, String)],
    index: usize,
    key: String,
    value: String,
) -> bool {
    let Some(label) = labels.get_mut(index) else {
        return false;
    };
    *label = (key, value);
    true
}

fn remove_label_at(labels: &mut Vec<(String, String)>, index: usize) -> bool {
    if index >= labels.len() {
        return false;
    }
    labels.remove(index);
    true
}

fn editing_index_after_removal(
    editing_index: Option<usize>,
    removed_index: usize,
) -> Option<usize> {
    match editing_index {
        Some(index) if index == removed_index => None,
        Some(index) if index > removed_index => Some(index - 1),
        current => current,
    }
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run the Step 2 command again. Expected: all `components::note_editor::tests` tests pass.

- [ ] **Step 5: Commit the tested transitions**

```sh
git add crates/note-frontend/src/components/note_editor.rs
git commit -m "fix(frontend): add label draft transitions"
```

### Task 2: Wire edit and remove controls into `NoteEditor`

**Files:**
- Modify: `crates/note-frontend/src/components/note_editor.rs:46-173`
- Modify: `crates/note-frontend/src/components/note_editor.rs:301-344`
- Modify: `crates/note-frontend/app.css:1189-1195`

- [ ] **Step 1: Add edit-mode state**

Add beside the existing picker state:

```rust
let picker_key = use_state(String::new);
let picker_value = use_state(String::new);
let editing_label_index = use_state(|| None::<usize>);
```

- [ ] **Step 2: Make the picker add or replace the staged label**

Replace `on_add_label` with:

```rust
let on_apply_label = {
    let labels = labels.clone();
    let picker_key = picker_key.clone();
    let picker_value = picker_value.clone();
    let editing_label_index = editing_label_index.clone();
    Callback::from(move |_| {
        let key = (*picker_key).clone();
        let value = (*picker_value).clone();
        if key.is_empty() {
            return;
        }

        let mut next = (*labels).clone();
        if let Some(index) = *editing_label_index {
            if !replace_label_at(&mut next, index, key, value) {
                return;
            }
            editing_label_index.set(None);
        } else {
            next.push((key, value));
        }
        labels.set(next);
        picker_value.set(String::new());
    })
};
```

Replace the picker button with:

```rust
<button type="button" class="btn btn-outline" onclick={on_apply_label}>
    {
        if (*editing_label_index).is_some() {
            "Update label"
        } else {
            "Add label"
        }
    }
</button>
```

- [ ] **Step 3: Add per-label Edit and Remove callbacks**

Render `labels.iter().enumerate()`. For each pair, build these callbacks before the `html!` block:

```rust
let on_edit = {
    let picker_key = picker_key.clone();
    let picker_value = picker_value.clone();
    let editing_label_index = editing_label_index.clone();
    let key = key.clone();
    let value = value.clone();
    Callback::from(move |_| {
        picker_key.set(key.clone());
        picker_value.set(value.clone());
        editing_label_index.set(Some(index));
    })
};
let on_remove = {
    let labels = labels.clone();
    let picker_key = picker_key.clone();
    let picker_value = picker_value.clone();
    let editing_label_index = editing_label_index.clone();
    Callback::from(move |_| {
        let current_edit = *editing_label_index;
        let mut next = (*labels).clone();
        if !remove_label_at(&mut next, index) {
            return;
        }
        labels.set(next);
        editing_label_index.set(editing_index_after_removal(current_edit, index));
        if current_edit == Some(index) {
            picker_key.set(String::new());
            picker_value.set(String::new());
        }
    })
};
```

Inside each existing `Chip`, render the label text and these native controls:

```rust
<span class="applied-label-content">
    <span>{ format!("{key}: {value}") }</span>
    <span class="applied-label-actions">
        <button
            type="button"
            class="applied-label-action"
            aria-label={format!("Edit label {key}")}
            onclick={on_edit}
        >
            { "Edit" }
        </button>
        <button
            type="button"
            class="applied-label-action"
            aria-label={format!("Remove label {key}")}
            onclick={on_remove}
        >
            { "Remove" }
        </button>
    </span>
</span>
```

- [ ] **Step 4: Style the compact label actions**

Add after the existing `.applied-labels` rule:

```css
.applied-label-content,
.applied-label-actions {
    display: inline-flex;
    align-items: center;
}

.applied-label-content {
    gap: 0.5rem;
}

.applied-label-actions {
    gap: 0.2rem;
}

.applied-label-action {
    border: 0;
    padding: 0.1rem 0.2rem;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
}

.applied-label-action:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 1px;
}
```

- [ ] **Step 5: Run focused tests and the Wasm check**

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml \
  components::note_editor::tests
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
```

Expected: focused tests pass and the frontend compiles for Wasm.

- [ ] **Step 6: Commit the controls**

```sh
git add crates/note-frontend/src/components/note_editor.rs \
  crates/note-frontend/app.css
git commit -m "fix(frontend): edit and remove note labels"
```

### Task 3: Run scoped final verification

**Files:**
- Verify: `crates/note-frontend/src/components/note_editor.rs`
- Verify: `crates/note-frontend/app.css`

- [ ] **Step 1: Run the complete frontend unit suite**

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: all frontend tests pass.

- [ ] **Step 2: Run the frontend Wasm check**

```sh
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
```

Expected: the check exits successfully.

- [ ] **Step 3: Verify formatting and whitespace**

```sh
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

Expected: both commands exit successfully with no diff errors.

- [ ] **Step 4: Inspect the final scope and repository state**

```sh
git status --short --branch
git diff HEAD~2 -- crates/note-frontend/src/components/note_editor.rs \
  crates/note-frontend/app.css
```

Expected: implementation changes are confined to the shared note editor and its label-action CSS; the branch is ahead only by the approved design, plan, and focused implementation commits.
