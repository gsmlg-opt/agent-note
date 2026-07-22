use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
use pulldown_cmark_to_cmark::cmark;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use yew::prelude::*;
use yew_duskmoon::{Alert, Card, Chip, DmMarkdown};
use yew_router::prelude::*;

use crate::api;
use crate::components::Modal;
use crate::routes::{NotesQueryParams, Route};
use crate::state::{AttachmentContent, NoteSummary};

#[derive(Properties, PartialEq)]
pub struct NoteShowProps {
    pub id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CopyStatus {
    #[default]
    Ready,
    Copied,
    Failed,
}

impl CopyStatus {
    fn action_label(self) -> &'static str {
        match self {
            Self::Ready => "Copy",
            Self::Copied => "Copied",
            Self::Failed => "Copy failed",
        }
    }
}

fn copy_chip_text(id: &str, status: CopyStatus) -> String {
    format!("ID: {id} · {}", status.action_label())
}

fn copy_announcement(status: CopyStatus) -> &'static str {
    match status {
        CopyStatus::Ready => "",
        CopyStatus::Copied => "Note ID copied to clipboard.",
        CopyStatus::Failed => "Unable to copy note ID.",
    }
}

fn delete_confirmation_message(title: &str) -> String {
    format!("Move the note \u{201c}{title}\u{201d} to Trash? You can restore it later.")
}

fn browser_clipboard() -> Option<web_sys::Clipboard> {
    let navigator = web_sys::window()?.navigator();
    let clipboard = js_sys::Reflect::get(
        navigator.as_ref(),
        &wasm_bindgen::JsValue::from_str("clipboard"),
    )
    .ok()?;

    if clipboard.is_null() || clipboard.is_undefined() {
        None
    } else {
        Some(clipboard.unchecked_into())
    }
}

