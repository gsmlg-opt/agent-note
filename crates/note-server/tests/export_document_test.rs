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

fn solid_animated_gif(width: u32, height: u32, color: image::Rgba<u8>) -> Vec<u8> {
    use image::{codecs::gif::GifEncoder, Delay, Frame, RgbaImage};
    let mut bytes = Vec::new();
    let mut encoder = GifEncoder::new(&mut bytes);
    let frame = Frame::from_parts(
        RgbaImage::from_pixel(width, height, color),
        0,
        0,
        Delay::from_numer_denom_ms(10, 1),
    );
    encoder.encode_frame(frame).unwrap();
    drop(encoder);
    bytes
}

fn html_attribute_values(html: &str, attribute: &str) -> Vec<String> {
    let prefix = format!("{attribute}=\"");
    let mut rest = html;
    let mut values = Vec::new();
    while let Some(start) = rest.find(&prefix) {
        rest = &rest[start + prefix.len()..];
        let Some(end) = rest.find('"') else { break };
        values.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    values
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

    assert!(package
        .index_html
        .contains("id=\"export-footnote-ref-0001-1\""));
    assert!(package
        .index_html
        .contains("id=\"export-footnote-ref-0001-2\""));
    assert_eq!(
        package
            .index_html
            .matches("href=\"#export-footnote-def-0001\"")
            .count(),
        2
    );
    assert!(package
        .index_html
        .contains("id=\"export-footnote-def-0001\""));
    assert!(package
        .index_html
        .contains("<strong>bold</strong> first line"));
    assert!(package
        .index_html
        .contains("continued with <code>code</code>"));
    assert!(package.index_html.contains("Second <em>paragraph</em>."));
    assert!(package
        .index_html
        .contains("href=\"#export-footnote-ref-0001-1\""));
    assert!(package
        .index_html
        .contains("href=\"#export-footnote-ref-0001-2\""));
}

#[test]
fn footnote_ids_avoid_author_and_heading_collisions_and_keep_repeated_targets_valid() {
    let markdown = r#"<div id="fn-0001"></div>
<div id="export-footnote-def-0001"></div>
<h2 id="export-footnote-ref-0001-1">Heading collision</h2>

First[^detail], repeated[^detail].

[^detail]: body"#;

    let package =
        build_export_document(&frozen(markdown, vec![]), ExportDocumentLimits::default()).unwrap();
    let ids = html_attribute_values(&package.index_html, "id");
    let hrefs = html_attribute_values(&package.index_html, "href");
    let unique = ids.iter().collect::<std::collections::HashSet<_>>();

    assert_eq!(ids.len(), unique.len(), "all exported IDs must be unique");
    for href in hrefs.iter().filter_map(|href| href.strip_prefix('#')) {
        assert!(
            ids.iter().any(|id| id == href),
            "missing target for #{href}"
        );
    }
    assert_eq!(
        hrefs
            .iter()
            .filter(|href| ids.iter().any(|id| href == &&format!("#{id}")))
            .count(),
        4,
        "two references and two backlinks must target generated unique IDs"
    );
}

#[test]
fn encoded_marker_spoof_and_sequential_adversarial_ids_cannot_collide_with_footnotes() {
    let adversarial_ids = (1..=256)
        .map(|index| {
            format!(
                "<div id=\"export-footnote-{index}-def-0001\"></div><div id=\"export-footnote-{index}-ref-0001-1\"></div>"
            )
        })
        .collect::<String>();
    let markdown = format!(
        "<div data-export-footnote-marker=\"note-export-generated-footnote-&#48;\" data-export-footnote-definition=\"0001\" id=\"author-marker\"></div><div id=\"export-footnote-def-0001\"></div><div id=\"export-footnote-ref-0001-1\"></div>{adversarial_ids}\n\nOne[^note], two[^note].\n\n[^note]: body"
    );

    let package =
        build_export_document(&frozen(&markdown, vec![]), ExportDocumentLimits::default()).unwrap();
    let ids = html_attribute_values(&package.index_html, "id");
    let hrefs = html_attribute_values(&package.index_html, "href");
    let unique = ids.iter().collect::<std::collections::HashSet<_>>();

    assert_eq!(ids.len(), unique.len());
    assert!(ids.iter().any(|id| id == "author-marker"));
    assert!(ids.iter().any(|id| id == "export-footnote-257-def-0001"));
    for target in hrefs.iter().filter_map(|href| href.strip_prefix('#')) {
        assert!(
            ids.iter().any(|id| id == target),
            "missing target for #{target}"
        );
    }
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
fn percent_encoded_unicode_non_http_image_matches_external_omission() {
    let destination = "ssh://example.invalid/图 片.png";
    let mut export = frozen(
        "![remote](ssh://example.invalid/%E5%9B%BE%20%E7%89%87.png)",
        vec![],
    );
    export.omissions.push(ExportAssetOmission {
        kind: ExportAssetKind::External,
        destination: destination.into(),
    });

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert!(package
        .index_html
        .contains("External image omitted: remote"));
    assert!(!package.index_html.contains("ssh://"));
}

#[test]
fn rewrites_unicode_spaces_reserved_percent_forms_and_front_matter_images_by_source_order() {
    let markdown = r#"---
cover: ![not rendered](front.png)
---
![unicode](<图 片.png>)
![encoded](图%20片.png)
![hash](<hash#mark.png>)
![question](<query?mark.png>)
![percent](<literal%mark.png>)
![body](body.png)
![repeat](<图 片.png>)"#;
    let export = frozen(
        markdown,
        vec![
            asset("图 片.png", "image/png", one_pixel_png()),
            asset("图%20片.png", "image/png", one_pixel_png()),
            asset("hash#mark.png", "image/png", one_pixel_png()),
            asset("query?mark.png", "image/png", one_pixel_png()),
            asset("literal%mark.png", "image/png", one_pixel_png()),
            asset("body.png", "image/png", one_pixel_png()),
        ],
    );

    let package = build_export_document(&export, ExportDocumentLimits::default()).unwrap();

    assert_eq!(
        package.index_html.matches("src=\"asset-0001.png\"").count(),
        2
    );
    for index in 2..=6 {
        assert!(
            package
                .index_html
                .contains(&format!("src=\"asset-{index:04}.png\"")),
            "missing rewrite for asset {index}"
        );
    }
    assert!(!package.index_html.contains("src=\"front.png\""));
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
    let flattened =
        image::load_from_memory_with_format(&package.assets[0].bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
    assert_eq!(flattened.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_ne!(flattened.get_pixel(0, 0).0, [0, 0, 255, 255]);
    assert_ne!(package.assets[0].bytes, original);
}

#[test]
fn rejects_transcoded_gif_that_exceeds_per_asset_packaging_limit() {
    let gif = solid_animated_gif(64, 64, image::Rgba([255, 0, 0, 255]));
    let limits = ExportDocumentLimits {
        max_packaged_asset_bytes: 100,
        max_combined_packaged_asset_bytes: 1_000,
        ..ExportDocumentLimits::default()
    };

    assert!(matches!(
        build_export_document(
            &frozen(
                "![gif](tiny.gif)",
                vec![asset("tiny.gif", "image/gif", gif)]
            ),
            limits
        ),
        Err(ExportDocumentError::PackagedAssetLimitExceeded { .. })
    ));
}

#[test]
fn rejects_combined_transcoded_package_bytes_with_checked_budget() {
    let first = solid_animated_gif(32, 32, image::Rgba([255, 0, 0, 255]));
    let second = solid_animated_gif(32, 32, image::Rgba([0, 0, 255, 255]));
    let limits = ExportDocumentLimits {
        max_packaged_asset_bytes: 10_000,
        max_combined_packaged_asset_bytes: 150,
        ..ExportDocumentLimits::default()
    };

    assert!(matches!(
        build_export_document(
            &frozen(
                "![one](one.gif) ![two](two.gif)",
                vec![
                    asset("one.gif", "image/gif", first),
                    asset("two.gif", "image/gif", second),
                ],
            ),
            limits
        ),
        Err(ExportDocumentError::CombinedPackagedAssetLimitExceeded)
    ));
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
fn svg_nesting_depth_is_bounded_before_recursive_serialization() {
    let nested_svg = |groups| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\">{}<rect width=\"1\" height=\"1\"/>{}</svg>",
            "<g>".repeat(groups),
            "</g>".repeat(groups)
        )
    };
    let svg_at_boundary = nested_svg(126);
    let boundary = frozen(
        "![svg](boundary.svg)",
        vec![asset(
            "boundary.svg",
            "image/svg+xml",
            svg_at_boundary.into_bytes(),
        )],
    );
    assert!(build_export_document(&boundary, ExportDocumentLimits::default()).is_ok());

    let too_deep_svg = nested_svg(256);
    let too_deep = frozen(
        "![svg](deep.svg)",
        vec![asset(
            "deep.svg",
            "image/svg+xml",
            too_deep_svg.into_bytes(),
        )],
    );
    assert!(matches!(
        build_export_document(&too_deep, ExportDocumentLimits::default()),
        Err(ExportDocumentError::MarkupDepthLimitExceeded)
    ));
}

#[test]
fn generated_renderer_markup_is_depth_checked_before_sanitizer_recursion() {
    let markdown = format!("{}content", "> ".repeat(129));

    assert!(matches!(
        build_export_document(&frozen(&markdown, vec![]), ExportDocumentLimits::default()),
        Err(ExportDocumentError::MarkupDepthLimitExceeded)
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
fn unicode_whitespace_cannot_bypass_mermaid_pre_render_limits() {
    let limits = ExportDocumentLimits {
        max_diagram_bytes: 4,
        ..ExportDocumentLimits::default()
    };
    let markdown = "```mermaid\u{00a0}ignored\nflowchart LR\nA --> B\n```";

    assert!(matches!(
        validate_export_diagrams(markdown, limits),
        Err(ExportDocumentError::DiagramInputLimitExceeded)
    ));
}

#[test]
fn generated_html_preflight_accounts_for_escaping_and_mermaid_expansion() {
    let ordinary_limits = ExportDocumentLimits {
        max_generated_html_bytes: 9_000,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(&frozen(&"<&".repeat(200), vec![]), ordinary_limits),
        Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
    ));

    let mermaid_limits = ExportDocumentLimits {
        max_generated_html_bytes: 12_000,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(
            &frozen("```mermaid\nflowchart LR\nA --> B\n```", vec![]),
            mermaid_limits
        ),
        Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
    ));
}

#[test]
fn sanitizer_and_final_wrapper_refuse_incremental_output_over_cap() {
    let sanitizer_limits = ExportDocumentLimits {
        max_generated_html_bytes: 9_000,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(
            &frozen(&format!("<div>{}</div>", "&".repeat(300)), vec![]),
            sanitizer_limits
        ),
        Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
    ));

    let mut final_wrapper = frozen("body", vec![]);
    final_wrapper.note.title = "&".repeat(300);
    let wrapper_limits = ExportDocumentLimits {
        max_generated_html_bytes: 9_000,
        ..ExportDocumentLimits::default()
    };
    assert!(matches!(
        build_export_document(&final_wrapper, wrapper_limits),
        Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
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
fn print_css_preserves_non_flow_mermaid_diagram_semantics() {
    let markdown = r#"```mermaid
journey
  title Customer journey
  section Purchase
    Checkout: 4: Customer
```

```mermaid
sankey
  Source,Target,10
```

```mermaid
kanban
  todo[To Do]
    task[Write tests]
```

```mermaid
pie showData
  "Complete" : 70
  "Remaining" : 30
```"#;
    let package =
        build_export_document(&frozen(markdown, vec![]), ExportDocumentLimits::default()).unwrap();

    for rendered_class in [
        "dm-mermaid-journey",
        "dm-journey-task",
        "dm-mermaid-sankey",
        "dm-sankey-link",
        "dm-sankey-node",
        "dm-mermaid-kanban",
        "dm-kanban-column",
        "dm-kanban-card",
        "dm-mermaid-pie",
        "dm-mermaid-pie-graphic",
        "dm-mermaid-swatch-1",
    ] {
        assert!(
            package.index_html.contains(rendered_class),
            "missing rendered class {rendered_class}"
        );
    }

    for css_contract in [
        "--dm-mermaid-slice-1: #6750a4",
        "--dm-mermaid-slice-6: #386a20",
        ".dm-journey-task { fill: #eaddff; stroke: #6750a4",
        ".dm-sankey-link { fill: none; stroke: #6750a4",
        ".dm-sankey-node rect { fill: #e8def8; stroke: #6750a4",
        ".dm-kanban-column > rect { fill: #f7f2fa; stroke: #79747e",
        ".dm-kanban-card { fill: #ffffff; stroke: #6750a4",
        ".dm-mermaid-swatch-1 { background: var(--dm-mermaid-slice-1)",
    ] {
        assert!(
            package.index_html.contains(css_contract),
            "missing print CSS contract {css_contract}"
        );
    }
}

#[test]
fn every_supported_mermaid_family_survives_rendering_and_export_sanitization() {
    // These minimal valid sources follow yew-duskmoon 0.9.0's own renderer fixtures.
    let fixtures = [
        (
            "flowchart LR\n  A --> B",
            "dm-mermaid-node",
            ".dm-mermaid-node rect",
        ),
        (
            "swimlane-beta LR\n  subgraph Customer\n    request[Request]\n  end",
            "dm-swimlane-lane",
            ".dm-swimlane-lane rect",
        ),
        (
            "sequenceDiagram\n  Author->>Renderer: Render",
            "dm-sequence-lane",
            ".dm-sequence-lane rect",
        ),
        (
            "classDiagram\n  class Note {\n    +render()\n  }",
            "dm-class-node",
            ".dm-class-node rect",
        ),
        (
            "stateDiagram-v2\n  [*] --> Ready",
            "dm-state-start",
            ".dm-state-start",
        ),
        (
            "erDiagram\n  NOTE ||--o{ ASSET : contains",
            "dm-er-entity",
            ".dm-er-entity rect",
        ),
        (
            "journey\n  section Export\n    Render: 5: User",
            "dm-journey-task",
            ".dm-journey-task",
        ),
        (
            "gantt\n  title Export\n  Task :2026-01-01, 1d",
            "dm-gantt-bar",
            ".dm-gantt-bar",
        ),
        (
            "pie showData\n  \"Rendered\" : 100",
            "dm-mermaid-pie-graphic",
            ".dm-mermaid-pie-graphic",
        ),
        (
            "quadrantChart\n  Note: [0.4, 0.7]",
            "dm-mermaid-quadrant-plane",
            ".dm-mermaid-quadrant-plane",
        ),
        (
            "requirementDiagram\n  requirement safe_render {\n    id: EXP-1\n  }",
            "dm-requirement-node",
            ".dm-requirement-node rect",
        ),
        (
            "gitGraph\n  commit id: \"export\"",
            "dm-git-commit",
            ".dm-git-commit circle",
        ),
        (
            "C4Context\n  Person(user, \"User\")",
            "dm-c4-node",
            ".dm-c4-node rect",
        ),
        (
            "mindmap\n  root((Export))\n    PDF",
            "dm-mindmap-node",
            ".dm-mindmap-node rect",
        ),
        (
            "timeline\n  Source : Markdown",
            "dm-timeline-event",
            ".dm-timeline-event",
        ),
        (
            "zenuml\n  Author->Renderer: Render",
            "dm-zenuml-participant",
            ".dm-zenuml-participant rect",
        ),
        (
            "sankey-beta\n  Markdown,PDF,10",
            "dm-sankey-node",
            ".dm-sankey-node rect",
        ),
        ("xychart-beta\n  bar [1, 2]", "dm-xy-bar", ".dm-xy-bar"),
        (
            "block-beta\n  Source --> PDF",
            "dm-block-node",
            ".dm-block-node rect",
        ),
        (
            "packet-beta\n  0-7: \"type\"",
            "dm-packet-field",
            ".dm-packet-field rect",
        ),
        (
            "kanban\n  Todo\n    Export",
            "dm-kanban-column",
            ".dm-kanban-column > rect",
        ),
        (
            "architecture-beta\n  service api(server)[API]",
            "dm-architecture-service",
            ".dm-architecture-service rect",
        ),
        (
            "radar-beta\n  axis Safety, Speed\n  curve Export{4,3}",
            "dm-radar-area",
            ".dm-radar-area",
        ),
        (
            "eventModeling\n  event ExportRequested",
            "dm-event-modeling-node",
            ".dm-event-modeling-node rect",
        ),
        (
            "treemap-beta\n  \"Export\"\n    \"PDF\": 40",
            "dm-treemap-leaf",
            ".dm-treemap-leaf rect",
        ),
        (
            "venn\n  Markdown: 40\n  PDF: 35\n  Markdown & PDF: 15",
            "dm-venn-left",
            ".dm-venn-left",
        ),
        (
            "ishikawa\n  root((Quality))\n    Export",
            "dm-ishikawa-head",
            ".dm-ishikawa-head",
        ),
        (
            "wardley\n  component Export [0.6, 0.5]",
            "dm-wardley-plane",
            ".dm-wardley-plane",
        ),
        (
            "cynefin\n  Simple: Export",
            "dm-cynefin-domain",
            ".dm-cynefin-domain rect",
        ),
        (
            "treeview\n  root\n    export",
            "dm-treeview-glyph",
            ".dm-treeview-glyph",
        ),
    ];
    assert_eq!(fixtures.len(), 30, "update the full Mermaid family matrix");

    for (source, rendered_class, css_selector) in fixtures {
        let markdown = format!("```mermaid\n{source}\n```");
        let package =
            build_export_document(&frozen(&markdown, vec![]), ExportDocumentLimits::default())
                .unwrap();

        assert!(
            package.index_html.contains(rendered_class),
            "renderer or sanitizer dropped {rendered_class} for:\n{source}"
        );
        assert!(
            package.index_html.contains(css_selector),
            "missing export-owned styling for emitted {rendered_class}"
        );
        assert!(
            !package.index_html.contains("Diagram could not be rendered"),
            "valid renderer fixture unexpectedly fell back for:\n{source}"
        );
    }
}

fn simple_selector_specificity(selector: &str) -> (usize, usize, usize) {
    let ids = selector.matches('#').count();
    let classes = selector.matches('.').count();
    let elements = selector
        .split_ascii_whitespace()
        .filter(|part| part.chars().next().is_some_and(char::is_alphabetic))
        .count();
    (ids, classes, elements)
}

#[test]
fn light_mermaid_text_overrides_win_the_css_cascade() {
    let package =
        build_export_document(&frozen("body", vec![]), ExportDocumentLimits::default()).unwrap();
    let shared = ".dm-mermaid-chart text";

    assert_eq!(simple_selector_specificity(shared), (0, 1, 1));
    for semantic_override in [
        ".dm-mermaid-chart .dm-journey-score-text",
        ".dm-mermaid-chart .dm-gantt-task",
    ] {
        assert!(
            package
                .index_html
                .contains(&format!("{semantic_override} {{ fill: #ffffff")),
            "missing scoped light-text rule {semantic_override}"
        );
        assert!(
            simple_selector_specificity(semantic_override) > simple_selector_specificity(shared),
            "{semantic_override} must outrank {shared}"
        );
    }
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
