use std::{cell::Cell, collections::HashSet, rc::Rc};

use gloo_timers::callback::Interval;
use note_core::sync::{SyncConfig, SyncRule};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::Alert;

use super::sync_connection_editor::SyncConnectionEditor;
use super::sync_rule_labels::SyncRuleLabels;
use crate::api::{self, SyncConnectionSummary, SyncProfile, SyncStatus};
use crate::state::LabelKey;

#[derive(Properties, PartialEq)]
pub struct SystemSyncProps {
    pub active: bool,
    pub value: SyncConfig,
    pub onchange: Callback<SyncConfig>,
    pub saving: bool,
    pub onsave: Callback<MouseEvent>,
    pub save_disabled: bool,
    pub oneditorchange: Callback<bool>,
    pub labels: Vec<LabelKey>,
    pub labels_error: Option<String>,
}

pub fn sync_config_error(value: &SyncConfig) -> Option<String> {
    note_core::validate_system_config(&note_core::SystemConfig {
        sync: value.clone(),
        ..Default::default()
    })
    .err()
    .map(|error| error.to_string())
}

#[function_component(SystemSync)]
pub fn system_sync(props: &SystemSyncProps) -> Html {
    let status = use_state(|| None::<SyncStatus>);
    let error = use_state(|| None::<String>);
    let retrying = use_state(|| false);
    let active = use_mut_ref(|| true);
    let editor = use_state(|| None::<Option<String>>);
    let connection_saved = use_state(|| false);
    let label_drafts = use_mut_ref(HashSet::<String>::new);
    let has_label_drafts = use_state(|| false);
    {
        let active = active.clone();
        use_effect_with((), move |_| move || *active.borrow_mut() = false);
    }
    {
        let oneditorchange = props.oneditorchange.clone();
        use_effect_with(
            (editor.is_some(), *has_label_drafts),
            move |(editor_open, labels_pending)| {
                oneditorchange.emit(*editor_open || *labels_pending);
                || ()
            },
        );
    }
    let on_add_connection = {
        let editor = editor.clone();
        let connection_saved = connection_saved.clone();
        Callback::from(move |_| {
            editor.set(Some(None));
            connection_saved.set(false);
        })
    };
    let on_edit_connection = {
        let editor = editor.clone();
        let connection_saved = connection_saved.clone();
        let profile = props.value.connection_profile.clone();
        Callback::from(move |_| {
            if let Some(id) = &profile {
                editor.set(Some(Some(id.clone())));
                connection_saved.set(false);
            }
        })
    };
    let on_cancel_connection = {
        let editor = editor.clone();
        Callback::from(move |_| editor.set(None))
    };
    let on_connection_saved = {
        let editor = editor.clone();
        let connection_saved = connection_saved.clone();
        let status = status.clone();
        let error = error.clone();
        let active = active.clone();
        let value = props.value.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |summary: SyncConnectionSummary| {
            if matches!(&*editor, Some(None)) {
                let mut next = value.clone();
                next.connection_profile = Some(summary.id.clone());
                onchange.emit(next);
            }
            if let Some(mut current) = (*status).clone() {
                let profile = SyncProfile {
                    id: summary.id.clone(),
                    url: summary.url.clone(),
                };
                if let Some(item) = current
                    .profiles
                    .iter_mut()
                    .find(|item| item.id == summary.id)
                {
                    *item = profile;
                } else {
                    current.profiles.push(profile);
                }
                status.set(Some(current));
            }
            editor.set(None);
            connection_saved.set(true);
            let status = status.clone();
            let error = error.clone();
            let active = active.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = api::get_sync_status().await;
                if !*active.borrow() {
                    return;
                }
                match result {
                    Ok(next) => {
                        status.set(Some(next));
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        })
    };
    let controls_disabled = props.saving || editor.is_some();
    {
        let status = status.clone();
        let error = error.clone();
        let active = active.clone();
        use_effect_with(props.active, move |selected| {
            let in_flight = Rc::new(Cell::new(false));
            let fetch = {
                let active = active.clone();
                Callback::from(move |()| {
                    if in_flight.replace(true) {
                        return;
                    }
                    let active = active.clone();
                    let in_flight = in_flight.clone();
                    let status = status.clone();
                    let error = error.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = api::get_sync_status().await;
                        in_flight.set(false);
                        if !*active.borrow() {
                            return;
                        }
                        match result {
                            Ok(next) => {
                                status.set(Some(next));
                                error.set(None);
                            }
                            Err(message) => error.set(Some(message)),
                        }
                    });
                })
            };
            let timer = selected.then(|| {
                fetch.emit(());
                Interval::new(5_000, move || fetch.emit(()))
            });
            move || drop(timer)
        });
    }

    let on_enabled = {
        let value = props.value.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let mut next = value.clone();
            next.enabled = input.checked();
            onchange.emit(next);
        })
    };
    let on_profile = {
        let value = props.value.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            let mut next = value.clone();
            next.connection_profile = match select.value() {
                value if value.is_empty() => None,
                value => Some(value),
            };
            onchange.emit(next);
        })
    };
    let on_add = {
        let value = props.value.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |_| {
            let mut next = value.clone();
            next.rules.push(SyncRule {
                id: uuid::Uuid::new_v4().to_string(),
                enabled: true,
                selector: String::new(),
            });
            onchange.emit(next);
        })
    };
    let on_retry = {
        let retrying = retrying.clone();
        let error = error.clone();
        let status = status.clone();
        let active = active.clone();
        Callback::from(move |_| {
            retrying.set(true);
            let retrying = retrying.clone();
            let error = error.clone();
            let status = status.clone();
            let active = active.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = match api::retry_sync().await {
                    Ok(()) => api::get_sync_status().await,
                    Err(message) => Err(message),
                };
                if !*active.borrow() {
                    return;
                }
                match result {
                    Ok(next) => {
                        status.set(Some(next));
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
                retrying.set(false);
            });
        })
    };
    let validation_error = sync_config_error(&props.value);
    let unavailable_profile = props.value.connection_profile.as_ref().filter(|id| {
        status
            .as_ref()
            .is_some_and(|current| !current.profiles.iter().any(|profile| &profile.id == *id))
    });

    html! {
        <section class="system-section" aria-labelledby="sync-title">
            <div class="system-section-head">
                <div>
                    <h3 id="sync-title">{ "GaoNote sync" }</h3>
                    <p>{ "Sync matching notes and attachments in both directions. The latest complete version wins." }</p>
                </div>
                <label class="system-toggle">
                    <input type="checkbox" class="toggle" checked={props.value.enabled}
                        disabled={controls_disabled} onchange={on_enabled} />
                    <span>{ if props.value.enabled { "Enabled" } else { "Paused" } }</span>
                </label>
            </div>
            <label class="field">
                <span>{ "Connection" }</span>
                <select class="select" disabled={controls_disabled} onchange={on_profile}>
                    <option value="" selected={props.value.connection_profile.is_none()}>{ "Choose a connection" }</option>
                    if let Some(id) = unavailable_profile {
                        <option value={id.clone()} selected={true}>{ format!("{id} (unavailable)") }</option>
                    }
                    if let Some(current) = &*status {
                        { for current.profiles.iter().map(|profile| html! {
                            <option value={profile.id.clone()} selected={props.value.connection_profile.as_deref() == Some(profile.id.as_str())}>{ format!("{} — {}", profile.id, profile.url) }</option>
                        }) }
                    }
                </select>
            </label>
            <div class="system-section-actions">
                <button type="button" class="btn btn-outline" disabled={controls_disabled || status.is_none()} onclick={on_add_connection}>{ "Add connection" }</button>
                <button type="button" class="btn btn-outline" disabled={controls_disabled || props.value.connection_profile.is_none() || unavailable_profile.is_some()} onclick={on_edit_connection}>{ "Edit connection" }</button>
            </div>
            if let Some(profile_id) = &*editor {
                <SyncConnectionEditor profile_id={profile_id.clone()}
                    existing_ids={status.as_ref().map(|current| current.profiles.iter().map(|profile| profile.id.clone()).collect::<Vec<_>>()).unwrap_or_default()}
                    onsaved={on_connection_saved} oncancel={on_cancel_connection} />
            }
            if *connection_saved {
                <p role="status">{ "Connection saved. Connection details apply immediately. Save settings to apply your sync selection, enabled state and rules." }</p>
            }
            <p class="system-score-help">{ "After pairing, this database keeps its connection identity. Save settings with sync paused to generate the local node ID, then register it in GaoNote before enabling sync." }</p>
            if let Some(current) = &*status {
                if current.profiles.is_empty() {
                    <p class="empty compact">{ "No connections configured. Use Add connection to choose how to reach your GaoNote server." }</p>
                }
            }
            <div class="duplicate-rules">
                { for props.value.rules.iter().enumerate().map(|(index, rule)| {
                    let on_selector = {
                        let value = props.value.clone();
                        let onchange = props.onchange.clone();
                        Callback::from(move |selector: String| {
                            let mut next = value.clone();
                            next.rules[index].selector = selector;
                            onchange.emit(next);
                        })
                    };
                    let on_label_draft = {
                        let label_drafts = label_drafts.clone();
                        let has_label_drafts = has_label_drafts.clone();
                        let id = rule.id.clone();
                        Callback::from(move |pending| {
                            let mut drafts = label_drafts.borrow_mut();
                            if pending {
                                drafts.insert(id.clone());
                            } else {
                                drafts.remove(&id);
                            }
                            has_label_drafts.set(!drafts.is_empty());
                        })
                    };
                    let on_rule_enabled = {
                        let value = props.value.clone();
                        let onchange = props.onchange.clone();
                        Callback::from(move |event: Event| {
                            let input: HtmlInputElement = event.target_unchecked_into();
                            let mut next = value.clone();
                            next.rules[index].enabled = input.checked();
                            onchange.emit(next);
                        })
                    };
                    let on_remove = {
                        let value = props.value.clone();
                        let onchange = props.onchange.clone();
                        let label_drafts = label_drafts.clone();
                        let has_label_drafts = has_label_drafts.clone();
                        let id = rule.id.clone();
                        Callback::from(move |_| {
                            let mut drafts = label_drafts.borrow_mut();
                            drafts.remove(&id);
                            has_label_drafts.set(!drafts.is_empty());
                            let mut next = value.clone();
                            next.rules.remove(index);
                            onchange.emit(next);
                        })
                    };
                    html! {
                        <div class="duplicate-rule" key={rule.id.clone()}>
                            <div class="duplicate-rule-head">
                                <label class="system-toggle">
                                    <input type="checkbox" checked={rule.enabled} disabled={controls_disabled} onchange={on_rule_enabled} />
                                    <span>{ format!("Rule {}", index + 1) }</span>
                                </label>
                                <button type="button" class="btn btn-outline" disabled={controls_disabled} onclick={on_remove}
                                    aria-label={format!("Remove sync rule {}", index + 1)}>{ "Remove" }</button>
                            </div>
                            <SyncRuleLabels id={rule.id.clone()} selector={rule.selector.clone()}
                                labels={props.labels.clone()} disabled={controls_disabled}
                                onchange={on_selector} ondraftchange={on_label_draft} />
                        </div>
                    }
                }) }
            </div>
            if props.value.rules.is_empty() {
                <p class="empty compact">{ "No sync rules. Add a rule and choose labels to select which notes sync to GaoNote." }</p>
            }
            <p class="system-score-help">{ "Only notes matching an enabled rule sync to GaoNote. All label conditions in a rule must match; any enabled rule can match. Removing a matching label keeps both copies and stops their sync. Save settings to apply changes." }</p>
            if props.labels_error.is_some() {
                <p class="system-score-help" role="status">{ "Label suggestions could not be loaded. You can still enter label keys and values." }</p>
            }
            if let Some(message) = validation_error {
                <p role="alert" class="system-score-error">{ message }</p>
            }
            <div class="system-section-actions">
                <button type="button" class="btn btn-outline" disabled={controls_disabled} onclick={on_add}>{ "Add sync rule" }</button>
            </div>
            if let Some(current) = &*status {
                <dl class="system-info" aria-live="polite">
                    <div><dt>{ "Status" }</dt><dd>{ if current.initializing { "Preparing existing notes".to_string() } else { current.state.clone() } }</dd></div>
                    <div><dt>{ "Pending notes" }</dt><dd>{ current.pending_notes }</dd></div>
                    if let Some(node_id) = &current.node_id {
                        <div><dt>{ "Local node ID" }</dt><dd>{ node_id }</dd></div>
                    }
                    if let Some(peer_node_id) = &current.peer_node_id {
                        <div><dt>{ "Paired server ID" }</dt><dd>{ peer_node_id }</dd></div>
                    }
                    if let Some(timestamp) = current.last_success_at {
                        <div><dt>{ "Last successful sync" }</dt><dd>{ chrono::DateTime::from_timestamp_millis(timestamp).map(|time| time.to_rfc3339()).unwrap_or_default() }</dd></div>
                    }
                </dl>
                if let Some(message) = &current.last_error {
                    <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
                }
                if !current.blocked_notes.is_empty() {
                    <div class="system-score-error" role="status">
                        <p>{ "These notes need attention. Other notes can continue syncing." }</p>
                        <ul>
                            { for current.blocked_notes.iter().map(|note| {
                                let message = match note.reason.as_str() {
                                    "catalog_conflict" => "Label definitions differ between the two systems.",
                                    "validation_failed" => "The receiving system rejected this note's data.",
                                    "local_preparation_failed" => "This note or its attachments could not be prepared for transfer.",
                                    _ => "This note could not be synchronized.",
                                };
                                html! { <li key={note.event_id.clone()}>{ format!("{}: {message}", note.note_id) }</li> }
                            }) }
                        </ul>
                        <p>{ "Correct the note or its labels, then save it or retry sync." }</p>
                    </div>
                }
            }
            if let Some(message) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
            }
            <div class="system-section-actions">
                if let Some(current) = &*status {
                    <button type="button" class="btn btn-outline" disabled={*retrying || controls_disabled || current.state == "disabled"}
                        onclick={on_retry}>{ if *retrying { "Retrying..." } else { "Retry sync" } }</button>
                }
                <button type="button" class="btn btn-primary" onclick={props.onsave.clone()}
                    disabled={controls_disabled || props.save_disabled || *has_label_drafts}>
                    { if props.saving { "Saving..." } else { "Save settings" } }
                </button>
            </div>
        </section>
    }
}
