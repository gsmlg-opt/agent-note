use std::{collections::HashSet, ops::Deref, rc::Rc};

use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::{Alert, Button};
use yew_router::components::LinkProps;
use yew_router::prelude::*;

use crate::api;
use crate::components::icons;
use crate::components::Modal;
use crate::routes::{NotesQueryParams, Route, DEFAULT_NOTES_PAGE_SIZE};
use crate::state::{
    stale_retry_blocked, LabelFilter, LabelKey, NoteSummary, SearchResultSummary,
    StaleRevisionGate, StaleRevisionGateAction,
};

const MAX_PAGE_SIZE: usize = 1000;
const PAGE_SIZE_OPTIONS: [usize; 5] = [10, 30, 50, 100, 1000];
const RETRIEVAL_PLACEHOLDER: &str = "Retrieve by title or content";
const RETRIEVE_BUTTON_LABEL: &str = "Retrieve";
const LABEL_FILTER_PARSER_OPERATORS: [&str; 9] =
    [">=", "<=", "!=", "^=", "$=", "~=", "=", ">", "<"];
const LABEL_FILTER_DISPLAY_OPERATORS: [&str; 10] =
    ["==", "=", "!=", "^=", "$=", "~=", ">", ">=", "<", "<="];

#[derive(Clone, Default, PartialEq)]
struct SelectionState(HashSet<String>);

impl Deref for SelectionState {
    type Target = HashSet<String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

enum SelectionAction {
    Replace(HashSet<String>),
    RetainVisible(HashSet<String>),
    Toggle { id: String, selected: bool },
    Clear,
}

impl Reducible for SelectionState {
    type Action = SelectionAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = self.0.clone();
        match action {
            SelectionAction::Replace(replacement) => next = replacement,
            SelectionAction::RetainVisible(visible) => next.retain(|id| visible.contains(id)),
            SelectionAction::Toggle { id, selected } => {
                if selected {
                    next.insert(id);
                } else {
                    next.remove(&id);
                }
            }
            SelectionAction::Clear => next.clear(),
        }
        Self(next).into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NoteSelectionTarget {
    id: String,
    revision: i64,
}

impl NoteSelectionTarget {
    fn new(id: impl Into<String>, revision: i64) -> Self {
        Self {
            id: id.into(),
            revision,
        }
    }
}

fn note_selection_targets(notes: &[NoteSummary]) -> Vec<NoteSelectionTarget> {
    notes
        .iter()
        .map(|note| NoteSelectionTarget::new(note.id.clone(), note.revision))
        .collect()
}

fn search_result_selection_targets(hits: &[SearchResultSummary]) -> Vec<NoteSelectionTarget> {
    hits.iter()
        .map(|hit| NoteSelectionTarget::new(hit.id.clone(), hit.revision))
        .collect()
}

fn selected_note_targets(
    visible: &[NoteSelectionTarget],
    selected: &HashSet<String>,
) -> Vec<api::BatchNoteTarget> {
    visible
        .iter()
        .filter(|target| selected.contains(&target.id))
        .map(|target| api::BatchNoteTarget::new(target.id.clone(), target.revision))
        .collect()
}

fn select_all_is_indeterminate(selected_visible_count: usize, visible_count: usize) -> bool {
    selected_visible_count > 0 && selected_visible_count < visible_count
}

#[derive(Clone, PartialEq)]
pub(crate) struct NotesUrlState {
    current: usize,
    page_size: usize,
    search: String,
    pub(crate) labels: Vec<LabelFilter>,
    pub(crate) invalid_labels: bool,
}

fn default_notes_url_state() -> NotesUrlState {
    NotesUrlState {
        current: 1,
        page_size: DEFAULT_NOTES_PAGE_SIZE,
        search: String::new(),
        labels: Vec::new(),
        invalid_labels: false,
    }
}

pub(crate) fn parse_notes_query(query: &str) -> NotesUrlState {
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
            "labels" => match parse_label_filters(&value) {
                Ok(filters) => state.labels.extend(filters),
                Err(()) => {
                    state.labels.clear();
                    state.invalid_labels = true;
                }
            },
            _ => {}
        }
    }
    state
}

pub(crate) fn notes_query_params(state: &NotesUrlState) -> NotesQueryParams {
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
    } else if page_size > MAX_PAGE_SIZE {
        MAX_PAGE_SIZE
    } else {
        DEFAULT_NOTES_PAGE_SIZE
    }
}

fn decode_query_component(value: &str) -> String {
    let value = value.replace('+', " ");
    urlencoding::decode(&value)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| value.to_string())
}

fn parse_label_filters(selector: &str) -> Result<Vec<LabelFilter>, ()> {
    let mut filters = Vec::new();
    for term in selector.split('&') {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        if let Some(encoded) = term.strip_prefix('~') {
            let (key, value) = encoded.split_once("==").ok_or(())?;
            let key = decode_exact_selector_component(key)?;
            if key.is_empty() {
                return Err(());
            }
            filters.push(LabelFilter {
                key,
                operator: "==".to_string(),
                value: decode_exact_selector_component(value)?,
            });
            continue;
        }
        let operator = LABEL_FILTER_PARSER_OPERATORS
            .iter()
            .filter_map(|operator| term.find(operator).map(|idx| (idx, *operator)))
            .min_by(|(left_idx, left_operator), (right_idx, right_operator)| {
                left_idx
                    .cmp(right_idx)
                    .then_with(|| right_operator.len().cmp(&left_operator.len()))
            });
        if let Some((idx, operator)) = operator {
            let key = term[..idx].trim();
            let value = term[idx + operator.len()..].trim();
            if key.is_empty() {
                continue;
            }
            filters.push(LabelFilter {
                key: key.to_string(),
                operator: operator.to_string(),
                value: value.to_string(),
            });
            continue;
        }
        filters.push(LabelFilter {
            key: term.to_string(),
            operator: "=".to_string(),
            value: String::new(),
        });
    }
    Ok(filters)
}

fn decode_exact_selector_component(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    urlencoding::decode(value)
        .map(|value| value.into_owned())
        .map_err(|_| ())
}

fn quick_label_filters(filters: &[LabelFilter], key: &str, value: &str) -> Vec<LabelFilter> {
    let clicked = LabelFilter {
        key: key.to_string(),
        operator: "==".to_string(),
        value: value.to_string(),
    };
    if filters.contains(&clicked) {
        return filters.to_vec();
    }

    let mut inserted = false;
    let mut next = Vec::with_capacity(filters.len() + 1);
    for filter in filters {
        if filter.key == key && matches!(filter.operator.as_str(), "=" | "==") {
            if !inserted {
                next.push(clicked.clone());
                inserted = true;
            }
        } else {
            next.push(filter.clone());
        }
    }
    if !inserted {
        next.push(clicked);
    }
    next
}

