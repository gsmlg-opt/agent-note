# PDF export release qualification

PDF export stays disabled until the pinned renderer deployment and this qualification suite pass in the target environment. Markdown export and normal note operations do not depend on Gotenberg.

## Automated release gate

The gate uses the exact image declared in `deployment/pdf-export/compose.yaml`, keeps it solely on the internal renderer network with no published port, verifies the production Compose hardening, and then runs real conversions. On Linux, the script resolves the container's internal bridge address for host-side test traffic without attaching the renderer to an egress-capable network:

```sh
./pdf-tests/run-release-validation.sh
```

Required commands are `docker` with Compose, `curl`, `python3`, `cargo`, `qpdf`, `pdfinfo`, `pdftotext`, `pdftoppm`, and ImageMagick's `identify`. On NixOS they can be supplied without modifying the host:

```sh
nix shell \
  nixpkgs#poppler-utils \
  nixpkgs#qpdf \
  nixpkgs#imagemagick \
  --command ./pdf-tests/run-release-validation.sh
```

The script fails if a required tool, renderer, security control, font, or artifact is missing. It does not contain a skip-success path. It checks the pinned manifest digest, container runtime controls, locally resolved CJK/Japanese/Latin fonts, blocked network/resource vectors, disabled JavaScript and event handlers, path traversal, and cross-request asset isolation.

It then builds a representative document through the production `FrozenNoteExport` → document package → `GotenbergRenderer` path. The fixture contains Chinese, Japanese, and English text; searchable code and long lines; footnotes and anchors; a multi-page table; PNG, JPEG, WebP, animated-GIF-first-frame, sanitized SVG, and data images; Mermaid; print colors; A4 sizing; and page numbers.

The real-renderer Rust test is opt-in so the ordinary workspace suite stays renderer-free:

```sh
NOTE_TEST_REAL_PDF=1 \
NOTE_TEST_GOTENBERG_URL=http://RENDERER_INTERNAL_BRIDGE_IP:3000 \
NOTE_TEST_PDF_ARTIFACT_DIR="$PWD/target/pdf-export-validation" \
cargo test -p note-server --test export_real_renderer_test -- --nocapture
```

When `NOTE_TEST_REAL_PDF=1`, the URL, artifact directory, renderer, and PDF inspection tools are mandatory. Missing dependencies fail the test. Without that flag the single integration test reports that real qualification was skipped; do not report that default-mode result as a renderer pass.

## Evidence and visual review

The gate writes these files under `target/pdf-export-validation/`:

- `agent-note-export.pdf` — the actual representative export;
- `agent-note-export.txt` — Poppler text extraction used for multilingual/searchability assertions;
- `agent-note-export-page-*.png` — every page, rendered at 120 DPI;
- `security-checks.log` — blocked vectors and isolation assertions;
- `qualification-record.txt` — commit, exact image/digest/platform, fonts, tool versions, browser coverage, and skips.

Before enabling PDF in a trusted environment, inspect every page preview. Record whether pagination, CJK/Japanese glyphs, Mermaid, code wrapping, table headers/page breaks, images, light print colors, footnotes/backlinks, page numbers, and blank pages are acceptable. Do not compare PDF bytes, and do not review only the first page.

The committed evidence directory records the last qualified release candidate. Regenerate it when the renderer image, fonts, export HTML/CSS, sanitizer, image pipeline, or renderer request fields change. A Linux run cannot pass the real Safari workflow; Safari coverage remains a separate macOS job.

## Production and rollback

Use `deployment/pdf-export/README.md` for the optional private renderer topology. The automated script runs only on Linux because its host-side test process addresses the private Docker bridge directly. Rollback is removing or disabling the application's `[export.pdf]` configuration and stopping the renderer; Markdown export remains available and no schema/data rollback is needed.
