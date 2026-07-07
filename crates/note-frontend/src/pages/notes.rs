use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::{Alert, Button, Card, Chip};

use crate::api;
use crate::routes::Route;
use crate::state::{NoteSummary, SearchResultSummary};

/// Default page: the note list, with a search bar that swaps the list for ranked results.
#[function_component(NotesPage)]
pub fn notes_page() -> Html {
    let notes = use_state(Vec::<NoteSummary>::new);
    // `Some` while a search is active; `None` shows the full list.
    let results = use_state(|| None::<Vec<SearchResultSummary>>);
    let query = use_state(String::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    // Load all notes on first render.
    {
        let notes = notes.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_notes().await {
                    Ok(list) => notes.set(list),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
            || ()
        });
    }

    let on_query_input = {
        let query = query.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            query.set(input.value());
        })
    };

    let on_search = {
        let query = query.clone();
        let results = results.clone();
        let loading = loading.clone();
        let error = error.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let q = (*query).trim().to_string();
            if q.is_empty() {
                results.set(None);
                return;
            }
            let results = results.clone();
            let loading = loading.clone();
            let error = error.clone();
            loading.set(true);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::search(&q, 20).await {
                    Ok(r) => results.set(Some(r)),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
        })
    };

    let on_clear = {
        let query = query.clone();
        let results = results.clone();
        Callback::from(move |_| {
            query.set(String::new());
            results.set(None);
        })
    };

    html! {
        <section class="stack">
            <form class="search-bar" onsubmit={on_search}>
                <input
                    class="input input-primary"
                    type="text"
                    placeholder="Search your notes"
                    value={(*query).clone()}
                    oninput={on_query_input}
                />
                <Button variant={Some("primary".to_string())}>{ "Search" }</Button>
                if results.is_some() {
                    <button type="button" class="btn btn-ghost" onclick={on_clear}>{ "Clear" }</button>
                }
            </form>

            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(hits) = &*results {
                { search_results_view(hits) }
            } else {
                { note_list_view(&notes) }
            }
        </section>
    }
}

fn search_results_view(hits: &[SearchResultSummary]) -> Html {
    if hits.is_empty() {
        return html! { <p class="empty">{ "No matches. Try different words." }</p> };
    }
    html! {
        <ul class="results">
            { for hits.iter().map(|r| html! {
                <li class="result" key={r.id.clone()}>
                    <div class="result-title">{ r.title.clone() }</div>
                    <div class="result-id">{ r.id.clone() }</div>
                    // RRF rank-fusion score, not a raw similarity/distance (docs/design.md §7).
                    <div class="result-score">{ format!("fused score: {:.4}", r.score) }</div>
                </li>
            }) }
        </ul>
    }
}

fn note_list_view(notes: &[NoteSummary]) -> Html {
    if notes.is_empty() {
        return html! {
            <div class="empty">
                <p>{ "No notes yet." }</p>
                <Link<Route> to={Route::NewNote} classes={classes!("btn", "btn-primary")}>
                    { "Write your first note" }
                </Link<Route>>
            </div>
        };
    }
    html! {
        <div class="note-grid">
            { for notes.iter().map(|note| html! {
                <Card key={note.id.clone()} classes={classes!("note-card")}>
                    <h3 class="note-title">{ note.title.clone() }</h3>
                    <p class="note-snippet">{ snippet(&note.content) }</p>
                    if !note.labels.is_empty() {
                        <div class="applied-labels">
                            { for note.labels.iter().map(|(k, v)| html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{k}: {v}") }</span>
                                </Chip>
                            }) }
                        </div>
                    }
                </Card>
            }) }
        </div>
    }
}

fn snippet(content: &str) -> String {
    const MAX: usize = 140;
    if content.chars().count() <= MAX {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}