/// Read-only view of a single note.
#[function_component(NoteShowPage)]
pub fn note_show_page(props: &NoteShowProps) -> Html {
    let navigator = use_navigator().expect("router navigator");
    let notes_query = use_location().and_then(|location| location.query::<NotesQueryParams>().ok());
    let note = use_state(|| None::<NoteSummary>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let copy_status = use_state(CopyStatus::default);
    let delete_open = use_state(|| false);

    {
        let note = note.clone();
        let loading = loading.clone();
        let error = error.clone();
        let copy_status = copy_status.clone();
        let delete_open = delete_open.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            loading.set(true);
            note.set(None);
            error.set(None);
            copy_status.set(CopyStatus::Ready);
            delete_open.set(false);
            wasm_bindgen_futures::spawn_local(async move {
                match api::get_note(&id).await {
                    Ok(n) => note.set(Some(n)),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
            || ()
        });
    }

    let copy_id = (*note)
        .as_ref()
        .map(|loaded_note| loaded_note.id.clone())
        .unwrap_or_default();
    let on_copy_id = {
        let id = copy_id;
        let copy_status = copy_status.clone();
        Callback::from(move |_| {
            let id = id.clone();
            let copy_status = copy_status.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let copied = match browser_clipboard() {
                    Some(clipboard) => JsFuture::from(clipboard.write_text(&id)).await.is_ok(),
                    None => false,
                };
                copy_status.set(if copied {
                    CopyStatus::Copied
                } else {
                    CopyStatus::Failed
                });
            });
        })
    };

    let delete_modal = match (*delete_open, (*note).clone()) {
        (true, Some(note)) => {
            let on_close = {
                let delete_open = delete_open.clone();
                Callback::from(move |_: ()| delete_open.set(false))
            };
            let on_cancel = {
                let delete_open = delete_open.clone();
                Callback::from(move |_: MouseEvent| delete_open.set(false))
            };
            let on_confirm = {
                let delete_open = delete_open.clone();
                let error = error.clone();
                let navigator = navigator.clone();
                let notes_query = notes_query.clone();
                let id = note.id.clone();
                Callback::from(move |_: MouseEvent| {
                    let delete_open = delete_open.clone();
                    let error = error.clone();
                    let navigator = navigator.clone();
                    let notes_query = notes_query.clone();
                    let id = id.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::delete_note(&id).await {
                            Ok(()) => {
                                delete_open.set(false);
                                if let Some(query) = notes_query {
                                    let _ = navigator.push_with_query(&Route::Notes, query);
                                } else {
                                    navigator.push(&Route::Notes);
                                }
                            }
                            Err(delete_error) => error.set(Some(delete_error)),
                        }
                    });
                })
            };

            html! {
                <Modal title="Move note to Trash" on_close={on_close}>
                    <p>{ delete_confirmation_message(&note.title) }</p>
                    <div class="app-modal-actions">
                        <button type="button" class="btn btn-ghost" onclick={on_cancel}>
                            { "Cancel" }
                        </button>
                        <button type="button" class="btn btn-error" onclick={on_confirm}>
                            { "Move to Trash" }
                        </button>
                    </div>
                </Modal>
            }
        }
        _ => html! {},
    };

    html! {
        <section class="stack">
            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }
            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(n) = &*note {
                <Card title={Some(html! { <span>{ n.title.clone() }</span> })}>
                    <div class="note-id-copy">
                        <button
                            type="button"
                            class="chip chip-clickable chip-primary note-id-copy-chip"
                            aria-label={format!("Copy note ID {}", n.id)}
                            onclick={on_copy_id}
                        >
                            <span class="note-id-copy-text">
                                { copy_chip_text(&n.id, *copy_status) }
                            </span>
                        </button>
                        <span class="sr-only" aria-live="polite" aria-atomic="true">
                            { copy_announcement(*copy_status) }
                        </span>
                    </div>
                    <div class="note-page-actions">
                        <Link<Route, NotesQueryParams>
                            to={Route::Notes}
                            query={notes_query.clone()}
                            classes={classes!("btn","btn-ghost")}
                        >
                            { "Back" }
                        </Link<Route, NotesQueryParams>>
                        <button
                            type="button"
                            class="btn btn-error"
                            onclick={{
                                let delete_open = delete_open.clone();
                                Callback::from(move |_| delete_open.set(true))
                            }}
                        >
                            { "Delete note" }
                        </button>
                        <Link<Route, NotesQueryParams>
                            to={Route::NoteEdit { id: n.id.clone() }}
                            query={notes_query.clone()}
                            classes={classes!("btn","btn-primary")}
                        >
                            { "Edit note" }
                        </Link<Route, NotesQueryParams>>
                    </div>
                    if !n.labels.is_empty() {
                        <div class="applied-labels">
                            { for n.labels.iter().map(|(k, v)| html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{k}: {v}") }</span>
                                </Chip>
                            }) }
                        </div>
                    }
                    <DmMarkdown
                        markdown={AttrValue::from(note_markdown(n))}
                        base_url={Some(format!("/api/notes/{}/attachments", n.id))}
                    />
                    if !n.attachments.is_empty() {
                        <section class="attachment-view-section">
                            <h3>{ "Attachments" }</h3>
                            <div class="attachment-view-list">
                                { for n.attachments.iter().map(|attachment| {
                                    let attachment_base = format!("/api/notes/{}/attachments", n.id);
                                    let download_url = api::attachment_url(&attachment_base, &attachment.path);
                                    html! {
                                        <section class="attachment-view-item" key={attachment.id.clone()}>
                                            <div class="attachment-view-header">
                                                <strong>{ attachment.path.clone() }</strong>
                                                <span>{ attachment.mime.clone() }</span>
                                            </div>
                                            if !attachment.description.is_empty() {
                                                <p class="attachment-description">{ attachment.description.clone() }</p>
                                            }
                                            { match &attachment.content {
                                                AttachmentContent::Text(content) => html! {
                                                    <pre>{ content.clone() }</pre>
                                                },
                                                AttachmentContent::Base64(_) => html! {
                                                    <div class="attachment-binary-actions">
                                                        <span>{ "Binary attachment" }</span>
                                                        <a class="btn btn-outline" href={download_url} download="">{ "Download" }</a>
                                                    </div>
                                                },
                                            } }
                                        </section>
                                    }
                                }) }
                            </div>
                        </section>
                    }
                </Card>
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
            { delete_modal }
        </section>
    }
}

