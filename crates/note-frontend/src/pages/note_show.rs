use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::{Alert, Card, Chip};

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
                    <p class="note-content">{ n.content.clone() }</p>
                    if !n.labels.is_empty() {
                        <div class="applied-labels">
                            { for n.labels.iter().map(|(k, v)| html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{k}: {v}") }</span>
                                </Chip>
                            }) }
                        </div>
                    }
                    <div class="modal-actions">
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
