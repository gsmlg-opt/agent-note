use base64::{engine::general_purpose::STANDARD, Engine as _};
use html5ever::{local_name, ns, parse_fragment, tendril::TendrilSink, QualName};
use image::{
    codecs::png::PngEncoder, guess_format, ImageEncoder, ImageFormat, ImageReader,
    Limits as ImageLimits,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use note_pipelines::{ExportAssetKind, FrozenNoteExport};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use roxmltree::{Document, Node};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    io::{Cursor, Write},
    ops::Range,
};
use uuid::Uuid;

const PRINT_CSS: &str = include_str!("print.css");
const DEFAULT_MAX_PIXELS_PER_IMAGE: u64 = 20_000_000;
const DEFAULT_MAX_COMBINED_PIXELS: u64 = 80_000_000;
const DEFAULT_MAX_GENERATED_HTML_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_SVG_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_SVG_ELEMENTS: usize = 10_000;
const DEFAULT_MAX_SVG_ATTRIBUTES: usize = 50_000;
const DEFAULT_MAX_DIAGRAM_COUNT: usize = 32;
const DEFAULT_MAX_DIAGRAM_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_COMBINED_DIAGRAM_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_DIAGRAM_LINES: usize = 4_096;
const DEFAULT_MAX_PACKAGED_ASSET_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_COMBINED_PACKAGED_ASSET_BYTES: usize = 32 * 1024 * 1024;
/// Recursive serializers only receive trees at or below this element nesting depth.
const MAX_MARKUP_NESTING_DEPTH: usize = 128;
/// Bounds parser-produced non-element nodes before any recursive tree consumer runs.
const MAX_MARKUP_NODES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportDocumentLimits {
    pub max_pixels_per_image: u64,
    pub max_combined_pixels: u64,
    pub max_generated_html_bytes: usize,
    pub max_svg_bytes: usize,
    pub max_svg_elements: usize,
    pub max_svg_attributes: usize,
    pub max_diagram_count: usize,
    pub max_diagram_bytes: usize,
    pub max_combined_diagram_bytes: usize,
    pub max_diagram_lines: usize,
    pub max_packaged_asset_bytes: usize,
    pub max_combined_packaged_asset_bytes: usize,
}

