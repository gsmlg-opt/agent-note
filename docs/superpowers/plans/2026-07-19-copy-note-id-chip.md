# Copy Note ID Chip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the canonical note ID in a clickable chip beneath the note title and copy that ID to the browser clipboard with visible, accessible status feedback.

**Architecture:** Keep the behavior in `note_show.rs`: a small pure `CopyStatus` model supplies native-testable display and announcement text, while the Yew component owns clipboard I/O and state. Use a semantic native button with DuskMoon chip classes, and add only the browser bindings needed to detect Clipboard API availability safely.

**Tech Stack:** Rust 2021, Yew 0.23, `web-sys`, `js-sys`, `wasm-bindgen`, DuskMoon CSS, browser Clipboard API.

---

## File Structure

- `crates/note-frontend/src/pages/note_show.rs`: copy state, pure copy-status helpers, safe Clipboard API lookup, click handler, chip markup, and unit tests.
- `crates/note-frontend/Cargo.toml`: direct browser-binding dependencies and `web-sys` features.
- `crates/note-frontend/app.css`: chip-button layout, wrapping, and focus-compatible presentation.

No backend, route, API, storage, label, or vendored DuskMoon file changes.

### Task 1: Add the Test-Driven Copy Status Model

**Files:**

- Modify: `crates/note-frontend/src/pages/note_show.rs`
- Test: `crates/note-frontend/src/pages/note_show.rs`

- [ ] **Step 1: Write failing tests for initial, success, and failure text**

Extend the test imports and add these tests to the existing `#[cfg(test)]` module:

```rust
use super::{
    copy_announcement, copy_chip_text, rewrite_attachment_urls, CopyStatus,
};

#[test]
fn copy_chip_initially_offers_to_copy_the_canonical_id() {
    assert_eq!(
        copy_chip_text("note-123", CopyStatus::Ready),
        "ID: note-123 · Copy"
    );
    assert_eq!(copy_announcement(CopyStatus::Ready), "");
}

#[test]
fn copy_chip_reports_success() {
    assert_eq!(
        copy_chip_text("note-123", CopyStatus::Copied),
        "ID: note-123 · Copied"
    );
    assert_eq!(
        copy_announcement(CopyStatus::Copied),
        "Note ID copied to clipboard."
    );
}

#[test]
fn copy_chip_reports_failure() {
    assert_eq!(
        copy_chip_text("note-123", CopyStatus::Failed),
        "ID: note-123 · Copy failed"
    );
    assert_eq!(
        copy_announcement(CopyStatus::Failed),
        "Unable to copy note ID."
    );
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml copy_chip_
```

Expected: compilation fails because `CopyStatus`, `copy_chip_text`, and
`copy_announcement` do not exist.

- [ ] **Step 3: Add the minimal pure status implementation**

Add this code after `NoteShowProps`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CopyStatus {
    #[default]
    Ready,
    Copied,
    Failed,
}

impl CopyStatus {
    fn action_label(self) -> &'static str {
        match self {
            Self::Ready => "Copy",
            Self::Copied => "Copied",
            Self::Failed => "Copy failed",
        }
    }
}

fn copy_chip_text(id: &str, status: CopyStatus) -> String {
    format!("ID: {id} · {}", status.action_label())
}

fn copy_announcement(status: CopyStatus) -> &'static str {
    match status {
        CopyStatus::Ready => "",
        CopyStatus::Copied => "Note ID copied to clipboard.",
        CopyStatus::Failed => "Unable to copy note ID.",
    }
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml copy_chip_
```

Expected: all three `copy_chip_...` tests pass.

- [ ] **Step 5: Commit the status model**

```sh
git add crates/note-frontend/src/pages/note_show.rs
git commit -m "test(frontend): define copy note id status"
```

### Task 2: Connect the Chip to the Browser Clipboard

**Files:**

- Modify: `crates/note-frontend/Cargo.toml`
- Modify: `crates/note-frontend/src/pages/note_show.rs`
- Test: `crates/note-frontend/src/pages/note_show.rs`

- [ ] **Step 1: Add direct browser-binding dependencies**

Change the dependency block to include:

```toml
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4.76"
js-sys = "0.3"
web-sys = { version = "0.3", features = [
    "Clipboard",
    "HtmlElement",
    "HtmlInputElement",
    "HtmlSelectElement",
    "HtmlTextAreaElement",
    "Navigator",
    "Window",
] }
```

Keep every existing dependency unchanged. `wasm-bindgen-futures` already exists,
so move it next to the other browser bindings instead of duplicating it.

- [ ] **Step 2: Add safe clipboard discovery**

Add imports:

```rust
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
```

Add this helper after `copy_announcement`:

```rust
fn browser_clipboard() -> Option<web_sys::Clipboard> {
    let navigator = web_sys::window()?.navigator();
    let clipboard = js_sys::Reflect::get(
        navigator.as_ref(),
        &wasm_bindgen::JsValue::from_str("clipboard"),
    )
    .ok()?;

    if clipboard.is_null() || clipboard.is_undefined() {
        None
    } else {
        Some(clipboard.unchecked_into())
    }
}
```

This returns `None` on browsers or HTTP origins where `navigator.clipboard` is
unavailable, so the component can report failure without trapping.

- [ ] **Step 3: Add copy state and reset it when the route ID changes**

Initialize state beside `note`, `loading`, and `error`:

```rust
let copy_status = use_state(CopyStatus::default);
```

Clone it into the existing note-loading effect and reset it before the request:

```rust
let copy_status = copy_status.clone();
// ...
copy_status.set(CopyStatus::Ready);
```

This prevents a `Copied` or `Copy failed` result from leaking into another note
when Yew reuses the page component for a different route ID.

- [ ] **Step 4: Add the clipboard click handler and chip markup**

After the note-loading effect and before `html!`, derive the callback from the
loaded canonical ID. The callback is not rendered until a note exists:

```rust
let copy_id = (*note)
    .as_ref()
    .map(|loaded_note| loaded_note.id.clone())
    .unwrap_or_default();
