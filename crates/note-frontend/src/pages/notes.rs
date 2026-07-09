use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew::virtual_dom::AttrValue;
use yew_duskmoon::{Alert, Button, Popover, PopoverTrigger};
use yew_router::prelude::*;

use crate::api;
use crate::components::icons;
use crate::components::Modal;
use crate::routes::Route;
use crate::state::{LabelFilter, LabelKey, NoteSummary, SearchResultSummary};

/// Notes shown per page in the list view.
const PAGE_SIZE: usize = 10;

/// Default page: a table of all notes (title, labels, per-row view/edit/remove actions), with a
/// search bar that swaps the table for ranked results.
#[function_component(NotesPage)]
pub fn notes_page() -> Html {
    let notes = use_state(Vec::<NoteSummary>::new);
    let label_keys = use_state(Vec::<LabelKey>::new);
    // `Some` while a search is active; `None` shows the full table.
    let results = use_state(|| None::<Vec<SearchResultSummary>>);
    let query = use_state(String::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    // The note pending deletion (id, title) — drives the confirm modal.
    let delete_target = use_state(|| None::<(String, String)>);
    // Current list-view page (0-based).
    let page = use_state(|| 0usize);
    let label_filters = use_state(Vec::<LabelFilter>::new);
    let filter_key = use_state(String::new);
    let filter_operator = use_state(|| "=".to_string());
    let filter_value = use_state(String::new);

    let run_query = {
        let query = query.clone();
        let notes = notes.clone();
        let results = results.clone();
        let loading = loading.clone();
        let error = error.clone();
        let page = page.clone();
        Callback::from(move |(raw_query, filters): (String, Vec<LabelFilter>)| {
            let notes = notes.clone();
            let results = results.clone();
            let loading = loading.clone();
            let error = error.clone();
            let page = page.clone();
            let query = query.clone();
            let q = raw_query.trim().to_string();
            loading.set(true);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                if q.is_empty() {
                    match api::list_notes_filtered(&filters).await {
                        Ok(list) => {
                            notes.set(list);
                            results.set(None);
                            page.set(0);
                        }
                        Err(e) => error.set(Some(e)),
                    }
                } else {
                    match api::search_filtered(&q, 20, &filters).await {
                        Ok(r) => results.set(Some(r)),
                        Err(e) => error.set(Some(e)),
                    }
                }
                query.set(raw_query);
                loading.set(false);
            });
        })
    };

    let reload = {
        let run_query = run_query.clone();
        let query = query.clone();
        let label_filters = label_filters.clone();
        Callback::from(move |_: ()| run_query.emit(((*query).clone(), (*label_filters).clone())))
    };

    {
        let run_query = run_query.clone();
        use_effect_with((), move |_| {
            run_query.emit((String::new(), Vec::new()));
            || ()
        });
    }

    {
        let label_keys = label_keys.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(keys) = api::list_labels().await {
                    label_keys.set(keys);
                }
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
    let on_filter_key_input = {
        let filter_key = filter_key.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            filter_key.set(input.value());
        })
    };
    let on_filter_operator_change = {
        let filter_operator = filter_operator.clone();
        Callback::from(move |e: Event| {
            let select: HtmlSelectElement = e.target_unchecked_into();
            filter_operator.set(select.value());
        })
    };
    let on_filter_value_input = {
        let filter_value = filter_value.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            filter_value.set(input.value());
        })
    };

    let on_search = {
        let query = query.clone();
        let label_filters = label_filters.clone();
        let run_query = run_query.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            run_query.emit(((*query).clone(), (*label_filters).clone()));
        })
    };

    let on_clear = {
        let query = query.clone();
        let results = results.clone();
        let label_filters = label_filters.clone();
        let filter_key = filter_key.clone();
        let filter_operator = filter_operator.clone();
        let filter_value = filter_value.clone();
        let run_query = run_query.clone();
        Callback::from(move |_| {
            query.set(String::new());
            results.set(None);
            label_filters.set(Vec::new());
            filter_key.set(String::new());
            filter_operator.set("=".to_string());
            filter_value.set(String::new());
            run_query.emit((String::new(), Vec::new()));
        })
    };

    // Build the remove-confirm modal from the current target. Capturing `id`/`title` here (rather
    // than re-reading `delete_target` inside the click handler) keeps the confirm handler correct.
    let delete_modal = match (*delete_target).clone() {
        None => html! {},
        Some((id, title)) => {
            let on_close = {
                let d = delete_target.clone();
                Callback::from(move |_: ()| d.set(None))
            };
            let on_cancel = {
                let d = delete_target.clone();
                Callback::from(move |_: MouseEvent| d.set(None))
            };
            let on_confirm = {
                let d = delete_target.clone();
                let reload = reload.clone();
                let error = error.clone();
                Callback::from(move |_: MouseEvent| {
                    let d = d.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    let id = id.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::delete_note(&id).await {
                            Ok(()) => {
                                d.set(None);
                                reload.emit(());
                            }
                            Err(e) => error.set(Some(e)),
                        }
                    });
                })
            };
            html! {
                <Modal title="Remove note" on_close={on_close}>
                    <p>{ format!("Remove the note \u{201c}{title}\u{201d}? This cannot be undone.") }</p>
                    <div class="app-modal-actions">
                        <button type="button" class="btn btn-ghost" onclick={on_cancel}>{ "Cancel" }</button>
                        <button type="button" class="btn btn-error" onclick={on_confirm}>{ "Remove note" }</button>
                    </div>
                </Modal>
            }
        }
    };

    let on_refresh = {
        let reload = reload.clone();
        Callback::from(move |_: MouseEvent| reload.emit(()))
    };

    let on_add_filter = {
        let label_filters = label_filters.clone();
        let filter_key = filter_key.clone();
        let filter_operator = filter_operator.clone();
        let filter_value = filter_value.clone();
        let query = query.clone();
        let run_query = run_query.clone();
        Callback::from(move |_: MouseEvent| {
            let key = (*filter_key).trim().to_string();
            let value = (*filter_value).trim().to_string();
            if key.is_empty() {
                return;
            }
            let filter = LabelFilter {
                key,
                operator: (*filter_operator).clone(),
                value,
            };
            let mut filters = (*label_filters).clone();
            if !filters.iter().any(|item| item == &filter) {
                filters.push(filter);
            }
            label_filters.set(filters.clone());
            filter_value.set(String::new());
            run_query.emit(((*query).clone(), filters));
        })
    };

    let on_remove_filter = {
        let label_filters = label_filters.clone();
        let query = query.clone();
        let run_query = run_query.clone();
        Callback::from(move |idx: usize| {
            let mut filters = (*label_filters).clone();
            if idx < filters.len() {
                filters.remove(idx);
                label_filters.set(filters.clone());
                run_query.emit(((*query).clone(), filters));
            }
        })
    };

    html! {
        <section class="stack">
            <div class="search-panel">
                <form class="search-bar" onsubmit={on_search}>
                    <input
                        class="input input-primary"
                        type="text"
                        placeholder="Search your notes"
                        value={(*query).clone()}
                        oninput={on_query_input}
                    />
                    <Button variant={Some("primary".to_string())}>{ "Search" }</Button>
                    if results.is_some() || !(*label_filters).is_empty() || !(*query).is_empty() {
                        <button type="button" class="btn btn-ghost" onclick={on_clear}>{ "Clear" }</button>
                    }
                    <button type="button" class="btn btn-ghost btn-icon" title="Refresh" onclick={on_refresh}>
                        { icons::refresh() }<span class="sr-only">{ "Refresh" }</span>
                    </button>
                </form>
                { label_filter_bar(
                    (*label_keys).as_slice(),
                    (*label_filters).as_slice(),
                    (*filter_key).as_str(),
                    (*filter_operator).as_str(),
                    (*filter_value).as_str(),
                    on_filter_key_input,
                    on_filter_operator_change,
                    on_filter_value_input,
                    on_add_filter,
                    on_remove_filter,
                ) }
            </div>

            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(hits) = &*results {
                { search_results_view(hits) }
            } else {
                { list_view(&notes, &page, &delete_target) }
            }

            { delete_modal }
        </section>
    }
}