impl Default for ExportDocumentLimits {
    fn default() -> Self {
        Self {
            max_pixels_per_image: DEFAULT_MAX_PIXELS_PER_IMAGE,
            max_combined_pixels: DEFAULT_MAX_COMBINED_PIXELS,
            max_generated_html_bytes: DEFAULT_MAX_GENERATED_HTML_BYTES,
            max_svg_bytes: DEFAULT_MAX_SVG_BYTES,
            max_svg_elements: DEFAULT_MAX_SVG_ELEMENTS,
            max_svg_attributes: DEFAULT_MAX_SVG_ATTRIBUTES,
            max_diagram_count: DEFAULT_MAX_DIAGRAM_COUNT,
            max_diagram_bytes: DEFAULT_MAX_DIAGRAM_BYTES,
            max_combined_diagram_bytes: DEFAULT_MAX_COMBINED_DIAGRAM_BYTES,
            max_diagram_lines: DEFAULT_MAX_DIAGRAM_LINES,
            max_packaged_asset_bytes: DEFAULT_MAX_PACKAGED_ASSET_BYTES,
            max_combined_packaged_asset_bytes: DEFAULT_MAX_COMBINED_PACKAGED_ASSET_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedExportAsset {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDocumentPackage {
    pub index_html: String,
    pub footer_html: String,
    pub assets: Vec<PackagedExportAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportDocumentError {
    InvalidLimits,
    InvalidImage { destination: String },
    UnsupportedImage { destination: String },
    MimeSignatureMismatch { destination: String },
    ImageDimensionsExceeded { destination: String },
    CombinedImageDimensionsExceeded,
    UnsafeSvg { destination: String },
    SvgComplexityExceeded { destination: String },
    InvalidDataImage,
    AssetNotInManifest { destination: String },
    AssetChecksumMismatch { destination: String },
    AssetSizeMismatch { destination: String },
    GeneratedHtmlLimitExceeded,
    DiagramCountLimitExceeded,
    DiagramInputLimitExceeded,
    CombinedDiagramInputLimitExceeded,
    DiagramComplexityExceeded,
    PackagedAssetLimitExceeded { destination: String },
    CombinedPackagedAssetLimitExceeded,
    MarkupDepthLimitExceeded,
    MarkupNodeLimitExceeded,
    FootnoteMarkerUnavailable,
}

impl fmt::Display for ExportDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLimits => "invalid export document limits",
            Self::InvalidImage { .. } => "export image is malformed",
            Self::UnsupportedImage { .. } => "export image format is unsupported",
            Self::MimeSignatureMismatch { .. } => {
                "export image MIME type does not match its content"
            }
            Self::ImageDimensionsExceeded { .. } => "export image dimensions exceed the limit",
            Self::CombinedImageDimensionsExceeded => {
                "combined export image dimensions exceed the limit"
            }
            Self::UnsafeSvg { .. } => "export SVG contains unsafe content",
            Self::SvgComplexityExceeded { .. } => "export SVG exceeds the complexity limit",
            Self::InvalidDataImage => "embedded export image is invalid",
            Self::AssetNotInManifest { .. } => "export image is not present in the frozen manifest",
            Self::AssetChecksumMismatch { .. } => {
                "export image checksum does not match frozen metadata"
            }
            Self::AssetSizeMismatch { .. } => "export image size does not match frozen metadata",
            Self::GeneratedHtmlLimitExceeded => "generated export HTML exceeds the limit",
            Self::DiagramCountLimitExceeded => "export diagram count exceeds the limit",
            Self::DiagramInputLimitExceeded => "export diagram input exceeds the limit",
            Self::CombinedDiagramInputLimitExceeded => {
                "combined export diagram input exceeds the limit"
            }
            Self::DiagramComplexityExceeded => "export diagram complexity exceeds the limit",
            Self::PackagedAssetLimitExceeded { .. } => {
                "packaged export image exceeds the byte limit"
            }
            Self::CombinedPackagedAssetLimitExceeded => {
                "combined packaged export images exceed the byte limit"
            }
            Self::MarkupDepthLimitExceeded => "export markup nesting exceeds the limit",
            Self::MarkupNodeLimitExceeded => "export markup node count exceeds the limit",
            Self::FootnoteMarkerUnavailable => "could not reserve an export footnote marker",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ExportDocumentError {}

pub fn build_export_document(
    frozen: &FrozenNoteExport,
    limits: ExportDocumentLimits,
) -> Result<ExportDocumentPackage, ExportDocumentError> {
    validate_limits(limits)?;
    validate_export_diagrams(&frozen.note.content, limits)?;
    let prepared = prepare_footnotes(&frozen.note.content)?;
    // WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#12
    preflight_generated_html(&prepared, &frozen.note.title, limits)?;

    let mut assets = Vec::new();
    let mut rewrites = RewriteManifest::default();
    let mut combined_pixels = 0_u64;
    let mut combined_packaged_bytes = 0_usize;
    for hydrated in &frozen.assets {
        if let Some(storage) = &hydrated.attachment.storage {
            if storage.size_bytes != hydrated.bytes.len() as u64 {
                return Err(ExportDocumentError::AssetSizeMismatch {
                    destination: hydrated.destination.clone(),
                });
            }
            if format!("{:x}", Sha256::digest(&hydrated.bytes)) != storage.checksum_sha256 {
                return Err(ExportDocumentError::AssetChecksumMismatch {
                    destination: hydrated.destination.clone(),
                });
            }
        }
        let validated = validate_image(
            &hydrated.destination,
            &hydrated.attachment.mime,
            &hydrated.bytes,
            limits,
            &mut combined_pixels,
        )?;
        add_packaged_bytes(
            &hydrated.destination,
            validated.bytes.len(),
            limits,
            &mut combined_packaged_bytes,
        )?;
        let filename = asset_filename(assets.len() + 1, validated.extension);
        rewrites.insert(&hydrated.destination, &filename);
        assets.push(PackagedExportAsset {
            filename,
            mime: validated.mime.into(),
            bytes: validated.bytes,
        });
    }

    for data_url in &frozen.data_images {
        let (mime, bytes) = decode_data_image(data_url)?;
        let validated = validate_image(
            "embedded data image",
            &mime,
            &bytes,
            limits,
            &mut combined_pixels,
        )?;
        add_packaged_bytes(
            "embedded data image",
            validated.bytes.len(),
            limits,
            &mut combined_packaged_bytes,
        )?;
        let filename = asset_filename(assets.len() + 1, validated.extension);
        rewrites.insert(data_url, &filename);
        assets.push(PackagedExportAsset {
            filename,
            mime: validated.mime.into(),
            bytes: validated.bytes,
        });
    }

    let mut rendered = CappedString::new(limits.max_generated_html_bytes);
    rendered.push_str(&render_with_existing_contract(&prepared.body_markdown));
    render_footnote_definitions(&prepared, &mut rendered);
    let rendered = rendered.finish()?;

    let external_omissions = frozen
        .omissions
        .iter()
        .filter(|omission| omission.kind == ExportAssetKind::External)
        .flat_map(|omission| source_aliases(&omission.destination))
        .collect::<HashSet<_>>();
    let mut original_image_sources =
        collect_original_image_sources(markdown_body_after_front_matter(&prepared.body_markdown))?;
    for footnote in &prepared.footnotes {
        original_image_sources.extend(collect_original_image_sources(
            markdown_body_after_front_matter(&footnote.markdown),
        )?);
    }
    let policy = HtmlPolicy {
        rewrites: &rewrites,
        external_omissions: &external_omissions,
        original_image_sources: &original_image_sources,
        footnote_marker: &prepared.marker_token,
    };
    let SanitizedHtml {
        html: body,
        removed,
    } = sanitize_html(&rendered, &policy, limits, &mut combined_pixels)?;
    let notices = export_notices(!external_omissions.is_empty(), removed);
    let index_html = assemble_index_html(
        &frozen.note.title,
        &notices,
        &body,
        limits.max_generated_html_bytes,
    )?;
    Ok(ExportDocumentPackage {
        index_html,
        footer_html: "<!doctype html><html><head><style>body{width:100%;margin:0;color:#5f6368;font:8pt \"Noto Sans\",\"DejaVu Sans\",sans-serif}div{text-align:center}</style></head><body><div><span class=\"pageNumber\"></span> / <span class=\"totalPages\"></span></div></body></html>".into(),
        assets,
    })
}

fn assemble_index_html(
    title: &str,
    notices: &str,
    body: &str,
    max_bytes: usize,
) -> Result<String, ExportDocumentError> {
    let mut index_html = CappedString::new(max_bytes);
    index_html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"light\"><title>");
    index_html.push_escaped_text(title);
    index_html.push_str("</title><style>");
    index_html.push_str(PRINT_CSS);
    index_html.push_str("</style></head><body><main class=\"export-document markdown-body\"><h1 class=\"export-title\">");
    index_html.push_escaped_text(title);
    index_html.push_str("</h1>");
    index_html.push_str(notices);
    index_html.push_str(body);
    index_html.push_str("</main></body></html>");
    index_html.finish()
}

fn validate_limits(limits: ExportDocumentLimits) -> Result<(), ExportDocumentError> {
    if limits.max_combined_pixels < limits.max_pixels_per_image
        || limits.max_generated_html_bytes == 0
        || limits.max_svg_bytes == 0
        || limits.max_svg_elements == 0
        || limits.max_svg_attributes == 0
        || limits.max_diagram_count == 0
        || limits.max_diagram_bytes == 0
        || limits.max_combined_diagram_bytes < limits.max_diagram_bytes
        || limits.max_diagram_lines == 0
        || limits.max_packaged_asset_bytes == 0
        || limits.max_combined_packaged_asset_bytes == 0
    {
        return Err(ExportDocumentError::InvalidLimits);
    }
    Ok(())
}

pub fn validate_export_diagrams(
    markdown: &str,
    limits: ExportDocumentLimits,
) -> Result<(), ExportDocumentError> {
    validate_limits(limits)?;
    let mut diagram_count = 0_usize;
    let mut combined_bytes = 0_usize;
    let mut diagram_bytes = 0_usize;
    let mut diagram_lines = 0_usize;
    let mut in_diagram = false;

    for event in Parser::new_ext(markdown, Options::ENABLE_FOOTNOTES) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if is_mermaid_language(&info) =>
            {
                diagram_count += 1;
                if diagram_count > limits.max_diagram_count {
                    return Err(ExportDocumentError::DiagramCountLimitExceeded);
                }
                diagram_bytes = 0;
                diagram_lines = 0;
                in_diagram = true;
            }
            Event::Text(text) if in_diagram => {
                diagram_bytes = diagram_bytes
                    .checked_add(text.len())
                    .ok_or(ExportDocumentError::DiagramInputLimitExceeded)?;
                diagram_lines = diagram_lines
                    .checked_add(text.lines().count().max(1))
                    .ok_or(ExportDocumentError::DiagramComplexityExceeded)?;
                if diagram_bytes > limits.max_diagram_bytes {
                    return Err(ExportDocumentError::DiagramInputLimitExceeded);
                }
                if diagram_lines > limits.max_diagram_lines {
                    return Err(ExportDocumentError::DiagramComplexityExceeded);
                }
            }
            Event::SoftBreak | Event::HardBreak if in_diagram => {
                diagram_bytes = diagram_bytes
                    .checked_add(1)
                    .ok_or(ExportDocumentError::DiagramInputLimitExceeded)?;
                diagram_lines = diagram_lines
                    .checked_add(1)
                    .ok_or(ExportDocumentError::DiagramComplexityExceeded)?;
            }
            Event::End(TagEnd::CodeBlock) if in_diagram => {
                if diagram_bytes > limits.max_diagram_bytes {
                    return Err(ExportDocumentError::DiagramInputLimitExceeded);
                }
                if diagram_lines > limits.max_diagram_lines {
                    return Err(ExportDocumentError::DiagramComplexityExceeded);
                }
                combined_bytes = combined_bytes
                    .checked_add(diagram_bytes)
                    .ok_or(ExportDocumentError::CombinedDiagramInputLimitExceeded)?;
                if combined_bytes > limits.max_combined_diagram_bytes {
                    return Err(ExportDocumentError::CombinedDiagramInputLimitExceeded);
                }
                in_diagram = false;
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_mermaid_language(info: &str) -> bool {
    info.split_whitespace().next().is_some_and(|language| {
        let language = language.trim().to_ascii_lowercase();
        matches!(language.as_str(), "mermaid" | "mmd")
    })
}

fn preflight_generated_html(
    prepared: &PreparedFootnotes,
    title: &str,
    limits: ExportDocumentLimits,
) -> Result<(), ExportDocumentError> {
    let escaped_title = escaped_text_len(title)?;
    let mut estimated = PRINT_CSS
        .len()
        .checked_add(512)
        .and_then(|value| value.checked_add(escaped_title.checked_mul(2)?))
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    for markdown in std::iter::once(prepared.body_markdown.as_str()).chain(
        prepared
            .footnotes
            .iter()
            .map(|footnote| footnote.markdown.as_str()),
    ) {
        estimated = estimated
            .checked_add(estimate_renderer_html(markdown)?)
            .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    }
    let footnote_references = prepared
        .footnotes
        .iter()
        .try_fold(0_usize, |total, footnote| {
            total.checked_add(footnote.reference_ids.len())
        })
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    estimated = estimated
        .checked_add(
            prepared
                .footnotes
                .len()
                .checked_mul(512)
                .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?,
        )
        .and_then(|value| value.checked_add(footnote_references.checked_mul(256)?))
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    if estimated > limits.max_generated_html_bytes {
        return Err(ExportDocumentError::GeneratedHtmlLimitExceeded);
    }
    Ok(())
}

fn estimate_renderer_html(markdown: &str) -> Result<usize, ExportDocumentError> {
    let (front_matter, body) = split_front_matter(markdown)
        .map(|(source, body)| (Some(source), body))
        .unwrap_or((None, markdown));
    let mut estimate = front_matter
        .map(|source| highlighted_code_bound(source.len(), source.lines().count().max(1)))
        .transpose()?
        .unwrap_or(0);
    let mut code_kind = None;
    for event in Parser::new_ext(body, renderer_markdown_options()) {
        let addition =
            match event {
                Event::Start(tag) => {
                    let addition = estimate_start_tag(&tag)?;
                    if let Tag::CodeBlock(kind) = tag {
                        code_kind = Some(match kind {
                            CodeBlockKind::Fenced(info) if is_mermaid_language(&info) => {
                                PreflightCodeKind::Mermaid
                            }
                            _ => PreflightCodeKind::Highlighted,
                        });
                    }
                    addition
                }
                Event::End(tag) => {
                    if tag == TagEnd::CodeBlock {
                        code_kind = None;
                    }
                    estimate_end_tag(tag)
                }
                Event::Text(text) => match code_kind {
                    Some(PreflightCodeKind::Highlighted) => {
                        highlighted_code_bound(text.len(), text.lines().count().max(1))?
                    }
                    Some(PreflightCodeKind::Mermaid) => mermaid_html_bound(&text)?,
                    None => escaped_text_len(&text)?,
                },
                Event::Code(code) => inline_code_bound(&code)?,
                Event::InlineMath(math) | Event::DisplayMath(math) => escaped_text_len(&math)?
                    .checked_add(128)
                    .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?,
                Event::Html(html) | Event::InlineHtml(html) => html
                    .len()
                    .checked_mul(6)
                    .and_then(|value| value.checked_add(512))
                    .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?,
                Event::SoftBreak | Event::HardBreak => 16,
                Event::Rule | Event::TaskListMarker(_) => 128,
                Event::FootnoteReference(reference) => escaped_text_len(&reference)?
                    .checked_add(256)
                    .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?,
            };
        estimate = estimate
            .checked_add(addition)
            .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    }
    Ok(estimate)
}

fn estimate_start_tag(tag: &Tag<'_>) -> Result<usize, ExportDocumentError> {
    let payload = match tag {
        Tag::Paragraph
        | Tag::HtmlBlock
        | Tag::Item
        | Tag::TableHead
        | Tag::TableRow
        | Tag::TableCell
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::Emphasis
        | Tag::Strong
        | Tag::Strikethrough
        | Tag::Superscript
        | Tag::Subscript
        | Tag::MetadataBlock(_) => 0,
        Tag::BlockQuote(_) | Tag::List(_) => 64,
        Tag::Table(alignments) => alignments
            .len()
            .checked_mul(32)
            .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?,
        Tag::FootnoteDefinition(label) => escaped_attribute_len(label)?,
        Tag::CodeBlock(CodeBlockKind::Indented) => 0,
        Tag::CodeBlock(CodeBlockKind::Fenced(info)) => escaped_attribute_len(info)?,
        Tag::Heading {
            id, classes, attrs, ..
        } => {
            let mut payload = id
                .as_deref()
                .map(escaped_attribute_len)
                .transpose()?
                .unwrap_or(0);
            for class in classes {
                payload = checked_add_html(payload, escaped_attribute_len(class)?)?;
            }
            for (name, value) in attrs {
                payload = checked_add_html(payload, escaped_attribute_len(name)?)?;
                if let Some(value) = value {
                    payload = checked_add_html(payload, escaped_attribute_len(value)?)?;
                }
            }
            payload
        }
        Tag::Link {
            dest_url,
            title,
            id,
            ..
        }
        | Tag::Image {
            dest_url,
            title,
            id,
            ..
        } => checked_add_html(
            checked_add_html(
                escaped_attribute_len(dest_url)?,
                escaped_attribute_len(title)?,
            )?,
            escaped_attribute_len(id)?,
        )?,
    };
    checked_add_html(payload, 512)
}

fn estimate_end_tag(tag: TagEnd) -> usize {
    match tag {
        TagEnd::Paragraph
        | TagEnd::Heading(_)
        | TagEnd::BlockQuote(_)
        | TagEnd::CodeBlock
        | TagEnd::HtmlBlock
        | TagEnd::List(_)
        | TagEnd::Item
        | TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListTitle
        | TagEnd::DefinitionListDefinition
        | TagEnd::Table
        | TagEnd::TableHead
        | TagEnd::TableRow
        | TagEnd::TableCell
        | TagEnd::Emphasis
        | TagEnd::Strong
        | TagEnd::Strikethrough
        | TagEnd::Superscript
        | TagEnd::Subscript
        | TagEnd::Link
        | TagEnd::Image
        | TagEnd::MetadataBlock(_) => 64,
    }
}

fn checked_add_html(left: usize, right: usize) -> Result<usize, ExportDocumentError> {
    left.checked_add(right)
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)
}

fn inline_code_bound(code: &str) -> Result<usize, ExportDocumentError> {
    // yew-duskmoon 0.9 turns any accepted CSS color into a fixed color-chip template
    // containing the text once and the escaped attribute twice. Conservatively charge
    // that template for every inline code event so preflight need not duplicate its parser.
    let text = escaped_text_len(code)?;
    let attributes = escaped_attribute_len(code)?
        .checked_mul(2)
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)?;
    checked_add_html(checked_add_html(text, attributes)?, 2_048)
}

#[derive(Clone, Copy)]
enum PreflightCodeKind {
    Highlighted,
    Mermaid,
}

fn renderer_markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

fn escaped_text_len(value: &str) -> Result<usize, ExportDocumentError> {
    value
        .chars()
        .try_fold(0_usize, |total, character| {
            total.checked_add(match character {
                '&' => 5,
                '<' | '>' => 4,
                _ => character.len_utf8(),
            })
        })
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)
}

fn escaped_attribute_len(value: &str) -> Result<usize, ExportDocumentError> {
    value
        .chars()
        .try_fold(0_usize, |total, character| {
            total.checked_add(match character {
                '&' => 5,
                '<' | '>' => 4,
                '"' => 6,
                '\'' => 6,
                _ => character.len_utf8(),
            })
        })
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)
}

fn highlighted_code_bound(bytes: usize, lines: usize) -> Result<usize, ExportDocumentError> {
    bytes
        .checked_mul(64)
        .and_then(|value| value.checked_add(lines.checked_mul(128)?))
        .and_then(|value| value.checked_add(512))
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)
}

fn mermaid_html_bound(source: &str) -> Result<usize, ExportDocumentError> {
    source
        .len()
        .checked_mul(128)
        .and_then(|value| value.checked_add(source.lines().count().max(1).checked_mul(1_024)?))
        .and_then(|value| value.checked_add(64 * 1024))
        .ok_or(ExportDocumentError::GeneratedHtmlLimitExceeded)
}

struct CappedString {
    value: String,
    max: usize,
    exceeded: bool,
}

impl CappedString {
    fn new(max: usize) -> Self {
        Self {
            value: String::new(),
            max,
            exceeded: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        let fits = self
            .value
            .len()
            .checked_add(value.len())
            .is_some_and(|length| length <= self.max);
        if fits {
            self.value.push_str(value);
        } else {
            self.exceeded = true;
        }
    }

    fn push(&mut self, value: char) {
        let mut buffer = [0_u8; 4];
        self.push_str(value.encode_utf8(&mut buffer));
    }

    fn push_escaped_text(&mut self, value: &str) {
        for character in value.chars() {
            self.push_str(match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                _ => {
                    self.push(character);
                    continue;
                }
            });
        }
    }

