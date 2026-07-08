use pulldown_cmark::{Event, Options, Parser};
use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::{Alert, Card, Chip};

use crate::api;
use crate::routes::Route;
use crate::state::NoteSummary;

/// Render a note's Markdown body to HTML for display. Raw HTML embedded in the Markdown is
/// dropped (not passed through), so note content — which can be written by any MCP client on the
/// unauthenticated endpoint — cannot inject arbitrary markup/scripts. Standard Markdown formatting
/// (headings, lists, code, tables, task lists, strikethrough) still renders.
fn render_markdown(md: &str) -> Html {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, options)
        .filter(|ev| !matches!(ev, Event::Html(_) | Event::InlineHtml(_)));
    let mut html_out = String::new();
    pulldown_cmark::html::push_html(&mut html_out, parser);
    Html::from_html_unchecked(AttrValue::from(html_out))
}

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
                    <div class="markdown-body">{ render_markdown(&n.content) }</div>
                    if !n.labels.is_empty() {
                        <div class="applied-labels">
                            { for n.labels.iter().map(|(k, v)| html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{k}: {v}") }</span>
                                </Chip>
                            }) }
                        </div>
                    }
                    <div class="app-modal-actions">
                        <Link<Route> to={Route::Notes} classes={classes!("btn","btn-ghost")}>
                            { "Back" }
                        </Link<Route>>
                        <Link<Route> to={Route::NoteEdit { id: n.id.clone() }}
                            classes={classes!("btn","btn-primary")}>
                            { "Edit note" }
                        </Link<Route>>
                    </div>
                </Card>
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
        </section>
    }
}
