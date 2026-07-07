use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::Alert;

use crate::api;
use crate::components::NoteEditor;
use crate::routes::Route;
use crate::state::NoteSummary;

#[derive(Properties, PartialEq)]
pub struct NoteEditProps {
    pub id: String,
}

/// Edit an existing note: fetch it, prefill the editor, and PUT the changes on save.
#[function_component(NoteEditPage)]
pub fn note_edit_page(props: &NoteEditProps) -> Html {
    let navigator = use_navigator().expect("router navigator");
    let note = use_state(|| None::<NoteSummary>);
    let available_labels = use_state(Vec::<(String, String)>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    {
        let note = note.clone();
        let available_labels = available_labels.clone();
        let loading = loading.clone();
        let error = error.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                // Load the note and the label suggestions together.
                match api::get_note(&id).await {
                    Ok(n) => note.set(Some(n)),
                    Err(e) => error.set(Some(e)),
                }
                if let Ok(keys) = api::list_labels().await {
                    available_labels.set(keys.into_iter().map(|k| (k.key, k.description)).collect());
                }
                loading.set(false);
            });
            || ()
        });
    }

    let on_submit = {
        let navigator = navigator.clone();
        let error = error.clone();
        let id = props.id.clone();
        Callback::from(
            move |(title, content, labels): (String, String, Vec<(String, String)>)| {
                let navigator = navigator.clone();
                let error = error.clone();
                let id = id.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match api::update_note(&id, &title, &content, &labels).await {
                        Ok(()) => navigator.push(&Route::NoteShow { id }),
                        Err(e) => error.set(Some(e)),
                    }
                });
            },
        )
    };

    html! {
        <section class="stack">
            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }
            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(n) = &*note {
                <NoteEditor
                    available_labels={(*available_labels).clone()}
                    initial_title={n.title.clone()}
                    initial_content={n.content.clone()}
                    initial_labels={n.labels.clone()}
                    card_title="Edit note"
                    submit_label="Save changes"
                    {on_submit}
                />
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
        </section>
    }
}
