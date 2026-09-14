use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{Delay, DynamicImage, Frame, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use note_core::{Note, NoteAttachment};
use note_pipelines::{FrozenNoteExport, HydratedNoteExportAsset};
use note_server::export::document::{build_export_document, ExportDocumentLimits};
use note_server::export::renderer::{GotenbergRenderer, PdfRenderer};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const PDF_NAME: &str = "agent-note-export.pdf";
const TEXT_NAME: &str = "agent-note-export.txt";
const PREVIEW_PREFIX: &str = "agent-note-export-page";

#[tokio::test]
async fn qualifies_a_real_renderer_and_writes_review_artifacts() {
    if env::var("NOTE_TEST_REAL_PDF").as_deref() != Ok("1") {
        eprintln!(
            "skipped real PDF qualification; set NOTE_TEST_REAL_PDF=1 to require the renderer and fixture tools"
        );
        return;
    }

    let renderer_url = required_env("NOTE_TEST_GOTENBERG_URL");
    let artifact_dir = PathBuf::from(required_env("NOTE_TEST_PDF_ARTIFACT_DIR"));
    let qpdf = required_tool("NOTE_TEST_QPDF", "qpdf");
    let pdfinfo = required_tool("NOTE_TEST_PDFINFO", "pdfinfo");
    let pdftotext = required_tool("NOTE_TEST_PDFTOTEXT", "pdftotext");
    let pdftoppm = required_tool("NOTE_TEST_PDFTOPPM", "pdftoppm");
    let identify = required_tool("NOTE_TEST_IDENTIFY", "identify");

    fs::create_dir_all(&artifact_dir).expect("create PDF qualification artifact directory");
    clear_old_artifacts(&artifact_dir);

    let frozen = representative_export();
    let package = build_export_document(&frozen, ExportDocumentLimits::default())
        .expect("build representative export document");
    assert!(
        package.assets.len() >= 6,
        "all fixture image kinds must be packaged"
    );
    assert!(package.index_html.contains("dm-mermaid-chart"));
    assert!(!package.index_html.contains("Unsafe content removed"));
    assert!(!package.index_html.contains("https://"));
    assert!(!package.index_html.contains("http://"));

    let renderer = GotenbergRenderer::new(&renderer_url, 32 * 1024 * 1024)
        .expect("construct configured Gotenberg renderer");
    let pdf = renderer
        .render(&package, Duration::from_secs(35))
        .await
        .expect("render representative PDF with the configured real renderer");
    let pdf_path = artifact_dir.join(PDF_NAME);
    fs::write(&pdf_path, pdf).expect("write representative PDF");

    run_checked(&qpdf, [OsStr::new("--check"), pdf_path.as_os_str()]);
    let info = run_checked(&pdfinfo, [&pdf_path.as_os_str()]);
    let info = String::from_utf8(info.stdout).expect("pdfinfo output is UTF-8");
    let pages = pdf_page_count(&info);
    assert!(
        pages >= 3,
        "fixture must exercise multi-page layout; pdfinfo:\n{info}"
    );
    assert!(
        info.lines()
            .any(|line| line.starts_with("Page size:") && line.contains("A4")),
        "CSS page size must resolve to A4; pdfinfo:\n{info}"
    );

    let text_path = artifact_dir.join(TEXT_NAME);
    run_checked(
        &pdftotext,
        [
            OsStr::new("-layout"),
            pdf_path.as_os_str(),
            text_path.as_os_str(),
        ],
    );
    let extracted = fs::read_to_string(&text_path).expect("read extracted PDF text");
    for needle in [
        "Agent Note PDF qualification",
        "English searchable sentence",
        "中文可搜索正文",
        "日本語の検索可能な本文",
        "fn render_export",
        "Qualification footnote text",
        "Row 260",
    ] {
        assert!(
            extracted.contains(needle),
            "missing searchable PDF text: {needle}"
        );
    }

    let preview_prefix = artifact_dir.join(PREVIEW_PREFIX);
    run_checked(
        &pdftoppm,
        [
            OsStr::new("-png"),
            OsStr::new("-r"),
            OsStr::new("120"),
            pdf_path.as_os_str(),
            preview_prefix.as_os_str(),
        ],
    );
    let previews = preview_paths(&artifact_dir);
    assert_eq!(previews.len(), pages, "every PDF page must have a preview");
    for preview in &previews {
        let output = run_checked(
            &identify,
            [
                OsStr::new("-colorspace"),
                OsStr::new("Gray"),
                OsStr::new("-format"),
                OsStr::new("%[fx:standard_deviation]"),
                preview.as_os_str(),
            ],
        );
        let deviation: f64 = String::from_utf8(output.stdout)
            .expect("ImageMagick output is UTF-8")
            .trim()
            .parse()
            .expect("ImageMagick returned a numeric standard deviation");
        assert!(
            deviation > 0.001,
            "blank PDF preview: {}",
            preview.display()
        );
    }

    eprintln!(
        "real PDF qualification wrote {} pages to {}",
        pages,
        artifact_dir.display()
    );
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required when NOTE_TEST_REAL_PDF=1"))
}

fn required_tool(variable: &str, default: &str) -> OsString {
    let tool = env::var_os(variable).unwrap_or_else(|| OsString::from(default));
    Command::new(&tool)
        .output()
        .unwrap_or_else(|error| panic!("required fixture tool {:?} is unavailable: {error}", tool));
    tool
}

fn run_checked<I, S>(program: &OsStr, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_os_string())
        .collect::<Vec<_>>();
    let output = Command::new(program)
        .args(&arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {:?}: {error}", program));
    assert!(
        output.status.success(),
        "{:?} {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        program,
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn clear_old_artifacts(directory: &Path) {
    let entries = fs::read_dir(directory).expect("read PDF qualification artifact directory");
    for entry in entries {
        let path = entry.expect("read PDF artifact entry").path();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if name == PDF_NAME
            || name == TEXT_NAME
            || (name.starts_with(PREVIEW_PREFIX) && name.ends_with(".png"))
        {
            fs::remove_file(&path).expect("remove stale PDF qualification artifact");
        }
    }
}

fn pdf_page_count(info: &str) -> usize {
    info.lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|value| value.trim().parse().ok())
        .expect("pdfinfo output includes a numeric page count")
}

fn preview_paths(directory: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(directory)
        .expect("read preview directory")
        .map(|entry| entry.expect("read preview entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(PREVIEW_PREFIX) && name.ends_with(".png"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn representative_export() -> FrozenNoteExport {
    let data_png = raster(ImageFormat::Png, Rgb([72, 128, 196]));
    let data_url = format!("data:image/png;base64,{}", STANDARD.encode(&data_png));
    let table = (1..=260)
        .map(|row| format!("| Row {row} | 中文 {row} | 日本語 {row} |"))
        .collect::<Vec<_>>()
        .join("\n");
    let long_line = "long-line-".repeat(48);
    let markdown = format!(
        r#"# Agent Note PDF qualification

English searchable sentence. 中文可搜索正文。日本語の検索可能な本文。

[Jump to details](#details) and a footnote.[^quality]

```rust
fn render_export(note_revision: i64) -> &'static str {{ "searchable code" }}
```

`{long_line}`

![PNG](fixture.png) ![JPEG](fixture.jpg) ![WebP](fixture.webp)

![Animated GIF first frame](fixture.gif) ![Sanitized SVG](fixture.svg)

![Embedded data image]({data_url})

```mermaid
flowchart LR
    Snapshot --> Sanitize
    Sanitize --> Render
    Render --> Validate
```

## Details

| Sequence | Chinese | Japanese |
| --- | --- | --- |
{table}

[^quality]: Qualification footnote text with a backlink.
"#
    );
    let assets = vec![
        asset("fixture.png", "image/png", raster(ImageFormat::Png, Rgb([190, 55, 70]))),
        asset("fixture.jpg", "image/jpeg", raster(ImageFormat::Jpeg, Rgb([50, 145, 90]))),
        asset("fixture.webp", "image/webp", raster(ImageFormat::WebP, Rgb([150, 85, 190]))),
        asset("fixture.gif", "image/gif", animated_gif()),
        asset(
            "fixture.svg",
            "image/svg+xml",
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="240" height="80" viewBox="0 0 240 80"><rect width="240" height="80" rx="12" fill="#dbeafe"/><text x="18" y="48" font-size="24" fill="#1e3a8a">Safe local SVG</text></svg>"##.to_vec(),
        ),
    ];
    FrozenNoteExport {
        note: Note {
            id: "pdf-qualification-note".into(),
            title: "Agent Note PDF qualification".into(),
            content: markdown,
            attachments: assets
                .iter()
                .map(|value| value.attachment.clone())
                .collect(),
            labels: vec![],
            created_at: 1,
            updated_at: 1,
            revision: 7,
            deleted_at: None,
        },
        assets,
        omissions: vec![],
        data_images: vec![data_url],
    }
}

fn asset(destination: &str, mime: &str, bytes: Vec<u8>) -> HydratedNoteExportAsset {
    HydratedNoteExportAsset {
        destination: destination.into(),
        attachment: NoteAttachment {
            id: format!("fixture-{destination}"),
            path: destination.into(),
            mime: mime.into(),
            description: format!("Qualification {destination}"),
            content: vec![],
            storage: None,
        },
        bytes,
    }
}

fn raster(format: ImageFormat, color: Rgb<u8>) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(RgbImage::from_pixel(240, 96, color))
        .write_to(&mut bytes, format)
        .expect("encode fixture raster");
    bytes.into_inner()
}

fn animated_gif() -> Vec<u8> {
    use image::codecs::gif::GifEncoder;

    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        let first = Frame::from_parts(
            RgbaImage::from_pixel(240, 96, Rgba([235, 155, 45, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(80, 1),
        );
        let second = Frame::from_parts(
            RgbaImage::from_pixel(240, 96, Rgba([40, 110, 210, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(80, 1),
        );
        encoder
            .encode_frames([first, second])
            .expect("encode animated GIF fixture");
    }
    bytes
}