fn note_markdown(note: &NoteSummary) -> String {
    let attachment_base = format!("/api/notes/{}/attachments", note.id);
    rewrite_attachment_urls(&note.content, &attachment_base)
}

// WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#8
fn rewrite_attachment_urls(markdown: &str, attachment_base: &str) -> String {
    let (front_matter, body) = split_front_matter(markdown);
    let events = Parser::new_ext(body, markdown_options()).map(|event| match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: rewrite_url_needing_encoding(dest_url, attachment_base),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: rewrite_url_needing_encoding(dest_url, attachment_base),
            title,
            id,
        }),
        Event::Html(html) => Event::Html(rewrite_relative_html_urls(html, attachment_base)),
        Event::InlineHtml(html) => {
            Event::InlineHtml(rewrite_relative_html_urls(html, attachment_base))
        }
        event => event,
    });

    let mut rewritten = String::from(front_matter);
    cmark(events, &mut rewritten).expect("parsed Markdown events must serialize");
    rewritten
}

fn split_front_matter(markdown: &str) -> (&str, &str) {
    let bom_len = if markdown.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let Some(opening_end) = markdown[bom_len..].find('\n').map(|end| bom_len + end) else {
        return ("", markdown);
    };
    if markdown[bom_len..opening_end].trim_end_matches('\r') != "---" {
        return ("", markdown);
    }

    let mut cursor = opening_end + 1;
    while cursor < markdown.len() {
        let line_end = markdown[cursor..]
            .find('\n')
            .map(|end| cursor + end)
            .unwrap_or(markdown.len());
        let line = markdown[cursor..line_end].trim_end_matches('\r');
        if matches!(line, "---" | "...") {
            let body_start = (line_end + 1).min(markdown.len());
            return markdown.split_at(body_start);
        }
        cursor = line_end + 1;
    }

    ("", markdown)
}

fn rewrite_url_needing_encoding<'a>(url: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    let Some(path) = url.strip_prefix("./") else {
        return url;
    };

    let encoded = api::attachment_url(attachment_base, path);
    let unencoded = format!("{}/{path}", attachment_base.trim_end_matches('/'));
    if encoded == unencoded {
        url
    } else {
        CowStr::Boxed(encoded.into_boxed_str())
    }
}

fn rewrite_relative_html_urls<'a>(html: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    if !html.contains("href=\"./") && !html.contains("src=\"./") {
        return html;
    }

    let rewritten = rewrite_relative_html_attribute(&html, "href", attachment_base);
    CowStr::Boxed(
        rewrite_relative_html_attribute(&rewritten, "src", attachment_base).into_boxed_str(),
    )
}

