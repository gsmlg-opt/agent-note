# Note Show Copy Content Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Copy Content chip beside Copy ID that writes only the note's raw Markdown body to the clipboard.

**Architecture:** Extend `NoteShowPage` with an independent content-copy status and callback while reusing the existing browser Clipboard lookup. Keep the two chip-label and announcement helpers separate so ID and content state cannot overwrite each other, then turn the existing copy container into a wrapping two-action row.

**Tech Stack:** Rust 2021, Yew 0.23, `web-sys::Clipboard`, `wasm-bindgen-futures`, existing DuskMoon chip styles.

---

## File Structure

- Modify `crates/note-frontend/src/pages/note_show.rs`: add content-copy status helpers/tests, state, callback, markup, and independent announcements.
- Modify `crates/note-frontend/app.css`: convert the copy container into a wrapping row and retain long-ID overflow protection.
- No backend, API-client, route, or dependency files change.

### Task 1: Define Content Copy Labels and Raw Payload

**Files:**
- Modify: `crates/note-frontend/src/pages/note_show.rs`
- Test: `crates/note-frontend/src/pages/note_show.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write failing tests for content labels and announcements**

Update the test import and add these tests:

```rust
use super::{
    content_copy_announcement, content_copy_chip_text, content_copy_payload,
    copy_announcement, copy_chip_text, delete_confirmation_message,
    rewrite_attachment_urls, try_start_delete, CopyStatus,
};

#[test]
fn content_copy_chip_starts_ready_and_preserves_raw_markdown() {
    let markdown = "# Heading\n\n[attachment](./report.pdf)\n";

    assert_eq!(
        content_copy_chip_text(CopyStatus::Ready),
        "Content · Copy"
    );
    assert_eq!(content_copy_payload(markdown), markdown);
    assert_eq!(content_copy_announcement(CopyStatus::Ready), "");
}

#[test]
fn content_copy_chip_reports_success_independently_from_copy_id() {
    assert_eq!(
        copy_chip_text("note-123", CopyStatus::Ready),
        "ID: note-123 · Copy"
    );
    assert_eq!(
        content_copy_chip_text(CopyStatus::Copied),
        "Content · Copied"
    );
    assert_eq!(
        content_copy_announcement(CopyStatus::Copied),
        "Note content copied to clipboard."
    );
}

#[test]
fn content_copy_chip_reports_failure() {
    assert_eq!(
        content_copy_chip_text(CopyStatus::Failed),
        "Content · Copy failed"
    );
    assert_eq!(
        content_copy_announcement(CopyStatus::Failed),
        "Unable to copy note content."
    );
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml \
  pages::note_show::tests::content_copy
```

Expected: compilation fails because `content_copy_chip_text`,
`content_copy_payload`, and `content_copy_announcement` do not exist.

- [ ] **Step 3: Add the minimal helpers**

Add beside the existing Copy ID helpers:

```rust
fn content_copy_chip_text(status: CopyStatus) -> String {
    format!("Content · {}", status.action_label())
}

fn content_copy_announcement(status: CopyStatus) -> &'static str {
    match status {
        CopyStatus::Ready => "",
        CopyStatus::Copied => "Note content copied to clipboard.",
        CopyStatus::Failed => "Unable to copy note content.",
    }
}

fn content_copy_payload(content: &str) -> &str {
    content
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml \
  pages::note_show::tests::content_copy
```

Expected: 3 content-copy tests pass.

- [ ] **Step 5: Commit the tested content-copy contract**

```sh
git add crates/note-frontend/src/pages/note_show.rs
git commit -m "test(frontend): define note content copy states"
```

### Task 2: Wire the Content Clipboard Action

**Files:**
- Modify: `crates/note-frontend/src/pages/note_show.rs`
- Modify: `crates/note-frontend/app.css`
- Test: `crates/note-frontend/src/pages/note_show.rs`

- [ ] **Step 1: Add and reset independent content-copy state**

Add the hook beside `copy_status`:

```rust
let content_copy_status = use_state(CopyStatus::default);
```

Clone it into the note-loading effect and reset it when `props.id` changes:

```rust
let content_copy_status = content_copy_status.clone();
// ...
content_copy_status.set(CopyStatus::Ready);
```

The existing ID `copy_status` remains separate and unchanged.

- [ ] **Step 2: Add the raw-body copy callback**

Build the source from the loaded note and add this callback beside
`on_copy_id`:

```rust
let content_to_copy = (*note)
    .as_ref()
    .map(|loaded_note| content_copy_payload(&loaded_note.content).to_string())
    .unwrap_or_default();
let on_copy_content = {
    let content = content_to_copy;
    let content_copy_status = content_copy_status.clone();
    Callback::from(move |_| {
        let content = content.clone();
        let content_copy_status = content_copy_status.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let copied = match browser_clipboard() {
                Some(clipboard) => {
                    JsFuture::from(clipboard.write_text(&content)).await.is_ok()
                }
                None => false,
            };
            content_copy_status.set(if copied {
                CopyStatus::Copied
            } else {
                CopyStatus::Failed
            });
        });
    })
};
```

This captures `NoteSummary.content` before Markdown rendering and attachment URL
rewriting, so the clipboard receives the exact API body.

- [ ] **Step 3: Render the second semantic chip and live result**

Replace the current `note-id-copy` wrapper with:

```rust
<div class="note-copy-actions">
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

    <button
        type="button"
        class="chip chip-clickable chip-primary"
        aria-label="Copy note Markdown content"
        onclick={on_copy_content}
    >
        { content_copy_chip_text(*content_copy_status) }
    </button>
    <span class="sr-only" aria-live="polite" aria-atomic="true">
        { content_copy_announcement(*content_copy_status) }
    </span>
</div>
```

Keep Copy ID first so existing visual order and canonical-ID text remain
unchanged.

- [ ] **Step 4: Make the copy row wrap**

Replace `.note-id-copy` in `crates/note-frontend/app.css` with:

```css
.note-copy-actions {
    display: flex;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: 0.5rem;
    max-width: 100%;
    margin-bottom: 0.9rem;
}
```

Keep `.note-id-copy-chip` and `.note-id-copy-text` unchanged so long note IDs
continue to wrap safely.

- [ ] **Step 5: Run the frontend suite and verify GREEN**

Run:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
```

Expected: all frontend tests pass, including existing Copy ID tests and the new
Copy Content tests.

- [ ] **Step 6: Commit the Copy Content UI**

```sh
git add crates/note-frontend/src/pages/note_show.rs crates/note-frontend/app.css
git commit -m "feat(frontend): copy raw note content"
```

### Task 3: Verify the Copy Content Feature

**Files:**
- Verify: `crates/note-frontend/src/pages/note_show.rs`
- Verify: `crates/note-frontend/app.css`

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
