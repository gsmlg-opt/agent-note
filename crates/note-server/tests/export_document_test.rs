use base64::{engine::general_purpose::STANDARD, Engine as _};
use note_core::{AttachmentStorageMetadata, Note, NoteAttachment};
use note_pipelines::{
    ExportAssetKind, ExportAssetOmission, FrozenNoteExport, HydratedNoteExportAsset,
};
use note_server::export::document::{
    build_export_document, validate_export_diagrams, ExportDocumentError, ExportDocumentLimits,
};
use sha2::Digest;
use std::io::Cursor;

fn frozen(markdown: &str, assets: Vec<HydratedNoteExportAsset>) -> FrozenNoteExport {
    FrozenNoteExport {
        note: Note {
            id: "note-1".into(),
            title: "<unsafe & title>".into(),
            content: markdown.into(),
            attachments: assets
                .iter()
                .map(|asset| asset.attachment.clone())
                .collect(),
            labels: vec![],
            created_at: 1,
            updated_at: 1,
            revision: 1,
            deleted_at: None,
        },
        assets,
        omissions: vec![],
        data_images: vec![],
    }
}

fn asset(destination: &str, mime: &str, bytes: Vec<u8>) -> HydratedNoteExportAsset {
    HydratedNoteExportAsset {
        destination: destination.into(),
        attachment: NoteAttachment {
            id: format!("attachment-{destination}"),
            path: destination.into(),
            mime: mime.into(),
            description: String::new(),
            content: vec![],
            storage: None,
        },
        bytes,
    }
}

fn one_pixel_png() -> Vec<u8> {
    encoded_raster(image::ImageFormat::Png)
}

fn encoded_raster(format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn renders_title_and_renderer_features_without_deduplicating_headings() {
    let export = frozen(
        "# &lt;unsafe &amp; title&gt;\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n- [x] task\n\n```rust\nfn main() {}\n```\n\n```mermaid\nflowchart LR\n A --> B\n```\n\nfootnote[^1]\n\n[^1]: detail",
        vec![],
    );

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert!(package
        .index_html
        .contains("<h1 class=\"export-title\">&lt;unsafe &amp; title&gt;</h1>"));
    assert!(
        package
            .index_html
            .matches("&lt;unsafe &amp; title&gt;")
            .count()
            >= 2
    );
    assert!(package.index_html.contains("<table>"));
    assert!(package.index_html.contains("type=\"checkbox\""));
    assert!(package.index_html.contains("dm-token-keyword"));
    assert!(package.index_html.contains("dm-mermaid-chart"));
    assert!(package.index_html.contains("footnote-reference"));
    assert!(package.index_html.contains("footnote-definition"));
}

#[test]
fn renders_multiline_formatted_footnotes_with_unique_repeated_backlinks() {
    let markdown = "First[^detail], repeated[^detail].\n\n[^detail]: **bold** first line\n    continued with `code`\n\n    Second *paragraph*.\n";

    let package =
        build_export_document(&frozen(markdown, vec![]), ExportDocumentLimits::default()).unwrap();

    assert!(package.index_html.contains("id=\"fnref-0001-1\""));
    assert!(package.index_html.contains("id=\"fnref-0001-2\""));
    assert_eq!(package.index_html.matches("href=\"#fn-0001\"").count(), 2);
    assert!(package.index_html.contains("id=\"fn-0001\""));
    assert!(package
        .index_html
        .contains("<strong>bold</strong> first line"));
    assert!(package
        .index_html
        .contains("continued with <code>code</code>"));
    assert!(package.index_html.contains("Second <em>paragraph</em>."));
    assert!(package.index_html.contains("href=\"#fnref-0001-1\""));
    assert!(package.index_html.contains("href=\"#fnref-0001-2\""));
}

#[test]
fn packages_validated_images_with_flat_names_and_omits_external_fetches() {
    let mut export = frozen(
        "![local](images/a.png)\n\n![external](https://example.com/tracker.png)\n\n[site](https://example.com) [mail](mailto:a@example.com) [app](/notes/2) [manual](manual.pdf) [jump](#part)\n\n<a href=\"javascript:alert(1)\">bad</a>\n\n<h2 id=\"part\">Part</h2>",
        vec![asset("images/a.png", "image/png", one_pixel_png())],
    );
    export.note.attachments.push(NoteAttachment {
        id: "manual".into(),
        path: "manual.pdf".into(),
        mime: "application/pdf".into(),
        description: "Manual".into(),
        content: vec![],
        storage: None,
    });
    export.omissions.push(ExportAssetOmission {
        kind: ExportAssetKind::External,
        destination: "https://example.com/tracker.png".into(),
    });

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert_eq!(package.assets.len(), 1);
    assert_eq!(package.assets[0].filename, "asset-0001.png");
    assert!(package.index_html.contains("src=\"asset-0001.png\""));
    assert!(package.index_html.contains("External image omitted"));
    assert!(package.index_html.contains("export-notice"));
    assert!(package.index_html.contains("href=\"https://example.com\""));
    assert!(package.index_html.contains("href=\"mailto:a@example.com\""));
    assert!(package.index_html.contains("data-app-path=\"/notes/2\""));
    assert!(package
        .index_html
        .contains("manual <span class=\"attachment-path\">(manual.pdf)</span>"));
    assert!(package.index_html.contains("href=\"#part\""));
    assert!(!package.index_html.contains("tracker.png\""));
    assert!(!package.index_html.contains("javascript:"));
}

#[test]
fn removes_executable_and_fetch_capable_author_html_but_preserves_text() {
    let export = frozen(
        r#"<script>alert('x')</script><iframe src="https://example.com">frame text</iframe><form action="/steal"><label>Name<input autofocus onfocus="alert(1)"></label></form><div onclick="go()" style="background:url(https://example.com/x);color:red">kept text</div><img srcset="https://example.com/a 2x" src="https://example.com/b"><style>@import url(https://example.com/x)</style>

```html
<script>readable example</script>
```"#,
        vec![],
    );

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    for forbidden in [
        "<script",
        "<iframe",
        "<form",
        "<input",
        "onfocus",
        "onclick",
        "srcset",
        "@import",
        "example.com/x",
    ] {
        assert!(!package.index_html.contains(forbidden), "found {forbidden}");
    }
    assert!(package.index_html.contains("kept text"));
    assert!(package
        .index_html
        .contains("&lt;script&gt;readable example&lt;/script&gt;"));
    assert!(package.index_html.contains("Unsafe content removed"));
}

#[test]
fn validates_raster_svg_and_embedded_data_images() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20" fill="#123456"/></svg>"##.to_vec();
    let data = format!("data:image/png;base64,{}", STANDARD.encode(one_pixel_png()));
    let mut export = frozen(
        &format!("![png](a.png) ![svg](b.svg) ![data]({data})"),
        vec![
            asset("a.png", "image/png", one_pixel_png()),
            asset("b.svg", "image/svg+xml", svg),
        ],
    );
    export.data_images.push(data);

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert_eq!(package.assets.len(), 3);
    assert_eq!(package.assets[0].filename, "asset-0001.png");
    assert_eq!(package.assets[1].filename, "asset-0002.svg");
    assert_eq!(package.assets[2].filename, "asset-0003.png");
    assert!(String::from_utf8_lossy(&package.assets[1].bytes).contains("<rect"));
    assert!(!package.index_html.contains("data:image"));
}

