# Note View Soft Delete Design

## Goal

Allow a user viewing a note to move that note to Trash without returning to the
notes list first.

## Scope

The change is limited to the frontend single-note page. It reuses the existing
soft-delete API and modal component. Backend routes, storage behavior, the notes
list delete flow, and Trash management are unchanged.

## Interaction

- Add a destructive `Delete note` button to the single-note action row beside
  the existing Back and Edit actions.
- Clicking the button opens the existing application modal.
- The modal identifies the note by title and explains that deletion moves it to
  Trash, where it can be restored.
- Cancel, Escape, or a backdrop click closes the modal without making a request.
- Confirming calls the existing `api::delete_note` function once.
- A successful request navigates to the Notes list.
- A failed request leaves the user on the note page, closes no content, and
  displays the existing page-level error alert.

## Implementation

`NoteShowPage` owns the confirmation modal's open state and callbacks. The page
uses the shared `Modal` component and the existing danger-button styling so the
interaction matches deletion from the notes list. No shared delete abstraction
is introduced because only the note-view behavior is changing.

## Testing

Add focused frontend tests for the pure text/state decisions introduced by the
page and retain the existing note-view Markdown tests. Verify with the frontend
test suite, native frontend compilation, the Wasm target check, and formatting.

## Success Criteria

- The note-view page presents a clear delete action.
- The action requires explicit confirmation.
- Confirmation soft-deletes through `DELETE /api/notes/{id}`.
- Success returns to the Notes list and the note is available in Trash.
- Cancellation sends no delete request.
- Failure remains visible to the user without navigation.
