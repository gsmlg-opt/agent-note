# Frontend browser checks

The browser checks run against the Trunk development server and mock every `/api/` response in the browser. They do not require a database or Gotenberg.

On NixOS, realize the Playwright browser set matching the pinned Python package:

```sh
nix build --no-link nixpkgs#playwright-driver.browsers
export PLAYWRIGHT_BROWSERS_PATH="$(nix eval --raw nixpkgs#playwright-driver.browsers.outPath)"
```

On other systems, use `uv run --with playwright==1.59.0 playwright install chromium firefox`.

Start the frontend from the repository root in one terminal:

```sh
cd crates/note-frontend
NO_COLOR=false trunk serve index.html --address 127.0.0.1 --port 6221
```

Then run each required engine from the repository root in another terminal:

```sh
nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (ps: [ ps.playwright ])' \
  --command python browser-tests/test_note_export.py --browser chromium

nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (ps: [ ps.playwright ])' \
  --command python browser-tests/test_note_export.py --browser firefox
```

The macOS Safari smoke workflow is separate because Safari/WebDriver is not available on Linux. It enables PDF in a local mock, verifies the exact revision request, observes accessible busy and success states, and confirms that the application initiates the Blob download. Safari WebDriver does not provide a portable downloaded-file inspection API, so the smoke does not claim to inspect a saved file. A skipped or unavailable Safari job is not a passing Safari result.

The Safari API fixture itself is portable and can be checked without Selenium:

```sh
python browser-tests/test_safari_smoke_fixture.py
```

Sync diagnostics checks use the same frontend server and mock every API call. They cover failure details, retry and recovery history, status outages, older API responses, and a scrollable keyboard-accessible history at mobile widths:

```sh
uv run --no-project --with playwright==1.59.0 python browser-tests/test_sync_diagnostics.py
```

Use `--base-url` for a different local port and `--executable` for a system Chromium installation. Screenshots and results default to `/tmp/agent-note-sync-diagnostics-browser`.

Form submission checks click the real new-note, edit-note, label create/edit, and Notes retrieval buttons, asserting their requests and saved UI state. Every API request is intercepted; no backend writes occur. Run against current compiled frontend assets on a loopback URL:

```sh
uv run --no-project --with playwright==1.59.0 python browser-tests/test_note_form_submit.py \
  --base-url http://127.0.0.1:6221 --executable /run/current-system/sw/bin/chromium
```

Omit `--executable` when using Playwright's matching browser installation. `--case new_note` runs one case; screenshots of failures and `browser-result.json` default to `/tmp/agent-note-form-submit-browser`.
