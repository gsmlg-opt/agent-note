# Trash Delete Selected Design

## Goal

Allow users to permanently delete multiple selected notes from Trash with one
confirmed action.

## Scope

The change is limited to the Trash frontend page. It reuses the existing
`DELETE /api/trash/{id}` endpoint for each selected note. Backend routes,
pipeline and storage deletion semantics, Trash retention, restore behavior, and
single-note request, copy, and result behavior remain unchanged.

## Interaction

- Add a destructive `Delete selected` button beside `Restore selected` in the
  existing Trash selection toolbar.
- The button is disabled when no notes are selected or while a restore or
  permanent-delete operation is running.
- Clicking it snapshots the selected notes in their visible list order and
  opens the existing application modal.
- The modal title is `Delete selected notes permanently`. Its message includes
  the selected count and states that the action cannot be undone.
- Cancel, Escape, or a backdrop click closes the modal without sending a
  request.
- The confirm button is destructive, includes the selected count, and changes
  to `Deleting...` while requests are in progress. The modal cannot be
  dismissed during deletion.

## Best-Effort Deletion

After confirmation, the frontend calls the existing single-note permanent
delete API sequentially for every snapshotted ID. One failure does not stop
later deletions.

When all requests finish:

- successfully deleted attempted IDs are removed from the latest selection;
- failed IDs that are still present after refresh remain selected so the user
  can retry them;
- selections added while the sequential requests are running survive because
  they were not part of the attempted snapshot;
- Trash is refreshed once;
- the modal closes;
- if every request succeeds, no error alert is shown;
- if any request fails, the existing page alert reports the variable counts in
  copy such as `Deleted 3 of 5 selected notes; 2 failed.` and explains that
  failed notes remain selected.

Sequential requests keep the implementation small and avoid creating a burst
of irreversible requests. A new bulk API is intentionally out of scope because
the selected best-effort behavior can use the existing endpoint without
changing transport contracts.

## Component Design

`TrashPage` extends its delete-target state so it can represent either the
existing single-note target or a batch target containing an ordered snapshot of
selected note IDs. The existing delete modal handles both target types:

- single targets retain the current title-specific copy and one-request
  behavior;
- batch targets use count-based copy and collect per-ID success or failure
  outcomes.

Small pure helpers select visible IDs and summarize batch outcomes. They keep
selection reconciliation and failure copy deterministic and independently
testable. Selection uses a reducer whose actions replace or clear the
selection, retain visible IDs after refresh, toggle one row, remove restored
IDs, or reconcile a batch-delete result. `ReconcileDelete` always reduces
against the latest selection, so an async completion cannot overwrite row
selections made after confirmation. A separate batch-outcome error state
renders through the existing page-alert location so the refresh does not clear
a partial-failure summary. No shared deletion abstraction or CSS changes are
required.

## Error and Concurrency Handling

- The existing `deleting` state prevents duplicate confirmation.
- Restore and bulk-delete buttons cannot start overlapping batch operations.
- Per-row Restore and Delete controls are disabled while either a restore or
  delete request is running, and their callbacks repeat the same guard.
- Selection checkboxes intentionally remain enabled during requests; reducer
  reconciliation preserves selections that were not in the attempted batch.
- The modal ignores close and cancel attempts while deletion is running.
- Network and HTTP failures count only against the affected note; remaining
  requests continue.
- A post-operation refresh failure uses the existing page-level error path and
  does not reselect successfully deleted IDs. The batch result and refresh
  failure can be combined in the same alert location.

## Testing and Verification

Use test-driven development for pure Trash-page behavior:

- visible selected notes produce an ordered batch snapshot;
- successful IDs are removed while failed IDs remain selected;
- reducer reconciliation keeps a row selected after it is toggled on during
  the async delete loop, while removing the successful attempted ID and
  retaining the failed attempted ID;
- partial failure copy reports success, total, and failed counts;
- a one-note failure reports `Deleted 0 of 1 selected note; 1 failed. Failed
  note still in Trash remains selected.`;
- all-success outcomes produce no error copy;
- existing timestamp formatting and single-note behavior remain intact.

Verify with:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

## Acceptance Criteria

1. Trash shows `Delete selected` beside `Restore selected`.
2. The action is unavailable without a selection.
3. The action requires one explicit irreversible-action confirmation.
4. Every selected note receives at most one permanent-delete request per
   confirmation.
5. A failed request does not prevent later selected notes from being attempted.
6. Successfully deleted notes disappear after the final refresh.
7. Failed notes that remain in Trash stay selected and the page reports the
   partial result.
8. Selections made while the sequential deletion is running survive when they
   were not part of the attempted snapshot.
9. Existing single-note delete request/copy/result behavior, restore,
   select-all, and retention behavior are unchanged.