fn label_filter_bar(
    label_keys: &[LabelKey],
    filters: &[LabelFilter],
    filter_key: &str,
    filter_operator: &str,
    filter_value: &str,
    on_key_input: Callback<InputEvent>,
    on_operator_change: Callback<Event>,
    on_value_input: Callback<InputEvent>,
    on_add: Callback<MouseEvent>,
    on_remove: Callback<usize>,
) -> Html {
    let value_type = label_keys
        .iter()
        .find(|label| label.key == filter_key)
        .map(|label| label.value_type.as_str())
        .unwrap_or("text");

    html! {
        <div class="label-filter-panel">
            <div class="label-filter-builder">
                <input
                    class="input label-filter-key"
                    type="text"
                    list="label-filter-key-options"
                    placeholder="label"
                    value={filter_key.to_string()}
                    oninput={on_key_input}
                />
                <datalist id="label-filter-key-options">
                    { for label_keys.iter().map(|label| html! {
                        <option value={label.key.clone()} />
                    }) }
                </datalist>
                <select class="input label-filter-operator" onchange={on_operator_change} value={filter_operator.to_string()}>
                    { for ["=", "!=", ">", ">=", "<", "<="].iter().map(|operator| html! {
                        <option value={(*operator).to_string()}>{ *operator }</option>
                    }) }
                </select>
                <input
                    class="input label-filter-value"
                    type={label_value_input_type(value_type)}
                    placeholder="value"
                    value={filter_value.to_string()}
                    oninput={on_value_input}
                />
                <button type="button" class="btn btn-outline" onclick={on_add}>{ "Add filter" }</button>
            </div>
            if !filters.is_empty() {
                <div class="active-label-filters">
                    { for filters.iter().enumerate().map(|(idx, filter)| {
                        let on_remove = on_remove.clone();
                        let label = label_filter_label(filter);
                        html! {
                            <button
                                type="button"
                                class="chip chip-primary active-label-filter"
                                title="Remove filter"
                                onclick={Callback::from(move |_: MouseEvent| on_remove.emit(idx))}
                            >
                                <span>{ label }</span>
                                <span aria-hidden="true">{ "x" }</span>
                            </button>
                        }
                    }) }
                </div>
            }
        </div>
    }
}

