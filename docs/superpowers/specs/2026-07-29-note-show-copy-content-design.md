# Note Show Copy Content Design

## Goal

Let users copy a note's raw Markdown body from the read-only Note Show page
without selecting rendered text manually.

## Scope

The change is limited to the frontend Note Show page and its copy-action
layout. It reuses the browser Clipboard API already used by Copy ID. Backend
routes, note data, Markdown rendering, attachments, and navigation are
unchanged.

## Clipboard Content

Copy Content writes exactly the loaded note's `content` string to the
clipboard. It does not include the title, note ID, labels, attachment metadata,
rendered HTML, or any rewritten attachment URLs. Whitespace and Markdown syntax
are preserved exactly as returned by the note API, including an empty body if
one is ever loaded.

## Interaction

- Keep the existing Copy ID chip and its text unchanged.
- Add a second chip beside it with initial text `Content · Copy`.
- Clicking the new chip starts one clipboard write of the raw Markdown body.
- A successful write changes its text to `Content · Copied`.
- A failed or unavailable clipboard changes its text to
  `Content · Copy failed`.
- Starting another Copy Content attempt replaces the previous content-copy
  result.
- Copy ID and Copy Content have independent status, so using one does not reset
  or overwrite the other's visible or announced result.
- Loading a different note resets both copy actions to their ready states.

## Layout and Accessibility

The current single-chip container becomes a copy-actions row containing both
semantic buttons. The chips appear directly beneath the note title, with Copy
ID first and Copy Content second.

The row uses flex layout with wrapping so both actions remain usable on narrow
screens. The existing ID chip may still wrap its long canonical ID without
causing horizontal overflow.

Copy Content has an accessible label that identifies the action. Its result is
announced through a polite, atomic live region:

- success: `Note content copied to clipboard.`
- failure: `Unable to copy note content.`

## Component Design

`NoteShowPage` adds a second `CopyStatus` state for content. The existing status
labels and browser clipboard lookup are reused through small helpers that
accept the copied value and announcement subject. The ID and content callbacks
each capture their own source string and update only their own status.

No generalized clipboard component is introduced because both actions are
local to the same page and share only a small amount of behavior. CSS changes
are limited to making the existing copy container a wrapping row with a
consistent gap.

## Error Handling

Clipboard access can fail because permission is denied, the page is not in a
secure context, or the browser does not expose the API. Failure changes only
the Content chip and its live announcement. The note remains visible and no
navigation, API request, or note mutation occurs.

## Testing and Verification

Use test-driven development for the content-copy labels and announcements:

- ready state displays `Content · Copy`;
- success displays `Content · Copied` and announces that note content was
  copied;
- failure displays `Content · Copy failed` and announces the failure;
- content-copy status changes do not alter ID-copy labels;
- the callback source is the raw body string rather than rendered Markdown.

Verify both planned frontend changes with:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

## Acceptance Criteria

1. Note Show displays a Copy Content chip beside Copy ID.
2. Copy Content writes only the exact raw Markdown body.
3. The chip reports ready, success, and failure states.
4. Copy ID and Copy Content statuses remain independent.
5. Both actions expose accessible labels and result announcements.
6. The copy-action row wraps without horizontal overflow.
7. Existing Copy ID, rendering, navigation, edit, and delete behavior is
   unchanged.
