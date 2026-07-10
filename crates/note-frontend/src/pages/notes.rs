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
const DEFAULT_PAGE_SIZE: usize = 30;
const PAGE_SIZE_OPTIONS: [usize; 4] = [10, 30, 50, 100];

#[derive(Clone, PartialEq)]
struct NotesUrlState {
    current: usize,
    page_size: usize,
    search: String,
    labels: Vec<LabelFilter>,
}

#[derive(serde::Serialize)]
struct NotesQueryParams {
    current: usize,
    page_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<String>,
}

fn default_notes_url_state() -> NotesUrlState {
    NotesUrlState {
        current: 1,
        page_size: DEFAULT_PAGE_SIZE,
        search: String::new(),
        labels: Vec::new(),
    }
}

fn parse_notes_query(query: &str) -> NotesUrlState {
    let mut state = default_notes_url_state();
    for pair in query.trim_start_matches('?').split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_query_component(key);
        let value = decode_query_component(value);
        match key.as_str() {
            "current" => {
                if let Ok(current) = value.parse::<usize>() {
                    state.current = current.max(1);
                }
            }
            "page_size" => {
                if let Ok(page_size) = value.parse::<usize>() {
                    state.page_size = normalize_page_size(page_size);
                }
            }
            "search" => state.search = value,
            "labels" => state.labels.extend(parse_label_filters(&value)),
            _ => {}
        }
    }
    state
}

fn notes_query_params(state: &NotesUrlState) -> NotesQueryParams {
    NotesQueryParams {
        current: state.current.max(1),
        page_size: normalize_page_size(state.page_size),
        search: non_empty_param(&state.search),
        labels: api::label_filter_selector(&state.labels),
    }
}

fn non_empty_param(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn normalize_page_size(page_size: usize) -> usize {
    if PAGE_SIZE_OPTIONS.contains(&page_size) {
        page_size
    } else {
        DEFAULT_PAGE_SIZE
    }
}

fn decode_query_component(value: &str) -> String {
    let value = value.replace('+', " ");
    urlencoding::decode(&value)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| value.to_string())
}