#[test]
fn data_image_rewrite_is_structural_and_cannot_collide_with_attachment_or_code() {
    let data = format!("data:image/png;base64,{}", STANDARD.encode(one_pixel_png()));
    let markdown = format!(
        "The literal export-data-image-2 and `{data}` stay readable.\n\n```text\n{data}\nexport-data-image-2\n```\n\n![attachment](export-data-image-2)\n\n![embedded]({data})"
    );
    let mut export = frozen(
        &markdown,
        vec![asset("export-data-image-2", "image/png", one_pixel_png())],
    );
    export.data_images.push(data);

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert!(package
        .index_html
        .contains("The literal export-data-image-2"));
    assert!(package.index_html.matches("data:image/png;base64,").count() >= 2);
    assert!(package.index_html.contains("src=\"asset-0001.png\""));
    assert!(package.index_html.contains("src=\"asset-0002.png\""));
}

#[test]
fn validates_png_jpeg_webp_and_gif_first_frame_with_collision_free_names() {
    let export = frozen(
        "![png](same/a) ![jpeg](same/b) ![webp](same/c) ![gif](same/d)",
        vec![
            asset(
                "same/a",
                "image/png",
                encoded_raster(image::ImageFormat::Png),
            ),
            asset(
                "same/b",
                "image/jpeg",
                encoded_raster(image::ImageFormat::Jpeg),
            ),
            asset(
                "same/c",
                "image/webp",
                encoded_raster(image::ImageFormat::WebP),
            ),
            asset(
                "same/d",
                "image/gif",
                encoded_raster(image::ImageFormat::Gif),
            ),
        ],
    );

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert_eq!(
        package
            .assets
            .iter()
            .map(|asset| asset.filename.as_str())
            .collect::<Vec<_>>(),
        [
            "asset-0001.png",
            "asset-0002.jpg",
            "asset-0003.webp",
            "asset-0004.png"
        ]
    );
    assert_eq!(package.index_html.matches("<img src=\"asset-").count(), 4);
}

