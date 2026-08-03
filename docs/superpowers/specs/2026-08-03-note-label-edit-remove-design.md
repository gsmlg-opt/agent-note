# Note Label Edit and Remove Design

Status: Approved

## Goal

Let users edit or remove labels attached to a note while creating or editing
that note in the web frontend. Editing includes changing both the label key and
its value.

## Scope

The change is limited to the shared frontend `NoteEditor` and the small amount
of CSS needed for its applied-label controls. Both New Note and Edit Note use
this component, so they receive identical behavior.

The existing create and update requests already submit the editor's complete
label list. No route, API, storage, global label-definition, or backend change
is required. Adding a new label keeps its current behavior; duplicate-key
validation is outside this bug fix.

## Interaction

Applied labels remain visible beneath the existing key/value picker. Each
applied label gains two visible actions:

- **Edit** loads that label's key and value into the existing picker. The
  picker button changes from `Add label` to `Update label`.
- **Remove** immediately removes that label from the editor's draft. The note
  is not persisted until the user submits the note form.

Submitting `Update label` replaces the edited label at the same position,
allows both key and value to change, clears the staged value, exits edit mode,
and restores the `Add label` action. Editing another label moves edit mode to
that label. Removing the label currently being edited also clears edit mode.

As today, an empty key does not add or update a label. Saving the note continues
to send the complete current label list through the existing submit callback.

## Component and State Design

`NoteEditor` keeps the existing draft `labels`, `picker_key`, and
`picker_value` state and adds an optional editing index.

- Add mode appends the staged key/value pair as it does now.
- Edit mode replaces the pair at the selected index.
- Remove deletes only the selected index.

The transitions for replacing and removing labels will be expressed as small
pure helpers. This keeps the Yew callbacks thin and makes the regression
behavior testable by the frontend's existing native unit-test setup without
adding a browser test framework.

## Layout and Accessibility

The current compact applied-label presentation is retained. Edit and Remove
use native `type="button"` controls so neither action can submit the note form.
Each control receives an accessible label containing the attached label key,
for example `Edit label status` and `Remove label status`.

The applied-label container continues to wrap on narrow screens. CSS changes
are scoped to grouping the label text and actions without changing the rest of
the note editor.

## Error Handling

Edit and Remove mutate only local draft state and cannot make a network request.
An out-of-range edit or remove index is treated as a no-op by the pure state
helper. Existing API error handling remains responsible for save failures.

## Testing and Verification

Use test-driven development in the existing `note_editor.rs` test module. Add
focused regression tests that first fail because the label state transitions do
not exist, then implement the minimum helpers and UI wiring needed to pass.

Cover these behaviors:

- editing a draft label replaces its key and value in place;
- removing a draft label removes only the targeted pair;
- an invalid index leaves the draft unchanged.

Run the scoped frontend verification:

```sh
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
git diff --check
```

## Acceptance Criteria

1. New Note and Edit Note show Edit and Remove actions for every applied label.
2. Edit can change both the selected label's key and value.
3. Update replaces the selected draft label rather than appending another pair.
4. Remove deletes only the selected draft label.
5. Removing the label being edited exits edit mode.
6. Label actions never submit the note form themselves.
7. Saving submits the editor's updated complete label list through the existing
   create or update flow.
8. Focused frontend tests, Wasm compilation, formatting, and diff checks pass.