    fn push_escaped_attribute(&mut self, value: &str) {
        for character in value.chars() {
            self.push_str(match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                _ => {
                    self.push(character);
                    continue;
                }
            });
        }
    }

    fn finish(self) -> Result<String, ExportDocumentError> {
        if self.exceeded {
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        } else {
            Ok(self.value)
        }
    }
}

struct ValidatedImage {
    extension: &'static str,
    mime: &'static str,
    bytes: Vec<u8>,
}

fn add_packaged_bytes(
    destination: &str,
    bytes: usize,
    limits: ExportDocumentLimits,
    combined: &mut usize,
) -> Result<(), ExportDocumentError> {
    if bytes > limits.max_packaged_asset_bytes {
        return Err(ExportDocumentError::PackagedAssetLimitExceeded {
            destination: destination.into(),
        });
    }
    *combined = combined
        .checked_add(bytes)
        .ok_or(ExportDocumentError::CombinedPackagedAssetLimitExceeded)?;
    if *combined > limits.max_combined_packaged_asset_bytes {
        return Err(ExportDocumentError::CombinedPackagedAssetLimitExceeded);
    }
    Ok(())
}

fn validate_image(
    destination: &str,
    declared_mime: &str,
    bytes: &[u8],
    limits: ExportDocumentLimits,
    combined_pixels: &mut u64,
) -> Result<ValidatedImage, ExportDocumentError> {
    let mime = declared_mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime == "image/svg+xml" {
        return validate_svg(destination, bytes, limits, combined_pixels);
    }
    let expected = match mime.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::WebP,
        "image/gif" => ImageFormat::Gif,
        _ => {
            return Err(ExportDocumentError::UnsupportedImage {
                destination: destination.into(),
            })
        }
    };
    let actual = guess_format(bytes).map_err(|_| ExportDocumentError::InvalidImage {
        destination: destination.into(),
    })?;
    if actual != expected {
        return Err(ExportDocumentError::MimeSignatureMismatch {
            destination: destination.into(),
        });
    }
    let reader = ImageReader::with_format(Cursor::new(bytes), actual);
    let (width, height) =
        reader
            .into_dimensions()
            .map_err(|_| ExportDocumentError::InvalidImage {
                destination: destination.into(),
            })?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| ExportDocumentError::ImageDimensionsExceeded {
            destination: destination.into(),
        })?;
    add_pixels(destination, pixels, limits, combined_pixels)?;
    let max_alloc =
        pixels
            .checked_mul(16)
            .ok_or_else(|| ExportDocumentError::ImageDimensionsExceeded {
                destination: destination.into(),
            })?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), actual);
    let mut image_limits = ImageLimits::default();
    image_limits.max_image_width = Some(width);
    image_limits.max_image_height = Some(height);
    image_limits.max_alloc = Some(max_alloc);
    reader.limits(image_limits);
    let decoded = reader
        .decode()
        .map_err(|_| ExportDocumentError::InvalidImage {
            destination: destination.into(),
        })?;
    if actual == ImageFormat::Gif {
        let mut flattened = BoundedVecWriter::new(limits.max_packaged_asset_bytes);
        let encoded = PngEncoder::new(&mut flattened).write_image(
            decoded.as_bytes(),
            decoded.width(),
            decoded.height(),
            decoded.color().into(),
        );
        if flattened.exceeded {
            return Err(ExportDocumentError::PackagedAssetLimitExceeded {
                destination: destination.into(),
            });
        }
        encoded.map_err(|_| ExportDocumentError::InvalidImage {
            destination: destination.into(),
        })?;
        return Ok(ValidatedImage {
            extension: "png",
            mime: "image/png",
            bytes: flattened.bytes,
        });
    }
    let (extension, canonical_mime) = match actual {
        ImageFormat::Png => ("png", "image/png"),
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::WebP => ("webp", "image/webp"),
        ImageFormat::Gif => unreachable!("GIF returned as flattened PNG above"),
        _ => unreachable!("format restricted above"),
    };
    Ok(ValidatedImage {
        extension,
        mime: canonical_mime,
        bytes: bytes.to_vec(),
    })
}

struct BoundedVecWriter {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl BoundedVecWriter {
    fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
            exceeded: false,
        }
    }
}

