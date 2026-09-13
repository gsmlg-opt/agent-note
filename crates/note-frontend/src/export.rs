use note_core::{export_filename, ExportFormat};

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
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMenuDecision {
    OpenAt(usize),
    Focus(usize),
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
        ExportMenuKey::Escape => ExportMenuDecision::CloseAndRestoreFocus,
        ExportMenuKey::Enter | ExportMenuKey::Space if focused_item == 0 => {
            ExportMenuDecision::DownloadMarkdownAndRestoreFocus
        }
        _ => ExportMenuDecision::Stay,
    }
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

#[cfg(not(target_arch = "wasm32"))]
pub fn initiate_browser_download(_download: &PreparedMarkdownDownload) -> Result<(), String> {
    Err("Browser downloads are only available in the Wasm application.".to_string())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::{
        export_menu_decision, initiate_captured_markdown_download, prepare_markdown_download,
        ExportMenuDecision, ExportMenuKey,
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
    fn note_page_exposes_the_accessible_export_menu_contract() {
        let source = include_str!("pages/note_show.rs");

        assert!(source.contains(r#"aria-haspopup="menu""#));
        assert!(source.contains(r#"role="menu""#));
        assert!(source.contains(r#"role="menuitem""#));
        assert!(source.contains(r#"aria-disabled="true""#));
        assert!(source.contains("PDF export is not configured yet."));
    }
}
