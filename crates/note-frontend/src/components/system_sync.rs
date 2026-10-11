use std::{cell::Cell, rc::Rc};

use gloo_timers::callback::Interval;
use note_core::sync::{SyncConfig, SyncRule};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::Alert;

use crate::api::{self, SyncStatus};

#[derive(Properties, PartialEq)]
pub struct SystemSyncProps {
    pub value: SyncConfig,
    pub onchange: Callback<SyncConfig>,
    pub saving: bool,
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
    {
        let status = status.clone();
        let error = error.clone();
        let active = active.clone();
        use_effect_with((), move |_| {
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
            fetch.emit(());
            let timer = Interval::new(5_000, move || fetch.emit(()));
            move || {
                *active.borrow_mut() = false;
                drop(timer);
            }
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
                        disabled={props.saving} onchange={on_enabled} />
                    <span>{ if props.value.enabled { "Enabled" } else { "Paused" } }</span>
                </label>
            </div>
            <label class="field">
                <span>{ "Connection" }</span>
                <select class="select" value={props.value.connection_profile.clone().unwrap_or_default()}
                    disabled={props.saving} onchange={on_profile}>
                    <option value="">{ "Choose a connection" }</option>
                    if let Some(id) = unavailable_profile {
                        <option value={id.clone()}>{ format!("{id} (unavailable)") }</option>
                    }
                    if let Some(current) = &*status {
                        { for current.profiles.iter().map(|profile| html! {
                            <option value={profile.id.clone()}>{ format!("{} — {}", profile.id, profile.url) }</option>
                        }) }
                    }
                </select>
            </label>
            <p class="system-score-help">{ "After pairing, this database keeps its connection identity. Pause sync to stop delivery." }</p>
            if let Some(current) = &*status {
                if current.profiles.is_empty() {
                    <p class="empty compact">{ "No connections configured. Add a connection and mTLS credentials on the server first." }</p>
                }
            }
            <div class="duplicate-rules">
                { for props.value.rules.iter().enumerate().map(|(index, rule)| {
                    let on_selector = {
                        let value = props.value.clone();
                        let onchange = props.onchange.clone();
                        Callback::from(move |event: InputEvent| {
                            let input: HtmlInputElement = event.target_unchecked_into();
                            let mut next = value.clone();
                            next.rules[index].selector = input.value();
                            onchange.emit(next);
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
                        Callback::from(move |_| {
                            let mut next = value.clone();
                            next.rules.remove(index);
                            onchange.emit(next);
                        })
                    };
                    html! {
                        <div class="duplicate-rule" key={rule.id.clone()}>
                            <div class="duplicate-rule-head">
                                <label class="system-toggle">
                                    <input type="checkbox" checked={rule.enabled} disabled={props.saving} onchange={on_rule_enabled} />
                                    <span>{ format!("Rule {}", index + 1) }</span>
                                </label>
                                <button type="button" class="btn btn-outline" disabled={props.saving} onclick={on_remove}
                                    aria-label={format!("Remove sync rule {}", index + 1)}>{ "Remove" }</button>
                            </div>
                            <label class="field">
                                <span>{ "Label selector" }</span>
                                <input type="text" class="input" value={rule.selector.clone()} placeholder="project=bear_note"
                                    disabled={props.saving} oninput={on_selector} />
                            </label>
                        </div>
                    }
                }) }
            </div>
            <p class="system-score-help">{ "Rules match any selected group. Removing a matching label keeps both copies and stops their sync. Save settings to apply changes." }</p>
            if let Some(message) = validation_error {
                <p role="alert" class="system-score-error">{ message }</p>
            }
            <div class="system-section-actions">
                <button type="button" class="btn btn-outline" disabled={props.saving} onclick={on_add}>{ "Add sync rule" }</button>
            </div>
            if let Some(current) = &*status {
                <dl class="system-info" aria-live="polite">
                    <div><dt>{ "Status" }</dt><dd>{ if current.initializing { "Preparing existing notes".to_string() } else { current.state.clone() } }</dd></div>
                    <div><dt>{ "Pending notes" }</dt><dd>{ current.pending_notes }</dd></div>
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
                <div class="system-section-actions">
                    <button type="button" class="btn btn-outline" disabled={*retrying || props.saving || current.state == "disabled"}
                        onclick={on_retry}>{ if *retrying { "Retrying..." } else { "Retry sync" } }</button>
                </div>
            }
            if let Some(message) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
            }
        </section>
    }
}
