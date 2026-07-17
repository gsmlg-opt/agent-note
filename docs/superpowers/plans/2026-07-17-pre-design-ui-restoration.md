# Pre-`DESIGN.md` UI Restoration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the complete pre-`DESIGN.md` Web UI while retaining current routes, features, accessibility safeguards, responsive table behavior, and automatic DuskMoon light/dark themes.

**Architecture:** Selectively three-way merge `app.css` with current `HEAD` as ours, ZDNS commit `a8f712f` as the merge base, and pre-design commit `ee93dc3` as theirs. Resolve visual overlaps toward the historical side, then explicitly reapply current-only accessibility and responsive safeguards. Restore the historical horizontal shell in Yew while retaining current route activation and Trash navigation.

**Tech Stack:** Rust 2021, Yew 0.23, yew-router 0.20, yew-duskmoon 0.8, CSS, Trunk, Chromium/Chrome DevTools.

**Reference:** `docs/superpowers/specs/2026-07-17-pre-design-ui-restoration-design.md`

---

## File Map

- Modify `crates/note-frontend/src/main.rs`: remove the ZDNS shell and compose the router, shared header, main content, and route switch directly.
- Modify `crates/note-frontend/src/components/app_bar.rs`: render the historical horizontal header while retaining current active-route logic, Trash, and accessible System navigation.
- Modify `crates/note-frontend/app.css`: restore historical shared styling, retain current-only feature selectors, and reconcile accessibility/responsive safeguards.
- Do not modify `crates/note-frontend/duskmoon-core.css`, `crates/note-frontend/index.html`, backend crates, lowercase `docs/design.md`, or restore root `DESIGN.md`.

There is no existing frontend DOM test harness. The red/green behavior test for
this visual change is therefore an automated browser DOM assertion run against
the current shell before editing and rerun after editing. Existing native tests,
Wasm compilation, and Trunk bundling remain regression gates.

---

### Task 1: Establish the red baseline

**Files:**
- Verify only; no file changes

- [ ] **Step 1: Confirm the repository and history baseline**

```sh
git status --short --branch
test "$(git rev-parse a8f712f^)" = "$(git rev-parse ee93dc3)"
test "$(git rev-parse HEAD:crates/note-frontend/app.css)" \
  = "$(git hash-object crates/note-frontend/app.css)"
```

Expected: `main` is ahead only by the approved design/plan documentation;
`a8f712f^` equals `ee93dc3`; `app.css` has no uncommitted change.

- [ ] **Step 2: Provision the documented frontend builder**

```sh
if ! command -v trunk >/dev/null 2>&1; then
  cargo install --locked trunk
fi
trunk --version
test -d "$(rustc --print sysroot)/lib/rustlib/wasm32-unknown-unknown"
```

Expected: Trunk prints a version and the Wasm standard library exists.

- [ ] **Step 3: Run the pre-change automated baseline**

```sh
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
(
  cd crates/note-frontend
  trunk build
)
```

Expected: formatting passes, 12 frontend tests pass, the Wasm check succeeds,
and Trunk produces `crates/note-frontend/dist`.

- [ ] **Step 4: Start an isolated baseline runtime**

Run this in a persistent terminal session and leave it running through Task 3:

```sh
VERIFY_DIR="$(mktemp -d /tmp/agent-note-ui-verify.XXXXXX)"
env \
  -u NOTE_MODEL_PATH \
  -u NOTE_STATIC_DIR \
  -u NOTE_EMBEDDING_MODE \
  NOTE_DB_PATH="$VERIFY_DIR/notes.db" \
  NOTE_ATTACHMENTS_DIR="$VERIFY_DIR/attachments" \
  cargo run -p note-server
```

Expected log lines:

```text
embedder: stub
note-server listening on http://127.0.0.1:6222
frontend dev server listening on http://0.0.0.0:6221
```

- [ ] **Step 5: Run the browser shell assertion and verify RED**

Open `http://127.0.0.1:6221/` in Chromium and evaluate:

