mod api;
mod components;
mod state;

use yew::prelude::*;

use components::{NoteEditor, VectorSearch};
use state::{reduce, Action, AppState};

#[function_component(App)]
fn app() -> Html {
    let app_state = use_state(AppState::default);

    let on_submit = {
        let app_state = app_state.clone();
        Callback::from(move |(title, content, labels): (String, String, Vec<(String, String)>)| {
            let app_state = app_state.clone();
            wasm_bindgen_futures::spawn_local(async move {
                // No SaveSucceeded action exists in state.rs and a successful save
                // needs no state change; surface only failures into the error slot
                // (reusing SearchFailed) so the user isn't left guessing.
                if let Err(e) = api::save_note(&title, &content, &labels).await {
                    app_state.set(reduce(&app_state, Action::SearchFailed(e.to_string())));
                }
            });
        })
    };

    let on_query = {
        let app_state = app_state.clone();
        Callback::from(move |query: String| {
            let app_state = app_state.clone();
            app_state.set(reduce(&app_state, Action::SearchStarted));
            wasm_bindgen_futures::spawn_local(async move {
                match api::search(&query, 10).await {
                    Ok(results) => {
                        app_state.set(reduce(&app_state, Action::SearchSucceeded(results)))
                    }
                    Err(e) => app_state.set(reduce(&app_state, Action::SearchFailed(e.to_string()))),
                }
            });
        })
    };

    let available_labels = vec![
        ("project".to_string(), "The project this note belongs to".to_string()),
        ("status".to_string(), "Workflow status of the note".to_string()),
    ];

    html! {
        <main class="app">
            <h1>{ "agent-note" }</h1>
            if let Some(err) = &app_state.error {
                <p class="error">{ err }</p>
            }
            <NoteEditor {available_labels} {on_submit} />
            <VectorSearch
                results={app_state.search_results.clone()}
                loading={app_state.loading}
                {on_query}
            />
        </main>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
