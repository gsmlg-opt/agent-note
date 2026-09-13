use base64::{engine::general_purpose::STANDARD, Engine as _};
use html5ever::{local_name, ns, parse_fragment, tendril::TendrilSink, QualName};
use image::{guess_format, ImageFormat, ImageReader};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use note_pipelines::{ExportAssetKind, FrozenNoteExport};
use percent_encoding::percent_decode_str;
use roxmltree::{Document, Node};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fmt, io::Cursor};

const PRINT_CSS: &str = include_str!("print.css");
const DEFAULT_MAX_PIXELS_PER_IMAGE: u64 = 20_000_000;
const DEFAULT_MAX_COMBINED_PIXELS: u64 = 80_000_000;
const DEFAULT_MAX_GENERATED_HTML_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_SVG_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_SVG_ELEMENTS: usize = 10_000;
const DEFAULT_MAX_SVG_ATTRIBUTES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportDocumentLimits {
    pub max_pixels_per_image: u64,
    pub max_combined_pixels: u64,
    pub max_generated_html_bytes: usize,
    pub max_svg_bytes: usize,
    pub max_svg_elements: usize,
    pub max_svg_attributes: usize,
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
    let mut prepared_markdown = prepare_footnotes(&frozen.note.content);

    let mut assets = Vec::new();
    let mut rewrites = HashMap::new();
    let mut combined_pixels = 0_u64;
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
        let filename = asset_filename(assets.len() + 1, validated.extension);
        rewrites.insert(hydrated.destination.clone(), filename.clone());
        rewrites.insert(normalize_relative(&hydrated.destination), filename.clone());
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
        let filename = asset_filename(assets.len() + 1, validated.extension);
        // The shared renderer intentionally blanks data image URLs. Substitute an inert local
        // marker before rendering so the structural HTML rewriter can bind it to validated bytes.
        let marker = format!("export-data-image-{}", assets.len() + 1);
        prepared_markdown = replace_outside_fences(&prepared_markdown, data_url, &marker);
        rewrites.insert(marker, filename.clone());
        assets.push(PackagedExportAsset {
            filename,
            mime: validated.mime.into(),
            bytes: validated.bytes,
        });
    }

    // WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#11
    let rendered = yew_duskmoon::render_markdown_to_html_with_options(
        &prepared_markdown,
        yew_duskmoon::DmMarkdownOptions::default(),
    );
    if rendered.len() > limits.max_generated_html_bytes {
        return Err(ExportDocumentError::GeneratedHtmlLimitExceeded);
    }

    let external_omissions = frozen
        .omissions
        .iter()
        .filter(|omission| omission.kind == ExportAssetKind::External)
        .map(|omission| omission.destination.as_str())
        .collect::<Vec<_>>();
    let policy = HtmlPolicy {
        rewrites: &rewrites,
        external_omissions: &external_omissions,
    };
    let SanitizedHtml {
        html: body,
        removed,
    } = sanitize_html(&rendered, &policy, limits, &mut combined_pixels)?;
    let title = escape_text(&frozen.note.title);
    let notices = export_notices(!external_omissions.is_empty(), removed);
    let index_html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"light\"><title>{title}</title><style>{PRINT_CSS}</style></head><body><main class=\"export-document markdown-body\"><h1 class=\"export-title\">{title}</h1>{notices}{body}</main></body></html>"
    );
    if index_html.len() > limits.max_generated_html_bytes {
        return Err(ExportDocumentError::GeneratedHtmlLimitExceeded);
    }
    Ok(ExportDocumentPackage {
        index_html,
        footer_html: "<!doctype html><html><head><style>body{width:100%;margin:0;color:#5f6368;font:8pt \"Noto Sans\",\"DejaVu Sans\",sans-serif}div{text-align:center}</style></head><body><div><span class=\"pageNumber\"></span> / <span class=\"totalPages\"></span></div></body></html>".into(),
        assets,
    })
}

fn validate_limits(limits: ExportDocumentLimits) -> Result<(), ExportDocumentError> {
    if limits.max_combined_pixels < limits.max_pixels_per_image
        || limits.max_generated_html_bytes == 0
        || limits.max_svg_bytes == 0
        || limits.max_svg_elements == 0
        || limits.max_svg_attributes == 0
    {
        return Err(ExportDocumentError::InvalidLimits);
    }
    Ok(())
}

struct ValidatedImage {
    extension: &'static str,
    mime: &'static str,
    bytes: Vec<u8>,
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
    ImageReader::with_format(Cursor::new(bytes), actual)
        .decode()
        .map_err(|_| ExportDocumentError::InvalidImage {
            destination: destination.into(),
        })?;
    let (extension, canonical_mime) = match actual {
        ImageFormat::Png => ("png", "image/png"),
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::WebP => ("webp", "image/webp"),
        ImageFormat::Gif => ("gif", "image/gif"),
        _ => unreachable!("format restricted above"),
    };
    Ok(ValidatedImage {
        extension,
        mime: canonical_mime,
        bytes: bytes.to_vec(),
    })
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
    let document = Document::parse(source).map_err(|_| ExportDocumentError::InvalidImage {
        destination: destination.into(),
    })?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(ExportDocumentError::InvalidImage {
            destination: destination.into(),
        });
    }
    let element_count = document.descendants().filter(Node::is_element).count();
    let attribute_count = document
        .descendants()
        .map(|node| node.attributes().len())
        .sum::<usize>();
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

