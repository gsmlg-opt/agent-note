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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchLabelMode {
    Add,
    Update,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchLabelInputFocus {
    Source,
    Destination,
}

fn batch_label_initial_focus(mode: Option<BatchLabelMode>) -> Option<BatchLabelInputFocus> {
    match mode {
        Some(BatchLabelMode::Add) => Some(BatchLabelInputFocus::Destination),
        Some(BatchLabelMode::Update | BatchLabelMode::Remove) => Some(BatchLabelInputFocus::Source),
        None => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BatchLabelUiState {
    mode: Option<BatchLabelMode>,
    source_key: String,
    destination_key: String,
    value: String,
    mutating: bool,
    error: Option<String>,
    success: Option<String>,
}

enum BatchLabelUiAction {
    Open(BatchLabelMode),
    SourceKeyChanged(String),
    DestinationKeyChanged(String),
    ValueChanged(String),
    SubmitStarted,
    Success(String),
    GenericFailure(String),
    StaleFailure,
    Close,
    ClearSuccess,
}

impl BatchLabelUiState {
    fn reset_dialog(&mut self) {
        self.mode = None;
        self.source_key.clear();
        self.destination_key.clear();
        self.value.clear();
        self.error = None;
    }
}

impl Reducible for BatchLabelUiState {
    type Action = BatchLabelUiAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = (*self).clone();

        match action {
            BatchLabelUiAction::Open(mode) => {
                next.source_key.clear();
                next.destination_key.clear();
                next.value.clear();
                next.mode = Some(mode);
                next.mutating = false;
                next.error = None;
                next.success = None;
            }
            BatchLabelUiAction::SourceKeyChanged(value) => next.source_key = value,
            BatchLabelUiAction::DestinationKeyChanged(value) => next.destination_key = value,
            BatchLabelUiAction::ValueChanged(value) => next.value = value,
            BatchLabelUiAction::SubmitStarted => {
                next.mutating = true;
                next.error = None;
                next.success = None;
            }
            BatchLabelUiAction::Success(message) => {
                next.reset_dialog();
                next.mutating = false;
                next.success = Some(message);
            }
            BatchLabelUiAction::GenericFailure(message) => {
                next.mutating = false;
                next.error = Some(message);
            }
            BatchLabelUiAction::StaleFailure => {
                next.mutating = false;
                next.error = None;
            }
            BatchLabelUiAction::Close if !next.mutating => next.reset_dialog(),
            BatchLabelUiAction::Close => {}
            BatchLabelUiAction::ClearSuccess => next.success = None,
        }

        next.into()
    }
}

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

fn selected_visible_count(visible: &[NoteSelectionTarget], selected: &HashSet<String>) -> usize {
    visible
        .iter()
        .filter(|target| selected.contains(&target.id))
        .count()
}

fn label_action_from_draft(
    mode: BatchLabelMode,
    source_key: &str,
    destination_key: &str,
    value: &str,
) -> Option<api::BatchLabelAction> {
    let source_key = source_key.trim();
    let destination_key = destination_key.trim();

    match mode {
        BatchLabelMode::Add if !destination_key.is_empty() => Some(api::BatchLabelAction::Add {
            key: destination_key.to_string(),
            value: value.to_string(),
        }),
        BatchLabelMode::Update if !source_key.is_empty() && !destination_key.is_empty() => {
            Some(api::BatchLabelAction::Update {
                from_key: source_key.to_string(),
                key: destination_key.to_string(),
                value: value.to_string(),
            })
        }
        BatchLabelMode::Remove if !source_key.is_empty() => Some(api::BatchLabelAction::Remove {
            key: source_key.to_string(),
        }),
        _ => None,
    }
}

fn batch_label_result_message(updated: usize, requested: usize, unchanged: usize) -> String {
    let selected_noun = if requested == 1 { "note" } else { "notes" };
    format!("Updated {updated} of {requested} selected {selected_noun}; {unchanged} unchanged.")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BatchDeleteModalCopy {
    title: String,
    body: String,
    confirm: String,
}

fn batch_delete_modal_copy(count: usize) -> BatchDeleteModalCopy {
    let selected_noun = if count == 1 { "note" } else { "notes" };
    let title = if count == 1 {
        "Delete selected note"
    } else {
        "Delete selected notes"
    };

    BatchDeleteModalCopy {
        title: title.to_string(),
        body: format!("Move {count} selected {selected_noun} to Trash?"),
        confirm: format!("Delete {count} {selected_noun}"),
    }
}

fn batch_delete_result_message(count: usize) -> String {
    let selected_noun = if count == 1 { "note" } else { "notes" };
    format!("Moved {count} selected {selected_noun} to Trash.")
}

fn page_after_batch_delete(
    current_zero_based: usize,
    page_size: usize,
    deleted: usize,
    total_before: usize,
) -> usize {
    let total_after = total_before.saturating_sub(deleted);
    let pages_after = total_after.div_ceil(page_size.max(1));
    current_zero_based.min(pages_after.saturating_sub(1))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BatchDeleteUiState {
    open: bool,
    mutating: bool,
    error: Option<String>,
    success: Option<String>,
}

enum BatchDeleteUiAction {
    Open,
    SubmitStarted,
    Success(String),
    GenericFailure(String),
    StaleFailure,
    Close,
    ClearSuccess,
}

impl Reducible for BatchDeleteUiState {
    type Action = BatchDeleteUiAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = (*self).clone();

        match action {
            BatchDeleteUiAction::Open => {
                next.open = true;
                next.mutating = false;
                next.error = None;
                next.success = None;
            }
            BatchDeleteUiAction::SubmitStarted => {
                next.mutating = true;
                next.error = None;
                next.success = None;
            }
            BatchDeleteUiAction::Success(message) => {
                next.open = false;
                next.mutating = false;
                next.error = None;
                next.success = Some(message);
            }
            BatchDeleteUiAction::GenericFailure(message) => {
                next.mutating = false;
                next.error = Some(message);
            }
            BatchDeleteUiAction::StaleFailure => {
                next.mutating = false;
                next.error = None;
            }
            BatchDeleteUiAction::Close if !next.mutating => {
                next.open = false;
                next.error = None;
            }
            BatchDeleteUiAction::Close => {}
            BatchDeleteUiAction::ClearSuccess => next.success = None,
        }

        next.into()
    }
}

fn batch_toolbar(
    selected_count: usize,
    batch_mutating: bool,
    workflow_open: bool,
    refs: BatchLabelToolbarRefs,
    on_open: Callback<BatchLabelMode>,
    on_delete: Callback<()>,
) -> Html {
    let disabled = selected_count == 0 || batch_mutating || workflow_open;
    let open = |mode| {
        let callback = batch_label_open_callback(
            selected_count,
            batch_mutating,
            workflow_open,
            mode,
            on_open.clone(),
        );
        Callback::from(move |_: MouseEvent| callback.emit(()))
    };
    let delete = {
        let callback =
            batch_delete_open_callback(selected_count, batch_mutating, workflow_open, on_delete);
        Callback::from(move |_: MouseEvent| callback.emit(()))
    };
    let BatchLabelToolbarRefs {
        add: add_ref,
        update: update_ref,
        remove: remove_ref,
        delete: delete_ref,
    } = refs;

    html! {
        <div class="notes-batch-toolbar">
            <span class="notes-batch-selection-count" aria-live="polite">
                { format!("{selected_count} selected") }
            </span>
            <div class="notes-batch-actions">
                <button ref={add_ref} type="button" class="btn btn-outline" disabled={disabled} onclick={open(BatchLabelMode::Add)}>{ "Add label" }</button>
                <button ref={update_ref} type="button" class="btn btn-outline" disabled={disabled} onclick={open(BatchLabelMode::Update)}>{ "Update label" }</button>
                <button ref={remove_ref} type="button" class="btn btn-outline" disabled={disabled} onclick={open(BatchLabelMode::Remove)}>{ "Remove label" }</button>
                <button ref={delete_ref} type="button" class="btn btn-error" disabled={disabled} onclick={delete}>{ "Delete selected" }</button>
            </div>
        </div>
    }
}

fn batch_label_open_callback(
    selected_count: usize,
    batch_mutating: bool,
    workflow_open: bool,
    mode: BatchLabelMode,
    on_open: Callback<BatchLabelMode>,
) -> Callback<()> {
    Callback::from(move |_: ()| {
        if selected_count > 0 && !batch_mutating && !workflow_open {
            on_open.emit(mode);
        }
    })
}

fn batch_delete_open_callback(
    selected_count: usize,
    batch_mutating: bool,
    workflow_open: bool,
    on_open: Callback<()>,
) -> Callback<()> {
    Callback::from(move |_: ()| {
        if selected_count > 0 && !batch_mutating && !workflow_open {
            on_open.emit(());
        }
    })
}

fn batch_delete_submit_allowed(
    batch_mutating: bool,
    batch_in_flight: bool,
    stale_revision_blocked: bool,
) -> bool {
    !batch_mutating && !batch_in_flight && !stale_revision_blocked
}

fn batch_label_draft_input_allowed(batch_mutating: bool) -> bool {
    !batch_mutating
}

fn batch_label_submit_allowed(
    batch_mutating: bool,
    batch_in_flight: bool,
    stale_revision_blocked: bool,
) -> bool {
    !batch_mutating && !batch_in_flight && !stale_revision_blocked
}

fn batch_label_error_notice(generic_error: Option<&str>, stale_revision_blocked: bool) -> Html {
    let message = if stale_revision_blocked {
        Some("Selected notes changed after the list was loaded. Reload notes before retrying.")
    } else {
        generic_error
    };

    match message {
        Some(message) => html! { <p class="notes-batch-label-error" role="alert">{ message }</p> },
        None => html! {},
    }
}

#[derive(Clone)]
struct BatchLabelFormCallbacks {
    on_source_input: Callback<InputEvent>,
    on_destination_input: Callback<InputEvent>,
    on_value_input: Callback<InputEvent>,
    on_submit: Callback<SubmitEvent>,
    on_cancel: Callback<MouseEvent>,
    on_reload: Callback<MouseEvent>,
}

#[derive(Clone, Default)]
struct BatchLabelInputRefs {
    source: NodeRef,
    destination: NodeRef,
}

#[derive(Clone, Default)]
struct BatchLabelToolbarRefs {
    add: NodeRef,
    update: NodeRef,
    remove: NodeRef,
    delete: NodeRef,
}

#[derive(Clone)]
struct BatchDeleteFormCallbacks {
    on_submit: Callback<SubmitEvent>,
    on_cancel: Callback<MouseEvent>,
    on_reload: Callback<MouseEvent>,
}

impl BatchLabelFormCallbacks {
    #[cfg(test)]
    fn noop() -> Self {
        Self {
            on_source_input: Callback::noop(),
            on_destination_input: Callback::noop(),
            on_value_input: Callback::noop(),
            on_submit: Callback::noop(),
            on_cancel: Callback::noop(),
            on_reload: Callback::noop(),
        }
    }
}

impl BatchDeleteFormCallbacks {
    #[cfg(test)]
    fn noop() -> Self {
        Self {
            on_submit: Callback::noop(),
            on_cancel: Callback::noop(),
            on_reload: Callback::noop(),
        }
    }
}

fn batch_label_submit_copy(mode: BatchLabelMode, selected_count: usize) -> String {
    let selected_noun = if selected_count == 1 { "note" } else { "notes" };
    match mode {
        BatchLabelMode::Add => format!("Add label to {selected_count} selected {selected_noun}"),
        BatchLabelMode::Update => {
            format!("Update label on {selected_count} selected {selected_noun}")
        }
        BatchLabelMode::Remove => {
            format!("Remove label from {selected_count} selected {selected_noun}")
        }
    }
}

fn batch_label_title(mode: BatchLabelMode) -> &'static str {
    match mode {
        BatchLabelMode::Add => "Add label",
        BatchLabelMode::Update => "Update label",
        BatchLabelMode::Remove => "Remove label",
    }
}

fn batch_delete_form(
    selected_count: usize,
    batch_mutating: bool,
    stale_revision_blocked: bool,
    generic_error: Option<&str>,
    cancel_ref: NodeRef,
    callbacks: BatchDeleteFormCallbacks,
) -> Html {
    let BatchDeleteFormCallbacks {
        on_submit,
        on_cancel,
        on_reload,
    } = callbacks;
    let copy = batch_delete_modal_copy(selected_count);
    let submit_disabled = batch_mutating || stale_revision_blocked || selected_count == 0;
    let error = if stale_revision_blocked {
        Some("Selected notes changed after the list was loaded. Reload notes before retrying.")
    } else {
        generic_error
    };

    html! {
        <form class="notes-batch-delete-form" aria-busy={batch_mutating.to_string()} onsubmit={on_submit}>
            <p>{ copy.body }</p>
            if let Some(error) = error {
                <p class="notes-batch-delete-error" role="alert">{ error }</p>
            }
            <div class="app-modal-actions">
                if stale_revision_blocked {
                    <button type="button" class="btn btn-outline" disabled={batch_mutating} onclick={on_reload}>{ "Reload notes" }</button>
                }
                <button ref={cancel_ref} type="button" class="btn btn-ghost" disabled={batch_mutating} onclick={on_cancel}>{ "Cancel" }</button>
                <button type="submit" class="btn btn-error" disabled={submit_disabled}>{ if batch_mutating { "Deleting…" } else { copy.confirm.as_str() } }</button>
            </div>
        </form>
    }
}

#[allow(clippy::too_many_arguments)]
fn batch_label_form(
    mode: BatchLabelMode,
    label_keys: &[LabelKey],
    source_key: &str,
    destination_key: &str,
    value: &str,
    selected_count: usize,
    batch_mutating: bool,
    stale_revision_blocked: bool,
    generic_error: Option<&str>,
    input_refs: BatchLabelInputRefs,
    callbacks: BatchLabelFormCallbacks,
) -> Html {
    let BatchLabelFormCallbacks {
        on_source_input,
        on_destination_input,
        on_value_input,
        on_submit,
        on_cancel,
        on_reload,
    } = callbacks;
    let BatchLabelInputRefs {
        source: source_input_ref,
        destination: destination_input_ref,
    } = input_refs;
    let destination_value_type = label_keys
        .iter()
        .find(|label| label.key == destination_key.trim())
        .map(|label| label.value_type.as_str())
        .unwrap_or("text");
    let disabled = batch_mutating
        || stale_revision_blocked
        || selected_count == 0
        || label_action_from_draft(mode, source_key, destination_key, value).is_none();
    let submit_copy = if batch_mutating {
        "Updating…".to_string()
    } else {
        batch_label_submit_copy(mode, selected_count)
    };

    html! {
        <form class="notes-batch-label-form" aria-busy={batch_mutating.to_string()} onsubmit={on_submit}>
            if matches!(mode, BatchLabelMode::Update | BatchLabelMode::Remove) {
                <label>
                    <span>{ "Source label" }</span>
                    <input
                        class="input"
                        type="text"
                        list="notes-batch-source-label-options"
                        value={source_key.to_string()}
                        ref={source_input_ref}
                        disabled={batch_mutating}
                        oninput={on_source_input}
                        aria-label="Source label"
                    />
                </label>
                <datalist id="notes-batch-source-label-options">
                    { for label_keys.iter().map(|label| html! { <option value={label.key.clone()} /> }) }
                </datalist>
            }
            if matches!(mode, BatchLabelMode::Add | BatchLabelMode::Update) {
                <label>
                    <span>{ "Destination label" }</span>
                    <input
                        class="input"
                        type="text"
                        list="notes-batch-destination-label-options"
                        value={destination_key.to_string()}
                        ref={destination_input_ref}
                        disabled={batch_mutating}
                        oninput={on_destination_input}
                        aria-label="Destination label"
                    />
                </label>
                <datalist id="notes-batch-destination-label-options">
                    { for label_keys.iter().map(|label| html! { <option value={label.key.clone()} /> }) }
                </datalist>
                <label>
                    <span>{ "Label value" }</span>
                    <input
                        class="input"
                        type={label_value_input_type(destination_value_type, "=")}
                        value={value.to_string()}
                        disabled={batch_mutating}
                        oninput={on_value_input}
                        aria-label="Label value"
                    />
                </label>
            }
            { batch_label_error_notice(generic_error, stale_revision_blocked) }
            <div class="app-modal-actions">
                if stale_revision_blocked {
                    <button type="button" class="btn btn-outline" disabled={batch_mutating} onclick={on_reload}>{ "Reload notes" }</button>
                }
                <button type="button" class="btn btn-ghost" disabled={batch_mutating} onclick={on_cancel}>{ "Cancel" }</button>
                <button type="submit" class="btn btn-primary" disabled={disabled}>{ submit_copy }</button>
            </div>
        </form>
    }
}

fn select_all_is_indeterminate(selected_visible_count: usize, visible_count: usize) -> bool {
    selected_visible_count > 0 && selected_visible_count < visible_count
}

fn selection_reconciliation_visible_ids(
    loading: bool,
    visible_ids: &HashSet<String>,
) -> Option<HashSet<String>> {
    (!loading).then(|| visible_ids.clone())
}

fn notes_load_is_current(started_generation: u64, current_generation: u64) -> bool {
    started_generation == current_generation
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NotesUrlState {
    current: usize,
    page_size: usize,
    search: String,
    pub(crate) labels: Vec<LabelFilter>,
    pub(crate) invalid_labels: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum BatchDeleteCompletionDecision {
    Ignore,
    ReloadCurrent,
    Replace(NotesUrlState),
}

fn batch_delete_completion_decision(
    component_alive: bool,
    submitted_generation: u64,
    current_generation: u64,
    url_state: &NotesUrlState,
    rendered_current_page: usize,
    total_before: usize,
    deleted: usize,
) -> BatchDeleteCompletionDecision {
    if !component_alive {
        return BatchDeleteCompletionDecision::Ignore;
    }
    if submitted_generation != current_generation {
        return BatchDeleteCompletionDecision::ReloadCurrent;
    }
    let target_page = page_after_batch_delete(
        rendered_current_page,
        url_state.page_size,
        deleted,
        total_before,
    );
    if target_page == rendered_current_page {
        BatchDeleteCompletionDecision::ReloadCurrent
    } else {
        let mut next = url_state.clone();
        next.current = target_page.saturating_add(1);
        BatchDeleteCompletionDecision::Replace(next)
    }
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
    let batch_label_ui = use_reducer(BatchLabelUiState::default);
    let batch_delete_ui = use_reducer(BatchDeleteUiState::default);
    let batch_in_flight = use_mut_ref(|| false);
    let batch_toolbar_refs = BatchLabelToolbarRefs {
        add: use_node_ref(),
        update: use_node_ref(),
        remove: use_node_ref(),
        delete: use_node_ref(),
    };
    let batch_input_refs = BatchLabelInputRefs {
        source: use_node_ref(),
        destination: use_node_ref(),
    };
    let batch_success_ref = use_node_ref();
    let batch_focus_return_mode = use_mut_ref(|| None::<BatchLabelMode>);
    let batch_delete_cancel_ref = use_node_ref();
    let batch_delete_success_ref = use_node_ref();
    let batch_delete_focus_return = use_mut_ref(|| false);
    // Current list-view page (0-based).
    let page = use_state(|| 0usize);
    let page_size = use_state(|| DEFAULT_NOTES_PAGE_SIZE);
    let label_filters = use_state(Vec::<LabelFilter>::new);
    let filter_key = use_state(String::new);
    let filter_operator = use_state(|| "=".to_string());
    let filter_value = use_state(String::new);
    let refresh_tick = use_state(|| 0usize);
    let load_generation = use_mut_ref(|| 0_u64);
    let component_alive = use_mut_ref(|| true);
    {
        let component_alive = component_alive.clone();
        use_effect_with((), move |_| {
            *component_alive.borrow_mut() = true;
            move || *component_alive.borrow_mut() = false
        });
    }
    let navigator = use_navigator();
    let location = use_location();
    let query_string = location
        .as_ref()
        .map(|location| location.query_str().to_string())
        .unwrap_or_default();
    let url_state = parse_notes_query(&query_string);
    let notes_query = notes_query_params(&url_state);
    let batch_mutating = batch_label_ui.mutating || batch_delete_ui.mutating;
    let batch_workflow_open = batch_label_ui.mode.is_some() || batch_delete_ui.open;
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
    let selected_visible_count = selected_visible_count(&visible_targets, &selected);
    let select_all_ref = use_node_ref();

    {
        let selected = selected.clone();
        use_effect_with(
            (*loading, visible_ids.clone()),
            move |(loading, visible_ids)| {
                if let Some(visible_ids) =
                    selection_reconciliation_visible_ids(*loading, visible_ids)
                {
                    selected.dispatch(SelectionAction::RetainVisible(visible_ids));
                }
                || ()
            },
        );
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

    {
        let source_ref = batch_input_refs.source.clone();
        let destination_ref = batch_input_refs.destination.clone();
        use_effect_with(batch_label_ui.mode, move |mode| {
            let input = match batch_label_initial_focus(*mode) {
                Some(BatchLabelInputFocus::Source) => source_ref.cast::<HtmlInputElement>(),
                Some(BatchLabelInputFocus::Destination) => {
                    destination_ref.cast::<HtmlInputElement>()
                }
                None => None,
            };
            if let Some(input) = input {
                let _ = input.focus();
            }
            || ()
        });
    }

    {
        let return_mode = batch_focus_return_mode.clone();
        let add_ref = batch_toolbar_refs.add.clone();
        let update_ref = batch_toolbar_refs.update.clone();
        let remove_ref = batch_toolbar_refs.remove.clone();
        use_effect_with(batch_label_ui.mode, move |mode| {
            if mode.is_none() {
                let trigger = match return_mode.borrow_mut().take() {
                    Some(BatchLabelMode::Add) => add_ref.cast::<web_sys::HtmlElement>(),
                    Some(BatchLabelMode::Update) => update_ref.cast::<web_sys::HtmlElement>(),
                    Some(BatchLabelMode::Remove) => remove_ref.cast::<web_sys::HtmlElement>(),
                    None => None,
                };
                if let Some(trigger) = trigger {
                    let _ = trigger.focus();
                }
            }
            || ()
        });
    }

    {
        let batch_success_ref = batch_success_ref.clone();
        use_effect_with(batch_label_ui.success.clone(), move |success| {
            if success.is_some() {
                if let Some(status) = batch_success_ref.cast::<web_sys::HtmlElement>() {
                    let _ = status.focus();
                }
            }
            || ()
        });
    }

    {
        let cancel_ref = batch_delete_cancel_ref.clone();
        use_effect_with(batch_delete_ui.open, move |open| {
            if *open {
                if let Some(cancel) = cancel_ref.cast::<web_sys::HtmlElement>() {
                    let _ = cancel.focus();
                }
            }
            || ()
        });
    }

    {
        let return_focus = batch_delete_focus_return.clone();
        let delete_ref = batch_toolbar_refs.delete.clone();
        use_effect_with(batch_delete_ui.open, move |open| {
            if !*open && *return_focus.borrow() {
                *return_focus.borrow_mut() = false;
                if let Some(trigger) = delete_ref.cast::<web_sys::HtmlElement>() {
                    let _ = trigger.focus();
                }
            }
            || ()
        });
    }

    {
        let success_ref = batch_delete_success_ref.clone();
        use_effect_with(batch_delete_ui.success.clone(), move |success| {
            if success.is_some() {
                if let Some(status) = success_ref.cast::<web_sys::HtmlElement>() {
                    let _ = status.focus();
                }
            }
            || ()
        });
    }

    let on_select_all = {
        let selected = selected.clone();
        let visible_ids = visible_ids.clone();
        let batch_mutating = batch_mutating;
        Callback::from(move |event: Event| {
            if batch_mutating {
                return;
            }
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
        let batch_mutating = batch_mutating;
        Callback::from(move |(id, selected_now): (String, bool)| {
            if batch_mutating {
                return;
            }
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
        let load_generation = load_generation.clone();
        use_effect_with((url_state.clone(), *refresh_tick), move |(state, _)| {
            let started_generation = {
                let mut generation = load_generation.borrow_mut();
                *generation = generation.saturating_add(1);
                *generation
            };
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
                if notes_load_is_current(started_generation, *load_generation.borrow()) {
                    loading.set(false);
                    error.set(Some("Invalid label filter in URL.".to_string()));
                    stale_revision_gate.dispatch(StaleRevisionGateAction::RefreshFinished {
                        started_epoch: refresh_epoch,
                        succeeded: false,
                    });
                }
            } else {
                let load_generation = load_generation.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let search = state.search.trim().to_string();
                    if search.is_empty() {
                        let offset = state
                            .current
                            .saturating_sub(1)
                            .saturating_mul(state.page_size);
                        match api::list_notes_page(&state.labels, state.page_size, offset).await {
                            Ok(page) => {
                                if notes_load_is_current(
                                    started_generation,
                                    *load_generation.borrow(),
                                ) {
                                    notes.set(page.notes);
                                    total_notes.set(page.total);
                                    stale_revision_gate.dispatch(
                                        StaleRevisionGateAction::RefreshFinished {
                                            started_epoch: refresh_epoch,
                                            succeeded: true,
                                        },
                                    );
                                }
                            }
                            Err(e) => {
                                if notes_load_is_current(
                                    started_generation,
                                    *load_generation.borrow(),
                                ) {
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
                    } else {
                        let limit = state
                            .current
                            .saturating_add(1)
                            .saturating_mul(state.page_size)
                            .max(state.page_size);
                        match api::search_filtered(&search, limit, &state.labels).await {
                            Ok(r) => {
                                if notes_load_is_current(
                                    started_generation,
                                    *load_generation.borrow(),
                                ) {
                                    results.set(Some(r));
                                    stale_revision_gate.dispatch(
                                        StaleRevisionGateAction::RefreshFinished {
                                            started_epoch: refresh_epoch,
                                            succeeded: true,
                                        },
                                    );
                                }
                            }
                            Err(e) => {
                                if notes_load_is_current(
                                    started_generation,
                                    *load_generation.borrow(),
                                ) {
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
                    }
                    if notes_load_is_current(started_generation, *load_generation.borrow()) {
                        loading.set(false);
                    }
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

    let on_open_batch_label = {
        let batch_label_ui = batch_label_ui.clone();
        Callback::from(move |mode: BatchLabelMode| {
            batch_label_ui.dispatch(BatchLabelUiAction::Open(mode));
        })
    };

    let on_open_batch_delete = {
        let batch_delete_ui = batch_delete_ui.clone();
        Callback::from(move |_: ()| {
            batch_delete_ui.dispatch(BatchDeleteUiAction::ClearSuccess);
            batch_delete_ui.dispatch(BatchDeleteUiAction::Open);
        })
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

    let batch_label_modal = match batch_label_ui.mode {
        None => html! {},
        Some(mode) => {
            let on_close = {
                let batch_label_ui = batch_label_ui.clone();
                let batch_focus_return_mode = batch_focus_return_mode.clone();
                Callback::from(move |_: ()| {
                    if batch_label_ui.mutating {
                        return;
                    }
                    *batch_focus_return_mode.borrow_mut() = Some(mode);
                    batch_label_ui.dispatch(BatchLabelUiAction::Close);
                })
            };
            let on_source_input = {
                let batch_label_ui = batch_label_ui.clone();
                Callback::from(move |event: InputEvent| {
                    if !batch_label_draft_input_allowed(batch_mutating) {
                        return;
                    }
                    let input: HtmlInputElement = event.target_unchecked_into();
                    batch_label_ui.dispatch(BatchLabelUiAction::SourceKeyChanged(input.value()));
                })
            };
            let on_destination_input = {
                let batch_label_ui = batch_label_ui.clone();
                Callback::from(move |event: InputEvent| {
                    if !batch_label_draft_input_allowed(batch_mutating) {
                        return;
                    }
                    let input: HtmlInputElement = event.target_unchecked_into();
                    batch_label_ui
                        .dispatch(BatchLabelUiAction::DestinationKeyChanged(input.value()));
                })
            };
            let on_value_input = {
                let batch_label_ui = batch_label_ui.clone();
                Callback::from(move |event: InputEvent| {
                    if !batch_label_draft_input_allowed(batch_mutating) {
                        return;
                    }
                    let input: HtmlInputElement = event.target_unchecked_into();
                    batch_label_ui.dispatch(BatchLabelUiAction::ValueChanged(input.value()));
                })
            };
            let on_cancel = {
                let on_close = on_close.clone();
                Callback::from(move |_: MouseEvent| on_close.emit(()))
            };
            let on_reload = {
                let reload = reload.clone();
                Callback::from(move |_: MouseEvent| reload.emit(()))
            };
            let on_submit = {
                let visible_targets = visible_targets.clone();
                let selected = selected.clone();
                let batch_label_ui = batch_label_ui.clone();
                let stale_revision_gate = stale_revision_gate.clone();
                let reload = reload.clone();
                let batch_in_flight = batch_in_flight.clone();
                let component_alive = component_alive.clone();
                Callback::from(move |event: SubmitEvent| {
                    event.prevent_default();
                    if !batch_label_submit_allowed(
                        batch_mutating,
                        *batch_in_flight.borrow(),
                        stale_revision_gate.blocked(),
                    ) {
                        return;
                    }
                    let targets = selected_note_targets(&visible_targets, &selected);
                    if targets.is_empty() {
                        return;
                    }
                    let Some(action) = label_action_from_draft(
                        mode,
                        &batch_label_ui.source_key,
                        &batch_label_ui.destination_key,
                        &batch_label_ui.value,
                    ) else {
                        return;
                    };

                    *batch_in_flight.borrow_mut() = true;
                    batch_label_ui.dispatch(BatchLabelUiAction::SubmitStarted);
                    let selected = selected.clone();
                    let batch_in_flight = batch_in_flight.clone();
                    let batch_label_ui = batch_label_ui.clone();
                    let stale_revision_gate = stale_revision_gate.clone();
                    let reload = reload.clone();
                    let component_alive = component_alive.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = api::batch_update_note_labels(&targets, &action).await;
                        *batch_in_flight.borrow_mut() = false;
                        if !*component_alive.borrow() {
                            return;
                        }
                        match result {
                            Ok(result) => {
                                selected.dispatch(SelectionAction::Clear);
                                batch_label_ui.dispatch(BatchLabelUiAction::Success(
                                    batch_label_result_message(
                                        result.updated,
                                        result.requested,
                                        result.unchanged,
                                    ),
                                ));
                                reload.emit(());
                            }
                            Err(error) if error.is_stale_revision() => {
                                stale_revision_gate.dispatch(StaleRevisionGateAction::Conflict);
                                batch_label_ui.dispatch(BatchLabelUiAction::StaleFailure);
                            }
                            Err(_) => {
                                batch_label_ui.dispatch(BatchLabelUiAction::GenericFailure(
                                    "Could not update selected note labels. Please try again."
                                        .to_string(),
                                ));
                            }
                        }
                    });
                })
            };
            let callbacks = BatchLabelFormCallbacks {
                on_source_input,
                on_destination_input,
                on_value_input,
                on_submit,
                on_cancel,
                on_reload,
            };
            html! {
                <Modal title={batch_label_title(mode)} on_close={on_close}>
                    { batch_label_form(
                        mode,
                        &label_keys,
                        &batch_label_ui.source_key,
                        &batch_label_ui.destination_key,
                        &batch_label_ui.value,
                        selected_visible_count,
                        batch_mutating,
                        stale_revision_gate.blocked(),
                        batch_label_ui.error.as_deref(),
                        batch_input_refs.clone(),
                        callbacks,
                    ) }
                </Modal>
            }
        }
    };

    let batch_delete_modal = if !batch_delete_ui.open {
        html! {}
    } else {
        let (rendered_current_page, total_before) = if let Some(hits) = &*results {
            let search_page = search_result_page(hits.len(), *page, *page_size, &notes_query);
            (search_page.current, hits.len())
        } else {
            let total_pages = (*total_notes).div_ceil((*page_size).max(1));
            ((*page).min(total_pages.saturating_sub(1)), *total_notes)
        };
        let on_close = {
            let batch_delete_ui = batch_delete_ui.clone();
            let stale_revision_gate = stale_revision_gate.clone();
            let batch_delete_focus_return = batch_delete_focus_return.clone();
            Callback::from(move |_: ()| {
                if batch_mutating {
                    return;
                }
                *batch_delete_focus_return.borrow_mut() = true;
                stale_revision_gate.dispatch(StaleRevisionGateAction::Dismiss);
                batch_delete_ui.dispatch(BatchDeleteUiAction::Close);
            })
        };
        let on_cancel = {
            let on_close = on_close.clone();
            Callback::from(move |_: MouseEvent| on_close.emit(()))
        };
        let on_reload = {
            let reload = reload.clone();
            Callback::from(move |_: MouseEvent| {
                if !batch_mutating {
                    reload.emit(());
                }
            })
        };
        let on_submit = {
            let visible_targets = visible_targets.clone();
            let selected = selected.clone();
            let batch_delete_ui = batch_delete_ui.clone();
            let batch_label_ui = batch_label_ui.clone();
            let stale_revision_gate = stale_revision_gate.clone();
            let reload = reload.clone();
            let replace_notes_url = replace_notes_url.clone();
            let url_state = url_state.clone();
            let batch_in_flight = batch_in_flight.clone();
            let load_generation = load_generation.clone();
            let component_alive = component_alive.clone();
            Callback::from(move |event: SubmitEvent| {
                event.prevent_default();
                if !batch_delete_submit_allowed(
                    batch_mutating,
                    *batch_in_flight.borrow(),
                    stale_revision_gate.blocked(),
                ) {
                    return;
                }
                let targets = selected_note_targets(&visible_targets, &selected);
                if targets.is_empty() {
                    return;
                }

                let submitted_generation = *load_generation.borrow();
                *batch_in_flight.borrow_mut() = true;
                batch_delete_ui.dispatch(BatchDeleteUiAction::SubmitStarted);
                let selected = selected.clone();
                let batch_delete_ui = batch_delete_ui.clone();
                let batch_label_ui = batch_label_ui.clone();
                let stale_revision_gate = stale_revision_gate.clone();
                let reload = reload.clone();
                let replace_notes_url = replace_notes_url.clone();
                let batch_in_flight = batch_in_flight.clone();
                let url_state = url_state.clone();
                let load_generation = load_generation.clone();
                let component_alive = component_alive.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = api::batch_delete_notes(&targets).await;
                    *batch_in_flight.borrow_mut() = false;
                    if !*component_alive.borrow() {
                        return;
                    }
                    match result {
                        Ok(result) => {
                            let decision = batch_delete_completion_decision(
                                true,
                                submitted_generation,
                                *load_generation.borrow(),
                                &url_state,
                                rendered_current_page,
                                total_before,
                                result.deleted,
                            );
                            selected.dispatch(SelectionAction::Clear);
                            batch_label_ui.dispatch(BatchLabelUiAction::ClearSuccess);
                            batch_delete_ui.dispatch(BatchDeleteUiAction::Success(
                                batch_delete_result_message(result.deleted),
                            ));
                            match decision {
                                BatchDeleteCompletionDecision::Ignore => {}
                                BatchDeleteCompletionDecision::ReloadCurrent => reload.emit(()),
                                BatchDeleteCompletionDecision::Replace(state) => {
                                    replace_notes_url.emit(state)
                                }
                            }
                        }
                        Err(error) if error.is_stale_revision() => {
                            stale_revision_gate.dispatch(StaleRevisionGateAction::Conflict);
                            batch_delete_ui.dispatch(BatchDeleteUiAction::StaleFailure);
                        }
                        Err(_) => batch_delete_ui.dispatch(BatchDeleteUiAction::GenericFailure(
                            "Could not delete selected notes. Please try again.".to_string(),
                        )),
                    }
                });
            })
        };
        let callbacks = BatchDeleteFormCallbacks {
            on_submit,
            on_cancel,
            on_reload,
        };
        let copy = batch_delete_modal_copy(selected_visible_count);
        html! {
            <Modal title={copy.title} on_close={on_close}>
                { batch_delete_form(
                    selected_visible_count,
                    batch_mutating,
                    stale_revision_gate.blocked(),
                    batch_delete_ui.error.as_deref(),
                    batch_delete_cancel_ref.clone(),
                    callbacks,
                ) }
            </Modal>
        }
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
            <div class="notes-page-content" inert={batch_workflow_open} aria-busy={batch_mutating.to_string()}>
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

            if let Some(message) = &batch_label_ui.success {
                <Alert variant={Some("success".to_string())}>
                    <span ref={batch_success_ref.clone()} role="status" tabindex="-1">{ message.clone() }</span>
                </Alert>
            }

            if let Some(message) = &batch_delete_ui.success {
                <Alert variant={Some("success".to_string())}>
                    <span ref={batch_delete_success_ref.clone()} role="status" tabindex="-1">{ message.clone() }</span>
                </Alert>
            }

            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else {
                if results.as_ref().is_some_and(|hits| !hits.is_empty()) || (results.is_none() && !notes.is_empty()) {
                    { batch_toolbar(selected_visible_count, batch_mutating, batch_workflow_open, batch_toolbar_refs.clone(), on_open_batch_label, on_open_batch_delete) }
                }
                if let Some(hits) = &*results {
                    { search_results_view(hits, &page, &page_size, &selected, &select_all_ref, on_select_all.clone(), on_selection_change.clone(), batch_mutating, &delete_target, &notes_query, on_quick_add_filter.clone(), on_page_change.clone(), on_page_size_change.clone(), on_refresh.clone()) }
                } else {
                    { list_view(&notes, *total_notes, &page, &page_size, &selected, &select_all_ref, on_select_all, on_selection_change, batch_mutating, &delete_target, &notes_query, on_quick_add_filter, on_page_change, on_page_size_change, on_refresh) }
                }
            }

            </div>
            { batch_label_modal }
            { batch_delete_modal }
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
        Callback::from(move |_| {
            if !selection_disabled {
                emit_delete_target(&on_delete_target, &id, &title, revision);
            }
        })
    };
    let on_select = {
        let id = id.clone();
        Callback::from(move |event: Event| {
            if selection_disabled {
                return;
            }
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
                        disabled={selection_disabled} onclick={on_remove}>
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
            VNode::VTag(tag) => tag.children().map(visible_text).unwrap_or_default(),
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

    fn descendants_with_tag<'a>(node: &'a VNode, tag_name: &str) -> Vec<&'a VNode> {
        fn visit<'a>(node: &'a VNode, tag_name: &str, matches: &mut Vec<&'a VNode>) {
            if matches!(node, VNode::VTag(tag) if tag.tag() == tag_name) {
                matches.push(node);
            }
            match node {
                VNode::VTag(tag) => {
                    if let Some(children) = tag.children() {
                        visit(children, tag_name, matches);
                    }
                }
                VNode::VList(children) => {
                    for child in children.iter() {
                        visit(child, tag_name, matches);
                    }
                }
                _ => {}
            }
        }

        let mut matches = Vec::new();
        visit(node, tag_name, &mut matches);
        matches
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
    fn selection_reconciliation_waits_for_completed_refresh_rows() {
        let visible = HashSet::from(["first".to_string(), "second".to_string()]);
        assert_eq!(
            selection_reconciliation_visible_ids(true, &HashSet::new()),
            None
        );
        assert_eq!(
            selection_reconciliation_visible_ids(false, &HashSet::new()),
            Some(HashSet::new())
        );

        let selected = Rc::new(SelectionState(HashSet::from([
            "first".to_string(),
            "hidden".to_string(),
        ])));
        let reconciled = selection_reconciliation_visible_ids(false, &visible)
            .expect("completed refresh should reconcile visible rows");
        let selected = selected.reduce(SelectionAction::RetainVisible(reconciled));

        assert_eq!(**selected, HashSet::from(["first".to_string()]));
    }

    #[test]
    fn notes_load_generation_rejects_an_older_completion_after_a_newer_start() {
        let older_generation = 4;
        let latest_generation = 5;

        assert!(!notes_load_is_current(older_generation, latest_generation));
        assert!(notes_load_is_current(latest_generation, latest_generation));
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
    fn label_action_from_draft_trims_keys_preserves_values_and_rejects_blank_requirements() {
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Add, "", " project ", " padded "),
            Some(api::BatchLabelAction::Add {
                key: "project".into(),
                value: " padded ".into(),
            })
        );
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Update, " old ", " new ", "exact value ",),
            Some(api::BatchLabelAction::Update {
                from_key: "old".into(),
                key: "new".into(),
                value: "exact value ".into(),
            })
        );
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Remove, " obsolete ", "", "ignored"),
            Some(api::BatchLabelAction::Remove {
                key: "obsolete".into(),
            })
        );

        assert_eq!(
            label_action_from_draft(BatchLabelMode::Add, "", " \t", "value"),
            None
        );
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Update, "", "new", "value"),
            None
        );
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Update, "old", " ", "value"),
            None
        );
        assert_eq!(
            label_action_from_draft(BatchLabelMode::Remove, "\n", "", ""),
            None
        );
    }

    #[test]
    fn batch_label_result_message_uses_exact_singular_and_plural_copy() {
        assert_eq!(
            batch_label_result_message(0, 0, 0),
            "Updated 0 of 0 selected notes; 0 unchanged."
        );
        assert_eq!(
            batch_label_result_message(1, 1, 0),
            "Updated 1 of 1 selected note; 0 unchanged."
        );
        assert_eq!(
            batch_label_result_message(2, 3, 1),
            "Updated 2 of 3 selected notes; 1 unchanged."
        );
        assert_eq!(
            batch_label_result_message(1, 3, 2),
            "Updated 1 of 3 selected notes; 2 unchanged."
        );
    }

    #[test]
    fn batch_delete_copy_uses_exact_singular_and_plural_words() {
        assert_eq!(
            batch_delete_modal_copy(1),
            BatchDeleteModalCopy {
                title: "Delete selected note".to_string(),
                body: "Move 1 selected note to Trash?".to_string(),
                confirm: "Delete 1 note".to_string(),
            }
        );
        assert_eq!(
            batch_delete_modal_copy(3),
            BatchDeleteModalCopy {
                title: "Delete selected notes".to_string(),
                body: "Move 3 selected notes to Trash?".to_string(),
                confirm: "Delete 3 notes".to_string(),
            }
        );
        assert_eq!(
            batch_delete_result_message(1),
            "Moved 1 selected note to Trash."
        );
        assert_eq!(
            batch_delete_result_message(3),
            "Moved 3 selected notes to Trash."
        );
    }

    #[test]
    fn page_after_batch_delete_backshifts_only_when_the_final_page_disappears() {
        assert_eq!(page_after_batch_delete(2, 10, 1, 21), 1);
        assert_eq!(page_after_batch_delete(1, 10, 1, 30), 1);
        assert_eq!(page_after_batch_delete(0, 10, 1, 1), 0);
        assert_eq!(page_after_batch_delete(3, 0, 1, 1), 0);
    }

    #[test]
    fn batch_delete_ui_reducer_lifecycle_keeps_failures_open_and_success_closes() {
        let open = Rc::new(BatchDeleteUiState::default()).reduce(BatchDeleteUiAction::Open);
        assert!(open.open);
        assert_eq!(open.error, None);
        assert_eq!(open.success, None);

        let submitting = open.reduce(BatchDeleteUiAction::SubmitStarted);
        assert!(submitting.open);
        assert!(submitting.mutating);
        assert_eq!(submitting.error, None);

        let close_ignored = submitting.clone().reduce(BatchDeleteUiAction::Close);
        assert_eq!(*close_ignored, *submitting);

        let generic = close_ignored.reduce(BatchDeleteUiAction::GenericFailure("Nope".into()));
        assert!(generic.open);
        assert!(!generic.mutating);
        assert_eq!(generic.error.as_deref(), Some("Nope"));

        let stale = generic.reduce(BatchDeleteUiAction::StaleFailure);
        assert!(stale.open);
        assert!(!stale.mutating);
        assert_eq!(stale.error, None);

        let success = stale.reduce(BatchDeleteUiAction::Success("Done".into()));
        assert!(!success.open);
        assert!(!success.mutating);
        assert_eq!(success.error, None);
        assert_eq!(success.success.as_deref(), Some("Done"));

        let cleared = success.reduce(BatchDeleteUiAction::ClearSuccess);
        assert_eq!(cleared.success, None);
        assert!(batch_delete_submit_allowed(false, false, false));
        assert!(!batch_delete_submit_allowed(false, true, false));
        assert!(!batch_delete_submit_allowed(true, false, false));
        assert!(!batch_delete_submit_allowed(false, false, true));
    }

    #[test]
    fn batch_delete_modal_renders_copy_errors_stale_reload_and_mutation_lock() {
        let generic = batch_delete_form(
            2,
            false,
            false,
            Some("Could not delete selected notes. Please try again."),
            NodeRef::default(),
            BatchDeleteFormCallbacks::noop(),
        );
        assert!(visible_text(&generic).contains(&"Move 2 selected notes to Trash?".to_string()));
        let alert = descendants_with_tag(&generic, "p")
            .into_iter()
            .find(|node| attribute(node, "role") == Some("alert"))
            .expect("generic failure is announced in the delete dialog");
        assert_eq!(
            visible_text(alert),
            ["Could not delete selected notes. Please try again."]
        );

        let stale = batch_delete_form(
            1,
            false,
            true,
            None,
            NodeRef::default(),
            BatchDeleteFormCallbacks::noop(),
        );
        assert!(visible_text(&stale).contains(&"Reload notes".to_string()));

        let mutating = batch_delete_form(
            1,
            true,
            false,
            None,
            NodeRef::default(),
            BatchDeleteFormCallbacks::noop(),
        );
        assert_eq!(attribute(&mutating, "aria-busy"), Some("true"));
        assert!(visible_text(&mutating).contains(&"Deleting…".to_string()));
        assert!(descendants_with_tag(&mutating, "button")
            .iter()
            .all(|button| attribute(button, "disabled").is_some()));
    }

    #[test]
    fn batch_delete_completion_ignores_unmounted_and_stale_url_generations() {
        let state = NotesUrlState {
            current: 3,
            page_size: 10,
            search: "needle".into(),
            labels: vec![LabelFilter {
                key: "project".into(),
                operator: "==".into(),
                value: "agent-note".into(),
            }],
            invalid_labels: false,
        };
        assert_eq!(
            batch_delete_completion_decision(true, 8, 8, &state, 2, 21, 1),
            BatchDeleteCompletionDecision::Replace(NotesUrlState {
                current: 2,
                ..state.clone()
            })
        );
        assert_eq!(
            batch_delete_completion_decision(true, 8, 8, &state, 1, 30, 1),
            BatchDeleteCompletionDecision::ReloadCurrent
        );
        assert_eq!(
            batch_delete_completion_decision(true, 8, 9, &state, 2, 21, 1),
            BatchDeleteCompletionDecision::ReloadCurrent
        );
        assert_eq!(
            batch_delete_completion_decision(false, 8, 8, &state, 2, 21, 1),
            BatchDeleteCompletionDecision::Ignore
        );
    }

    #[test]
    fn batch_toolbar_reports_selection_and_only_enables_its_four_actions_when_idle() {
        let empty = batch_toolbar(
            0,
            false,
            false,
            BatchLabelToolbarRefs::default(),
            Callback::noop(),
            Callback::noop(),
        );
        let empty_children = rendered_children(&empty);
        assert_eq!(attribute(empty_children[0], "aria-live"), Some("polite"));
        assert_eq!(visible_text(empty_children[0]), ["0 selected"]);
        let empty_actions = rendered_children(empty_children[1]);
        assert_eq!(
            empty_actions
                .iter()
                .flat_map(|button| visible_text(button))
                .collect::<Vec<_>>(),
            [
                "Add label",
                "Update label",
                "Remove label",
                "Delete selected"
            ]
        );
        assert!(empty_actions
            .iter()
            .all(|button| attribute(button, "disabled").is_some()));

        let selected = batch_toolbar(
            2,
            false,
            false,
            BatchLabelToolbarRefs::default(),
            Callback::noop(),
            Callback::noop(),
        );
        let selected_actions = rendered_children(rendered_children(&selected)[1]);
        assert!(selected_actions
            .iter()
            .all(|button| attribute(button, "disabled").is_none()));

        let mutating = batch_toolbar(
            2,
            true,
            false,
            BatchLabelToolbarRefs::default(),
            Callback::noop(),
            Callback::noop(),
        );
        let mutating_actions = rendered_children(rendered_children(&mutating)[1]);
        assert!(mutating_actions
            .iter()
            .all(|button| attribute(button, "disabled").is_some()));
        assert!(attribute(selected_actions[3], "class")
            .expect("delete class")
            .contains("btn-error"));
    }

    #[test]
    fn batch_label_toolbar_callback_guards_zero_selected_and_mutating_states() {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let on_open = {
            let opened = opened.clone();
            Callback::from(move |mode| opened.borrow_mut().push(mode))
        };

        batch_label_open_callback(0, false, false, BatchLabelMode::Add, on_open.clone()).emit(());
        batch_label_open_callback(2, true, false, BatchLabelMode::Update, on_open.clone()).emit(());
        batch_label_open_callback(2, false, false, BatchLabelMode::Remove, on_open).emit(());

        assert_eq!(*opened.borrow(), vec![BatchLabelMode::Remove]);
    }

    #[test]
    fn batch_delete_toolbar_callback_guards_zero_selected_busy_and_conflicting_workflows() {
        let opened = Rc::new(RefCell::new(0));
        let on_open = {
            let opened = opened.clone();
            Callback::from(move |_: ()| *opened.borrow_mut() += 1)
        };

        batch_delete_open_callback(0, false, false, on_open.clone()).emit(());
        batch_delete_open_callback(2, true, false, on_open.clone()).emit(());
        batch_delete_open_callback(2, false, true, on_open.clone()).emit(());
        batch_delete_open_callback(2, false, false, on_open).emit(());

        assert_eq!(*opened.borrow(), 1);
    }

    #[test]
    fn batch_label_initial_focus_targets_the_mode_specific_first_input() {
        assert_eq!(
            batch_label_initial_focus(Some(BatchLabelMode::Add)),
            Some(BatchLabelInputFocus::Destination)
        );
        assert_eq!(
            batch_label_initial_focus(Some(BatchLabelMode::Update)),
            Some(BatchLabelInputFocus::Source)
        );
        assert_eq!(
            batch_label_initial_focus(Some(BatchLabelMode::Remove)),
            Some(BatchLabelInputFocus::Source)
        );
        assert_eq!(batch_label_initial_focus(None), None);
    }

    #[test]
    fn batch_label_ui_reducer_opens_edits_and_locks_submission() {
        let state = Rc::new(BatchLabelUiState::default())
            .reduce(BatchLabelUiAction::Open(BatchLabelMode::Update));
        let state = state.reduce(BatchLabelUiAction::SourceKeyChanged("project".into()));
        let state = state.reduce(BatchLabelUiAction::DestinationKeyChanged("team".into()));
        let state = state.reduce(BatchLabelUiAction::ValueChanged("platform".into()));
        let state = state.reduce(BatchLabelUiAction::SubmitStarted);

        assert_eq!(state.mode, Some(BatchLabelMode::Update));
        assert_eq!(state.source_key, "project");
        assert_eq!(state.destination_key, "team");
        assert_eq!(state.value, "platform");
        assert!(state.mutating);
        assert!(state.error.is_none());
        assert!(batch_label_submit_allowed(false, false, false));
        assert!(!batch_label_submit_allowed(false, true, false));
        assert!(!batch_label_submit_allowed(false, false, true));
        assert!(!batch_label_submit_allowed(state.mutating, false, false));
    }

    #[test]
    fn batch_label_ui_reducer_success_closes_resets_and_sets_summary() {
        let state = Rc::new(BatchLabelUiState {
            mode: Some(BatchLabelMode::Add),
            source_key: "source".into(),
            destination_key: "project".into(),
            value: "agent-note".into(),
            mutating: true,
            error: Some("old error".into()),
            success: None,
        })
        .reduce(BatchLabelUiAction::Success(
            "Updated 1 of 1 selected note; 0 unchanged.".into(),
        ));

        assert_eq!(state.mode, None);
        assert_eq!(state.source_key, "");
        assert_eq!(state.destination_key, "");
        assert_eq!(state.value, "");
        assert!(!state.mutating);
        assert_eq!(state.error, None);
        assert_eq!(
            state.success.as_deref(),
            Some("Updated 1 of 1 selected note; 0 unchanged.")
        );
    }

    #[test]
    fn batch_label_ui_reducer_failures_keep_drafts_but_only_generic_failure_keeps_error() {
        let state = Rc::new(BatchLabelUiState {
            mode: Some(BatchLabelMode::Update),
            source_key: "project".into(),
            destination_key: "team".into(),
            value: "platform".into(),
            mutating: true,
            error: None,
            success: None,
        });
        let generic = state.clone().reduce(BatchLabelUiAction::GenericFailure(
            "Could not update selected note labels. Please try again.".into(),
        ));
        let stale = generic.clone().reduce(BatchLabelUiAction::StaleFailure);

        assert_eq!(generic.mode, Some(BatchLabelMode::Update));
        assert_eq!(generic.source_key, "project");
        assert!(!generic.mutating);
        assert_eq!(
            generic.error.as_deref(),
            Some("Could not update selected note labels. Please try again.")
        );
        assert_eq!(stale.mode, Some(BatchLabelMode::Update));
        assert_eq!(stale.destination_key, "team");
        assert!(!stale.mutating);
        assert_eq!(stale.error, None);
    }

    #[test]
    fn batch_label_ui_reducer_close_resets_an_idle_dialog_without_clearing_success() {
        let state = Rc::new(BatchLabelUiState {
            mode: Some(BatchLabelMode::Remove),
            source_key: "obsolete".into(),
            destination_key: String::new(),
            value: String::new(),
            mutating: false,
            error: Some("retry".into()),
            success: Some("Earlier success".into()),
        })
        .reduce(BatchLabelUiAction::Close);

        assert_eq!(state.mode, None);
        assert_eq!(state.source_key, "");
        assert_eq!(state.error, None);
        assert_eq!(state.success.as_deref(), Some("Earlier success"));
    }

    #[test]
    fn batch_label_ui_reducer_ignores_close_while_a_mutation_is_running() {
        let state = Rc::new(BatchLabelUiState {
            mode: Some(BatchLabelMode::Update),
            source_key: "project".into(),
            destination_key: "team".into(),
            value: "platform".into(),
            mutating: true,
            error: None,
            success: None,
        });

        assert_eq!(*state.clone().reduce(BatchLabelUiAction::Close), *state);
    }

    #[test]
    fn batch_label_form_renders_each_mode_with_typed_catalog_inputs_and_selected_count_copy() {
        let label_keys = vec![
            LabelKey {
                key: "priority".into(),
                description: String::new(),
                value_type: "number".into(),
            },
            LabelKey {
                key: "project".into(),
                description: String::new(),
                value_type: "text".into(),
            },
        ];
        let callbacks = BatchLabelFormCallbacks::noop();

        let add = batch_label_form(
            BatchLabelMode::Add,
            &label_keys,
            "",
            " priority ",
            "2",
            2,
            false,
            false,
            None,
            BatchLabelInputRefs::default(),
            callbacks.clone(),
        );
        assert!(visible_text(&add).contains(&"Add label to 2 selected notes".to_string()));
        let add_inputs = descendants_with_tag(&add, "input");
        assert_eq!(
            attribute(add_inputs[0], "list"),
            Some("notes-batch-destination-label-options")
        );
        assert_eq!(attribute(add_inputs[1], "type"), Some("number"));
        assert_eq!(descendants_with_tag(&add, "datalist").len(), 1);

        let update = batch_label_form(
            BatchLabelMode::Update,
            &label_keys,
            "project",
            "priority",
            "2",
            1,
            false,
            false,
            None,
            BatchLabelInputRefs::default(),
            callbacks.clone(),
        );
        assert!(visible_text(&update).contains(&"Update label on 1 selected note".to_string()));
        let update_inputs = descendants_with_tag(&update, "input");
        assert_eq!(
            attribute(update_inputs[0], "list"),
            Some("notes-batch-source-label-options")
        );
        assert_eq!(
            attribute(update_inputs[1], "list"),
            Some("notes-batch-destination-label-options")
        );
        assert_eq!(attribute(update_inputs[2], "type"), Some("number"));
        assert_eq!(descendants_with_tag(&update, "datalist").len(), 2);

        let remove = batch_label_form(
            BatchLabelMode::Remove,
            &label_keys,
            "project",
            "",
            "",
            1,
            false,
            false,
            None,
            BatchLabelInputRefs::default(),
            callbacks,
        );
        assert!(visible_text(&remove).contains(&"Remove label from 1 selected note".to_string()));
        let remove_inputs = descendants_with_tag(&remove, "input");
        assert_eq!(remove_inputs.len(), 1);
        assert_eq!(
            attribute(remove_inputs[0], "list"),
            Some("notes-batch-source-label-options")
        );
        assert_eq!(descendants_with_tag(&remove, "datalist").len(), 1);
    }

    #[test]
    fn batch_label_form_locks_every_draft_input_while_mutating() {
        let label_keys = vec![LabelKey {
            key: "priority".into(),
            description: String::new(),
            value_type: "number".into(),
        }];
        let form = batch_label_form(
            BatchLabelMode::Update,
            &label_keys,
            "project",
            "priority",
            "2",
            1,
            true,
            false,
            None,
            BatchLabelInputRefs::default(),
            BatchLabelFormCallbacks::noop(),
        );

        assert!(descendants_with_tag(&form, "input")
            .iter()
            .all(|input| attribute(input, "disabled").is_some()));
        assert_eq!(attribute(&form, "aria-busy"), Some("true"));
        assert!(visible_text(&form).contains(&"Updating…".to_string()));
        assert!(batch_label_draft_input_allowed(false));
        assert!(!batch_label_draft_input_allowed(true));
    }

    #[test]
    fn batch_label_error_notice_announces_generic_errors_and_derives_stale_copy_from_the_gate() {
        let generic = batch_label_error_notice(
            Some("Could not update selected note labels. Please try again."),
            false,
        );
        assert_eq!(attribute(&generic, "role"), Some("alert"));
        assert_eq!(
            visible_text(&generic),
            ["Could not update selected note labels. Please try again."]
        );

        let stale = batch_label_error_notice(None, true);
        assert_eq!(attribute(&stale, "role"), Some("alert"));
        assert_eq!(
            visible_text(&stale),
            ["Selected notes changed after the list was loaded. Reload notes before retrying."]
        );
    }

    #[test]
    fn batch_label_form_renders_the_active_generic_error_inside_the_dialog_form() {
        let form = batch_label_form(
            BatchLabelMode::Add,
            &[],
            "",
            "project",
            "agent-note",
            1,
            false,
            false,
            Some("Could not update selected note labels. Please try again."),
            BatchLabelInputRefs::default(),
            BatchLabelFormCallbacks::noop(),
        );
        let alert = descendants_with_tag(&form, "p")
            .into_iter()
            .find(|node| attribute(node, "role") == Some("alert"))
            .expect("generic error should be announced in the dialog form");

        assert_eq!(
            visible_text(alert),
            ["Could not update selected note labels. Please try again."]
        );
    }

    #[test]
    fn notes_page_source_keeps_background_inert_and_modal_outside_the_background_wrapper() {
        let source = include_str!("notes.rs");
        let background = source
            .find("<div class=\"notes-page-content\"")
            .expect("background wrapper");
        let modal = source
            .find("{ batch_label_modal }")
            .expect("batch label modal");

        assert!(source[background..].contains("inert={batch_workflow_open}"));
        assert!(source[background..].contains("aria-busy={batch_mutating.to_string()}"));
        assert!(source.contains("let batch_focus_return_mode = use_mut_ref"));
        assert!(source.contains("ref={batch_success_ref.clone()} role=\"status\" tabindex=\"-1\""));
        assert!(source.contains("let batch_delete_focus_return = use_mut_ref"));
        assert!(source
            .contains("ref={batch_delete_success_ref.clone()} role=\"status\" tabindex=\"-1\""));
        assert!(modal > background);
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
    fn batch_mutation_locks_table_checkboxes_and_per_row_remove() {
        let query = NotesQueryParams {
            current: 1,
            page_size: 10,
            search: None,
            labels: None,
        };
        let row = note_row(
            "note-1",
            "Locked note",
            4,
            None,
            &[],
            0,
            0,
            false,
            Callback::noop(),
            true,
            &query,
            Callback::noop(),
            Callback::noop(),
        );
        let cells = rendered_children(&row);
        let checkbox = only_child_with_tag(cells[0], "input");
        assert_eq!(attribute(checkbox, "disabled"), Some("disabled"));
        let actions = only_child_with_tag(cells[5], "div");
        assert_eq!(
            attribute(element_children(actions)[2], "disabled"),
            Some("disabled")
        );
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
    fn note_table_scroll_contains_layout_while_preserving_horizontal_overflow() {
        let app_css = include_str!("../../app.css");
        let table_scroll_rule = app_css
            .split_once(".table-scroll {")
            .and_then(|(_, remainder)| remainder.split_once('}'))
            .map(|(rule, _)| rule)
            .expect("table-scroll CSS rule should exist");

        assert!(table_scroll_rule.contains("overflow-x: auto;"));
        assert!(table_scroll_rule.contains("contain: layout;"));
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
