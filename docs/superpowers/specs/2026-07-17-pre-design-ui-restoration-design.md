# Design: Restore the Pre-`DESIGN.md` Web UI

Status: Approved (pending written-spec review)

## 1. Purpose

Restore the Web UI's visual style from immediately before root `DESIGN.md` was
introduced, while preserving all current product behavior and the current
automatic light/dark theme selection.

The historical reference is commit
`ee93dc30b4a4651f50b383102f2e32ebbee00e05`, the parent of
`a8f712fd6a6c038dd40d5328b5662dc19b15086d`. Commit `a8f712f` added root
`DESIGN.md`, a ZDNS token adapter, and the sidebar/topbar shell. Commit
`c32b7d40a298486651f94d3a13298a528d9a4af9` later removed root `DESIGN.md`
and the token adapter, but retained the redesigned shell and much of its page
styling.

Lowercase `docs/design.md` is an unrelated architecture document and remains
unchanged.

## 2. Approved Direction

Use a selective historical merge rather than checking out the old frontend or
approximating the old look on the current shell.

- Restore the old horizontal, sticky application header.
- Restore the centered `72rem` content column.
- Restore the old typography, density, spacing, borders, radii, surfaces,
  tables, forms, cards, editor, modal appearance, and responsive visual
  treatment across every route.
- Preserve the current automatic DuskMoon `sunshine`/`moonlight` selection.
- Preserve all features introduced after `a8f712f`.
- Preserve nonvisual usability and accessibility improvements introduced with
  or after the redesign.

This returns the product to one coherent visual era without rolling back
behavior.

## 3. Application Shell

`src/main.rs` will return to the direct composition used by the historical
reference:

1. `BrowserRouter`
2. shared `AppBar`
3. `<main class="app">`
4. route switch

The ZDNS-era `AppShell`, desktop sidebar, secondary topbar, breadcrumb, and
duplicate System shortcut will be removed.

`src/components/app_bar.rs` will render a full-width semantic header with a
centered inner row. The brand remains on the left and route links remain on the
right. The navigation will contain all current destinations:

- Home
- Notes
- New note
- Labels
- Trash
- System

The current route-matching logic and active classes remain. Notes stays active
for list, show, and edit routes. On narrow screens the row remains compact and
the navigation can scroll horizontally; the System text may be visually hidden
while its icon retains an accessible name.

## 4. Visual System

The historical `ee93dc3` values and composition are authoritative for selectors
that existed at that commit. In particular:

- System UI typography with the historical sizes and weights
- Warm, flat, border-led DuskMoon surfaces
- A `60px` sticky horizontal application bar
- A centered `72rem` page column with the old vertical rhythm
- Pill navigation and compact controls
- One-pixel outline-variant borders
- The historical per-selector radii, which are predominantly `0.5rem` and must
  not be normalized into a new radius scale
- Shadows reserved primarily for modals, popovers, and DuskMoon cards
- Historical dashboard tiles and two-column panels
- Historical table typography, headers, cell density, and icon actions
- Historical form, label-list, System-page, Markdown editor, attachment, search,
  loading, and modal treatments

The current vendored `duskmoon-core.css` remains the component and token source.
It must not be replaced with the historical vendored file. `index.html` remains
without a `data-theme` attribute so DuskMoon continues to choose `sunshine` or
`moonlight` from the operating-system preference.

Current-only UI surfaces must be styled in the same historical visual grammar:

- Trash tables, selection controls, restore actions, and delete confirmations
- Dashboard embedding status and progress
- Backup download controls
- Markdown rendering additions
- Text and binary attachment controls

The result must look like an extension of the old UI, not a mixture of the old
header and ZDNS-era page interiors.

## 5. Behavior Preservation

The restoration is visual. It must not remove or regress:

- URL-backed note search, filters, pagination, and refresh
- Markdown rendering, code, diagrams, color chips, and attachment URL rewriting
- Text and binary attachment create, replace, remove, preview, and download
- Trash navigation, selection, restore, and permanent deletion
- Dashboard embedding polling and progress
- Duplicate-note System settings and full-backup download
- Current API request and response handling

The following redesign-era safeguards remain even where the old implementation
lacked them:

- Active-route indication
- Modal dialog semantics, autofocus, Escape dismissal, and backdrop-only
  dismissal
- Responsive horizontal table scrolling
- Reduced-motion handling
- Accessible labels for icon-only controls

The notes table wrapper may remain for narrow-screen scrolling, but its border
and table styling must visually match the historical table.

## 6. Expected File Scope

Primary implementation files:

- `crates/note-frontend/src/main.rs`
- `crates/note-frontend/src/components/app_bar.rs`
- `crates/note-frontend/app.css`

Other frontend files may change only if verification identifies a markup hook
required to reproduce the approved historical style without removing current
behavior. Do not modify backend crates, `crates/note-frontend/duskmoon-core.css`,
lowercase `docs/design.md`, or restore root `DESIGN.md`.

## 7. Responsive Design

The historical responsive behavior remains the baseline:

- Compact, horizontally scrollable navigation near `620px`
- Stacked dashboard panels and rows near `520px`
- Stacked search and filter controls near `640px`
- Single-column attachment layouts near `900px`
- Wrapped pagination near `520px`

Keep any current breakpoint needed by current-only features when it prevents
overflow or loss of function. Breakpoint retention must not reintroduce the
desktop sidebar or ZDNS page hierarchy.

## 8. Verification

Automated frontend gates:

```sh
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
cd crates/note-frontend && trunk build
```

Browser verification must cover:

- Every route in desktop light mode
- Every route in desktop dark mode
- Navigation and representative pages at or below `620px`
- Dashboard stacking around `520px`
- Search and editor layouts around `640px` and `900px`
- Active navigation for list, show, edit, Trash, and System
- Modal autofocus, Escape, backdrop dismissal, and visible focus
- Responsive table scrolling without clipped actions
- Representative Markdown, attachment, Trash, dashboard progress, System
  settings, and backup interactions

## 9. Acceptance Criteria

The work is complete when:

1. The sidebar, breadcrumb, and secondary topbar are absent.
2. The shell and every page use the pre-`DESIGN.md` visual language.
3. Light mode matches the historical DuskMoon `sunshine` appearance.
4. Dark mode remains automatic and visually coherent.
5. All current routes and product capabilities remain available.
6. Accessibility and responsive safeguards listed above remain.
7. The scoped automated and browser verification passes.

## 10. Out of Scope

- Reintroducing root `DESIGN.md` or `design-system.css`
- Replacing or regenerating vendored DuskMoon CSS
- Changing backend behavior or API contracts
- Adding a theme switcher or new visual system
- Refactoring unrelated frontend state or page logic
