# Note View Soft Delete and Navigation Design

## Goal

Allow a user viewing a note to move that note to Trash without returning to the
notes list first. Preserve the originating Notes list state while the user views
and edits a note.

## Scope

The change is limited to frontend Notes, Show, and Edit navigation plus the
single-note delete interaction. It reuses the existing soft-delete API and modal
component. Backend routes, storage behavior, the notes-list delete flow, and
Trash management are unchanged.

## Notes Return Context

The Notes page already stores its list state in four URL query parameters:
`current`, `page_size`, `search`, and `labels`. These parameters form a typed
Notes return context.

- Title, View, Edit, and search-result links created by the Notes page carry the
  normalized context to the destination route.
- Show carries the same context to Edit.
- Show's Back action and successful soft delete return to Notes with the same
  context.
- Edit's Back action and successful save return to Show with the same context,
  so Show can still return to the originating Notes list.
- Direct Show/Edit URLs, Dashboard links, and application navigation without a
  valid Notes context retain their existing queryless behavior.
- Typed query serialization is used instead of raw string concatenation so
  encoded label selectors and search text remain intact.

## Interaction

- Add a destructive `Delete note` button to the single-note action row beside
  the existing Back and Edit actions.
- Clicking the button opens the existing application modal.
- The modal identifies the note by title and explains that deletion moves it to
  Trash, where it can be restored.
- Cancel, Escape, or a backdrop click closes the modal without making a request.
- Confirming calls the existing `api::delete_note` function once.
- A successful request navigates to the originating Notes list state when it is
  available, otherwise to the default Notes list.
- A failed request leaves the user on the note page, closes no content, and
  displays the existing page-level error alert.

## Implementation

`NoteShowPage` owns the confirmation modal's open state and callbacks. The page
uses the shared `Modal` component and the existing danger-button styling so the
interaction matches deletion from the notes list. No shared delete abstraction
is introduced because only the note-view behavior is changing.

The Notes query type is shared by the three pages. `NotesPage` constructs it
from the existing normalized URL state; `NoteShowPage` and `NoteEditPage` read
it as optional route context and forward it when building links or navigating
after a successful request. Edit renders a page-local `Back to note` action
without widening the shared note editor API.

## Testing

Add focused frontend tests for the confirmation copy and typed Notes query
conversion. Retain the existing note-view Markdown and Notes query parsing
tests. Verify with the frontend test suite, native frontend compilation, the
Wasm target check, and formatting.

## Success Criteria

- The note-view page presents a clear delete action.
- The action requires explicit confirmation.
- Confirmation soft-deletes through `DELETE /api/notes/{id}`.
- Success returns to the originating Notes list and the note is available in
  Trash.
- Cancellation sends no delete request.
- Failure remains visible to the user without navigation.
- Opening Show or Edit from Notes preserves pagination, page size, search, and
  label filters.
- Show's Back action restores the originating Notes list state.
- Edit provides a Back action to Show, and saving also returns to Show.
- Returning from Edit to Show retains the Notes return context.
