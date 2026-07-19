# Design: Copy Note ID Chip

Status: Approved

## Purpose

Make the canonical note ID visible and easy to copy from the read-only note
detail page. The copied value is the note's internal `NoteSummary.id`, which is
used by `GET /api/notes/{id}`. Label values such as `bear_id` are not involved.

## Interaction

Directly beneath the note title, render one semantic button styled as a
DuskMoon chip. The chip displays:

```text
ID: <canonical note ID> · Copy
```

Clicking anywhere on the chip writes the full canonical ID to the browser
clipboard. While copying, the chip remains usable and visible. After the
operation completes:

- success changes `Copy` to `Copied`;
- failure changes `Copy` to `Copy failed`.

Starting another copy attempt replaces the previous result. The chip exposes
its purpose through an accessible label, and its status text is announced
through an `aria-live` region.

## Layout

The chip appears as the first content beneath the DuskMoon card title and before
the existing `Back` and `Edit note` action row. Its text may wrap so the full ID
remains available on narrow screens; it must not cause horizontal overflow.
Existing labels, Markdown content, attachments, and page actions retain their
current order and behavior.

## Implementation Boundaries

The change is limited to the frontend:

- `crates/note-frontend/src/pages/note_show.rs` owns copy state, clipboard
  interaction, and markup.
- `crates/note-frontend/app.css` provides the chip-button layout and responsive
  wrapping.
- `crates/note-frontend/Cargo.toml` enables the minimal `web-sys` features
  required for the browser Clipboard API.

No API, route, storage, label, or backend behavior changes.

## Error Handling

Clipboard access can fail when the browser denies permission or the page lacks
the required secure context. The page remains functional, the note ID stays
visible, and the chip reports `Copy failed`. A failed copy does not navigate,
reload, or modify note data.

## Testing and Verification

Use test-driven development for the status-label behavior before connecting the
browser clipboard operation. Verify:

- the initial chip text includes the canonical note ID and `Copy`;
- a successful copy result produces `Copied`;
- a failed copy result produces `Copy failed`;
- native frontend tests pass;
- the frontend compiles for `wasm32-unknown-unknown`;
- formatting passes;
- the built page shows the chip beneath the title without overflow.

Scoped commands:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
```

## Acceptance Criteria

1. The detail page shows a chip button immediately below the title.
2. The chip visibly contains the canonical note ID used by get-by-ID.
3. Clicking the chip copies that complete ID.
4. Success and failure states are visible and accessible.
5. The chip wraps without horizontal overflow on narrow screens.
6. Existing detail-page behavior is unchanged.