impl Write for BoundedVecWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("packaged image limit exceeded"));
        };
        if next > self.max {
            self.exceeded = true;
            return Err(std::io::Error::other("packaged image limit exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn add_pixels(
    destination: &str,
    pixels: u64,
    limits: ExportDocumentLimits,
    combined_pixels: &mut u64,
) -> Result<(), ExportDocumentError> {
    if pixels > limits.max_pixels_per_image {
        return Err(ExportDocumentError::ImageDimensionsExceeded {
            destination: destination.into(),
        });
    }
    *combined_pixels = combined_pixels
        .checked_add(pixels)
        .ok_or(ExportDocumentError::CombinedImageDimensionsExceeded)?;
    if *combined_pixels > limits.max_combined_pixels {
        return Err(ExportDocumentError::CombinedImageDimensionsExceeded);
    }
    Ok(())
}

fn validate_svg(
    destination: &str,
    bytes: &[u8],
    limits: ExportDocumentLimits,
    combined_pixels: &mut u64,
) -> Result<ValidatedImage, ExportDocumentError> {
    if bytes.len() > limits.max_svg_bytes {
        return Err(ExportDocumentError::SvgComplexityExceeded {
            destination: destination.into(),
        });
    }
    let source = std::str::from_utf8(bytes).map_err(|_| ExportDocumentError::InvalidImage {
        destination: destination.into(),
    })?;
    preflight_xml_nesting(source)?;
    let document = Document::parse(source).map_err(|_| ExportDocumentError::InvalidImage {
        destination: destination.into(),
    })?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(ExportDocumentError::InvalidImage {
            destination: destination.into(),
        });
    }
    let (element_count, attribute_count) = validate_svg_tree(root)?;
    if element_count > limits.max_svg_elements || attribute_count > limits.max_svg_attributes {
        return Err(ExportDocumentError::SvgComplexityExceeded {
            destination: destination.into(),
        });
    }
    let (width, height) =
        svg_dimensions(root).ok_or_else(|| ExportDocumentError::InvalidImage {
            destination: destination.into(),
        })?;
    add_pixels(
        destination,
        width.saturating_mul(height),
        limits,
        combined_pixels,
    )?;
    let mut sanitized = String::new();
    serialize_svg_node(root, &mut sanitized).map_err(|_| ExportDocumentError::UnsafeSvg {
        destination: destination.into(),
    })?;
    Ok(ValidatedImage {
        extension: "svg",
        mime: "image/svg+xml",
        bytes: sanitized.into_bytes(),
    })
}

fn preflight_xml_nesting(source: &str) -> Result<(), ExportDocumentError> {
    let bytes = source.as_bytes();
    let mut cursor = 0_usize;
    let mut depth = 0_usize;
    while cursor < bytes.len() {
        let Some(relative_open) = bytes[cursor..].iter().position(|byte| *byte == b'<') else {
            break;
        };
        cursor += relative_open;
        if bytes[cursor..].starts_with(b"<!--") {
            let Some(end) = source[cursor + 4..].find("-->") else {
                break;
            };
            cursor += 4 + end + 3;
            continue;
        }
        if bytes[cursor..].starts_with(b"<![CDATA[") {
            let Some(end) = source[cursor + 9..].find("]]>") else {
                break;
            };
            cursor += 9 + end + 3;
            continue;
        }
        if bytes[cursor..].starts_with(b"<?") {
            let Some(end) = source[cursor + 2..].find("?>") else {
                break;
            };
            cursor += 2 + end + 2;
            continue;
        }

        let closing = bytes.get(cursor + 1) == Some(&b'/');
        let declaration = bytes.get(cursor + 1) == Some(&b'!');
        let mut scan = cursor + 1;
        let mut quote = None;
        let mut last_non_whitespace = None;
        while let Some(&byte) = bytes.get(scan) {
            if let Some(expected) = quote {
                if byte == expected {
                    quote = None;
                }
            } else if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if byte == b'>' {
                break;
            } else if !byte.is_ascii_whitespace() {
                last_non_whitespace = Some(byte);
            }
            scan += 1;
        }
        if scan >= bytes.len() {
            break;
        }
        if closing {
            depth = depth.saturating_sub(1);
        } else if !declaration && last_non_whitespace != Some(b'/') {
            depth = depth
                .checked_add(1)
                .ok_or(ExportDocumentError::MarkupDepthLimitExceeded)?;
            if depth > MAX_MARKUP_NESTING_DEPTH {
                return Err(ExportDocumentError::MarkupDepthLimitExceeded);
            }
        }
        cursor = scan + 1;
    }
    Ok(())
}

fn validate_svg_tree(root: Node<'_, '_>) -> Result<(usize, usize), ExportDocumentError> {
    let mut stack = vec![(root, 0_usize)];
    let mut node_count = 0_usize;
    let mut element_count = 0_usize;
    let mut attribute_count = 0_usize;
    while let Some((node, parent_element_depth)) = stack.pop() {
        node_count = node_count
            .checked_add(1)
            .ok_or(ExportDocumentError::MarkupNodeLimitExceeded)?;
        if node_count > MAX_MARKUP_NODES {
            return Err(ExportDocumentError::MarkupNodeLimitExceeded);
        }
        let element_depth = if node.is_element() {
            let depth = parent_element_depth
                .checked_add(1)
                .ok_or(ExportDocumentError::MarkupDepthLimitExceeded)?;
            if depth > MAX_MARKUP_NESTING_DEPTH {
                return Err(ExportDocumentError::MarkupDepthLimitExceeded);
            }
            element_count = element_count
                .checked_add(1)
                .ok_or(ExportDocumentError::MarkupNodeLimitExceeded)?;
            attribute_count = attribute_count
                .checked_add(node.attributes().len())
                .ok_or(ExportDocumentError::MarkupNodeLimitExceeded)?;
            depth
        } else {
            parent_element_depth
        };
        stack.extend(node.children().map(|child| (child, element_depth)));
    }
    Ok((element_count, attribute_count))
}

fn svg_dimensions(root: Node<'_, '_>) -> Option<(u64, u64)> {
    let parse = |value: &str| value.trim_end_matches("px").parse::<f64>().ok();
    let dimensions = root
        .attribute("width")
        .and_then(parse)
        .zip(root.attribute("height").and_then(parse))
        .or_else(|| {
            let values = root
                .attribute("viewBox")?
                .split_ascii_whitespace()
                .map(str::parse::<f64>);
            let values = values.collect::<Result<Vec<_>, _>>().ok()?;
            (values.len() == 4).then(|| (values[2], values[3]))
        })?;
    if !dimensions.0.is_finite()
        || !dimensions.1.is_finite()
        || dimensions.0 <= 0.0
        || dimensions.1 <= 0.0
    {
        return None;
    }
    Some((dimensions.0.ceil() as u64, dimensions.1.ceil() as u64))
}

fn serialize_svg_node(node: Node<'_, '_>, output: &mut String) -> Result<(), ()> {
    if node.is_text() {
        output.push_str(&escape_text(node.text().unwrap_or("")));
        return Ok(());
    }
    if !node.is_element() {
        return Ok(());
    }
    let tag = node.tag_name().name();
    if !matches!(
        tag,
        "svg"
            | "g"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "defs"
            | "marker"
            | "linearGradient"
            | "radialGradient"
            | "stop"
            | "clipPath"
            | "title"
            | "desc"
    ) {
        return Err(());
    }
    output.push('<');
    output.push_str(tag);
    if tag == "svg" {
        output.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
    }
    for attribute in node.attributes() {
        let name = attribute.name();
        let value = attribute.value();
        if name == "xmlns" {
            continue;
        }
        if name.starts_with("on")
            || matches!(name, "href" | "xlink:href" | "style")
            || value.to_ascii_lowercase().contains("url(")
        {
            return Err(());
        }
        if !is_safe_svg_attribute(name) {
            return Err(());
        }
        output.push(' ');
        output.push_str(name);
        output.push_str("=\"");
        output.push_str(&escape_attribute(value));
        output.push('"');
    }
    output.push('>');
    for child in node.children() {
        serialize_svg_node(child, output)?;
    }
    output.push_str("</");
    output.push_str(tag);
    output.push('>');
    Ok(())
}

fn is_safe_svg_attribute(name: &str) -> bool {
    matches!(
        name,
        "id" | "class"
            | "width"
            | "height"
            | "viewBox"
            | "x"
            | "y"
            | "x1"
            | "y1"
            | "x2"
            | "y2"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
            | "d"
            | "points"
            | "fill"
            | "fill-opacity"
            | "stroke"
            | "stroke-width"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-dasharray"
            | "opacity"
            | "transform"
            | "offset"
            | "stop-color"
            | "stop-opacity"
            | "markerWidth"
            | "markerHeight"
            | "refX"
            | "refY"
            | "orient"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "clip-path"
            | "text-anchor"
            | "font-size"
            | "font-weight"
            | "aria-hidden"
            | "role"
    )
}

fn decode_data_image(url: &str) -> Result<(String, Vec<u8>), ExportDocumentError> {
    let rest = url
        .strip_prefix("data:")
        .ok_or(ExportDocumentError::InvalidDataImage)?;
    let (metadata, payload) = rest
        .split_once(',')
        .ok_or(ExportDocumentError::InvalidDataImage)?;
    let mut parts = metadata.split(';');
    let mime = parts.next().unwrap_or("").to_ascii_lowercase();
    if !mime.starts_with("image/") {
        return Err(ExportDocumentError::InvalidDataImage);
    }
    let base64 = parts.any(|part| part.eq_ignore_ascii_case("base64"));
    let bytes = if base64 {
        STANDARD
            .decode(payload)
            .map_err(|_| ExportDocumentError::InvalidDataImage)?
    } else {
        percent_decode_str(payload).collect()
    };
    Ok((mime, bytes))
}

fn asset_filename(index: usize, extension: &str) -> String {
    format!("asset-{index:04}.{extension}")
}

fn normalize_relative(value: &str) -> String {
    value.strip_prefix("./").unwrap_or(value).to_owned()
}

#[derive(Default)]
struct RewriteManifest {
    exact: HashMap<String, String>,
    aliases: HashMap<String, Option<String>>,
}

impl RewriteManifest {
    fn insert(&mut self, source: &str, filename: &str) {
        self.exact.insert(source.to_owned(), filename.to_owned());
        self.exact
            .insert(normalize_relative(source), filename.to_owned());
        for alias in source_aliases(source) {
            match self.aliases.entry(alias) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some(filename.to_owned()));
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().as_deref() != Some(filename) =>
                {
                    entry.insert(None);
                }
                _ => {}
            }
        }
    }

    fn resolve(&self, original: Option<&str>, rendered: &str) -> Option<&str> {
        if let Some(original) = original {
            if let Some(filename) = self
                .exact
                .get(original)
                .or_else(|| self.exact.get(&normalize_relative(original)))
            {
                return Some(filename);
            }
            for alias in source_aliases(original) {
                if let Some(Some(filename)) = self.aliases.get(&alias) {
                    return Some(filename);
                }
            }
        }
        // Renderers may URL-encode the DOM src. Only use canonical aliases that identify one
        // manifest entry; an encoded spelling must never steal a distinct literal destination.
        for alias in source_aliases(rendered) {
            if let Some(Some(filename)) = self.aliases.get(&alias) {
                return Some(filename);
            }
        }
        None
    }
}