fn rewrite_relative_html_attribute(html: &str, attribute: &str, attachment_base: &str) -> String {
    let marker = format!(r#"{attribute}="./"#);
    let mut remaining = html;
    let mut rewritten = String::with_capacity(html.len());

    while let Some(marker_start) = remaining.find(&marker) {
        let path_start = marker_start + marker.len();
        let Some(path_end) = remaining[path_start..].find('"') else {
            break;
        };
        let path_end = path_start + path_end;
        rewritten.push_str(&remaining[..marker_start]);
        rewritten.push_str(attribute);
        rewritten.push_str("=\"");
        rewritten.push_str(&api::attachment_url(
            attachment_base,
            &remaining[path_start..path_end],
        ));
        remaining = &remaining[path_end..];
    }

    rewritten.push_str(remaining);
    rewritten
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

#[cfg(test)]
mod tests {
    use super::{
        copy_announcement, copy_chip_text, delete_confirmation_message, rewrite_attachment_urls,
        CopyStatus,
    };
    use yew_duskmoon::{render_markdown_to_html_with_options, DmMarkdownOptions};

    const BASE: &str = "/api/notes/note-1/attachments";

    #[test]
    fn delete_confirmation_explains_that_the_note_can_be_restored() {
        assert_eq!(
            delete_confirmation_message("Release notes"),
            "Move the note \u{201c}Release notes\u{201d} to Trash? You can restore it later."
        );
    }

    #[test]
    fn copy_chip_initially_offers_to_copy_the_canonical_id() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Ready),
            "ID: note-123 · Copy"
        );
        assert_eq!(copy_announcement(CopyStatus::Ready), "");
    }

    #[test]
    fn copy_chip_reports_success() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Copied),
            "ID: note-123 · Copied"
        );
        assert_eq!(
            copy_announcement(CopyStatus::Copied),
            "Note ID copied to clipboard."
        );
    }

    #[test]
    fn copy_chip_reports_failure() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Failed),
            "ID: note-123 · Copy failed"
        );
        assert_eq!(
            copy_announcement(CopyStatus::Failed),
            "Unable to copy note ID."
        );
    }

    #[test]
    fn rewrites_relative_link_image_and_html_urls() {
        let markdown = r#"[meta](./meta.json "metadata")

![diagram][diagram]

[diagram]: ./images/diagram.svg

<a href="./raw.txt">raw</a>
<img src="./raw.svg">"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);
        let html = render_markdown_to_html_with_options(
            &rewritten,
            DmMarkdownOptions {
                base_url: Some(BASE.to_string()),
                ..DmMarkdownOptions::default()
            },
        );

        assert!(rewritten.contains("[meta](./meta.json \"metadata\")"));
        assert!(rewritten.contains("[diagram]: ./images/diagram.svg"));
        assert!(rewritten.contains("href=\"/api/notes/note-1/attachments/raw.txt\""));
        assert!(rewritten.contains("src=\"/api/notes/note-1/attachments/raw.svg\""));
        assert!(html.contains("href=\"/api/notes/note-1/attachments/meta.json\""));
        assert!(html.contains("src=\"/api/notes/note-1/attachments/images/diagram.svg\""));
    }

    #[test]
    fn percent_encodes_reserved_characters_in_attachment_paths() {
        let markdown = r#"[report](./reports/progress#1?value=50%.txt)

<img src="./images/status #1?value=50%.png">"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);

        assert!(rewritten
            .contains("/api/notes/note-1/attachments/reports/progress%231%3Fvalue%3D50%25.txt"));
        assert!(rewritten.contains(
            "src=\"/api/notes/note-1/attachments/images/status%20%231%3Fvalue%3D50%25.png\""
        ));
    }

    #[test]
    fn preserves_other_urls_code_and_front_matter() {
        let markdown = r#"---
title: Example
---

[external](https://example.com)
[root](/docs)
[anchor](#top)
[mail](mailto:test@example.com)

`./inline.txt`

```text
./code.txt
```"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);

        assert!(rewritten.starts_with("---\ntitle: Example\n---"));
        assert!(rewritten.contains("https://example.com"));
        assert!(rewritten.contains("/docs"));
        assert!(rewritten.contains("#top"));
        assert!(rewritten.contains("mailto:test@example.com"));
        assert!(rewritten.contains("`./inline.txt`"));
        assert!(rewritten.contains("./code.txt"));
    }

    #[test]
    fn preserves_inline_css_colors_for_dm_markdown() {
        let rewritten = rewrite_attachment_urls(
            "| Token | Value |\n| --- | --- |\n| `primary` | `#0065FF` |",
            BASE,
        );
        let html = render_markdown_to_html_with_options(
            &rewritten,
            DmMarkdownOptions {
                base_url: Some(BASE.to_string()),
                ..DmMarkdownOptions::default()
            },
        );

        assert!(html.contains(r#"class="dm-color-code""#));
        assert!(html.contains(r#"class="dm-color-chip""#));
        assert!(html.contains("background-color:#0065FF;"));
        assert!(html.contains("display:inline-flex"));
    }
}