```js
() => {
  const result = {
    oldShellAbsent: !document.querySelector(
      ".app-shell, .app-topbar, .breadcrumb"
    ),
    horizontalHeaderPresent: !!document.querySelector("header.app-bar"),
    centeredMainPresent: !!document.querySelector("main.app")
  };
  if (
    !result.oldShellAbsent ||
    !result.horizontalHeaderPresent ||
    !result.centeredMainPresent
  ) {
    throw new Error(`pre-design shell missing: ${JSON.stringify(result)}`);
  }
  return result;
}
```

Expected: the assertion throws. Current `main` still contains `.app-shell`,
`.app-topbar`, and `.breadcrumb`, and uses `aside.app-bar`.

---

### Task 2: Restore the shell and full historical visual system atomically

**Files:**
- Modify: `crates/note-frontend/src/main.rs`
- Modify: `crates/note-frontend/src/components/app_bar.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Replace `src/main.rs` with the direct historical shell**

```rust
mod api;
mod components;
mod pages;
mod routes;
mod state;

use yew::prelude::*;
use yew_router::prelude::*;

use components::AppBar;
use routes::{switch, Route};

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <AppBar />
            <main class="app">
                <Switch<Route> render={switch} />
            </main>
        </BrowserRouter>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
```

- [ ] **Step 2: Replace `src/components/app_bar.rs` with the selective historical header**

```rust
use yew::prelude::*;
use yew_router::prelude::*;

use crate::components::icons;
use crate::routes::Route;

/// Primary application navigation in a full-width header with a centered inner row.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    let route = use_route::<Route>().unwrap_or(Route::NotFound);
    let home_active = route == Route::Home;
    let notes_active = matches!(
        &route,
        Route::Notes | Route::NoteShow { .. } | Route::NoteEdit { .. }
    );
    let new_note_active = route == Route::NewNote;
    let labels_active = route == Route::Labels;
    let trash_active = route == Route::Trash;
    let system_active = route == Route::System;

    html! {
        <header class="app-bar">
            <div class="app-bar-inner">
                <Link<Route> to={Route::Home} classes={classes!("app-brand")}>
                    { "agent-note" }
                </Link<Route>>
                <nav class="app-nav-group" aria-label="Primary navigation">
                    <div class="app-nav">
                        <Link<Route> to={Route::Home} classes={nav_classes(home_active)}>{ "Home" }</Link<Route>>
                        <Link<Route> to={Route::Notes} classes={nav_classes(notes_active)}>{ "Notes" }</Link<Route>>
                        <Link<Route> to={Route::NewNote} classes={nav_classes(new_note_active)}>{ "New note" }</Link<Route>>
                        <Link<Route> to={Route::Labels} classes={nav_classes(labels_active)}>{ "Labels" }</Link<Route>>
                        <Link<Route> to={Route::Trash} classes={nav_classes(trash_active)}>{ "Trash" }</Link<Route>>
                    </div>
                    <Link<Route>
                        to={Route::System}
                        classes={nav_classes_with(system_active, "nav-link-system")}
                    >
                        { icons::settings() }
                        <span>{ "System" }</span>
                    </Link<Route>>
                </nav>
            </div>
        </header>
    }
}

fn nav_classes(active: bool) -> Classes {
    nav_classes_with(active, "")
}

fn nav_classes_with(active: bool, extra: &'static str) -> Classes {
    let mut classes = classes!("nav-link");
    if active {
        classes.push("is-active");
    }
    if !extra.is_empty() {
        classes.push(extra);
    }
    classes
}
```

- [ ] **Step 3: Materialize the deterministic three-way CSS merge**

Run from the repository root:

```sh
css="crates/note-frontend/app.css"
ours="$(git hash-object "$css")"
base="$(git rev-parse a8f712f:"$css")"
historical="$(git rev-parse ee93dc3:"$css")"
merged="$(git merge-file --object-id --theirs "$ours" "$base" "$historical")"
test "$(git cat-file -t "$merged")" = "blob"

