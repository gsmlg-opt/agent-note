# Visual review — 2026-09-14

Reviewer inspected all 11 PNG previews at their original 992 × 1404 resolution after the automated release qualification passed at functional commit `a388f09fbb381d6053f177de90880f0a78f07c89`.

| Page | Observations | Result |
| ---: | --- | --- |
| 1 | English, Simplified Chinese, and Japanese glyphs are intact. The anchor and footnote reference are visible. Rust highlighting and the very long monospace line wrap within the page. PNG, JPEG, WebP, flattened GIF, and safe SVG fixtures render in the light print theme. | Pass |
| 2 | The embedded data PNG renders. Mermaid shows all four nodes (`Snapshot`, `Sanitize`, `Render`, `Validate`) and all three directed edges without clipping. The `Details` anchor target and first 14 table rows render. | Pass |
| 3 | Repeated table header and rows 15–42 are complete; no clipped row at either page boundary. | Pass |
| 4 | Repeated table header and rows 43–70 are complete; no clipped row at either page boundary. | Pass |
| 5 | Repeated table header and rows 71–98 are complete; no clipped row at either page boundary. | Pass |
| 6 | Repeated table header and rows 99–126 are complete; no clipped row at either page boundary. | Pass |
| 7 | Repeated table header and rows 127–154 are complete; no clipped row at either page boundary. | Pass |
| 8 | Repeated table header and rows 155–182 are complete; no clipped row at either page boundary. | Pass |
| 9 | Repeated table header and rows 183–210 are complete; no clipped row at either page boundary. | Pass |
| 10 | Repeated table header and rows 211–238 are complete; no clipped row at either page boundary. | Pass |
| 11 | Repeated table header and rows 239–260 are complete. The footnote text and backlink render below the table. | Pass |

Every page has the correct `current / 11` footer from `1 / 11` through `11 / 11`. No blank page, missing glyph, dark-theme artifact, overlapping element, truncated table cell, or clipped diagram was observed.

No sanitizer-removal or external-asset notice appears because the qualification fixture contains only supported, packaged content. PDF bytes are not used as a golden comparison.