fn source_aliases(value: &str) -> Vec<String> {
    let normalized = normalize_relative(value);
    let mut aliases = vec![normalized.clone()];
    if let Ok(decoded) = percent_decode_str(&normalized).decode_utf8() {
        if decoded != normalized {
            aliases.push(decoded.into_owned());
        }
    }
    aliases
}

fn markdown_body_after_front_matter(markdown: &str) -> &str {
    split_front_matter(markdown)
        .map(|(_, body)| body)
        .unwrap_or(markdown)
}

fn split_front_matter(markdown: &str) -> Option<(&str, &str)> {
    let markdown = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let (opening, mut cursor, has_line_ending) = logical_line(markdown, 0)?;
    if opening != "---" || !has_line_ending {
        return None;
    }
    let source_start = cursor;
    while cursor < markdown.len() {
        let line_start = cursor;
        let (line, next, _) = logical_line(markdown, cursor)?;
        if matches!(line, "---" | "...") {
            return Some((&markdown[source_start..line_start], &markdown[next..]));
        }
        cursor = next;
    }
    None
}

fn logical_line(source: &str, start: usize) -> Option<(&str, usize, bool)> {
    let rest = source.get(start..)?;
    if let Some(newline) = rest.find('\n') {
        let line = rest[..newline]
            .strip_suffix('\r')
            .unwrap_or(&rest[..newline]);
        Some((line, start + newline + 1, true))
    } else {
        Some((rest, source.len(), false))
    }
}

fn collect_original_image_sources(
    markdown: &str,
) -> Result<Vec<Option<String>>, ExportDocumentError> {
    let mut sources = Vec::new();
    for event in Parser::new_ext(markdown, Options::ENABLE_FOOTNOTES) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => sources.push(Some(dest_url.into_string())),
            Event::Html(fragment) | Event::InlineHtml(fragment) => {
                let dom = parse_checked_html_fragment(fragment.as_ref())?;
                collect_dom_image_sources(&dom.document, &mut sources);
            }
            _ => {}
        }
    }
    Ok(sources)
}

fn collect_dom_image_sources(node: &Handle, sources: &mut Vec<Option<String>>) {
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data {
            if matches!(
                name.local.as_ref(),
                "script" | "style" | "template" | "noscript" | "object"
            ) {
                continue;
            }
            if name.local.as_ref() == "img" {
                let source = attrs
                    .borrow()
                    .iter()
                    .find(|attribute| attribute.name.local.as_ref() == "src")
                    .map(|source| source.value.to_string());
                sources.push(source);
            }
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
}

fn parse_checked_html_fragment(fragment: &str) -> Result<RcDom, ExportDocumentError> {
    let dom = parse_fragment(
        RcDom::default(),
        Default::default(),
        QualName::new(None, ns!(html), local_name!("div")),
        vec![],
        true,
    )
    .one(fragment);
    validate_html_tree(&dom.document)?;
    Ok(dom)
}

fn validate_html_tree(root: &Handle) -> Result<(), ExportDocumentError> {
    let mut stack = vec![(root.clone(), 0_usize)];
    let mut node_count = 0_usize;
    while let Some((node, parent_element_depth)) = stack.pop() {
        node_count = node_count
            .checked_add(1)
            .ok_or(ExportDocumentError::MarkupNodeLimitExceeded)?;
        if node_count > MAX_MARKUP_NODES {
            return Err(ExportDocumentError::MarkupNodeLimitExceeded);
        }
        let element_depth = if let NodeData::Element { name, .. } = &node.data {
            // html5ever inserts one synthetic `html` container for fragments. It is not
            // author-controlled nesting and is excluded from the documented depth limit.
            if parent_element_depth == 0 && name.local.as_ref() == "html" {
                0
            } else {
                let depth = parent_element_depth
                    .checked_add(1)
                    .ok_or(ExportDocumentError::MarkupDepthLimitExceeded)?;
                if depth > MAX_MARKUP_NESTING_DEPTH {
                    return Err(ExportDocumentError::MarkupDepthLimitExceeded);
                }
                depth
            }
        } else {
            parent_element_depth
        };
        stack.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(|child| (child, element_depth)),
        );
    }
    Ok(())
}

struct HtmlPolicy<'a> {
    rewrites: &'a RewriteManifest,
    external_omissions: &'a HashSet<String>,
    original_image_sources: &'a [Option<String>],
    footnote_marker: &'a str,
}

struct SanitizedHtml {
    html: String,
    removed: bool,
}

fn sanitize_html(
    rendered: &str,
    policy: &HtmlPolicy<'_>,
    limits: ExportDocumentLimits,
    combined_pixels: &mut u64,
) -> Result<SanitizedHtml, ExportDocumentError> {
    let dom = parse_checked_html_fragment(rendered)?;
    let mut author_ids = HashSet::new();
    collect_author_ids(&dom.document, policy.footnote_marker, &mut author_ids);
    let footnote_namespace = choose_footnote_namespace(&author_ids);
    let mut state = HtmlSanitizer {
        output: CappedString::new(limits.max_generated_html_bytes),
        removed: false,
        policy,
        limits,
        combined_pixels,
        svg_elements: 0,
        svg_attributes: 0,
        code_depth: 0,
        image_sources: policy.original_image_sources.iter().cloned().collect(),
        footnote_namespace,
    };
    for child in dom.document.children.borrow().iter() {
        state.write_node(child)?;
    }
    Ok(SanitizedHtml {
        html: state.output.finish()?,
        removed: state.removed,
    })
}

fn collect_author_ids(node: &Handle, footnote_marker: &str, ids: &mut HashSet<String>) {
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { attrs, .. } = &node.data {
            let attrs = attrs.borrow();
            if generated_footnote_marker(&attrs, footnote_marker).is_none() {
                if let Some(id) = attrs
                    .iter()
                    .find(|attribute| attribute.name.local.as_ref() == "id")
                {
                    ids.insert(id.value.to_string());
                }
            }
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
}

fn choose_footnote_namespace(author_ids: &HashSet<String>) -> String {
    let blocked = author_ids
        .iter()
        .filter_map(|id| footnote_namespace_from_generated_shape(id))
        .collect::<HashSet<_>>();
    for suffix in 0..=blocked.len() {
        let namespace = if suffix == 0 {
            "export-footnote".to_owned()
        } else {
            format!("export-footnote-{suffix}")
        };
        if !blocked.contains(&namespace) {
            return namespace;
        }
    }
    unreachable!("n blocked namespaces cannot occupy n plus one candidates")
}

fn footnote_namespace_from_generated_shape(id: &str) -> Option<String> {
    let (namespace, suffix) = id.split_once("-def-").or_else(|| id.split_once("-ref-"))?;
    let valid_namespace = namespace == "export-footnote"
        || namespace
            .strip_prefix("export-footnote-")
            .is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            });
    (valid_namespace && !suffix.is_empty()).then(|| namespace.to_owned())
}

enum GeneratedFootnoteMarker<'a> {
    Definition(&'a str),
    Reference(&'a str),
    Target(&'a str),
    Backref(&'a str),
}

fn generated_footnote_marker<'a>(
    attrs: &'a [html5ever::Attribute],
    expected_marker: &str,
) -> Option<GeneratedFootnoteMarker<'a>> {
    if !attrs.iter().any(|attribute| {
        attribute.name.local.as_ref() == "data-export-footnote-marker"
            && attribute.value.as_ref() == expected_marker
    }) {
        return None;
    }
    attrs.iter().find_map(|attribute| {
        let value = attribute.value.as_ref();
        match attribute.name.local.as_ref() {
            "data-export-footnote-definition" => Some(GeneratedFootnoteMarker::Definition(value)),
            "data-export-footnote-reference" => Some(GeneratedFootnoteMarker::Reference(value)),
            "data-export-footnote-target" => Some(GeneratedFootnoteMarker::Target(value)),
            "data-export-footnote-backref" => Some(GeneratedFootnoteMarker::Backref(value)),
            _ => None,
        }
    })
}

struct HtmlSanitizer<'a> {
    output: CappedString,
    removed: bool,
    policy: &'a HtmlPolicy<'a>,
    limits: ExportDocumentLimits,
    combined_pixels: &'a mut u64,
    svg_elements: usize,
    svg_attributes: usize,
    code_depth: usize,
    image_sources: VecDeque<Option<String>>,
    footnote_namespace: String,
}

