use note_core::{export_filename, ExportFormat};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportCapabilitiesState {
    #[default]
    Loading,
    Available {
        pdf: bool,
    },
    Failed,
}

impl ExportCapabilitiesState {
    pub fn markdown_available(self) -> bool {
        true
    }

    pub fn pdf_available(self) -> bool {
        matches!(self, Self::Available { pdf: true })
    }

    pub fn pdf_description(self) -> &'static str {
        match self {
            Self::Loading => "Checking PDF availability…",
            Self::Available { pdf: true } => "Printable document",
            Self::Available { pdf: false } => "PDF export is not configured.",
            Self::Failed => "PDF availability could not be loaded.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfExportRequest {
    pub note_id: String,
    pub title: String,
    pub revision: i64,
    pub generation: u64,
    pub filename: String,
}

pub fn try_start_pdf_export(
    active: &mut Option<PdfExportRequest>,
    note_id: &str,
    title: &str,
    revision: i64,
    generation: u64,
) -> Option<PdfExportRequest> {
    if active.is_some() || revision <= 0 {
        return None;
    }
    let request = PdfExportRequest {
        note_id: note_id.to_owned(),
        title: title.to_owned(),
        revision,
        generation,
        filename: export_filename(title, note_id, ExportFormat::Pdf),
    };
    *active = Some(request.clone());
    Some(request)
}

pub fn invalidate_pdf_export(generation: &mut u64, active: &mut Option<PdfExportRequest>) -> bool {
    *generation = generation.saturating_add(1);
    active.take().is_some()
}

pub fn pdf_completion_is_current(
    request: &PdfExportRequest,
    generation: u64,
    note_id: &str,
    revision: i64,
) -> bool {
    request.generation == generation && request.note_id == note_id && request.revision == revision
}

pub fn pdf_response_is_downloadable(
    request: &PdfExportRequest,
    revision_header: Option<&str>,
    content_type: Option<&str>,
) -> bool {
    revision_header.and_then(|value| value.parse::<i64>().ok()) == Some(request.revision)
        && content_type
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct PdfErrorPayload {
    code: String,
    message: String,
    #[allow(dead_code)]
    details: serde_json::Value,
    retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfExportError {
    pub status: Option<u16>,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl PdfExportError {
    pub fn from_payload(status: u16, body: &str) -> Self {
        match serde_json::from_str::<PdfErrorPayload>(body) {
            Ok(payload) => Self {
                status: Some(status),
                code: payload.code,
                message: payload.message,
                retryable: payload.retryable,
            },
            Err(_) => Self {
                status: Some(status),
                code: "unexpected_response".into(),
                message: "The PDF export service returned an unexpected response.".into(),
                retryable: status >= 500 || status == 429,
            },
        }
    }

    pub fn transport() -> Self {
        Self {
            status: None,
            code: "transport_error".into(),
            message: "The PDF export service could not be reached.".into(),
            retryable: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfErrorAction {
    None,
    Retry,
    ReloadNote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedPdfError {
    pub message: String,
    pub action: PdfErrorAction,
}

pub fn classify_pdf_error(error: PdfExportError) -> ClassifiedPdfError {
    let action = if error.code == "stale_revision" {
        PdfErrorAction::ReloadNote
    } else if error.retryable {
        PdfErrorAction::Retry
    } else {
        PdfErrorAction::None
    };
    ClassifiedPdfError {
        message: error.message,
        action,
    }
}

pub const fn pdf_download_cleanup_delay_ms() -> u32 {
    1_000
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedMarkdownDownload {
    pub note_id: String,
    pub title: String,
    pub revision: i64,
    pub filename: String,
    pub bytes: Vec<u8>,
}

pub fn prepare_markdown_download(
    note_id: &str,
    title: &str,
    revision: i64,
    content: &str,
) -> PreparedMarkdownDownload {
    PreparedMarkdownDownload {
        note_id: note_id.to_string(),
        title: title.to_string(),
        revision,
        filename: export_filename(title, note_id, ExportFormat::Markdown),
        bytes: content.as_bytes().to_vec(),
    }
}

pub fn initiate_captured_markdown_download<F>(
    download: &PreparedMarkdownDownload,
    has_attachments: bool,
    initiate: F,
) -> String
where
    F: FnOnce(&PreparedMarkdownDownload) -> Result<(), String>,
{
    match initiate(download) {
        Ok(()) if has_attachments => {
            "Markdown download initiated. Attachment files are not included in this Markdown download."
                .to_string()
        }
        Ok(()) => "Markdown download initiated.".to_string(),
        Err(_) => "Unable to initiate Markdown download.".to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMenuKey {
    ArrowDown,
    ArrowUp,
    Home,
    End,
    Enter,
    Space,
    Escape,
    Tab,
    Other,
}

impl ExportMenuKey {
    pub fn from_key(key: &str) -> Self {
        match key {
            "ArrowDown" => Self::ArrowDown,
            "ArrowUp" => Self::ArrowUp,
            "Home" => Self::Home,
            "End" => Self::End,
            "Enter" => Self::Enter,
            " " => Self::Space,
            "Escape" => Self::Escape,
            "Tab" => Self::Tab,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMenuDecision {
    OpenAt(usize),
    Focus(usize),
    CloseWithoutRestoreFocus,
    CloseAndRestoreFocus,
    DownloadMarkdownAndRestoreFocus,
    Stay,
}

pub fn export_menu_decision(
    open: bool,
    focused_item: usize,
    key: ExportMenuKey,
) -> ExportMenuDecision {
    if !open {
        return match key {
            ExportMenuKey::ArrowUp => ExportMenuDecision::OpenAt(1),
            ExportMenuKey::ArrowDown | ExportMenuKey::Enter | ExportMenuKey::Space => {
                ExportMenuDecision::OpenAt(0)
            }
            _ => ExportMenuDecision::Stay,
        };
    }

    match key {
        ExportMenuKey::ArrowDown => ExportMenuDecision::Focus((focused_item + 1) % 2),
        ExportMenuKey::ArrowUp => ExportMenuDecision::Focus((focused_item + 1) % 2),
        ExportMenuKey::Home => ExportMenuDecision::Focus(0),
        ExportMenuKey::End => ExportMenuDecision::Focus(1),
        ExportMenuKey::Tab => ExportMenuDecision::CloseWithoutRestoreFocus,
        ExportMenuKey::Escape => ExportMenuDecision::CloseAndRestoreFocus,
        ExportMenuKey::Enter | ExportMenuKey::Space if focused_item == 0 => {
            ExportMenuDecision::DownloadMarkdownAndRestoreFocus
        }
        _ => ExportMenuDecision::Stay,
    }
}

pub fn export_menu_should_prevent_default(decision: ExportMenuDecision) -> bool {
    !matches!(
        decision,
        ExportMenuDecision::Stay | ExportMenuDecision::CloseWithoutRestoreFocus
    )
}

#[cfg(target_arch = "wasm32")]
pub fn initiate_browser_download(download: &PreparedMarkdownDownload) -> Result<(), String> {
    use gloo_file::{Blob, ObjectUrl};
    use gloo_timers::callback::Timeout;
    use wasm_bindgen::JsCast;

    let blob = Blob::new_with_options(
        download.bytes.as_slice(),
        Some("text/markdown;charset=utf-8"),
    );
    let object_url = ObjectUrl::from(blob);
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| "The browser document is unavailable.".to_string())?;
    let body = document
        .body()
        .ok_or_else(|| "The browser document body is unavailable.".to_string())?;
    let anchor = document
        .create_element("a")
        .map_err(|_| "Unable to create the download control.".to_string())?;
    anchor
        .set_attribute("href", &object_url)
        .and_then(|_| anchor.set_attribute("download", &download.filename))
        .and_then(|_| anchor.set_attribute("hidden", ""))
        .map_err(|_| "Unable to configure the download control.".to_string())?;
    body.append_child(&anchor)
        .map_err(|_| "Unable to prepare the download control.".to_string())?;
    let html_anchor = anchor
        .dyn_ref::<web_sys::HtmlElement>()
        .ok_or_else(|| "Unable to activate the download control.".to_string())?;
    html_anchor.click();

    // Give the browser time to consume the click before revoking the URL and removing its anchor.
    Timeout::new(1_000, move || {
        let _ = body.remove_child(&anchor);
        drop(object_url);
    })
    .forget();

    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn initiate_browser_pdf_download(
    download: &PdfExportRequest,
    bytes: &[u8],
) -> Result<(), String> {
    use gloo_file::{Blob, ObjectUrl};
    use gloo_timers::callback::Timeout;
    use wasm_bindgen::JsCast;

    let blob = Blob::new_with_options(bytes, Some("application/pdf"));
    let object_url = ObjectUrl::from(blob);
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| "The browser document is unavailable.".to_string())?;
    let body = document
        .body()
        .ok_or_else(|| "The browser document body is unavailable.".to_string())?;
    let anchor = document
        .create_element("a")
        .map_err(|_| "Unable to create the download control.".to_string())?;
    anchor
        .set_attribute("href", &object_url)
        .and_then(|_| anchor.set_attribute("download", &download.filename))
        .and_then(|_| anchor.set_attribute("hidden", ""))
        .map_err(|_| "Unable to configure the download control.".to_string())?;
    body.append_child(&anchor)
        .map_err(|_| "Unable to prepare the download control.".to_string())?;
    let html_anchor = anchor
        .dyn_ref::<web_sys::HtmlElement>()
        .ok_or_else(|| "Unable to activate the download control.".to_string())?;
    html_anchor.click();

    Timeout::new(pdf_download_cleanup_delay_ms(), move || {
        let _ = body.remove_child(&anchor);
        drop(object_url);
    })
    .forget();
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn initiate_browser_pdf_download(
    _download: &PdfExportRequest,
    _bytes: &[u8],
) -> Result<(), String> {
    Err("Browser downloads are only available in the Wasm application.".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn initiate_browser_download(_download: &PreparedMarkdownDownload) -> Result<(), String> {
    Err("Browser downloads are only available in the Wasm application.".to_string())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::{
        classify_pdf_error, export_menu_decision, export_menu_should_prevent_default,
        initiate_captured_markdown_download, invalidate_pdf_export, pdf_completion_is_current,
        pdf_download_cleanup_delay_ms, pdf_response_is_downloadable, prepare_markdown_download,
        try_start_pdf_export, ExportCapabilitiesState, ExportMenuDecision, ExportMenuKey,
        PdfErrorAction, PdfExportError, PdfExportRequest,
    };

    #[test]
    fn markdown_preparation_preserves_source_bytes_exactly() {
        let fixtures = [
            "",
            "\u{feff}---\r\ntitle: 例\r\n---\r\n",
            "# no final newline",
            "| a | b |\n| - | - |\n| 1 | 2 |\n",
            "```rust\nfn main() {}\n```\n<aside>raw</aside>\n",
            "  leading\n\ntrailing  \n",
        ];

        for source in fixtures {
            let prepared = prepare_markdown_download("note-1", "会議", 42, source);
            assert_eq!(prepared.note_id, "note-1");
            assert_eq!(prepared.title, "会議");
            assert_eq!(prepared.revision, 42);
            assert_eq!(prepared.bytes, source.as_bytes());
            assert_eq!(prepared.filename, "会議.md");
        }
    }

    #[test]
    fn captured_download_action_initiates_once_with_the_exact_prepared_snapshot() {
        let prepared = prepare_markdown_download(
            "note-42",
            "会議 / draft",
            17,
            "\u{feff}---\r\ntitle: exact\r\n---\r\nno final newline",
        );
        let recorded = Rc::new(RefCell::new(Vec::new()));
        let recorded_by_effect = recorded.clone();

        let announcement = initiate_captured_markdown_download(&prepared, true, move |captured| {
            recorded_by_effect.borrow_mut().push(captured.clone());
            Ok(())
        });

        assert_eq!(
            recorded.borrow().as_slice(),
            std::slice::from_ref(&prepared)
        );
        assert_eq!(recorded.borrow()[0].note_id, "note-42");
        assert_eq!(recorded.borrow()[0].title, "会議 / draft");
        assert_eq!(recorded.borrow()[0].revision, 17);
        assert_eq!(
            recorded.borrow()[0].bytes,
            "\u{feff}---\r\ntitle: exact\r\n---\r\nno final newline".as_bytes()
        );
        assert_eq!(recorded.borrow()[0].filename, "会議  draft.md");
        assert_eq!(
            announcement,
            "Markdown download initiated. Attachment files are not included in this Markdown download."
        );
    }

    #[test]
    fn captured_download_action_returns_success_without_an_attachment_warning() {
        let prepared = prepare_markdown_download("note-1", "Plain", 1, "body");

        assert_eq!(
            initiate_captured_markdown_download(&prepared, false, |_| Ok(())),
            "Markdown download initiated."
        );
    }

    #[test]
    fn captured_download_action_does_not_expose_effect_errors() {
        let prepared = prepare_markdown_download("note-1", "Plain", 1, "body");

        assert_eq!(
            initiate_captured_markdown_download(&prepared, true, |_| {
                Err("private browser detail".to_string())
            }),
            "Unable to initiate Markdown download."
        );
    }

    #[test]
    fn trigger_keys_open_the_menu_at_an_intentional_item() {
        assert_eq!(
            export_menu_decision(false, 0, ExportMenuKey::ArrowDown),
            ExportMenuDecision::OpenAt(0)
        );
        assert_eq!(
            export_menu_decision(false, 0, ExportMenuKey::ArrowUp),
            ExportMenuDecision::OpenAt(1)
        );
        assert_eq!(
            export_menu_decision(false, 0, ExportMenuKey::Enter),
            ExportMenuDecision::OpenAt(0)
        );
    }

    #[test]
    fn menu_keys_move_across_enabled_and_disabled_focusable_items() {
        assert_eq!(
            export_menu_decision(true, 0, ExportMenuKey::ArrowDown),
            ExportMenuDecision::Focus(1)
        );
        assert_eq!(
            export_menu_decision(true, 1, ExportMenuKey::ArrowDown),
            ExportMenuDecision::Focus(0)
        );
        assert_eq!(
            export_menu_decision(true, 1, ExportMenuKey::Home),
            ExportMenuDecision::Focus(0)
        );
        assert_eq!(
            export_menu_decision(true, 0, ExportMenuKey::End),
            ExportMenuDecision::Focus(1)
        );
        assert_eq!(
            export_menu_decision(true, 1, ExportMenuKey::Enter),
            ExportMenuDecision::Stay
        );
    }

    #[test]
    fn escape_and_markdown_selection_close_and_restore_trigger_focus() {
        assert_eq!(
            export_menu_decision(true, 0, ExportMenuKey::Escape),
            ExportMenuDecision::CloseAndRestoreFocus
        );
        assert_eq!(
            export_menu_decision(true, 0, ExportMenuKey::Enter),
            ExportMenuDecision::DownloadMarkdownAndRestoreFocus
        );
    }

    #[test]
    fn tab_closes_without_focus_restoration_or_preventing_native_traversal() {
        let decision = export_menu_decision(true, 0, ExportMenuKey::Tab);

        assert_eq!(decision, ExportMenuDecision::CloseWithoutRestoreFocus);
        assert!(!export_menu_should_prevent_default(decision));
        assert!(export_menu_should_prevent_default(
            ExportMenuDecision::CloseAndRestoreFocus
        ));
        assert!(export_menu_should_prevent_default(
            ExportMenuDecision::Focus(1)
        ));
    }

    #[test]
    fn note_page_exposes_the_accessible_export_menu_contract() {
        let source = include_str!("pages/note_show.rs");

        assert!(source.contains(r#"aria-haspopup="menu""#));
        assert!(source.contains(r#"role="menu""#));
        assert!(source.contains(r#"role="menuitem""#));
        assert!(source.contains("aria-disabled={"));
        assert!(source.contains("export_capabilities.pdf_description()"));
    }

    #[test]
    fn capability_states_keep_markdown_available_when_pdf_is_disabled_or_unknown() {
        assert!(ExportCapabilitiesState::Loading.markdown_available());
        assert!(!ExportCapabilitiesState::Loading.pdf_available());
        assert!(ExportCapabilitiesState::Failed.markdown_available());
        assert_eq!(
            ExportCapabilitiesState::Failed.pdf_description(),
            "PDF availability could not be loaded."
        );
        assert!(!ExportCapabilitiesState::Available { pdf: false }.pdf_available());
        assert_eq!(
            ExportCapabilitiesState::Available { pdf: false }.pdf_description(),
            "PDF export is not configured."
        );
        assert!(ExportCapabilitiesState::Available { pdf: true }.pdf_available());
    }

    #[test]
    fn pdf_start_captures_identity_once_and_suppresses_duplicates() {
        let mut active = None;
        let first = try_start_pdf_export(&mut active, "note-a", "Alpha", 7, 11).unwrap();

        assert_eq!(
            first,
            PdfExportRequest {
                note_id: "note-a".into(),
                title: "Alpha".into(),
                revision: 7,
                generation: 11,
                filename: "Alpha.pdf".into(),
            }
        );
        assert!(try_start_pdf_export(&mut active, "note-a", "Changed", 8, 11).is_none());
    }

    #[test]
    fn pdf_completion_requires_generation_identity_revision_header_and_mime() {
        let request = PdfExportRequest {
            note_id: "note-a".into(),
            title: "Alpha".into(),
            revision: 7,
            generation: 11,
            filename: "Alpha.pdf".into(),
        };

        assert!(pdf_completion_is_current(&request, 11, "note-a", 7));
        assert!(!pdf_completion_is_current(&request, 12, "note-a", 7));
        assert!(!pdf_completion_is_current(&request, 11, "note-b", 7));
        assert!(!pdf_completion_is_current(&request, 11, "note-a", 8));
        assert!(pdf_response_is_downloadable(
            &request,
            Some("7"),
            Some("application/pdf")
        ));
        assert!(pdf_response_is_downloadable(
            &request,
            Some("7"),
            Some("application/pdf; charset=binary")
        ));
        assert!(!pdf_response_is_downloadable(
            &request,
            None,
            Some("application/pdf")
        ));
        assert!(!pdf_response_is_downloadable(
            &request,
            Some("8"),
            Some("application/pdf")
        ));
        assert!(!pdf_response_is_downloadable(
            &request,
            Some("7"),
            Some("text/html")
        ));
    }

    #[test]
    fn typed_pdf_errors_offer_only_explicit_safe_actions() {
        let stale = classify_pdf_error(PdfExportError::from_payload(
            409,
            r#"{"code":"stale_revision","message":"changed","details":{},"retryable":false}"#,
        ));
        assert_eq!(stale.action, PdfErrorAction::ReloadNote);
        assert!(stale.message.contains("changed"));

        for (status, code) in [
            (429, "export_busy"),
            (502, "pdf_renderer_failed"),
            (503, "pdf_renderer_unavailable"),
            (504, "pdf_export_timeout"),
            (500, "export_storage_failure"),
        ] {
            let error = classify_pdf_error(PdfExportError::from_payload(
                status,
                &format!(r#"{{"code":"{code}","message":"safe","details":{{}},"retryable":true}}"#),
            ));
            assert_eq!(error.action, PdfErrorAction::Retry);
        }

        for (status, code) in [
            (413, "export_limit_exceeded"),
            (422, "export_asset_invalid"),
            (404, "note_not_found"),
            (503, "pdf_export_disabled"),
        ] {
            let error = classify_pdf_error(PdfExportError::from_payload(
                status,
                &format!(
                    r#"{{"code":"{code}","message":"safe","details":{{}},"retryable":false}}"#
                ),
            ));
            assert_eq!(error.action, PdfErrorAction::None);
        }

        let malformed = classify_pdf_error(PdfExportError::from_payload(502, "<private html>"));
        assert_eq!(malformed.action, PdfErrorAction::Retry);
        assert!(!malformed.message.contains("private html"));
    }

    #[test]
    fn browser_download_cleanup_waits_until_after_the_click_can_be_consumed() {
        assert!(pdf_download_cleanup_delay_ms() >= 1_000);
    }

    #[test]
    fn navigation_or_reload_invalidates_and_requests_abort_only_when_active() {
        let mut generation = 3;
        let mut active = Some(PdfExportRequest {
            note_id: "note-a".into(),
            title: "Alpha".into(),
            revision: 7,
            generation: 3,
            filename: "Alpha.pdf".into(),
        });

        assert!(invalidate_pdf_export(&mut generation, &mut active));
        assert_eq!(generation, 4);
        assert!(active.is_none());
        assert!(!invalidate_pdf_export(&mut generation, &mut active));
        assert_eq!(generation, 5);
    }
}