fn parse_label_filters(selector: &str) -> Vec<LabelFilter> {
    selector
        .split('&')
        .filter_map(|term| {
            let term = term.trim();
            if term.is_empty() {
                return None;
            }
            for operator in [">=", "<=", "!=", "=", ">", "<"] {
                if let Some(idx) = term.find(operator) {
                    let key = term[..idx].trim();
                    let value = term[idx + operator.len()..].trim();
                    if key.is_empty() {
                        return None;
                    }
                    return Some(LabelFilter {
                        key: key.to_string(),
                        operator: operator.to_string(),
                        value: value.to_string(),
                    });
                }
            }
            Some(LabelFilter {
                key: term.to_string(),
                operator: "=".to_string(),
                value: String::new(),
            })
        })
        .collect()
}

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
    let page_size = use_state(|| DEFAULT_PAGE_SIZE);
    let label_filters = use_state(Vec::<LabelFilter>::new);
    let filter_key = use_state(String::new);
    let filter_operator = use_state(|| "=".to_string());
    let filter_value = use_state(String::new);
    let refresh_tick = use_state(|| 0usize);
    let navigator = use_navigator();
    let location = use_location();
    let query_string = location
        .as_ref()
        .map(|location| location.query_str().to_string())
        .unwrap_or_default();
    let url_state = parse_notes_query(&query_string);

    let replace_notes_url = {
        let navigator = navigator.clone();
        Callback::from(move |state: NotesUrlState| {
            if let Some(navigator) = &navigator {
                let _ = navigator.replace_with_query(&Route::Notes, notes_query_params(&state));
            }
        })
    };

    {
        let notes = notes.clone();
        let results = results.clone();
        let query = query.clone();
        let page = page.clone();
        let page_size = page_size.clone();
        let label_filters = label_filters.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with((url_state.clone(), *refresh_tick), move |(state, _)| {
            let state = state.clone();
            notes.set(Vec::new());
            results.set(None);
            query.set(state.search.clone());
            page.set(state.current.saturating_sub(1));
            page_size.set(state.page_size);
            label_filters.set(state.labels.clone());
            loading.set(true);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                let search = state.search.trim().to_string();
                if search.is_empty() {
                    match api::list_notes_filtered(&state.labels).await {
                        Ok(list) => notes.set(list),
                        Err(e) => error.set(Some(e)),
                    }
                } else {
                    let limit = state
                        .current
                        .saturating_add(1)
                        .saturating_mul(state.page_size)
                        .max(state.page_size);
                    match api::search_filtered(&search, limit, &state.labels).await {
                        Ok(r) => results.set(Some(r)),
                        Err(e) => error.set(Some(e)),
                    }
                }
                loading.set(false);
            });
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

    let reload = {
        let refresh_tick = refresh_tick.clone();
        Callback::from(move |_: ()| refresh_tick.set((*refresh_tick).saturating_add(1)))
    };

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
        let page_size = page_size.clone();
        let replace_notes_url = replace_notes_url.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            replace_notes_url.emit(NotesUrlState {
                current: 1,
                page_size: *page_size,
                search: (*query).trim().to_string(),
                labels: (*label_filters).clone(),
            });
        })
    };

    let on_clear = {
        let query = query.clone();
        let label_filters = label_filters.clone();
        let filter_key = filter_key.clone();
        let filter_operator = filter_operator.clone();
        let filter_value = filter_value.clone();
        let page_size = page_size.clone();
        let replace_notes_url = replace_notes_url.clone();
        Callback::from(move |_| {
            query.set(String::new());
            label_filters.set(Vec::new());
            filter_key.set(String::new());
            filter_operator.set("=".to_string());
            filter_value.set(String::new());
            replace_notes_url.emit(NotesUrlState {
                current: 1,
                page_size: *page_size,
                search: String::new(),
                labels: Vec::new(),
            });
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
        let page_size = page_size.clone();
        let replace_notes_url = replace_notes_url.clone();
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
            replace_notes_url.emit(NotesUrlState {
                current: 1,
                page_size: *page_size,
                search: (*query).trim().to_string(),
                labels: filters,
            });
        })
    };

    let on_remove_filter = {
        let label_filters = label_filters.clone();
        let query = query.clone();
        let page_size = page_size.clone();
        let replace_notes_url = replace_notes_url.clone();
        Callback::from(move |idx: usize| {
            let mut filters = (*label_filters).clone();
            if idx < filters.len() {
                filters.remove(idx);
                label_filters.set(filters.clone());
                replace_notes_url.emit(NotesUrlState {
                    current: 1,
                    page_size: *page_size,
                    search: (*query).trim().to_string(),
                    labels: filters,
                });
            }
        })
    };

    let on_page_change = {
        let replace_notes_url = replace_notes_url.clone();
        let url_state = url_state.clone();
        Callback::from(move |target: usize| {
            let mut next = url_state.clone();
            next.current = target.saturating_add(1);
            replace_notes_url.emit(next);
        })
    };

    let on_page_size_change = {
        let replace_notes_url = replace_notes_url.clone();
        let url_state = url_state.clone();
        Callback::from(move |next_page_size: usize| {
            let mut next = url_state.clone();
            next.current = 1;
            next.page_size = normalize_page_size(next_page_size);
            replace_notes_url.emit(next);
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
                { search_results_view(hits, &page, &page_size, on_page_change.clone(), on_page_size_change.clone(), on_refresh.clone()) }
            } else {
                { list_view(&notes, &page, &page_size, &delete_target, on_page_change, on_page_size_change, on_refresh) }
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
    page_size: &UseStateHandle<usize>,
    delete_target: &UseStateHandle<Option<(String, String)>>,
    on_page_change: Callback<usize>,
    on_page_size_change: Callback<usize>,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    let total = notes.len();
    if total == 0 {
        return note_table(notes, delete_target);
    }
    let per_page = **page_size;
    let total_pages = total.div_ceil(per_page);
    let current = (**page).min(total_pages - 1);
    let start = current * per_page;
    let end = (start + per_page).min(total);

    html! {
        <>
            { note_table(&notes[start..end], delete_target) }
            { pagination_bar(current, total_pages, total, start, end, **page_size, on_page_change, on_page_size_change, on_refresh) }
        </>
    }
}

/// Compact pagination controls; hidden when everything fits on one page.
fn pagination_bar(
    current: usize,
    total_pages: usize,
    total: usize,
    start: usize,
    end: usize,
    page_size: usize,
    on_page_change: Callback<usize>,
    on_page_size_change: Callback<usize>,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    if total_pages <= 1 {
        return html! {};
    }
    let set_page = |target: usize| {
        let on_page_change = on_page_change.clone();
        Callback::from(move |_: MouseEvent| on_page_change.emit(target))
    };
    let on_page_input = {
        let on_page_change = on_page_change.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            if let Ok(value) = input.value().parse::<usize>() {
                let target = value.clamp(1, total_pages) - 1;
                on_page_change.emit(target);
            }
        })
    };
    let on_page_size_select = {
        let on_page_size_change = on_page_size_change.clone();
        Callback::from(move |e: Event| {
            let select: HtmlSelectElement = e.target_unchecked_into();
            if let Ok(value) = select.value().parse::<usize>() {
                on_page_size_change.emit(value);
            }
        })
    };

    html! {
        <nav class="pagination" aria-label="Notes pages">
            <span class="pagination-info">{ format!("{}-{} / {total}", start + 1, end) }</span>
            <button type="button" class="btn btn-ghost btn-icon pagination-prev" title="Previous page"
                disabled={current == 0} onclick={set_page(current.saturating_sub(1))}>
                <span aria-hidden="true">{ "<" }</span><span class="sr-only">{ "Previous page" }</span>
            </button>
            <input
                class="input pagination-page-input"
                type="number"
                min="1"
                max={total_pages.to_string()}
                value={(current + 1).to_string()}
                oninput={on_page_input}
                aria-label="Current page"
            />
            <span class="pagination-pages-total">{ format!("/ {total_pages}") }</span>
            <button type="button" class="btn btn-ghost btn-icon pagination-next" title="Next page"
                disabled={current + 1 >= total_pages} onclick={set_page(current + 1)}>
                <span aria-hidden="true">{ ">" }</span><span class="sr-only">{ "Next page" }</span>
            </button>
            <select class="input pagination-page-size" onchange={on_page_size_select} value={page_size.to_string()} aria-label="Notes per page">
                { for PAGE_SIZE_OPTIONS.iter().map(|size| html! {
                    <option value={size.to_string()}>{ format!("{size}条/页") }</option>
                }) }
            </select>
            <button type="button" class="btn btn-ghost btn-icon pagination-refresh" title="Refresh" onclick={on_refresh}>
                { icons::refresh() }<span class="sr-only">{ "Refresh" }</span>
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

fn search_results_view(
    hits: &[SearchResultSummary],
    page: &UseStateHandle<usize>,
    page_size: &UseStateHandle<usize>,
    on_page_change: Callback<usize>,
    on_page_size_change: Callback<usize>,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    if hits.is_empty() {
        return html! { <p class="empty">{ "No matches. Try different words." }</p> };
    }
    let total = hits.len();
    let per_page = **page_size;
    let total_pages = total.div_ceil(per_page);
    let current = (**page).min(total_pages - 1);
    let start = current * per_page;
    let end = (start + per_page).min(total);
    html! {
        <>
            <ul class="results">
                { for hits[start..end].iter().map(|r| html! {
                    <li class="result" key={r.id.clone()}>
                        <Link<Route> to={Route::NoteShow { id: r.id.clone() }} classes={classes!("result-title-link")}>
                            <div class="result-title">{ r.title.clone() }</div>
                        </Link<Route>>
                        // RRF rank-fusion score, not a raw similarity/distance (docs/design.md §7).
                        <div class="result-score">{ format!("fused score: {:.4}", r.score) }</div>
                    </li>
                }) }
            </ul>
            { pagination_bar(current, total_pages, total, start, end, **page_size, on_page_change, on_page_size_change, on_refresh) }
        </>
    }
}