fn animated_gif() -> Vec<u8> {
    use image::{codecs::gif::GifEncoder, Delay, Frame, RgbaImage};
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        let first = Frame::from_parts(
            RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(10, 1),
        );
        let second = Frame::from_parts(
            RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(10, 1),
        );
        encoder.encode_frames([first, second]).unwrap();
    }
    bytes
}

#[test]
fn animated_gif_is_flattened_to_one_png_frame() {
    let original = animated_gif();
    let package = build_export_document(
        &frozen(
            "![animated](animation.gif)",
            vec![asset("animation.gif", "image/gif", original.clone())],
        ),
        ExportDocumentLimits::default(),
    )
    .unwrap();

    assert_eq!(package.assets[0].filename, "asset-0001.png");
    assert_eq!(package.assets[0].mime, "image/png");
    assert_eq!(
        image::guess_format(&package.assets[0].bytes).unwrap(),
        image::ImageFormat::Png
    );
    assert_ne!(package.assets[0].bytes, original);
}

#[test]
fn rejects_mime_signature_malformed_svg_and_dimension_limits() {
    let mismatch = frozen(
        "![bad](bad.png)",
        vec![asset("bad.png", "image/png", b"GIF89a".to_vec())],
    );
    assert!(matches!(
        build_export_document(&mismatch, ExportDocumentLimits::default()),
        Err(ExportDocumentError::MimeSignatureMismatch { .. })
    ));

    let unsafe_svg = frozen(
        "![bad](bad.svg)",
        vec![asset(
            "bad.svg",
            "image/svg+xml",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="https://example.com/x"/></svg>"#.to_vec(),
        )],
    );
    assert!(matches!(
        build_export_document(&unsafe_svg, ExportDocumentLimits::default()),
        Err(ExportDocumentError::UnsafeSvg { .. })
    ));

    let limits = ExportDocumentLimits {
        max_pixels_per_image: 0,
        ..ExportDocumentLimits::default()
    };
    let too_large = frozen(
        "![png](a.png)",
        vec![asset("a.png", "image/png", one_pixel_png())],
    );
    assert!(matches!(
        build_export_document(&too_large, limits),
        Err(ExportDocumentError::ImageDimensionsExceeded { .. })
    ));
}

#[test]
fn rejects_size_mismatch_unsupported_mime_and_malformed_raster() {
    let mut wrong_size = asset("a.png", "image/png", one_pixel_png());
    wrong_size.attachment.storage = Some(AttachmentStorageMetadata {
        object_key: "immutable/a".into(),
        storage_generation: "1".into(),
        size_bytes: wrong_size.bytes.len() as u64 + 1,
        checksum_sha256: format!("{:x}", sha2::Sha256::digest(&wrong_size.bytes)),
    });
    assert!(matches!(
        build_export_document(
            &frozen("![a](a.png)", vec![wrong_size]),
            ExportDocumentLimits::default()
        ),
        Err(ExportDocumentError::AssetSizeMismatch { .. })
    ));
    assert!(matches!(
        build_export_document(
            &frozen(
                "![bmp](a.bmp)",
                vec![asset("a.bmp", "image/bmp", b"BMbad".to_vec())]
            ),
            ExportDocumentLimits::default()
        ),
        Err(ExportDocumentError::UnsupportedImage { .. })
    ));
    assert!(matches!(
        build_export_document(
            &frozen(
                "![png](a.png)",
                vec![asset("a.png", "image/png", b"not a png".to_vec())]
            ),
            ExportDocumentLimits::default()
        ),
        Err(ExportDocumentError::InvalidImage { .. })
    ));
}