impl HtmlSanitizer<'_> {
    fn write_node(&mut self, node: &Handle) -> Result<(), ExportDocumentError> {
        match &node.data {
            NodeData::Document => self.write_children(node),
            NodeData::Text { contents } => {
                let contents = contents.borrow();
                let lowered = contents.to_ascii_lowercase();
                if self.code_depth == 0
                    && ["<script", "<style", "<object", "<iframe", "<form"]
                        .iter()
                        .any(|tag| lowered.contains(tag))
                {
                    self.removed = true;
                    return Ok(());
                }
                self.output.push_escaped_text(&contents);
                Ok(())
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                if name.ns == ns!(svg) {
                    self.svg_elements += 1;
                    self.svg_attributes += attrs.borrow().len();
                    if self.svg_elements > self.limits.max_svg_elements
                        || self.svg_attributes > self.limits.max_svg_attributes
                    {
                        return Err(ExportDocumentError::SvgComplexityExceeded {
                            destination: "inline SVG".into(),
                        });
                    }
                    if tag == "svg" {
                        if let Some((width, height)) = svg_dimensions_from_html_attrs(attrs) {
                            add_pixels(
                                "inline SVG",
                                width.saturating_mul(height),
                                self.limits,
                                self.combined_pixels,
                            )?;
                        }
                    }
                }
                if matches!(tag, "script" | "style" | "template" | "noscript") {
                    self.removed = true;
                    return Ok(());
                }
                if tag == "input" {
                    return self.write_task_checkbox(attrs);
                }
                if matches!(
                    tag,
                    "iframe"
                        | "frame"
                        | "frameset"
                        | "object"
                        | "embed"
                        | "form"
                        | "input"
                        | "button"
                        | "select"
                        | "option"
                        | "textarea"
                        | "meta"
                        | "link"
                        | "base"
                        | "video"
                        | "audio"
                        | "source"
                        | "picture"
                ) {
                    self.removed = true;
                    return self.write_children(node);
                }
                if tag == "img" {
                    return self.write_image(attrs, node);
                }
                if tag == "a" {
                    return self.write_anchor(attrs, node);
                }
                if matches!(tag, "html" | "body") {
                    return self.write_children(node);
                }
                if !is_safe_html_tag(tag) {
                    self.removed = true;
                    return self.write_children(node);
                }
                if tag == "pre" && contains_unrendered_mermaid(node) {
                    self.output.push_str("<aside class=\"mermaid-warning\" role=\"note\">Diagram could not be rendered; source follows.</aside>");
                }
                self.output.push('<');
                self.output.push_str(tag);
                let attrs = attrs.borrow();
                let generated_marker =
                    generated_footnote_marker(&attrs, self.policy.footnote_marker);
                for attribute in attrs.iter() {
                    let attribute_name = attribute.name.local.as_ref();
                    if attribute_name.starts_with("data-export-footnote-")
                        || (generated_marker.is_some() && matches!(attribute_name, "id" | "href"))
                    {
                        continue;
                    }
                    self.write_attribute(tag, attribute_name, attribute.value.as_ref())?;
                }
                if let Some(marker) = generated_marker {
                    self.write_generated_footnote_attribute(marker);
                }
                self.output.push('>');
                let entering_code = tag == "code";
                if entering_code {
                    self.code_depth += 1;
                }
                let children_result = self.write_children(node);
                if entering_code {
                    self.code_depth -= 1;
                }
                children_result?;
                if !is_void_html_tag(tag) {
                    self.output.push_str("</");
                    self.output.push_str(tag);
                    self.output.push('>');
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn write_children(&mut self, node: &Handle) -> Result<(), ExportDocumentError> {
        for child in node.children.borrow().iter() {
            self.write_node(child)?;
        }
        Ok(())
    }

    fn write_image(
        &mut self,
        attrs: &std::cell::RefCell<Vec<html5ever::Attribute>>,
        _node: &Handle,
    ) -> Result<(), ExportDocumentError> {
        let attrs = attrs.borrow();
        let rendered_src = attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "src")
            .map(|attr| attr.value.to_string())
            .unwrap_or_default();
        let original_src = self.image_sources.pop_front().flatten();
        let src = if rendered_src.is_empty() {
            original_src.clone().unwrap_or_default()
        } else {
            rendered_src.clone()
        };
        let alt = attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "alt")
            .map(|attr| attr.value.as_ref())
            .unwrap_or("image");
        if let Some(filename) = self
            .policy
            .rewrites
            .resolve(original_src.as_deref(), &rendered_src)
        {
            self.output.push_str("<img src=\"");
            self.output.push_str(filename);
            self.output.push_str("\" alt=\"");
            self.output.push_escaped_attribute(alt);
            self.output.push_str("\">");
            return Ok(());
        }
        let omitted = original_src
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(rendered_src.as_str()))
            .flat_map(source_aliases)
            .any(|source| self.policy.external_omissions.contains(&source));
        if is_external_url(&src) || omitted {
            self.output
                .push_str("<span class=\"export-omission\" role=\"note\">External image omitted");
            if !alt.is_empty() {
                self.output.push_str(": ");
                self.output.push_escaped_text(alt);
            }
            self.output.push_str("</span>");
            return Ok(());
        }
        if src.is_empty() {
            self.removed = true;
            self.output.push_str("<span class=\"export-omission\" role=\"note\">Image omitted by export safety policy</span>");
            return Ok(());
        }
        Err(ExportDocumentError::AssetNotInManifest {
            destination: original_src.unwrap_or(src),
        })
    }

    fn write_task_checkbox(
        &mut self,
        attrs: &std::cell::RefCell<Vec<html5ever::Attribute>>,
    ) -> Result<(), ExportDocumentError> {
        let attrs = attrs.borrow();
        let is_checkbox = attrs.iter().any(|attribute| {
            attribute.name.local.as_ref() == "type"
                && attribute.value.as_ref().eq_ignore_ascii_case("checkbox")
        });
        let is_disabled = attrs
            .iter()
            .any(|attribute| attribute.name.local.as_ref() == "disabled");
        let has_unsafe_attribute = attrs.iter().any(|attribute| {
            let name = attribute.name.local.as_ref().to_ascii_lowercase();
            name.starts_with("on") || !matches!(name.as_str(), "type" | "disabled" | "checked")
        });
        if !is_checkbox || !is_disabled || has_unsafe_attribute {
            self.removed = true;
            return Ok(());
        }
        self.output
            .push_str("<input type=\"checkbox\" disabled=\"\"");
        if attrs
            .iter()
            .any(|attribute| attribute.name.local.as_ref() == "checked")
        {
            self.output.push_str(" checked=\"\"");
        }
        self.output.push('>');
        Ok(())
    }

    fn write_anchor(
        &mut self,
        attrs: &std::cell::RefCell<Vec<html5ever::Attribute>>,
        node: &Handle,
    ) -> Result<(), ExportDocumentError> {
        let attrs = attrs.borrow();
        if let Some(
            marker @ (GeneratedFootnoteMarker::Target(_) | GeneratedFootnoteMarker::Backref(_)),
        ) = generated_footnote_marker(&attrs, self.policy.footnote_marker)
        {
            self.output.push_str("<a");
            for attribute in attrs.iter() {
                let name = attribute.name.local.as_ref();
                if name.starts_with("data-export-footnote-") || name == "href" {
                    continue;
                }
                self.write_attribute("a", name, attribute.value.as_ref())?;
            }
            self.write_generated_footnote_attribute(marker);
            self.output.push('>');
            self.write_children(node)?;
            self.output.push_str("</a>");
            return Ok(());
        }
        let href = attrs
            .iter()
            .find(|attribute| attribute.name.local.as_ref() == "href")
            .map(|attribute| attribute.value.to_string())
            .unwrap_or_default();
        if is_app_relative(&href) && !href.starts_with('#') {
            self.output
                .push_str("<span class=\"attachment-link\" data-app-path=\"");
            self.output.push_escaped_attribute(&href);
            self.output.push_str("\">");
            self.write_children(node)?;
            self.output.push_str(" <span class=\"attachment-path\">(");
            self.output.push_escaped_text(&href);
            self.output.push_str(")</span></span>");
            return Ok(());
        }
        self.output.push_str("<a");
        for attribute in attrs.iter() {
            self.write_attribute("a", attribute.name.local.as_ref(), attribute.value.as_ref())?;
        }
        self.output.push('>');
        self.write_children(node)?;
        self.output.push_str("</a>");
        Ok(())
    }

    fn write_attribute(
        &mut self,
        tag: &str,
        name: &str,
        value: &str,
    ) -> Result<(), ExportDocumentError> {
        let lower_name = name.to_ascii_lowercase();
        if lower_name.starts_with("on")
            || matches!(
                lower_name.as_str(),
                "srcset" | "formaction" | "action" | "poster"
            )
        {
            self.removed = true;
            return Ok(());
        }
        if lower_name == "style" {
            if let Some(style) = safe_style(value) {
                self.push_attribute("style", &style);
            } else {
                self.removed = true;
            }
            return Ok(());
        }
        if value.to_ascii_lowercase().contains("url(")
            && !value.trim().to_ascii_lowercase().starts_with("url(#")
        {
            self.removed = true;
            return Ok(());
        }
        if lower_name == "href" && tag == "a" {
            if value.starts_with('#') || is_safe_hyperlink(value) {
                self.push_attribute("href", value);
            } else if is_app_relative(value) {
                self.push_attribute("data-app-path", value);
            } else {
                self.removed = true;
            }
            return Ok(());
        }
        if is_safe_html_attribute(&lower_name) {
            self.push_attribute(&lower_name, value);
        } else {
            self.removed = true;
        }
        Ok(())
    }

    fn push_attribute(&mut self, name: &str, value: &str) {
        let name = canonical_svg_attribute_name(name);
        self.output.push(' ');
        self.output.push_str(name);
        self.output.push_str("=\"");
        self.output.push_escaped_attribute(value);
        self.output.push('"');
    }

    fn write_generated_footnote_attribute(&mut self, marker: GeneratedFootnoteMarker<'_>) {
        match marker {
            GeneratedFootnoteMarker::Definition(index) => {
                self.push_attribute("id", &format!("{}-def-{index}", self.footnote_namespace))
            }
            GeneratedFootnoteMarker::Reference(reference) => self.push_attribute(
                "id",
                &format!("{}-ref-{reference}", self.footnote_namespace),
            ),
            GeneratedFootnoteMarker::Target(index) => {
                self.push_attribute("href", &format!("#{}-def-{index}", self.footnote_namespace))
            }
            GeneratedFootnoteMarker::Backref(reference) => self.push_attribute(
                "href",
                &format!("#{}-ref-{reference}", self.footnote_namespace),
            ),
        }
    }
}

