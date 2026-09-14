# Note export browser checks

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
