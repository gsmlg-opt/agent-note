use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
use pulldown_cmark_to_cmark::cmark;
use yew::prelude::*;
use yew_duskmoon::{Alert, Card, Chip, DmMarkdown};
use yew_router::prelude::*;

use crate::api;
use crate::routes::Route;
use crate::state::NoteSummary;

#[derive(Properties, PartialEq)]
pub struct NoteShowProps {
    pub id: String,
}

/// Read-only view of a single note.
#[function_component(NoteShowPage)]
pub fn note_show_page(props: &NoteShowProps) -> Html {
    let note = use_state(|| None::<NoteSummary>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    {
        let note = note.clone();
        let loading = loading.clone();
        let error = error.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            loading.set(true);
            note.set(None);
            error.set(None);
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

    html! {
        <section class="stack">
            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }
            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(n) = &*note {
                <Card title={Some(html! { <span>{ n.title.clone() }</span> })}>
                    <div class="note-page-actions">
                        <Link<Route> to={Route::Notes} classes={classes!("btn","btn-ghost")}>
                            { "Back" }
                        </Link<Route>>
                        <Link<Route> to={Route::NoteEdit { id: n.id.clone() }}
                            classes={classes!("btn","btn-primary")}>
                            { "Edit note" }
                        </Link<Route>>
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
                    <DmMarkdown markdown={AttrValue::from(note_markdown(n))} />
                    if !n.attachments.is_empty() {
                        <section class="attachment-view-section">
                            <h3>{ "Attachments" }</h3>
                            <div class="attachment-view-list">
                                { for n.attachments.iter().map(|attachment| html! {
                                    <section class="attachment-view-item" key={attachment.id.clone()}>
                                        <div class="attachment-view-header">
                                            <strong>{ attachment.path.clone() }</strong>
                                            <span>{ attachment.mime.clone() }</span>
                                        </div>
                                        if !attachment.description.is_empty() {
                                            <p class="attachment-description">{ attachment.description.clone() }</p>
                                        }
                                        <pre>{ attachment.content.clone() }</pre>
                                    </section>
                                }) }
                            </div>
                        </section>
                    }
                </Card>
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
        </section>
    }
}

fn note_markdown(note: &NoteSummary) -> String {
    let attachment_base = format!("/api/notes/{}/attachments", note.id);
    rewrite_attachment_urls(&note.content, &attachment_base)
}

// WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#4
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
            dest_url: rewrite_relative_url(dest_url, attachment_base),
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
            dest_url: rewrite_relative_url(dest_url, attachment_base),
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

fn rewrite_relative_url<'a>(url: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    let Some(path) = url.strip_prefix("./") else {
        return url;
    };

    CowStr::Boxed(format!("{}/{path}", attachment_base.trim_end_matches('/')).into_boxed_str())
}

fn rewrite_relative_html_urls<'a>(html: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    if !html.contains("href=\"./") && !html.contains("src=\"./") {
        return html;
    }

    let base = attachment_base.trim_end_matches('/');
    CowStr::Boxed(
        html.replace("href=\"./", &format!("href=\"{base}/"))
            .replace("src=\"./", &format!("src=\"{base}/"))
            .into_boxed_str(),
    )
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
    use super::rewrite_attachment_urls;
    use yew_duskmoon::render_markdown_to_html;

    const BASE: &str = "/api/notes/note-1/attachments";

    #[test]
    fn rewrites_relative_link_image_and_html_urls() {
        let markdown = r#"[meta](./meta.json "metadata")

![diagram][diagram]

[diagram]: ./images/diagram.svg

<a href="./raw.txt">raw</a>
<img src="./raw.svg">"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);

        assert!(rewritten.contains("/api/notes/note-1/attachments/meta.json"));
        assert!(rewritten.contains("/api/notes/note-1/attachments/images/diagram.svg"));
        assert!(rewritten.contains("href=\"/api/notes/note-1/attachments/raw.txt\""));
        assert!(rewritten.contains("src=\"/api/notes/note-1/attachments/raw.svg\""));

        let html = render_markdown_to_html(&rewritten);
        assert!(html.contains("href=\"/api/notes/note-1/attachments/meta.json\""));
        assert!(html.contains("src=\"/api/notes/note-1/attachments/images/diagram.svg\""));
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
        let html = render_markdown_to_html(&rewritten);

        assert!(html.contains(r#"class="dm-color-code""#));
        assert!(html.contains(r#"class="dm-color-chip""#));
        assert!(html.contains(r#"style="background-color:#0065FF""#));
    }
}