fn contains_unrendered_mermaid(node: &Handle) -> bool {
    node.children.borrow().iter().any(|child| {
        let NodeData::Element { name, attrs, .. } = &child.data else {
            return false;
        };
        name.local.as_ref() == "code"
            && attrs.borrow().iter().any(|attribute| {
                attribute.name.local.as_ref() == "class"
                    && attribute
                        .value
                        .split_ascii_whitespace()
                        .any(|class| class == "language-mermaid")
            })
    })
}

fn canonical_svg_attribute_name(name: &str) -> &str {
    match name {
        "viewbox" => "viewBox",
        "markerwidth" => "markerWidth",
        "markerheight" => "markerHeight",
        "refx" => "refX",
        "refy" => "refY",
        _ => name,
    }
}

fn svg_dimensions_from_html_attrs(
    attrs: &std::cell::RefCell<Vec<html5ever::Attribute>>,
) -> Option<(u64, u64)> {
    let attrs = attrs.borrow();
    let attribute = |name: &str| {
        attrs
            .iter()
            .find(|attribute| attribute.name.local.as_ref().eq_ignore_ascii_case(name))
            .map(|attribute| attribute.value.to_string())
    };
    let parse = |value: &str| value.trim_end_matches("px").parse::<f64>().ok();
    let dimensions = attribute("width")
        .as_deref()
        .and_then(parse)
        .zip(attribute("height").as_deref().and_then(parse))
        .or_else(|| {
            let view_box = attribute("viewBox")?;
            let values = view_box
                .split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            (values.len() == 4).then(|| (values[2], values[3]))
        })?;
    (dimensions.0.is_finite()
        && dimensions.1.is_finite()
        && dimensions.0 > 0.0
        && dimensions.1 > 0.0)
        .then(|| (dimensions.0.ceil() as u64, dimensions.1.ceil() as u64))
}

fn is_safe_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "abbr"
            | "b"
            | "blockquote"
            | "br"
            | "code"
            | "del"
            | "details"
            | "div"
            | "em"
            | "figcaption"
            | "figure"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "i"
            | "kbd"
            | "li"
            | "mark"
            | "ol"
            | "p"
            | "pre"
            | "s"
            | "section"
            | "small"
            | "span"
            | "strong"
            | "sub"
            | "summary"
            | "sup"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "u"
            | "ul"
            | "svg"
            | "g"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "defs"
            | "marker"
            | "clippath"
            | "lineargradient"
            | "radialgradient"
            | "stop"
            | "title"
            | "desc"
    )
}

fn is_void_html_tag(tag: &str) -> bool {
    matches!(tag, "br" | "hr")
}

fn is_safe_html_attribute(name: &str) -> bool {
    matches!(
        name,
        "id" | "class"
            | "title"
            | "alt"
            | "role"
            | "aria-hidden"
            | "aria-label"
            | "aria-checked"
            | "checked"
            | "disabled"
            | "type"
            | "data-language"
            | "colspan"
            | "rowspan"
            | "scope"
            | "start"
            | "open"
            | "width"
            | "height"
            | "viewbox"
            | "x"
            | "y"
            | "x1"
            | "y1"
            | "x2"
            | "y2"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
            | "d"
            | "points"
            | "fill"
            | "fill-opacity"
            | "stroke"
            | "stroke-width"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-dasharray"
            | "opacity"
            | "transform"
            | "offset"
            | "stop-color"
            | "stop-opacity"
            | "markerwidth"
            | "markerheight"
            | "refx"
            | "refy"
            | "orient"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "clip-path"
            | "text-anchor"
            | "font-size"
            | "font-weight"
    )
}

fn safe_style(value: &str) -> Option<String> {
    let lowered = value.to_ascii_lowercase();
    if [
        "\\",
        "url(",
        "@",
        "expression(",
        "javascript:",
        "data:",
        "http:",
        "https:",
        "image-set(",
        "cross-fade(",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return None;
    }
    let all_properties_allowed = value.split(';').all(|declaration| {
        let declaration = declaration.trim();
        if declaration.is_empty() {
            return true;
        }
        declaration
            .split_once(':')
            .is_some_and(|(property, _)| is_safe_style_property(property.trim()))
    });
    all_properties_allowed.then(|| value.to_owned())
}

fn is_safe_style_property(property: &str) -> bool {
    matches!(
        property.to_ascii_lowercase().as_str(),
        "display"
            | "align-items"
            | "gap"
            | "white-space"
            | "vertical-align"
            | "position"
            | "flex"
            | "width"
            | "height"
            | "overflow"
            | "border"
            | "border-radius"
            | "background"
            | "background-color"
            | "background-image"
            | "background-size"
            | "box-shadow"
            | "stroke-width"
            | "left"
            | "right"
            | "top"
            | "bottom"
            | "inset"
            | "color"
            | "font-weight"
    )
}

fn is_external_url(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")
}

fn is_safe_hyperlink(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

fn is_app_relative(value: &str) -> bool {
    !value.is_empty() && !value.starts_with("//") && !value.contains(':')
}

fn export_notices(external_omitted: bool, removed: bool) -> String {
    let mut notices = String::new();
    if external_omitted {
        notices.push_str("<aside class=\"export-notice\" role=\"note\">External images were omitted from this export.</aside>");
    }
    if removed {
        notices.push_str("<aside class=\"export-removal-notice\" role=\"note\">Unsafe content removed by the export safety policy.</aside>");
    }
    notices
}

struct PreparedFootnotes {
    body_markdown: String,
    footnotes: Vec<PreparedFootnote>,
    marker_token: String,
}

struct PreparedFootnote {
    markdown: String,
    reference_ids: Vec<String>,
}

fn prepare_footnotes(markdown: &str) -> Result<PreparedFootnotes, ExportDocumentError> {
    prepare_footnotes_with_nonce_source(markdown, || Uuid::new_v4().simple().to_string())
}

fn prepare_footnotes_with_nonce_source(
    markdown: &str,
    mut nonce_source: impl FnMut() -> String,
) -> Result<PreparedFootnotes, ExportDocumentError> {
    validate_author_html(markdown)?;
    let marker_token = reserve_footnote_marker(markdown, &mut nonce_source)?;
    let mut definitions = HashMap::<String, (Range<usize>, String)>::new();
    let mut references = Vec::<(String, Range<usize>)>::new();
    for (event, range) in Parser::new_ext(markdown, Options::ENABLE_FOOTNOTES).into_offset_iter() {
        match event {
            Event::Start(Tag::FootnoteDefinition(label)) => {
                definitions.insert(
                    label.to_string(),
                    (
                        range.clone(),
                        footnote_definition_markdown(&markdown[range]),
                    ),
                );
            }
            Event::FootnoteReference(label) => references.push((label.to_string(), range)),
            _ => {}
        }
    }

    let mut label_indexes = HashMap::<String, usize>::new();
    let mut footnotes = Vec::<PreparedFootnote>::new();
    let mut definition_ranges = definitions
        .values()
        .map(|(range, _)| range.clone())
        .collect::<Vec<_>>();
    definition_ranges.sort_by_key(|range| range.start);
    let mut edits = definitions
        .values()
        .map(|(range, _)| (range.clone(), String::new()))
        .collect::<Vec<_>>();
    for (label, range) in references {
        let enclosing_definition = definition_ranges
            .partition_point(|definition| definition.start <= range.start)
            .checked_sub(1)
            .and_then(|index| definition_ranges.get(index))
            .is_some_and(|definition| range.end <= definition.end);
        if enclosing_definition {
            continue;
        }
        let Some((_, definition)) = definitions.get(&label) else {
            continue;
        };
        let index = *label_indexes.entry(label).or_insert_with(|| {
            let index = footnotes.len();
            footnotes.push(PreparedFootnote {
                markdown: definition.clone(),
                reference_ids: Vec::new(),
            });
            index
        });
        let reference_id = format!(
            "{:04}-{}",
            index + 1,
            footnotes[index].reference_ids.len() + 1
        );
        footnotes[index].reference_ids.push(reference_id.clone());
        edits.push((
            range,
            format!(
                "<sup class=\"footnote-reference\" data-export-footnote-marker=\"{marker_token}\" data-export-footnote-reference=\"{reference_id}\"><a data-export-footnote-marker=\"{marker_token}\" data-export-footnote-target=\"{:04}\">[{}]</a></sup>",
                index + 1,
                index + 1
            ),
        ));
    }
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));
    let mut body_markdown = markdown.to_owned();
    for (range, replacement) in edits {
        body_markdown.replace_range(range, &replacement);
    }
    Ok(PreparedFootnotes {
        body_markdown,
        footnotes,
        marker_token,
    })
}