fn build_label_filter(key: &str, operator: &str, value: &str) -> Option<LabelFilter> {
    let (key, value) = if operator == "==" {
        (key.to_string(), value.to_string())
    } else {
        (key.trim().to_string(), value.trim().to_string())
    };
    if key.is_empty() {
        return None;
    }
    Some(LabelFilter {
        key,
        operator: operator.to_string(),
        value,
    })
}

/// Default page: a table of all notes (title, labels, per-row view/edit/remove actions), with a
/// retrieval bar that swaps the table for ranked results.
#[function_component(NotesPage)]
pub fn notes_page() -> Html {
    let notes = use_state(Vec::<NoteSummary>::new);
    let total_notes = use_state(|| 0usize);
    let label_keys = use_state(Vec::<LabelKey>::new);
    // `Some` while retrieval is active; `None` shows the full table.
    let results = use_state(|| None::<Vec<SearchResultSummary>>);
    let query = use_state(String::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    // The note pending deletion (id, title) — drives the confirm modal.
    let delete_target = use_state(|| None::<(String, String, i64)>);
    let selected = use_reducer(SelectionState::default);
    let stale_revision_gate = use_reducer(StaleRevisionGate::default);
    // Current list-view page (0-based).
    let page = use_state(|| 0usize);
    let page_size = use_state(|| DEFAULT_NOTES_PAGE_SIZE);
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
    let notes_query = notes_query_params(&url_state);
    let visible_targets = if let Some(hits) = &*results {
        let search_page = search_result_page(hits.len(), *page, *page_size, &notes_query);
        search_result_selection_targets(&hits[search_page.start..search_page.end])
    } else {
        note_selection_targets(&notes)
    };
    let visible_ids = visible_targets
        .iter()
        .map(|target| target.id.clone())
        .collect::<HashSet<_>>();
    let selected_visible_count = selected_note_targets(&visible_targets, &selected).len();
    let select_all_ref = use_node_ref();

    {
        let selected = selected.clone();
        use_effect_with(visible_ids.clone(), move |visible_ids| {
            selected.dispatch(SelectionAction::RetainVisible(visible_ids.clone()));
            || ()
        });
    }

    {
        let select_all_ref = select_all_ref.clone();
        use_effect_with(
            (selected_visible_count, visible_ids.len()),
            move |(selected_visible_count, visible_count)| {
                if let Some(input) = select_all_ref.cast::<HtmlInputElement>() {
                    input.set_indeterminate(select_all_is_indeterminate(
                        *selected_visible_count,
                        *visible_count,
                    ));
                }
                || ()
            },
        );
    }

    let on_select_all = {
        let selected = selected.clone();
        let visible_ids = visible_ids.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            if input.checked() {
                selected.dispatch(SelectionAction::Replace(visible_ids.clone()));
            } else {
                selected.dispatch(SelectionAction::Clear);
            }
        })
    };

    let on_selection_change = {
        let selected = selected.clone();
        Callback::from(move |(id, selected_now): (String, bool)| {
            selected.dispatch(SelectionAction::Toggle {
                id,
                selected: selected_now,
            });
        })
    };

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
        let total_notes = total_notes.clone();
        let results = results.clone();
        let query = query.clone();
        let page = page.clone();
        let page_size = page_size.clone();
        let label_filters = label_filters.clone();
        let loading = loading.clone();
        let error = error.clone();
        let stale_revision_gate = stale_revision_gate.clone();
        use_effect_with((url_state.clone(), *refresh_tick), move |(state, _)| {
            let state = state.clone();
            let refresh_epoch = stale_revision_gate.refresh_epoch();
            notes.set(Vec::new());
            total_notes.set(0);
            results.set(None);
            query.set(state.search.clone());
            page.set(state.current.saturating_sub(1));
            page_size.set(state.page_size);
            label_filters.set(state.labels.clone());
            loading.set(true);
            error.set(None);
            if state.invalid_labels {
                loading.set(false);
                error.set(Some("Invalid label filter in URL.".to_string()));
                stale_revision_gate.dispatch(StaleRevisionGateAction::RefreshFinished {
                    started_epoch: refresh_epoch,
                    succeeded: false,
                });
            } else {
                wasm_bindgen_futures::spawn_local(async move {
                    let search = state.search.trim().to_string();
                    if search.is_empty() {
                        let offset = state
                            .current
                            .saturating_sub(1)
                            .saturating_mul(state.page_size);
                        match api::list_notes_page(&state.labels, state.page_size, offset).await {
                            Ok(page) => {
                                notes.set(page.notes);
                                total_notes.set(page.total);
                                stale_revision_gate.dispatch(
                                    StaleRevisionGateAction::RefreshFinished {
                                        started_epoch: refresh_epoch,
                                        succeeded: true,
                                    },
                                );
                            }
                            Err(e) => {
                                stale_revision_gate.dispatch(
                                    StaleRevisionGateAction::RefreshFinished {
                                        started_epoch: refresh_epoch,
                                        succeeded: false,
                                    },
                                );
                                error.set(Some(e));
                            }
                        }
                    } else {
                        let limit = state
                            .current
                            .saturating_add(1)
                            .saturating_mul(state.page_size)
                            .max(state.page_size);
                        match api::search_filtered(&search, limit, &state.labels).await {
                            Ok(r) => {
                                results.set(Some(r));
                                stale_revision_gate.dispatch(
                                    StaleRevisionGateAction::RefreshFinished {
                                        started_epoch: refresh_epoch,
                                        succeeded: true,
                                    },
                                );
                            }
                            Err(e) => {
                                stale_revision_gate.dispatch(
                                    StaleRevisionGateAction::RefreshFinished {
                                        started_epoch: refresh_epoch,
                                        succeeded: false,
                                    },
                                );
                                error.set(Some(e));
                            }
                        }
                    }
                    loading.set(false);
                });
            }
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
                invalid_labels: false,
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
                invalid_labels: false,
            });
        })
    };

    // Build the remove-confirm modal from the current target. Capturing `id`/`title` here (rather
    // than re-reading `delete_target` inside the click handler) keeps the confirm handler correct.
    let delete_modal = match (*delete_target).clone() {
        None => html! {},
        Some((id, title, expected_revision)) => {
            let on_close = {
                let d = delete_target.clone();
                let stale_revision_gate = stale_revision_gate.clone();
                Callback::from(move |_: ()| {
                    d.set(None);
                    stale_revision_gate.dispatch(StaleRevisionGateAction::Dismiss);
                })
            };
            let on_cancel = {
                let d = delete_target.clone();
                let stale_revision_gate = stale_revision_gate.clone();
                Callback::from(move |_: MouseEvent| {
                    d.set(None);
                    stale_revision_gate.dispatch(StaleRevisionGateAction::Dismiss);
                })
            };
            let on_confirm = {
                let d = delete_target.clone();
                let reload = reload.clone();
                let error = error.clone();
                let stale_revision_gate = stale_revision_gate.clone();
                Callback::from(move |_: MouseEvent| {
                    if stale_revision_gate.conflict_notice_visible() {
                        return;
                    }
                    let d = d.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    let stale_revision_gate = stale_revision_gate.clone();
                    let id = id.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::delete_note(&id, expected_revision).await {
                            Ok(()) => {
                                d.set(None);
                                reload.emit(());
                            }
                            Err(e) => {
                                if e.is_stale_revision() {
                                    stale_revision_gate.dispatch(StaleRevisionGateAction::Conflict);
                                } else {
                                    error.set(Some(e.to_string()));
                                }
                            }
                        }
                    });
                })
            };
            let on_reload = {
                let d = delete_target.clone();
                let reload = reload.clone();
                Callback::from(move |_| {
                    d.set(None);
                    reload.emit(());
                })
            };
            html! {
                <Modal title="Remove note" on_close={on_close}>
                    <p>{ format!("Remove the note \u{201c}{title}\u{201d}? This cannot be undone.") }</p>
                    if stale_revision_gate.blocked() {
                        <Alert variant={Some("error".to_string())}>
                            <span>{ "This note changed after the list was loaded. Reload the notes before deciding whether to retry." }</span>
                        </Alert>
                    }
                    <div class="app-modal-actions">
                        if stale_revision_gate.conflict_notice_visible() {
                            <button type="button" class="btn btn-outline" onclick={on_reload}>{ "Reload notes" }</button>
                        }
                        <button type="button" class="btn btn-ghost" onclick={on_cancel}>{ "Cancel" }</button>
                        <button type="button" class="btn btn-error" disabled={stale_retry_blocked(false, stale_revision_gate.blocked())} onclick={on_confirm}>{ "Remove note" }</button>
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
            let Some(filter) = build_label_filter(&filter_key, &filter_operator, &filter_value)
            else {
                return;
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
                invalid_labels: false,
            });
        })
    };

    let on_quick_add_filter = {
        let label_filters = label_filters.clone();
        let query = query.clone();
        let page_size = page_size.clone();
        let replace_notes_url = replace_notes_url.clone();
        Callback::from(move |(key, value): (String, String)| {
            let filters = quick_label_filters(&label_filters, &key, &value);
            if filters == *label_filters {
                return;
            }
            label_filters.set(filters.clone());
            replace_notes_url.emit(NotesUrlState {
                current: 1,
                page_size: *page_size,
                search: (*query).trim().to_string(),
                labels: filters,
                invalid_labels: false,
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
                    invalid_labels: false,
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
                        placeholder={RETRIEVAL_PLACEHOLDER}
                        value={(*query).clone()}
                        oninput={on_query_input}
                    />
                    <Button variant={Some("primary".to_string())}>
                        <>{ RETRIEVE_BUTTON_LABEL }</>
                    </Button>
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
                { search_results_view(hits, &page, &page_size, &selected, &select_all_ref, on_select_all.clone(), on_selection_change.clone(), false, &delete_target, &notes_query, on_quick_add_filter.clone(), on_page_change.clone(), on_page_size_change.clone(), on_refresh.clone()) }
            } else {
                { list_view(&notes, *total_notes, &page, &page_size, &selected, &select_all_ref, on_select_all, on_selection_change, false, &delete_target, &notes_query, on_quick_add_filter, on_page_change, on_page_size_change, on_refresh) }
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
                    { for LABEL_FILTER_DISPLAY_OPERATORS.iter().map(|operator| {
                        let selected = *operator == filter_operator;
                        html! {
                            <option value={(*operator).to_string()} selected={selected}>{ *operator }</option>
                        }
                    }) }
                </select>
                <input
                    class="input label-filter-value"
                    type={label_value_input_type(value_type, filter_operator)}
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
                        let aria_label = remove_filter_aria_label(filter);
                        html! {
                            <button
                                type="button"
                                class="chip chip-primary active-label-filter"
                                title="Remove filter"
                                aria-label={aria_label}
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
    if filter.operator == "==" {
        let value = if filter.value.is_empty() {
            "(empty)".to_string()
        } else if filter.value.trim() != filter.value {
            format!("\"{}\"", filter.value)
        } else {
            filter.value.clone()
        };
        return format!("{}=={value}", filter.key);
    }
    if filter.value.is_empty() && filter.operator != "==" {
        filter.key.clone()
    } else {
        format!("{}{}{}", filter.key, filter.operator, filter.value)
    }
}

fn remove_filter_aria_label(filter: &LabelFilter) -> String {
    format!("Remove label filter {}", label_filter_label(filter))
}

fn label_value_input_type(value_type: &str, operator: &str) -> &'static str {
    if matches!(operator, "==" | "^=" | "$=" | "~=") {
        return "text";
    }

    match value_type {
        "number" => "number",
        "date" => "date",
        "datetime" => "datetime-local",
        "time" => "time",
        _ => "text",
    }
}

fn format_timestamp(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "-".to_string();
    }
    let Some(datetime) = chrono::DateTime::from_timestamp(timestamp, 0) else {
        return "-".to_string();
    };
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

/// Full-list view: the current page of notes plus the pagination bar.
fn list_view(
    notes: &[NoteSummary],
    total: usize,
    page: &UseStateHandle<usize>,
    page_size: &UseStateHandle<usize>,
    selected: &UseReducerHandle<SelectionState>,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
    on_selection_change: Callback<(String, bool)>,
    selection_disabled: bool,
    delete_target: &UseStateHandle<Option<(String, String, i64)>>,
    notes_query: &NotesQueryParams,
    on_quick_add_filter: Callback<(String, String)>,
    on_page_change: Callback<usize>,
    on_page_size_change: Callback<usize>,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    let on_delete_target = {
        let delete_target = delete_target.clone();
        Callback::from(move |target| delete_target.set(Some(target)))
    };
    if total == 0 {
        return note_table(
            notes,
            &**selected,
            select_all_ref,
            on_select_all,
            on_selection_change,
            selection_disabled,
            notes_query,
            on_delete_target,
            on_quick_add_filter,
        );
    }
    let per_page = **page_size;
    let total_pages = total.div_ceil(per_page);
    let current = (**page).min(total_pages - 1);
    let start = current * per_page;
    let end = (start + notes.len()).min(total);

    html! {
        <>
            { note_table(notes, &**selected, select_all_ref, on_select_all, on_selection_change, selection_disabled, notes_query, on_delete_target, on_quick_add_filter) }
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
                { for PAGE_SIZE_OPTIONS.iter().map(|size| {
                    let selected = *size == page_size;
                    html! {
                        <option value={size.to_string()} selected={selected}>{ format!("{size}条/页") }</option>
                    }
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
    selected: &HashSet<String>,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
    on_selection_change: Callback<(String, bool)>,
    selection_disabled: bool,
    notes_query: &NotesQueryParams,
    on_delete_target: Callback<(String, String, i64)>,
    on_quick_add_filter: Callback<(String, String)>,
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
    let all_selected = !notes.is_empty() && notes.iter().all(|note| selected.contains(&note.id));
    html! {
        <div class="table-scroll">
            <table class="table note-table">
                { note_table_head(false, all_selected, select_all_ref, on_select_all, selection_disabled) }
                <tbody>
                    { for notes.iter().map(|note| note_row(
                        &note.id,
                        &note.title,
                        note.revision,
                        None,
                        &note.labels,
                        note.created_at,
                        note.updated_at,
                        selected.contains(&note.id),
                        on_selection_change.clone(),
                        selection_disabled,
                        notes_query,
                        on_delete_target.clone(),
                        on_quick_add_filter.clone(),
                    )) }
                </tbody>
            </table>
        </div>
    }
}

fn note_table_head(
    show_score: bool,
    all_selected: bool,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
    selection_disabled: bool,
) -> Html {
    html! {
        <thead>
            <tr>
                <th class="col-select">
                    <input
                        ref={select_all_ref.clone()}
                        type="checkbox"
                        class="checkbox checkbox-primary"
                        checked={all_selected}
                        disabled={selection_disabled}
                        onchange={on_select_all}
                        aria-label="Select all visible notes"
                    />
                </th>
                if show_score {
                    <th class="col-score">{ "Score" }</th>
                }
                <th class="col-title">{ "Title" }</th>
                <th>{ "Labels" }</th>
                <th class="col-time">{ "Created" }</th>
                <th class="col-time">{ "Updated" }</th>
                <th class="col-actions">{ "Actions" }</th>
            </tr>
        </thead>
    }
}

struct NoteRowLinks {
    title: LinkProps<Route, NotesQueryParams>,
    view: LinkProps<Route, NotesQueryParams>,
    edit: LinkProps<Route, NotesQueryParams>,
}

fn note_row_links(id: &str, title: &str, notes_query: &NotesQueryParams) -> NoteRowLinks {
    NoteRowLinks {
        title: LinkProps {
            classes: classes!("note-title-link"),
            to: Route::NoteShow { id: id.into() },
            query: Some(notes_query.clone()),
            state: None,
            disabled: false,
            anchor_ref: NodeRef::default(),
            children: html! { title.to_string() },
        },
        view: LinkProps {
            classes: classes!("btn", "btn-ghost", "btn-icon"),
            to: Route::NoteShow { id: id.into() },
            query: Some(notes_query.clone()),
            state: None,
            disabled: false,
            anchor_ref: NodeRef::default(),
            children: html! {
                <>{ icons::eye() }<span class="sr-only">{ "View" }</span></>
            },
        },
        edit: LinkProps {
            classes: classes!("btn", "btn-ghost", "btn-icon"),
            to: Route::NoteEdit { id: id.into() },
            query: Some(notes_query.clone()),
            state: None,
            disabled: false,
            anchor_ref: NodeRef::default(),
            children: html! {
                <>{ icons::pencil() }<span class="sr-only">{ "Edit" }</span></>
            },
        },
    }
}

fn emit_delete_target(
    on_delete_target: &Callback<(String, String, i64)>,
    id: &str,
    title: &str,
    revision: i64,
) {
    on_delete_target.emit((id.to_string(), title.to_string(), revision));
}

fn emit_quick_label(on_quick_add_filter: &Callback<(String, String)>, key: &str, value: &str) {
    on_quick_add_filter.emit((key.to_string(), value.to_string()));
}

#[allow(clippy::too_many_arguments)]
fn note_row(
    id: &str,
    title: &str,
    revision: i64,
    score: Option<f32>,
    labels: &[(String, String)],
    created_at: i64,
    updated_at: i64,
    selected: bool,
    on_selection_change: Callback<(String, bool)>,
    selection_disabled: bool,
    notes_query: &NotesQueryParams,
    on_delete_target: Callback<(String, String, i64)>,
    on_quick_add_filter: Callback<(String, String)>,
) -> Html {
    let id = id.to_string();
    let title = title.to_string();
    let links = note_row_links(&id, &title, notes_query);
    let NoteRowLinks {
        title: title_link,
        view: view_link,
        edit: edit_link,
    } = links;
    let on_remove = {
        let id = id.clone();
        let title = title.clone();
        Callback::from(move |_| emit_delete_target(&on_delete_target, &id, &title, revision))
    };
    let on_select = {
        let id = id.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            on_selection_change.emit((id.clone(), input.checked()));
        })
    };
    html! {
        <tr key={id.clone()}>
            <td class="col-select">
                <input
                    type="checkbox"
                    class="checkbox checkbox-primary"
                    checked={selected}
                    disabled={selection_disabled}
                    onchange={on_select}
                    aria-label={format!("Select {title}")}
                />
            </td>
            if let Some(score) = score {
                <td class="col-score">{ format!("{score:.4}") }</td>
            }
            <td class="col-title">
                <Link<Route, NotesQueryParams> ..title_link />
            </td>
            <td>
                <div class="applied-labels">
                    { for labels.iter().map(|(key, value)| {
                        label_chip(key, value, on_quick_add_filter.clone())
                    }) }
                </div>
            </td>
            <td class="col-time">{ format_timestamp(created_at) }</td>
            <td class="col-time">{ format_timestamp(updated_at) }</td>
            <td class="col-actions">
                <div class="row-actions">
                    <Link<Route, NotesQueryParams> ..view_link />
                    <Link<Route, NotesQueryParams> ..edit_link />
                    <button type="button" class="btn btn-ghost btn-icon icon-danger"
                        onclick={on_remove}>
                        { icons::trash() }<span class="sr-only">{ "Remove" }</span>
                    </button>
                </div>
            </td>
        </tr>
    }
}

fn label_chip(key: &str, value: &str, on_quick_add_filter: Callback<(String, String)>) -> Html {
    let label = format!("{key}: {value}");
    let title = format!("Filter by {label}");
    let on_click = {
        let key = key.to_string();
        let value = value.to_string();
        Callback::from(move |_: MouseEvent| {
            emit_quick_label(&on_quick_add_filter, &key, &value);
        })
    };
    // TODO(upstream): duskmoon-dev/yew-duskmoon-ui#10
    // WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#10 renders a native action trigger
    // until Popover supports consumer click handlers and ARIA customization.
    html! {
        <div class="tooltip tooltip-primary tooltip-bottom tooltip-rich note-label-tooltip note-label-filter-action">
            <button
                type="button"
                class="chip chip-primary note-label-chip"
                aria-label={title}
                onclick={on_click}
            >
                { label }
            </button>
            <div class="tooltip-content" role="tooltip">
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
            </div>
        </div>
    }
}

fn search_results_view(
    hits: &[SearchResultSummary],
    page: &UseStateHandle<usize>,
    page_size: &UseStateHandle<usize>,
    selected: &UseReducerHandle<SelectionState>,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
    on_selection_change: Callback<(String, bool)>,
    selection_disabled: bool,
    delete_target: &UseStateHandle<Option<(String, String, i64)>>,
    notes_query: &NotesQueryParams,
    on_quick_add_filter: Callback<(String, String)>,
    on_page_change: Callback<usize>,
    on_page_size_change: Callback<usize>,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    if hits.is_empty() {
        return html! { <p class="empty">{ "No matches. Try different words." }</p> };
    }
    let total = hits.len();
    let search_page = search_result_page(total, **page, **page_size, notes_query);
    let on_delete_target = {
        let delete_target = delete_target.clone();
        Callback::from(move |target| delete_target.set(Some(target)))
    };
    html! {
        <>
            { search_result_table(&hits[search_page.start..search_page.end], &**selected, select_all_ref, on_select_all, on_selection_change, selection_disabled, &search_page.notes_query, on_delete_target, on_quick_add_filter) }
            { pagination_bar(search_page.current, search_page.total_pages, total, search_page.start, search_page.end, **page_size, on_page_change, on_page_size_change, on_refresh) }
        </>
    }
}

struct SearchResultPage {
    current: usize,
    total_pages: usize,
    start: usize,
    end: usize,
    notes_query: NotesQueryParams,
}

fn search_result_page(
    total: usize,
    requested_page: usize,
    page_size: usize,
    notes_query: &NotesQueryParams,
) -> SearchResultPage {
    let total_pages = total.div_ceil(page_size);
    let current = requested_page.min(total_pages.saturating_sub(1));
    let start = current * page_size;
    let end = (start + page_size).min(total);
    let mut notes_query = notes_query.clone();
    notes_query.current = current + 1;
    SearchResultPage {
        current,
        total_pages,
        start,
        end,
        notes_query,
    }
}

fn search_result_table(
    hits: &[SearchResultSummary],
    selected: &HashSet<String>,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
    on_selection_change: Callback<(String, bool)>,
    selection_disabled: bool,
    notes_query: &NotesQueryParams,
    on_delete_target: Callback<(String, String, i64)>,
    on_quick_add_filter: Callback<(String, String)>,
) -> Html {
    let all_selected = !hits.is_empty() && hits.iter().all(|hit| selected.contains(&hit.id));
    html! {
        <div class="table-scroll">
            <table class="table note-table search-result-table">
                { note_table_head(true, all_selected, select_all_ref, on_select_all, selection_disabled) }
                <tbody>
                    { for hits.iter().map(|result| note_row(
                        &result.id,
                        &result.title,
                        result.revision,
                        Some(result.score),
                        &result.labels,
                        result.created_at,
                        result.updated_at,
                        selected.contains(&result.id),
                        on_selection_change.clone(),
                        selection_disabled,
                        notes_query,
                        on_delete_target.clone(),
                        on_quick_add_filter.clone(),
                    )) }
                </tbody>
            </table>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;
    use yew::virtual_dom::VNode;

    fn attribute<'a>(node: &'a VNode, name: &str) -> Option<&'a str> {
        let VNode::VTag(tag) = node else {
            panic!("expected an element node");
        };
        tag.attributes
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    }

    fn element_children(node: &VNode) -> Vec<&VNode> {
        let VNode::VTag(tag) = node else {
            panic!("expected an element node");
        };
        match tag.children().expect("element should have children") {
            VNode::VList(children) => children.iter().collect(),
            child => vec![child],
        }
    }

    fn visible_text(node: &VNode) -> Vec<String> {
        match node {
            VNode::VText(text) => vec![text.text.to_string()],
            VNode::VTag(_) => element_children(node)
                .into_iter()
                .flat_map(visible_text)
                .collect(),
            VNode::VList(children) => children.iter().flat_map(visible_text).collect(),
            _ => Vec::new(),
        }
    }

    fn rendered_children(node: &VNode) -> Vec<&VNode> {
        fn append_rendered<'a>(node: &'a VNode, children: &mut Vec<&'a VNode>) {
            match node {
                VNode::VList(list) => {
                    for child in list.iter() {
                        append_rendered(child, children);
                    }
                }
                _ => children.push(node),
            }
        }

        let mut children = Vec::new();
        for child in element_children(node) {
            append_rendered(child, &mut children);
        }
        children
    }

    fn only_child_with_tag<'a>(node: &'a VNode, tag_name: &str) -> &'a VNode {
        let matches = rendered_children(node)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == tag_name))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "expected one {tag_name} child");
        matches[0]
    }

    #[test]
    fn selection_reducer_replaces_toggles_clears_and_retains_visible_ids() {
        let state =
            Rc::new(SelectionState::default()).reduce(SelectionAction::Replace(HashSet::from([
                "first".to_string(),
                "hidden".to_string(),
            ])));
        let state = state.reduce(SelectionAction::Toggle {
            id: "second".to_string(),
            selected: true,
        });
        let state = state.reduce(SelectionAction::RetainVisible(HashSet::from([
            "first".to_string(),
            "second".to_string(),
        ])));

        assert_eq!(
            **state,
            HashSet::from(["first".to_string(), "second".to_string()])
        );

        let state = state.reduce(SelectionAction::Clear);
        assert!(state.is_empty());
    }

    #[test]
    fn selected_note_targets_follow_visible_order_and_revisions() {
        let visible = vec![
            NoteSelectionTarget::new("third", 3),
            NoteSelectionTarget::new("second", 2),
            NoteSelectionTarget::new("first", 1),
        ];
        let selected = HashSet::from([
            "first".to_string(),
            "third".to_string(),
            "hidden".to_string(),
        ]);

        assert_eq!(
            selected_note_targets(&visible, &selected),
            vec![
                api::BatchNoteTarget::new("third", 3),
                api::BatchNoteTarget::new("first", 1),
            ]
        );
    }

    #[test]
    fn selection_target_helpers_map_normal_notes_and_only_the_current_search_page() {
        let notes = vec![NoteSummary {
            id: "normal".into(),
            title: "Normal".into(),
            content: String::new(),
            attachments: vec![],
            labels: vec![],
            created_at: 0,
            updated_at: 0,
            revision: 8,
        }];
        assert_eq!(
            note_selection_targets(&notes),
            vec![NoteSelectionTarget::new("normal", 8)]
        );

        let hits = (1..=5)
            .map(|index| SearchResultSummary {
                id: format!("hit-{index}"),
                title: format!("Hit {index}"),
                revision: index,
                score: index as f32,
                labels: vec![],
                created_at: 0,
                updated_at: 0,
            })
            .collect::<Vec<_>>();
        let page = search_result_page(
            5,
            1,
            2,
            &NotesQueryParams {
                current: 2,
                page_size: 2,
                search: Some("hit".into()),
                labels: None,
            },
        );

        assert_eq!(
            search_result_selection_targets(&hits[page.start..page.end]),
            vec![
                NoteSelectionTarget::new("hit-3", 3),
                NoteSelectionTarget::new("hit-4", 4),
            ]
        );
    }

    #[test]
    fn select_all_indeterminate_only_for_a_partial_visible_selection() {
        assert!(!select_all_is_indeterminate(0, 3));
        assert!(select_all_is_indeterminate(1, 3));
        assert!(!select_all_is_indeterminate(3, 3));
        assert!(!select_all_is_indeterminate(1, 0));
    }

    #[test]
    fn normal_note_table_renders_select_column_before_existing_note_columns() {
        let query = NotesQueryParams {
            current: 1,
            page_size: 10,
            search: None,
            labels: None,
        };
        let notes = vec![NoteSummary {
            id: "note-1".into(),
            title: "Normal note".into(),
            content: String::new(),
            attachments: vec![],
            labels: vec![("project".into(), "agent-note".into())],
            created_at: 1_700_000_000,
            updated_at: 1_700_000_060,
            revision: 4,
        }];
        let table_wrapper = note_table(
            &notes,
            &HashSet::from(["note-1".to_string()]),
            &NodeRef::default(),
            Callback::noop(),
            Callback::noop(),
            false,
            &query,
            Callback::noop(),
            Callback::noop(),
        );
        let table = only_child_with_tag(&table_wrapper, "table");
        let header_row = only_child_with_tag(only_child_with_tag(table, "thead"), "tr");
        let headers = rendered_children(header_row)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "th"))
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 6);
        assert_eq!(attribute(headers[0], "class"), Some("col-select"));
        let select_all = only_child_with_tag(headers[0], "input");
        assert_eq!(
            attribute(select_all, "aria-label"),
            Some("Select all visible notes")
        );
        assert!(format!("{select_all:?}").contains("checked: Some(true)"));
        assert_eq!(
            headers[1..]
                .iter()
                .flat_map(|header| visible_text(header))
                .collect::<Vec<_>>(),
            ["Title", "Labels", "Created", "Updated", "Actions"]
        );

        let row = only_child_with_tag(only_child_with_tag(table, "tbody"), "tr");
        let cells = rendered_children(row)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "td"))
            .collect::<Vec<_>>();
        assert_eq!(cells.len(), 6);
        assert_eq!(attribute(cells[0], "class"), Some("col-select"));
        let select_row = only_child_with_tag(cells[0], "input");
        assert_eq!(
            attribute(select_row, "class"),
            Some("checkbox checkbox-primary")
        );
        assert_eq!(
            attribute(select_row, "aria-label"),
            Some("Select Normal note")
        );
        assert!(format!("{select_row:?}").contains("checked: Some(true)"));
        assert_eq!(attribute(cells[1], "class"), Some("col-title"));
        let actions = only_child_with_tag(cells[5], "div");
        assert_eq!(element_children(actions).len(), 3);
        assert_eq!(visible_text(element_children(actions)[2]), ["Remove"]);
    }

    #[test]
    fn label_chip_uses_tooltip_container_without_popover_positioning() {
        let chip = label_chip("operation", "home/dgx-spark", Callback::noop());
        let classes = attribute(&chip, "class").expect("label wrapper should have classes");

        assert!(classes.split_whitespace().any(|class| class == "tooltip"));
        assert!(!classes.split_whitespace().any(|class| class == "popover"));
    }

    #[test]
    fn label_chip_does_not_render_a_duplicate_native_title_tooltip() {
        let chip = label_chip("operation", "home/dgx-spark", Callback::noop());
        let button = element_children(&chip)
            .into_iter()
            .find(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "button"))
            .expect("label wrapper should contain a button");

        assert_eq!(attribute(button, "title"), None);
    }

    #[test]
    fn label_tooltip_keeps_rich_details_at_a_readable_width() {
        let app_css = include_str!("../../app.css");

        assert!(app_css.contains(
            ".note-label-tooltip .tooltip-content {\n    min-width: min(12rem, calc(100vw - 2rem));"
        ));
    }

    #[test]
    fn note_table_reserves_space_for_label_actions() {
        let app_css = include_str!("../../app.css");

        assert!(app_css.contains(".note-table {\n    width: 100%;\n    min-width: 60rem;"));
    }

    #[test]
    fn search_result_table_renders_select_then_score_with_normal_note_row_content() {
        let query = NotesQueryParams {
            current: 2,
            page_size: 30,
            search: Some("match".into()),
            labels: Some("~project==agent-note".into()),
        };
        let table_wrapper = search_result_table(
            &[SearchResultSummary {
                id: "note-1".into(),
                title: "Matched note".into(),
                revision: 4,
                score: 0.12345,
                labels: vec![("project".into(), "agent-note".into())],
                created_at: 1_700_000_000,
                updated_at: 1_700_000_060,
            }],
            &HashSet::from(["note-1".to_string()]),
            &NodeRef::default(),
            Callback::noop(),
            Callback::noop(),
            false,
            &query,
            Callback::noop(),
            Callback::noop(),
        );

        assert_eq!(attribute(&table_wrapper, "class"), Some("table-scroll"));
        let table = only_child_with_tag(&table_wrapper, "table");
        assert_eq!(
            attribute(table, "class"),
            Some("table note-table search-result-table")
        );
        let thead = only_child_with_tag(table, "thead");
        let header_row = only_child_with_tag(thead, "tr");
        let headers = rendered_children(header_row)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "th"))
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 7);
        assert_eq!(
            headers[1..]
                .iter()
                .flat_map(|header| visible_text(header))
                .collect::<Vec<_>>(),
            ["Score", "Title", "Labels", "Created", "Updated", "Actions"]
        );
        assert_eq!(attribute(headers[0], "class"), Some("col-select"));
        let select_all = only_child_with_tag(headers[0], "input");
        assert_eq!(
            attribute(select_all, "aria-label"),
            Some("Select all visible notes")
        );
        assert!(format!("{select_all:?}").contains("checked: Some(true)"));

        let tbody = only_child_with_tag(table, "tbody");
        let row = only_child_with_tag(tbody, "tr");
        let cells = rendered_children(row)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "td"))
            .collect::<Vec<_>>();
        assert_eq!(cells.len(), 7);
        assert_eq!(attribute(cells[0], "class"), Some("col-select"));
        let select_row = only_child_with_tag(cells[0], "input");
        assert_eq!(
            attribute(select_row, "class"),
            Some("checkbox checkbox-primary")
        );
        assert_eq!(
            attribute(select_row, "aria-label"),
            Some("Select Matched note")
        );
        assert!(format!("{select_row:?}").contains("checked: Some(true)"));
        assert_eq!(attribute(cells[1], "class"), Some("col-score"));
        assert_eq!(visible_text(cells[1]), ["0.1235"]);
        assert_eq!(attribute(cells[2], "class"), Some("col-title"));
        assert!(matches!(
            element_children(cells[2]).as_slice(),
            [VNode::VComp(_)]
        ));
        let links = note_row_links("note-1", "Matched note", &query);
        assert_eq!(
            links.title.to,
            Route::NoteShow {
                id: "note-1".into()
            }
        );
        assert_eq!(links.title.query, Some(query.clone()));
        assert_eq!(visible_text(&links.title.children), ["Matched note"]);
        assert_eq!(
            links.view.to,
            Route::NoteShow {
                id: "note-1".into()
            }
        );
        assert_eq!(links.view.query, Some(query.clone()));
        assert_eq!(
            links.edit.to,
            Route::NoteEdit {
                id: "note-1".into()
            }
        );
        assert_eq!(links.edit.query, Some(query.clone()));
        assert!(visible_text(cells[3]).contains(&"project: agent-note".to_string()));
        assert_eq!(visible_text(cells[4]), ["2023-11-14 22:13"]);
        assert_eq!(visible_text(cells[5]), ["2023-11-14 22:14"]);
        let actions = only_child_with_tag(cells[6], "div");
        let action_children = element_children(actions);
        assert_eq!(action_children.len(), 3);
        assert!(matches!(action_children[0], VNode::VComp(_)));
        assert!(matches!(action_children[1], VNode::VComp(_)));
        assert_eq!(visible_text(action_children[2]), ["Remove"]);

        let app_css = include_str!("../../app.css");
        assert!(app_css.contains(".search-result-table {\n    min-width: 66rem;"));
        assert!(app_css.contains(
            ".col-score {\n    width: 6rem;\n    white-space: nowrap;\n    font-variant-numeric: tabular-nums;"
        ));
    }

    #[test]
    fn search_result_page_clamps_link_context_when_hits_shrink() {
        let stale_query = NotesQueryParams {
            current: 5,
            page_size: 2,
            search: Some("match".into()),
            labels: Some("~project==agent-note".into()),
        };

        let page = search_result_page(3, 4, 2, &stale_query);
        assert_eq!((page.current, page.start, page.end), (1, 2, 3));
        assert_eq!(page.notes_query.current, 2);
        assert_eq!(page.notes_query.page_size, stale_query.page_size);
        assert_eq!(page.notes_query.search, stale_query.search);
        assert_eq!(page.notes_query.labels, stale_query.labels);

        let last_match = SearchResultSummary {
            id: "note-3".into(),
            title: "Last match".into(),
            revision: 7,
            score: 0.25,
            labels: vec![("project".into(), "agent-note".into())],
            created_at: 1_700_000_000,
            updated_at: 1_700_000_060,
        };
        let hits = vec![last_match; 3];
        let rendered = search_result_table(
            &hits[page.start..page.end],
            &HashSet::new(),
            &NodeRef::default(),
            Callback::noop(),
            Callback::noop(),
            false,
            &page.notes_query,
            Callback::noop(),
            Callback::noop(),
        );
        let table = only_child_with_tag(&rendered, "table");
        let tbody = only_child_with_tag(table, "tbody");
        let row = only_child_with_tag(tbody, "tr");
        let title_cell = rendered_children(row)
            .into_iter()
            .filter(|child| matches!(child, VNode::VTag(tag) if tag.tag() == "td"))
            .nth(2)
            .expect("scored row should have a title cell");
        assert!(matches!(
            element_children(title_cell).as_slice(),
            [VNode::VComp(_)]
        ));

        let links = note_row_links("note-3", "Last match", &page.notes_query);
        assert_eq!(links.title.query, Some(page.notes_query.clone()));
        assert_eq!(links.view.query, Some(page.notes_query.clone()));
        assert_eq!(links.edit.query, Some(page.notes_query.clone()));

        let in_range = search_result_page(
            6,
            1,
            2,
            &NotesQueryParams {
                current: 2,
                ..stale_query.clone()
            },
        );
        assert_eq!(in_range.notes_query.current, 2);
        assert_eq!(in_range.notes_query.page_size, stale_query.page_size);
        assert_eq!(in_range.notes_query.search, stale_query.search);
        assert_eq!(in_range.notes_query.labels, stale_query.labels);
    }

    #[test]
    fn search_result_row_forwards_delete_and_quick_label_payloads() {
        let removed = Rc::new(RefCell::new(None));
        let on_delete_target = {
            let removed = removed.clone();
            Callback::from(move |target| *removed.borrow_mut() = Some(target))
        };
        emit_delete_target(&on_delete_target, "note-1", "Matched note", 4);
        assert_eq!(
            *removed.borrow(),
            Some(("note-1".into(), "Matched note".into(), 4))
        );

        let clicked_label = Rc::new(RefCell::new(None));
        let on_quick_add_filter = {
            let clicked_label = clicked_label.clone();
            Callback::from(move |label| *clicked_label.borrow_mut() = Some(label))
        };
        emit_quick_label(&on_quick_add_filter, "project", "agent-note");
        assert_eq!(
            *clicked_label.borrow(),
            Some(("project".into(), "agent-note".into()))
        );
    }

    #[test]
    fn retrieval_copy_names_title_and_content() {
        assert_eq!(RETRIEVAL_PLACEHOLDER, "Retrieve by title or content");
        assert_eq!(RETRIEVE_BUTTON_LABEL, "Retrieve");
    }

    #[test]
    fn notes_route_query_preserves_list_context() {
        let state = NotesUrlState {
            current: 3,
            page_size: 30,
            search: "release notes".to_string(),
            labels: vec![LabelFilter {
                key: "status".to_string(),
                operator: "=".to_string(),
                value: "draft".to_string(),
            }],
            invalid_labels: false,
        };

        assert_eq!(
            notes_query_params(&state),
            crate::routes::NotesQueryParams {
                current: 3,
                page_size: 30,
                search: Some("release notes".to_string()),
                labels: Some("status=draft".to_string()),
            }
        );
    }

    #[test]
    fn quick_label_filter_appends_a_new_equality_filter() {
        assert_eq!(
            quick_label_filters(&[], "status", "draft"),
            vec![LabelFilter {
                key: "status".to_string(),
                operator: "==".to_string(),
                value: "draft".to_string(),
            }]
        );
    }

    #[test]
    fn quick_label_filter_is_idempotent_for_an_exact_filter() {
        let filters = vec![LabelFilter {
            key: "status".to_string(),
            operator: "==".to_string(),
            value: "draft".to_string(),
        }];

        assert_eq!(quick_label_filters(&filters, "status", "draft"), filters);
    }

    #[test]
    fn quick_label_filter_canonicalizes_legacy_equalities_to_exact_equality() {
        let filters = vec![
            LabelFilter {
                key: "status".into(),
                operator: "=".into(),
                value: "draft".into(),
            },
            LabelFilter {
                key: "status".into(),
                operator: "=".into(),
                value: "review".into(),
            },
        ];

        assert_eq!(
            quick_label_filters(&filters, "status", "draft"),
            vec![LabelFilter {
                key: "status".into(),
                operator: "==".into(),
                value: "draft".into(),
            }]
        );
    }

    #[test]
    fn quick_label_filter_replaces_equalities_and_preserves_other_predicates() {
        let filters = vec![
            LabelFilter {
                key: "status".into(),
                operator: "==".into(),
                value: "draft".into(),
            },
            LabelFilter {
                key: "status".into(),
                operator: "!=".into(),
                value: "archived".into(),
            },
            LabelFilter {
                key: "project".into(),
                operator: "=".into(),
                value: "agent-note".into(),
            },
            LabelFilter {
                key: "status".into(),
                operator: "=".into(),
                value: "review".into(),
            },
        ];

        assert_eq!(
            quick_label_filters(&filters, "status", "published"),
            vec![
                LabelFilter {
                    key: "status".into(),
                    operator: "==".into(),
                    value: "published".into(),
                },
                filters[1].clone(),
                filters[2].clone(),
            ]
        );
    }

    #[test]
    fn parses_and_serializes_string_match_filters() {
        let selector = "topic^=Rust&topic$=LANG&topic~=^ru.*t$";
        let filters = parse_label_filters(selector).unwrap();
        assert_eq!(
            filters,
            vec![
                LabelFilter {
                    key: "topic".to_string(),
                    operator: "^=".to_string(),
                    value: "Rust".to_string(),
                },
                LabelFilter {
                    key: "topic".to_string(),
                    operator: "$=".to_string(),
                    value: "LANG".to_string(),
                },
                LabelFilter {
                    key: "topic".to_string(),
                    operator: "~=".to_string(),
                    value: "^ru.*t$".to_string(),
                },
            ]
        );
        assert_eq!(
            api::label_filter_selector(&filters),
            Some(selector.to_string())
        );
    }

    #[test]
    fn parses_string_match_filters_with_operator_text_in_values() {
        let filters = parse_label_filters(
            "topic~=^ru!=st$&prefix^=a>=b&suffix$=a<=b&name~=a==b&env=foo==bar&env==foo&~env==foo%3Dbar",
        )
        .unwrap();

        assert_eq!(
            filters,
            vec![
                LabelFilter {
                    key: "topic".to_string(),
                    operator: "~=".to_string(),
                    value: "^ru!=st$".to_string(),
                },
                LabelFilter {
                    key: "prefix".to_string(),
                    operator: "^=".to_string(),
                    value: "a>=b".to_string(),
                },
                LabelFilter {
                    key: "suffix".to_string(),
                    operator: "$=".to_string(),
                    value: "a<=b".to_string(),
                },
                LabelFilter {
                    key: "name".to_string(),
                    operator: "~=".to_string(),
                    value: "a==b".to_string(),
                },
                LabelFilter {
                    key: "env".to_string(),
                    operator: "=".to_string(),
                    value: "foo==bar".to_string(),
                },
                LabelFilter {
                    key: "env".to_string(),
                    operator: "=".to_string(),
                    value: "=foo".to_string()
                },
                LabelFilter {
                    key: "env".to_string(),
                    operator: "==".to_string(),
                    value: "foo=bar".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_longest_operator_at_the_same_offset() {
        assert_eq!(
            parse_label_filters("priority>=10&date<=2026-07-20").unwrap(),
            vec![
                LabelFilter {
                    key: "priority".to_string(),
                    operator: ">=".to_string(),
                    value: "10".to_string(),
                },
                LabelFilter {
                    key: "date".to_string(),
                    operator: "<=".to_string(),
                    value: "2026-07-20".to_string(),
                },
            ]
        );
    }

    #[test]
    fn label_filter_display_operators_have_expected_order() {
        assert_eq!(
            LABEL_FILTER_DISPLAY_OPERATORS,
            ["==", "=", "!=", "^=", "$=", "~=", ">", ">=", "<", "<="]
        );
    }

    #[test]
    fn exact_empty_label_filter_remains_distinct_from_presence() {
        assert_eq!(
            label_filter_label(&LabelFilter {
                key: "project".into(),
                operator: "==".into(),
                value: String::new(),
            }),
            "project==(empty)"
        );
    }

    #[test]
    fn malformed_exact_url_filters_are_explicitly_invalid() {
        for selector in ["~==secret", "status=ready&~project==%ZZ", "~project"] {
            let encoded = urlencoding::encode(selector);
            let state = parse_notes_query(&format!("labels={encoded}"));
            assert!(state.invalid_labels, "selector: {selector}");
            assert!(state.labels.is_empty(), "selector: {selector}");
        }
    }

    #[test]
    fn exact_filter_builder_preserves_raw_values_while_legacy_filters_trim() {
        for value in ["", " ", " padded ", "01"] {
            assert_eq!(
                build_label_filter("project", "==", value),
                Some(LabelFilter {
                    key: "project".into(),
                    operator: "==".into(),
                    value: value.into(),
                })
            );
        }
        assert_eq!(
            build_label_filter(" project ", "=", " ready "),
            Some(LabelFilter {
                key: "project".into(),
                operator: "=".into(),
                value: "ready".into(),
            })
        );
    }

    #[test]
    fn exact_filter_chips_describe_empty_and_whitespace_values() {
        for (value, expected) in [
            ("", "project==(empty)"),
            (" ", "project==\" \""),
            (" padded ", "project==\" padded \""),
            ("ready", "project==ready"),
        ] {
            let filter = LabelFilter {
                key: "project".into(),
                operator: "==".into(),
                value: value.into(),
            };
            assert_eq!(label_filter_label(&filter), expected);
            assert_eq!(
                remove_filter_aria_label(&filter),
                format!("Remove label filter {expected}")
            );
        }
    }

    #[test]
    fn string_match_operators_always_use_text_inputs() {
        for value_type in ["number", "date", "datetime", "time"] {
            for operator in ["^=", "$=", "~="] {
                assert_eq!(label_value_input_type(value_type, operator), "text");
            }
        }
    }

    #[test]
    fn comparison_operators_keep_typed_inputs() {
        assert_eq!(label_value_input_type("number", "="), "number");
        assert_eq!(label_value_input_type("date", ">="), "date");
        assert_eq!(label_value_input_type("datetime", "<"), "datetime-local");
        assert_eq!(label_value_input_type("time", "<="), "time");
    }
}
