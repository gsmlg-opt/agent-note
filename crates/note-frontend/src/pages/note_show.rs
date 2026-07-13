use yew::prelude::*;
use yew_duskmoon::{Alert, Card, Chip};
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
    let rendered_content = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    {
        let note = note.clone();
        let rendered_content = rendered_content.clone();
        let loading = loading.clone();
        let error = error.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            loading.set(true);
            note.set(None);
            rendered_content.set(None);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::get_note(&id).await {
                    Ok(n) => match api::render_markdown(
                        &n.content,
                        Some(&format!("/api/notes/{}/attachments", n.id)),
                    )
                    .await
                    {
                        Ok(html) => {
                            rendered_content.set(Some(html));
                            note.set(Some(n));
                        }
                        Err(e) => error.set(Some(e)),
                    },
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
            } else if let (Some(n), Some(rendered)) = (&*note, &*rendered_content) {
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
                    <div class="markdown-body">
                        { Html::from_html_unchecked(AttrValue::from(rendered.clone())) }
                    </div>
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