fn reserve_footnote_marker(
    markdown: &str,
    nonce_source: &mut impl FnMut() -> String,
) -> Result<String, ExportDocumentError> {
    const MAX_ATTEMPTS: usize = 8;
    let forbidden = author_html_attribute_values(markdown)?;
    for _ in 0..MAX_ATTEMPTS {
        let nonce = nonce_source();
        if !nonce.is_empty()
            && nonce.len() <= 64
            && nonce
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !forbidden.contains(&nonce)
        {
            return Ok(nonce);
        }
    }
    Err(ExportDocumentError::FootnoteMarkerUnavailable)
}

fn validate_author_html(markdown: &str) -> Result<(), ExportDocumentError> {
    for event in Parser::new_ext(markdown, Options::ENABLE_FOOTNOTES) {
        if let Event::Html(fragment) | Event::InlineHtml(fragment) = event {
            parse_checked_html_fragment(fragment.as_ref())?;
        }
    }
    Ok(())
}

fn author_html_attribute_values(markdown: &str) -> Result<HashSet<String>, ExportDocumentError> {
    let mut values = HashSet::new();
    for event in Parser::new_ext(markdown, Options::ENABLE_FOOTNOTES) {
        if let Event::Html(fragment) | Event::InlineHtml(fragment) = event {
            let dom = parse_checked_html_fragment(fragment.as_ref())?;
            collect_dom_attribute_values(&dom.document, &mut values);
        }
    }
    Ok(values)
}

fn collect_dom_attribute_values(node: &Handle, values: &mut HashSet<String>) {
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { attrs, .. } = &node.data {
            values.extend(
                attrs
                    .borrow()
                    .iter()
                    .map(|attribute| attribute.value.to_string()),
            );
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
}

fn footnote_definition_markdown(source: &str) -> String {
    let mut lines = source.split('\n');
    let first = lines.next().unwrap_or("");
    let mut markdown = first
        .split_once("]:")
        .map(|(_, body)| body.trim_start().to_owned())
        .unwrap_or_default();
    for line in lines {
        markdown.push('\n');
        markdown.push_str(
            line.strip_prefix("    ")
                .or_else(|| line.strip_prefix('\t'))
                .unwrap_or(line),
        );
    }
    markdown
}

fn render_with_existing_contract(markdown: &str) -> String {
    // WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#11
    yew_duskmoon::render_markdown_to_html_with_options(
        markdown,
        yew_duskmoon::DmMarkdownOptions::default(),
    )
}

fn render_footnote_definitions(prepared: &PreparedFootnotes, html: &mut CappedString) {
    if prepared.footnotes.is_empty() {
        return;
    }
    html.push_str("<section class=\"footnotes\"><ol>");
    for (index, footnote) in prepared.footnotes.iter().enumerate() {
        html.push_str("<li class=\"footnote-definition\" data-export-footnote-marker=\"");
        html.push_str(&prepared.marker_token);
        html.push_str("\" data-export-footnote-definition=\"");
        html.push_str(&format!("{:04}", index + 1));
        html.push_str("\">");
        html.push_str(&render_with_existing_contract(&footnote.markdown));
        html.push_str("<span class=\"footnote-backrefs\">");
        for (usage, reference_id) in footnote.reference_ids.iter().enumerate() {
            html.push_str(" <a data-export-footnote-marker=\"");
            html.push_str(&prepared.marker_token);
            html.push_str("\" data-export-footnote-backref=\"");
            html.push_str(reference_id);
            html.push_str("\" aria-label=\"Back to reference ");
            html.push_str(&(usage + 1).to_string());
            html.push_str("\">↩");
            if usage != 0 {
                html.push_str(&(usage + 1).to_string());
            }
            html.push_str("</a>");
        }
        html.push_str("</span></li>");
    }
    html.push_str("</ol></section>");
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(value: &str) -> String {
    escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_stops_growing_when_escaped_output_exceeds_cap() {
        let rewrites = RewriteManifest::default();
        let external_omissions = HashSet::new();
        let policy = HtmlPolicy {
            rewrites: &rewrites,
            external_omissions: &external_omissions,
            original_image_sources: &[],
            footnote_marker: "unused",
        };
        let limits = ExportDocumentLimits {
            max_generated_html_bytes: 8,
            ..ExportDocumentLimits::default()
        };
        let mut combined_pixels = 0;

        assert!(matches!(
            sanitize_html("&amp;&amp;", &policy, limits, &mut combined_pixels),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
    }

    #[test]
    fn final_wrapper_stops_before_appending_over_limit_title_or_body() {
        assert!(matches!(
            assemble_index_html("&".repeat(32).as_str(), "", "body", PRINT_CSS.len() + 128),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
        assert!(matches!(
            assemble_index_html("title", "", &"x".repeat(256), PRINT_CSS.len() + 128),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
    }

    #[test]
    fn syntax_aware_preflight_accepts_large_plain_text_within_final_html_limit() {
        let markdown = "a".repeat(1_500 * 1024);
        let prepared = prepare_footnotes(&markdown).unwrap();

        assert!(
            preflight_generated_html(&prepared, "title", ExportDocumentLimits::default()).is_ok()
        );
    }

    #[test]
    fn syntax_aware_preflight_rejects_high_expansion_highlighted_code() {
        let markdown = format!("```rust\n{}\n```", "fn ".repeat(220_000));
        assert!((600 * 1024..=700 * 1024).contains(&markdown.len()));
        let prepared = prepare_footnotes(&markdown).unwrap();

        assert!(matches!(
            preflight_generated_html(&prepared, "title", ExportDocumentLimits::default()),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
    }

    #[test]
    fn renderer_preflight_is_an_upper_bound_for_payload_heavy_markdown() {
        let ampersands = "&".repeat(16_384);
        let corpus = [
            format!("[label](https://example.test/{ampersands} \"{ampersands}\")"),
            format!("![alt](image.png \"{ampersands}\")"),
            format!("# heading {{#identifier .class key=\"{ampersands}\"}}"),
            format!("raw <span title=\"{ampersands}\">&</span> and `#fff`"),
            format!("---\nkey: {ampersands}\n---\nbody"),
        ];

        for markdown in corpus {
            let estimate = estimate_renderer_html(&markdown).unwrap();
            let rendered = render_with_existing_contract(&markdown);
            assert!(
                estimate >= rendered.len(),
                "estimate {estimate} was smaller than rendered {} bytes for corpus entry",
                rendered.len()
            );
        }
    }

    #[test]
    fn renderer_preflight_rejects_repeated_inline_color_chip_expansion() {
        let markdown = "`#fff` ".repeat(10_000);
        let rendered = render_with_existing_contract(&markdown);
        assert!(rendered.len() > DEFAULT_MAX_GENERATED_HTML_BYTES);
        let prepared = prepare_footnotes(&markdown).unwrap();

        assert!(matches!(
            preflight_generated_html(&prepared, "title", ExportDocumentLimits::default()),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
    }

    #[test]
    fn renderer_preflight_rejects_sub_two_mib_ampersand_link_title() {
        let markdown = format!("[label](destination \"{}\")", "&".repeat(1_700_000));
        assert!(markdown.len() < 2 * 1024 * 1024);
        let prepared = prepare_footnotes(&markdown).unwrap();

        assert!(matches!(
            preflight_generated_html(&prepared, "title", ExportDocumentLimits::default()),
            Err(ExportDocumentError::GeneratedHtmlLimitExceeded)
        ));
    }

    #[test]
    fn deeply_nested_author_html_is_rejected_before_recursive_processing() {
        let boundary = format!("{}content{}", "<div>".repeat(128), "</div>".repeat(128));
        let markdown = format!("{}content{}", "<div>".repeat(256), "</div>".repeat(256));

        assert!(prepare_footnotes(&boundary).is_ok());
        assert!(matches!(
            prepare_footnotes(&markdown),
            Err(ExportDocumentError::MarkupDepthLimitExceeded)
        ));
    }

    #[test]
    fn parsed_markup_node_count_is_bounded_by_iterative_validation() {
        let source = "<i></i>".repeat(MAX_MARKUP_NODES + 1);
        assert!(source.len() < 2 * 1024 * 1024);
        let dom = parse_fragment(
            RcDom::default(),
            Default::default(),
            QualName::new(None, ns!(html), local_name!("div")),
            vec![],
            true,
        )
        .one(source);

        assert!(matches!(
            validate_html_tree(&dom.document),
            Err(ExportDocumentError::MarkupNodeLimitExceeded)
        ));
    }

    #[test]
    fn nonce_reservation_rejects_html_entity_decoded_author_marker() {
        let markdown = r#"<div data-export-footnote-marker="nonce-&#48;"></div>

reference[^one]

[^one]: body"#;
        let mut candidates = ["nonce-0".to_owned(), "safe-nonce-1".to_owned()].into_iter();
        let prepared = prepare_footnotes_with_nonce_source(markdown, || {
            candidates.next().expect("bounded nonce candidate")
        })
        .unwrap();

        assert_eq!(prepared.marker_token, "safe-nonce-1");
    }
}
