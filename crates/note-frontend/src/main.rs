mod components;
mod state;

use yew::prelude::*;

use components::{NoteEditor, VectorSearch};
use state::{reduce, Action, AppState};

#[function_component(App)]
fn app() -> Html {
    let app_state = use_state(AppState::default);

    // Task 26 wires these callbacks to the real note-server REST API. For now
    // save is a no-op and search just flips the loading flag via the reducer
    // (SearchStarted) so the MVU loop is exercised end to end.
    let on_submit = Callback::from(|(_title, _content, _labels): (String, String, Vec<(String, String)>)| {
        // Placeholder: Task 26 POSTs the note to /api/notes.
    });

    let on_query = {
        let app_state = app_state.clone();
        Callback::from(move |_query: String| {
            let started = reduce(&app_state, Action::SearchStarted);
            app_state.set(started);
            // Task 26 performs the POST /api/notes/search fetch here and
            // dispatches SearchSucceeded / SearchFailed with the response.
        })
    };

    let available_labels = vec![
        ("project".to_string(), "The project this note belongs to".to_string()),
        ("status".to_string(), "Workflow status of the note".to_string()),
    ];

    html! {
        <main class="app">
            <h1>{ "agent-note" }</h1>
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