#[test]
fn rejects_diagram_limits_in_pure_pre_render_check() {
    let limits = ExportDocumentLimits {
        max_diagram_bytes: 8,
        ..ExportDocumentLimits::default()
    };
    let markdown = "```mermaid\nflowchart LR\nA --> B\n```";

    assert!(matches!(
        validate_export_diagrams(markdown, limits),
        Err(ExportDocumentError::DiagramInputLimitExceeded)
    ));
    assert!(matches!(
        build_export_document(&frozen(markdown, vec![]), limits),
        Err(ExportDocumentError::DiagramInputLimitExceeded)
    ));

    let count_limits = ExportDocumentLimits {
        max_diagram_count: 1,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        validate_export_diagrams(
            "```Mermaid\nA --> B\n```\n\n```mmd\nB --> C\n```",
            count_limits
        ),
        Err(ExportDocumentError::DiagramCountLimitExceeded)
    ));

    let combined_limits = ExportDocumentLimits {
        max_diagram_bytes: 6,
        max_combined_diagram_bytes: 9,
        ..ExportDocumentLimits::default()
    };
    let combined_result = validate_export_diagrams(
        "```mermaid\nabcde\n```\n\n```mermaid\nfghij\n```",
        combined_limits,
    );
    assert!(
        matches!(
            combined_result,
            Err(ExportDocumentError::CombinedDiagramInputLimitExceeded)
        ),
        "unexpected result: {combined_result:?}"
    );

    let complexity_limits = ExportDocumentLimits {
        max_diagram_lines: 1,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        validate_export_diagrams("```mermaid\na\nb\n```", complexity_limits),
        Err(ExportDocumentError::DiagramComplexityExceeded)
    ));
}

#[test]
fn rejects_combined_pixels_checksum_data_and_generated_html_limits() {
    let two_images = frozen(
        "![a](a.png) ![b](b.png)",
        vec![
            asset("a.png", "image/png", one_pixel_png()),
            asset("b.png", "image/png", one_pixel_png()),
        ],
    );
    let limits = ExportDocumentLimits {
        max_pixels_per_image: 1,
        max_combined_pixels: 1,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(&two_images, limits),
        Err(ExportDocumentError::CombinedImageDimensionsExceeded)
    ));

    let mut bad_checksum_asset = asset("a.png", "image/png", one_pixel_png());
    bad_checksum_asset.attachment.storage = Some(AttachmentStorageMetadata {
        object_key: "immutable/key".into(),
        storage_generation: "generation".into(),
        size_bytes: bad_checksum_asset.bytes.len() as u64,
        checksum_sha256: "00".repeat(32),
    });
    assert!(matches!(
        build_export_document(
            &frozen("![a](a.png)", vec![bad_checksum_asset]),
            ExportDocumentLimits::default()
        ),
        Err(ExportDocumentError::AssetChecksumMismatch { .. })
    ));

    let mut invalid_data = frozen("![data](data:image/png;base64,not-base64)", vec![]);
    invalid_data
        .data_images
        .push("data:image/png;base64,not-base64".into());
    assert!(matches!(
        build_export_document(&invalid_data, ExportDocumentLimits::default()),
        Err(ExportDocumentError::InvalidDataImage)
    ));

    let html_limit = ExportDocumentLimits {
        max_generated_html_bytes: 1,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(&frozen("body", vec![]), html_limit),
        Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
    ));
}

#[test]
fn print_css_has_bounded_a4_contract_without_tall_block_avoidance() {
    let package =
        build_export_document(&frozen("body", vec![]), ExportDocumentLimits::default()).unwrap();

    assert!(package.index_html.contains("@page"));
    assert!(package.index_html.contains("size: A4 portrait"));
    assert!(package.index_html.contains("margin: 18mm"));
    assert!(package
        .index_html
        .contains("thead { display: table-header-group"));
    assert!(package.index_html.contains("white-space: pre-wrap"));
    assert!(package.index_html.contains("max-width: 100%"));
    assert!(package.index_html.contains("--dm-syntax-keyword: #ffb454"));
    assert!(package.index_html.contains("background: #101720"));
    assert!(package
        .index_html
        .contains(".dm-token-keyword { color: var(--dm-syntax-keyword); font-weight: 700"));
    assert!(package.index_html.contains(".dm-mermaid-edge"));
    assert!(package.index_html.contains("stroke: #6750a4"));
    assert!(package.index_html.contains(".dm-mermaid-node rect"));
    assert!(package.index_html.contains("fill: #faf8ff"));
    assert!(package
        .index_html
        .contains(".dm-mermaid-chart text { fill: #1b1b1f"));
    assert!(!package.index_html.contains("pre { break-inside: avoid"));
    assert!(package.footer_html.contains("pageNumber"));
    assert!(package.footer_html.contains("totalPages"));
}

#[test]
fn unsupported_mermaid_retains_source_with_visible_warning() {
    let package = build_export_document(
        &frozen("```mermaid\nnot-a-supported-diagram\nA -> B\n```", vec![]),
        ExportDocumentLimits::default(),
    )
    .unwrap();

    assert!(package.index_html.contains("Diagram could not be rendered"));
    assert!(package.index_html.contains("not-a-supported-diagram"));
    assert!(package.index_html.contains("A -&gt; B"));
}
