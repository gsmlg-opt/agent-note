# Click a Note Label to Filter

## Goal

Let a user click a label in the Notes table to immediately filter the list by that exact label key and value.

## Scope

This is a frontend-only change to the Notes page. It reuses the existing URL-backed label-filter flow and does not change backend APIs, storage, or label-selector semantics.

## Interaction

- Each label chip in the Notes table remains available for hover details and becomes an accessible click target.
- Clicking a chip adds an equality filter for its exact `key=value` pair and immediately reloads the Notes page results.
- Clicking an already-active exact filter is idempotent: it does not add a duplicate or reload with a different selector.
- If another equality filter exists for the same label key, the clicked value replaces it. This avoids creating a conjunction that cannot match because a note has at most one value for a label key.
- Filters for other keys and non-equality filters for the same key remain unchanged.
- The current retrieval text and page size remain unchanged. Pagination resets to page 1 so matching results are visible.

## Data Flow

The Notes page creates a quick-filter callback and passes it through the list/table rendering helpers to each label chip. The callback produces the next filter list with a small pure helper, updates local filter state, and replaces the Notes URL using the existing `NotesUrlState` and `replace_notes_url` path. The existing URL effect performs the filtered list or retrieval request.

## Presentation and Accessibility

The chip should communicate that it is clickable with pointer styling and an action-oriented accessible label or title. The existing hover popover content should remain available. No vendored DuskMoon CSS is modified.

## Testing

Focused frontend unit tests cover the pure filter-update behavior:

- a new clicked label is appended as an equality filter;
- an exact active filter is unchanged;
- a conflicting equality filter for the same key is replaced;
- filters for other keys and non-equality filters are preserved.

Verification runs the frontend test suite, the frontend Wasm check, targeted formatting, and `git diff --check`.