let on_copy_id = {
    let id = copy_id;
    let copy_status = copy_status.clone();
    Callback::from(move |_| {
        let id = id.clone();
        let copy_status = copy_status.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let copied = match browser_clipboard() {
                Some(clipboard) => JsFuture::from(clipboard.write_text(&id)).await.is_ok(),
                None => false,
            };
            copy_status.set(if copied {
                CopyStatus::Copied
            } else {
                CopyStatus::Failed
            });
        });
    })
};
```

Insert this markup as the first `Card` child, before `.note-page-actions`:

```rust
<div class="note-id-copy">
    <button
        type="button"
        class="chip chip-clickable chip-primary note-id-copy-chip"
        aria-label={format!("Copy note ID {}", n.id)}
        onclick={on_copy_id}
    >
        <span class="note-id-copy-text">
            { copy_chip_text(&n.id, *copy_status) }
        </span>
    </button>
    <span class="sr-only" aria-live="polite" aria-atomic="true">
        { copy_announcement(*copy_status) }
    </span>
</div>
```

Keep the existing `Back`, `Edit note`, labels, Markdown, and attachments markup
unchanged.

- [ ] **Step 5: Run native tests**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: all frontend tests pass.

- [ ] **Step 6: Compile the browser path**

Run:

```sh
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
```

Expected: the frontend compiles successfully for Wasm.

- [ ] **Step 7: Commit the clipboard behavior**

```sh
git add crates/note-frontend/Cargo.toml crates/note-frontend/Cargo.lock \
  crates/note-frontend/src/pages/note_show.rs
git commit -m "feat(frontend): copy canonical note id"
```

### Task 3: Style and Verify the Chip Button

**Files:**

- Modify: `crates/note-frontend/app.css`
- Verify: `crates/note-frontend/src/pages/note_show.rs`

- [ ] **Step 1: Add the chip-button layout**

Add these rules immediately before `.note-page-actions`:

```css
.note-id-copy {
    display: flex;
    align-items: flex-start;
    max-width: 100%;
    margin-bottom: 0.9rem;
}

.note-id-copy-chip {
    max-width: 100%;
    min-width: 0;
    font-family: inherit;
    text-align: left;
    white-space: normal;
}

.note-id-copy-text {
    min-width: 0;
    overflow-wrap: anywhere;
}
```

The vendored `.chip`, `.chip-clickable`, `.chip-primary`, and `.chip:focus-visible`
rules supply the established DuskMoon visual and focus treatment.

- [ ] **Step 2: Run formatting and diff checks**

Run:

```sh
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

Expected: both commands exit successfully with no output.

- [ ] **Step 3: Build the Trunk frontend**

Run:

```sh
cd crates/note-frontend && trunk build
```

Expected: Trunk finishes successfully and writes the frontend distribution.

- [ ] **Step 4: Verify the rendered detail page**

Run the application, open a note detail route, and verify in the browser:

1. The chip is immediately beneath the card title and above `Back` / `Edit note`.
2. The visible value is the same canonical ID in the `/api/notes/{id}` request.
3. On a secure clipboard-capable origin, clicking changes `Copy` to `Copied`
   and the clipboard contains the complete ID.
4. If clipboard access is unavailable, clicking changes to `Copy failed` and
   the page remains usable.
5. At a viewport width of 375px, the ID wraps without horizontal overflow.
6. The chip has a visible keyboard focus indicator.

- [ ] **Step 5: Run the complete scoped verification**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

Expected: every command exits successfully.

- [ ] **Step 6: Commit the styling**

```sh
git add crates/note-frontend/app.css
git commit -m "style(frontend): present note id as copy chip"
```