fn label_filter_label(filter: &LabelFilter) -> String {
    if filter.value.is_empty() {
        filter.key.clone()
    } else {
        format!("{}{}{}", filter.key, filter.operator, filter.value)
    }
}

fn label_value_input_type(value_type: &str) -> &'static str {
    match value_type {
        "number" => "number",
        "date" => "date",
        "datetime" => "datetime-local",
        "time" => "time",
        _ => "text",
    }
}

/// Full-list view: the current page of notes plus the pagination bar.
fn list_view(
    notes: &[NoteSummary],
    page: &UseStateHandle<usize>,
    delete_target: &UseStateHandle<Option<(String, String)>>,
) -> Html {
    let total = notes.len();
    if total == 0 {
        return note_table(notes, delete_target);
    }
    let total_pages = total.div_ceil(PAGE_SIZE);
    let current = (**page).min(total_pages - 1);
    let start = current * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(total);

    html! {
        <>
            { note_table(&notes[start..end], delete_target) }
            { pagination_bar(current, total_pages, total, page) }
        </>
    }
}

/// `Prev [1] [2] … [N] Next` pagination controls; hidden when everything fits on one page.
fn pagination_bar(
    current: usize,
    total_pages: usize,
    total: usize,
    page: &UseStateHandle<usize>,
) -> Html {
    if total_pages <= 1 {
        return html! {};
    }
    let set_page = |target: usize| {
        let page = page.clone();
        Callback::from(move |_: MouseEvent| page.set(target))
    };
    html! {
        <nav class="pagination" aria-label="Notes pages">
            <span class="pagination-info">{ format!("{total} notes") }</span>
            <button type="button" class="btn btn-ghost pagination-prev"
                disabled={current == 0} onclick={set_page(current.saturating_sub(1))}>
                { "Prev" }
            </button>
            { for (0..total_pages).map(|p| {
                let classes = if p == current { "btn btn-primary pagination-item" } else { "btn btn-ghost pagination-item" };
                html! {
                    <button type="button" class={classes} onclick={set_page(p)}>{ p + 1 }</button>
                }
            }) }
            <button type="button" class="btn btn-ghost pagination-next"
                disabled={current + 1 >= total_pages} onclick={set_page(current + 1)}>
                { "Next" }
            </button>
        </nav>
    }
}

fn note_table(
    notes: &[NoteSummary],
    delete_target: &UseStateHandle<Option<(String, String)>>,
) -> Html {
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
                            <td class="col-title">
                                <Link<Route>
                                    to={Route::NoteShow { id: id.clone() }}
                                    classes={classes!("note-title-link")}
                                >
                                    { note.title.clone() }
                                </Link<Route>>
                            </td>
                            <td>
                                <div class="applied-labels">
                                    { for note.labels.iter().map(|(k, v)| html! {
                                        { label_chip(k, v) }
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

fn label_chip(key: &str, value: &str) -> Html {
    let label = format!("{key}: {value}");
    html! {
        <Popover
            class={classes!("note-label-popover", "popover-bottom")}
            variant={Some("primary".to_string())}
            trigger={PopoverTrigger::Hover}
            trigger_class={classes!("chip", "chip-primary", "note-label-chip")}
            trigger_label={AttrValue::from(label.clone())}
        >
            <dl class="note-label-popover-body">
                <div class="note-label-popover-row">
                    <dt>{ "name" }</dt>
                    <dd>{ key.to_string() }</dd>
                </div>
                <div class="note-label-popover-row">
                    <dt>{ "value" }</dt>
                    <dd>{ value.to_string() }</dd>
                </div>
            </dl>
        </Popover>
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
