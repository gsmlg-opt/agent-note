use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::Alert;

use crate::api;
use crate::components::NoteEditor;
use crate::routes::Route;

/// Dedicated page for composing a note. On save it returns to the note list.
#[function_component(NewNotePage)]
pub fn new_note_page() -> Html {
    let navigator = use_navigator().expect("router navigator");
    // Registered (key, description) pairs used to populate the label-key suggestions.
    let available_labels = use_state(Vec::<(String, String)>::new);
    let error = use_state(|| None::<String>);

    {
        let available_labels = available_labels.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(keys) = api::list_labels().await {
                    available_labels
                        .set(keys.into_iter().map(|k| (k.key, k.description)).collect());
                }
            });
            || ()
        });
    }

    let on_submit = {
        let navigator = navigator.clone();
        let error = error.clone();
        Callback::from(
            move |(title, content, labels): (String, String, Vec<(String, String)>)| {
                let navigator = navigator.clone();
                let error = error.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match api::save_note(&title, &content, &labels).await {
                        Ok(_) => navigator.push(&Route::Notes),
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
            <NoteEditor available_labels={(*available_labels).clone()} {on_submit} />
        </section>
    }
}