struct HtmlPolicy<'a> {
    rewrites: &'a HashMap<String, String>,
    external_omissions: &'a [&'a str],
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
    let dom = parse_fragment(
        RcDom::default(),
        Default::default(),
        QualName::new(None, ns!(html), local_name!("div")),
        vec![],
        true,
    )
    .one(rendered);
    let mut state = HtmlSanitizer {
        output: String::new(),
        removed: false,
        policy,
        limits,
        combined_pixels,
        svg_elements: 0,
        svg_attributes: 0,
        code_depth: 0,
    };
    for child in dom.document.children.borrow().iter() {
        state.write_node(child)?;
    }
    Ok(SanitizedHtml {
        html: state.output,
        removed: state.removed,
    })
}

struct HtmlSanitizer<'a> {
    output: String,
    removed: bool,
    policy: &'a HtmlPolicy<'a>,
    limits: ExportDocumentLimits,
    combined_pixels: &'a mut u64,
    svg_elements: usize,
    svg_attributes: usize,
    code_depth: usize,
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
                self.output.push_str(&escape_text(&contents));
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
                for attribute in attrs.borrow().iter() {
                    self.write_attribute(
                        tag,
                        attribute.name.local.as_ref(),
                        attribute.value.as_ref(),
                    )?;
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
        let src = attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "src")
            .map(|attr| attr.value.to_string())
            .unwrap_or_default();
        let alt = attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "alt")
            .map(|attr| attr.value.as_ref())
            .unwrap_or("image");
        if let Some(filename) = self
            .policy
            .rewrites
            .get(&src)
            .or_else(|| self.policy.rewrites.get(&normalize_relative(&src)))
        {
            self.output.push_str("<img src=\"");
            self.output.push_str(filename);
            self.output.push_str("\" alt=\"");
            self.output.push_str(&escape_attribute(alt));
            self.output.push_str("\">");
            return Ok(());
        }
        if is_external_url(&src)
            || self
                .policy
                .external_omissions
                .iter()
                .any(|omitted| **omitted == src)
        {
            self.output
                .push_str("<span class=\"export-omission\" role=\"note\">External image omitted");
            if !alt.is_empty() {
                self.output.push_str(": ");
                self.output.push_str(&escape_text(alt));
            }
            self.output.push_str("</span>");
            return Ok(());
        }
        if src.is_empty() {
            self.removed = true;
            self.output.push_str("<span class=\"export-omission\" role=\"note\">Image omitted by export safety policy</span>");
            return Ok(());
        }
        Err(ExportDocumentError::AssetNotInManifest { destination: src })
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
        let href = attrs
            .iter()
            .find(|attribute| attribute.name.local.as_ref() == "href")
            .map(|attribute| attribute.value.to_string())
            .unwrap_or_default();
        if is_app_relative(&href) && !href.starts_with('#') {
            self.output
                .push_str("<span class=\"attachment-link\" data-app-path=\"");
            self.output.push_str(&escape_attribute(&href));
            self.output.push_str("\">");
            self.write_children(node)?;
            self.output.push_str(" <span class=\"attachment-path\">(");
            self.output.push_str(&escape_text(&href));
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
        self.output.push_str(&escape_attribute(value));
        self.output.push('"');
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

fn prepare_footnotes(markdown: &str) -> String {
    let mut body = Vec::new();
    let mut definitions = Vec::<(String, String)>::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence {
            if let Some(rest) = line.strip_prefix("[^") {
                if let Some((id, text)) = rest.split_once("]: ") {
                    definitions.push((id.to_owned(), text.to_owned()));
                    continue;
                }
            }
        }
        body.push(line.to_owned());
    }
    if definitions.is_empty() {
        return markdown.to_owned();
    }
    let mut output = body.join("\n");
    for (index, (id, _)) in definitions.iter().enumerate() {
        let reference = format!("[^{id}]");
        let replacement = format!(
            "<sup class=\"footnote-reference\"><a href=\"#fn-{id}\" id=\"fnref-{id}\">{}</a></sup>",
            index + 1
        );
        output = replace_outside_fences(&output, &reference, &replacement);
    }
    output.push_str("\n\n<section class=\"footnotes\"><ol>");
    for (id, text) in definitions {
        output.push_str("<li class=\"footnote-definition\" id=\"fn-");
        output.push_str(&escape_attribute(&id));
        output.push_str("\">");
        output.push_str(&escape_text(&text));
        output.push_str(" <a href=\"#fnref-");
        output.push_str(&escape_attribute(&id));
        output.push_str("\" aria-label=\"Back to reference\">↩</a></li>");
    }
    output.push_str("</ol></section>");
    output
}

fn replace_outside_fences(markdown: &str, needle: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for (index, line) in markdown.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            in_fence = !in_fence;
        }
        if in_fence {
            output.push_str(line);
        } else {
            output.push_str(&line.replace(needle, replacement));
        }
    }
    output
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