git update-index --cacheinfo 100644 "$merged" "$css"
git checkout-index -f -- "$css"
git reset HEAD -- "$css"
```

This takes historical declarations for every visual overlap while retaining
nonconflicting additions made after `a8f712f`, including embedding, Trash, and
binary-attachment selectors. `--theirs` also removes the sidebar/topbar CSS.

- [ ] **Step 4: Reconcile active navigation and adaptive primary text**

The merged navigation state must be:

```css
.app-nav-group .nav-link.is-active {
    background: var(--color-primary, #6750a4);
    color: var(--color-primary-content, #fff);
}
```

Do not retain `.nav-link[aria-current]`; yew-router 0.20 does not emit that
attribute. Also change the active Markdown tab to:

```css
.markdown-input-tab.is-active {
    border-color: var(--color-primary, #6750a4);
    color: var(--color-primary-content, #fff);
    background: var(--color-primary, #6750a4);
}
```

- [ ] **Step 5: Restore table scrolling with the historical frame**

Use the wrapper for the visible historical border so the nested table does not
draw a double border:

```css
.table-scroll {
    width: 100%;
    overflow-x: auto;
    border: 1px solid var(--color-outline-variant, #c7c5d0);
    border-radius: 0.6rem;
    background: var(--color-surface, #faf8ff);
}

.note-table {
    width: 100%;
    min-width: 640px;
    max-width: 100%;
    box-sizing: border-box;
    table-layout: fixed;
    border-collapse: collapse;
    border: 0;
    border-radius: 0;
    overflow: visible;
}
```

Keep the historical table header typography, cell padding, separators, title
links, label popovers, and icon actions from the merge.

- [ ] **Step 6: Restore the historical modal appearance with current safeguards**

The merged modal blocks must have this final shape:

```css
.app-modal-backdrop {
    position: fixed;
    inset: 0;
    background: var(--color-scrim, rgb(0 0 0 / 40%));
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
    z-index: 1001;
}

.app-modal-panel {
    width: 100%;
    max-width: 420px;
    max-height: calc(80vh - 100px);
    padding: 1.25rem;
    border-radius: 0.9rem;
    outline: 0;
    color: var(--color-on-surface, #1b1b1f);
    background: var(--color-surface, #faf8ff);
    box-shadow: 0 12px 40px rgb(0 0 0 / 25%);
}

.app-modal-panel:focus-visible {
    outline: 2px solid var(--color-primary, #6750a4);
    outline-offset: 2px;
}

.app-modal-body {
    max-height: calc(80vh - 160px);
    overflow-y: auto;
}

.app-modal-title {
    margin: 0 0 0.75rem;
    font-size: 1.15rem;
    font-weight: 700;
}
```

- [ ] **Step 7: Retain the loading/reduced-motion safeguard in historical colors**

```css
.loading {
    min-height: 96px;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    color: var(--color-on-surface-variant, #46464f);
    font-style: italic;
}

.loading::before {
    width: 16px;
    height: 16px;
    box-sizing: border-box;
    border: 2px solid var(--color-outline-variant, #c7c5d0);
    border-top-color: var(--color-primary, #6750a4);
    border-radius: 999px;
    content: "";
    animation: loading-spin 700ms linear infinite;
}

@keyframes loading-spin {
    to {
        transform: rotate(360deg);
    }
}

@media (prefers-reduced-motion: reduce) {
    .loading::before {
        animation-duration: 1400ms;
    }
}
```

- [ ] **Step 8: Retain the narrow typed-filter flex reset**

Append these declarations inside the existing `@media (max-width: 640px)` block:

```css
.label-filter-key,
.label-filter-value,
.label-filter-operator {
    flex: 0 0 auto;
    min-width: 0;
}
```

- [ ] **Step 9: Run the structural and selector contract checks**

```sh
! rg -n \
  'app-shell|app-main|app-topbar|breadcrumb' \
  crates/note-frontend/src/main.rs \
  crates/note-frontend/app.css

rg -n '<header class="app-bar">' \
  crates/note-frontend/src/components/app_bar.rs
rg -n 'nav-link\\.is-active' crates/note-frontend/app.css
rg -n '^\\.table-scroll' crates/note-frontend/app.css
rg -n '^\\.app-modal-panel:focus-visible' crates/note-frontend/app.css
rg -n '^\\.app-modal-body' crates/note-frontend/app.css
rg -n '^@media \\(prefers-reduced-motion: reduce\\)' \
  crates/note-frontend/app.css

for selector in \
  '.dashboard-metric-detail' \
  '.trash-toolbar' \
  '.trash-selection-count' \
  '.attachment-description-field' \
  '.attachment-file-input' \
  '.attachment-binary-status' \
  '.attachment-binary-actions'
do
  rg -F "$selector" crates/note-frontend/app.css
done

! rg -n -- \
  '--space-|--color-divider|--color-selection|--color-surface-hover|--color-on-primary\\b' \
  crates/note-frontend/app.css
```

Expected: removed shell selectors are absent; the semantic header, current
active class, table/modal/reduced-motion safeguards, and all current feature
selectors are present; no ZDNS-only or obsolete token names remain.

- [ ] **Step 10: Format and run the green automated gates**

```sh
cargo fmt --all
cargo fmt --manifest-path crates/note-frontend/Cargo.toml
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
(
  cd crates/note-frontend
  trunk build
)
git diff --check
```

Expected: formatting passes, 12 tests pass, Wasm compilation and Trunk bundling
succeed, and the diff has no whitespace errors.

- [ ] **Step 11: Rerun the browser shell assertion and verify GREEN**

Wait for Trunk to rebuild, reload `/`, and rerun the exact JavaScript assertion
from Task 1 Step 5.

Expected result:

```json
{
  "oldShellAbsent": true,
  "horizontalHeaderPresent": true,
  "centeredMainPresent": true
}
```

- [ ] **Step 12: Review scope and commit the atomic restoration**

```sh
git diff --stat
git diff -- \
  crates/note-frontend/src/main.rs \
  crates/note-frontend/src/components/app_bar.rs \
  crates/note-frontend/app.css
git diff --name-only | sort
```

Expected changed implementation files:

```text
crates/note-frontend/app.css
crates/note-frontend/src/components/app_bar.rs
crates/note-frontend/src/main.rs
```

Commit:

```sh
git add \
  crates/note-frontend/src/main.rs \
  crates/note-frontend/src/components/app_bar.rs \
  crates/note-frontend/app.css
git commit -m "style(frontend): restore pre-design UI"
```

---

### Task 3: Verify every route, theme, breakpoint, and retained interaction

**Files:**
- Verify the files from Task 2
- Do not create test fixtures in the repository

- [ ] **Step 1: Create representative data through the real UI**

Using the isolated runtime from Task 1:

1. On `/labels`, create text label `status` and number label `priority`.
2. On `/new`, create a note titled `UI Markdown Fixture` with a Markdown table,
   task list, Rust code block, Mermaid diagram, and `#0065FF` color.
3. Add one text attachment in the editor. For a binary fixture, select the Wasm
   bundle returned by:

   ```sh
   WASM_FIXTURE="$(
     rg --files crates/note-frontend/dist |
       rg '[.]wasm$' |
       head -1
   )"
   test -n "$WASM_FIXTURE"
   ```

   Use MIME type `application/wasm`.
4. Create a second short note and remove it so `/trash` has a row.
5. Add enough isolated notes to exercise pagination:

   ```sh
   for i in $(seq 1 21); do
     title="$(printf 'Pagination Fixture %02d' "$i")"
     payload="$(
       jq -nc \
         --arg title "$title" \
         '{title: $title, content: "# Pagination fixture", attachments: [], labels: []}'
     )"
     curl -fsS \
       -H 'content-type: application/json' \
       --data "$payload" \
       http://127.0.0.1:6222/api/notes
   done
   ```

Expected: Dashboard, Notes, Show, Edit, Labels, Trash, and System all have
representative content, and Notes has multiple pages, without modifying
repository `dev-data`.

- [ ] **Step 2: Verify all desktop routes in light and dark**

At `1440x900`, emulate light and dark color schemes and visit:

```text
/
/notes
/new
/notes/<fixture-id>/show
/notes/<fixture-id>/edit
/labels
/trash
/system
/404
```

For each route:

- capture a viewport screenshot;
- inspect the accessibility tree;
- confirm no console errors or failed expected API requests;
- confirm `document.documentElement.scrollWidth <= clientWidth`;
- confirm the page uses the horizontal header and historical bordered visual
  language.

Evaluate:

```js
() => ({
  dataTheme: document.documentElement.getAttribute("data-theme"),
  themeName: getComputedStyle(document.documentElement)
    .getPropertyValue("--theme-name").trim(),
  colorScheme: getComputedStyle(document.documentElement).colorScheme,
  headerHeight: document.querySelector(".app-bar")
    ?.getBoundingClientRect().height,
  activeLinks: document.querySelectorAll(".nav-link.is-active").length,
  documentOverflow:
    document.documentElement.scrollWidth >
    document.documentElement.clientWidth
})
```

Expected: `dataTheme` is `null`; light is `sunshine`/`light`; dark is
`moonlight`/`dark`; header height is `60`; one link is active on routable pages;
there is no document-level overflow.

- [ ] **Step 3: Verify responsive boundaries**

| Boundary | Viewports | Required behavior |
|---|---|---|
| Header | `621x900`, `620x900`, `375x812` | Compact horizontally scrollable links; System retains the accessible name “System” |
| Dashboard | `521x900`, `520x900` | Metrics, panels, and rows stack according to the historical layout |
| Search/editor | `641x900`, `640x900` | Search and typed-label controls stack without overflow |
| Attachments | `901x900`, `900x900` | Attachment grids become one column and controls remain usable |
| Tables | `375x812` | Notes and Trash scroll inside `.table-scroll`; rightmost actions remain reachable |

Repeat the representative `375x812` routes in both light and dark.

- [ ] **Step 4: Verify retained interactions**

1. Notes: search, add/remove a typed label filter, paginate, refresh, and reload.
2. Show/Edit: render Markdown, open text/binary attachments, and add/replace/remove
   attachments.
3. Labels and delete modals: confirm `role="dialog"`, `aria-modal="true"`,
   autofocus, Escape dismissal, inside-click retention, and backdrop dismissal.
4. Trash: select, restore, and permanently delete representative notes.
5. Dashboard: observe embedding status update without a page reload.
6. System: change and save duplicate rules, then download a backup.
7. Reduced motion: emulate `prefers-reduced-motion: reduce` and confirm loading
   remains understandable without relying on animation.

Expected: all current capabilities work with visible keyboard focus and no
clipped controls.

- [ ] **Step 5: Stop the isolated runtime and remove temporary data**

Stop `cargo run`, then:

```sh
rm -rf "$VERIFY_DIR"
```

Expected: ports `6221` and `6222` are released and repository `dev-data` remains
untouched.

---

### Task 4: Run final completion gates

**Files:**
- Verify only

- [ ] **Step 1: Run fresh automated verification**

```sh
cargo fmt --all -- --check
cargo fmt --manifest-path crates/note-frontend/Cargo.toml -- --check
cargo test --manifest-path crates/note-frontend/Cargo.toml
cargo check --manifest-path crates/note-frontend/Cargo.toml \
  --target wasm32-unknown-unknown
(
  cd crates/note-frontend
  trunk build
)
git diff --check
```

Expected: every command exits zero; frontend tests report 12 passing tests.

- [ ] **Step 2: Audit the spec checklist and repository state**

```sh
git status --short --branch
git log -3 --oneline --decorate
git diff HEAD^ --name-only | sort
git diff HEAD^ --check
```

Expected: the implementation commit contains only `main.rs`, `app_bar.rs`, and
`app.css`; root `DESIGN.md` and `design-system.css` remain absent; vendored
`duskmoon-core.css`, backend crates, and lowercase `docs/design.md` are
unchanged.
