use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_router::prelude::*;
use yew_duskmoon::{Alert, Button, Chip};

use crate::api;
use crate::components::icons;
use crate::components::Modal;
use crate::routes::Route;
use crate::state::{NoteSummary, SearchResultSummary};

/// Default page: a table of all notes (title, labels, per-row view/edit/remove actions), with a
/// search bar that swaps the table for ranked results.
#[function_component(NotesPage)]
pub fn notes_page() -> Html {
    let notes = use_state(Vec::<NoteSummary>::new);
    // `Some` while a search is active; `None` shows the full table.
    let results = use_state(|| None::<Vec<SearchResultSummary>>);
    let query = use_state(String::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    // The note pending deletion (id, title) — drives the confirm modal.
    let delete_target = use_state(|| None::<(String, String)>);

    let reload = {
        let notes = notes.clone();
        let loading = loading.clone();
        let error = error.clone();
        Callback::from(move |_: ()| {
            let notes = notes.clone();
            let loading = loading.clone();
            let error = error.clone();
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_notes().await {
                    Ok(list) => notes.set(list),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
        })
    };

    {
        let reload = reload.clone();
        use_effect_with((), move |_| {
            reload.emit(());
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

    let confirm_delete = {
        let delete_target = delete_target.clone();
        let reload = reload.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let id = match &*delete_target {
                Some((id, _)) => id.clone(),
                None => return,
            };
            let delete_target = delete_target.clone();
            let reload = reload.clone();
            let error = error.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match api::delete_note(&id).await {
                    Ok(()) => {
                        delete_target.set(None);
                        reload.emit(());
                    }
                    Err(e) => error.set(Some(e)),
                }
            });
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
                { note_table(&notes, &delete_target) }
            }

            if let Some((_, title)) = &*delete_target {
                <Modal title="Remove note" on_close={let d=delete_target.clone(); Callback::from(move |_| d.set(None))}>
                    <p>{ format!("Remove the note \u{201c}{title}\u{201d}? This cannot be undone.") }</p>
                    <div class="modal-actions">
                        <button type="button" class="btn btn-ghost"
                            onclick={let d=delete_target.clone(); Callback::from(move |_| d.set(None))}>
                            { "Cancel" }
                        </button>
                        <button type="button" class="btn btn-error" onclick={confirm_delete}>{ "Remove note" }</button>
                    </div>
                </Modal>
            }
        </section>
    }
}

fn note_table(notes: &[NoteSummary], delete_target: &UseStateHandle<Option<(String, String)>>) -> Html {
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
        <table class="table note-table">
            <thead>
                <tr>
                    <th>{ "Title" }</th>
                    <th>{ "Labels" }</th>
                    <th class="col-actions">{ "Actions" }</th>
                </tr>
            </thead>
            <tbody>
                { for notes.iter().map(|note| {
                    let id = note.id.clone();
                    let on_remove = {
                        let delete_target = delete_target.clone();
                        let id = note.id.clone();
                        let title = note.title.clone();
                        Callback::from(move |_| delete_target.set(Some((id.clone(), title.clone()))))
                    };
                    html! {
                        <tr key={note.id.clone()}>
                            <td class="col-title">{ note.title.clone() }</td>
                            <td>
                                <div class="applied-labels">
                                    { for note.labels.iter().map(|(k, v)| html! {
                                        <Chip variant={Some("primary".to_string())}>
                                            <span>{ format!("{k}: {v}") }</span>
                                        </Chip>
                                    }) }
                                </div>
                            </td>
                            <td class="col-actions">
                                <div class="row-actions">
                                    <Link<Route> to={Route::NoteShow { id: id.clone() }}
                                        classes={classes!("btn","btn-ghost","btn-icon")}>
                                        { icons::eye() }<span class="sr-only">{ "View" }</span>
                                    </Link<Route>>
                                    <Link<Route> to={Route::NoteEdit { id: id.clone() }}
                                        classes={classes!("btn","btn-ghost","btn-icon")}>
                                        { icons::pencil() }<span class="sr-only">{ "Edit" }</span>
                                    </Link<Route>>
                                    <button type="button" class="btn btn-ghost btn-icon icon-danger"
                                        onclick={on_remove}>
                                        { icons::trash() }<span class="sr-only">{ "Remove" }</span>
                                    </button>
                                </div>
                            </td>
                        </tr>
                    }
                }) }
            </tbody>
        </table>
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
                    <Link<Route> to={Route::NoteShow { id: r.id.clone() }} classes={classes!("result-title-link")}>
                        <div class="result-title">{ r.title.clone() }</div>
                    </Link<Route>>
                    // RRF rank-fusion score, not a raw similarity/distance (docs/design.md §7).
                    <div class="result-score">{ format!("fused score: {:.4}", r.score) }</div>
                </li>
            }) }
        </ul>
    }
}
