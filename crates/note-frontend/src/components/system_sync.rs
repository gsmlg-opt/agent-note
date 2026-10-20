use std::{cell::Cell, collections::HashSet, rc::Rc};

use gloo_timers::callback::Interval;
use note_core::sync::{SyncConfig, SyncRule};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::Alert;

use super::sync_connection_editor::SyncConnectionEditor;
use super::sync_rule_labels::SyncRuleLabels;
use crate::api::{self, SyncConnectionSummary, SyncDirectionProgress, SyncProfile, SyncStatus};
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

fn sync_timestamp(timestamp: i64) -> Html {
    match chrono::DateTime::from_timestamp_millis(timestamp) {
        Some(time) => html! {
            <time datetime={time.to_rfc3339()}>{ time.format("%Y-%m-%d %H:%M:%S UTC").to_string() }</time>
        },
        None => html! { <span>{ "Unknown time" }</span> },
    }
}

fn direction_phase(progress: &SyncDirectionProgress) -> &str {
    if let Some(task) = &progress.active_task {
        match task.state.as_str() {
            "failed" => return "Task failed",
            "cancelled" => return "Task cancelled",
            "queued" | "running" if progress.phase == "idle" => return "Processing task",
            _ => {}
        }
    }
    if let Some(manager) = &progress.manager {
        if manager.state.as_deref() == Some("failed") {
            return "Sync manager failed";
        }
        if matches!(progress.phase.as_str(), "idle" | "checking") && !manager.scan_complete {
            return "Discovering notes";
        }
        if progress.phase == "idle" && manager.pages_checked < manager.pages_scanned {
            return "Comparing note pages";
        }
        if progress.phase == "idle"
            && manager.mode == "changes"
            && progress.notes_remaining == Some(0)
            && progress.blocked_notes == 0
        {
            return "Watching for changes";
        }
    }
    match progress.phase.as_str() {
        "checking" => "Checking notes",
        "transferring" => "Transferring notes",
        "blocked" => "Needs attention",
        "reconnecting" => "Reconnecting",
        "paused" => "Paused",
        "upgrade_required" => "Receiver upgrade required",
        "idle" if progress.notes_remaining == Some(0) && progress.blocked_notes == 0 => {
            "Round complete"
        }
        "idle" => "Waiting for notes",
        _ => "Waiting for progress",
    }
}

fn task_operation(operation: &str) -> &str {
    match operation {
        "inventory_start" => "Checking note inventory",
        "state_query" => "Comparing note versions",
        "prepare" | "preparation" => "Preparing attachments",
        "transfer_begin" => "Starting note transfer",
        "attachment_begin" => "Starting attachment transfer",
        "blob_chunk" => "Transferring data chunk",
        "blob_end" => "Verifying received data",
        "transfer_commit" => "Saving received note",
        "inventory_complete" => "Completing inventory check",
        "transfer_abort" => "Cleaning up transfer",
        _ => "Synchronizing notes",
    }
}

fn task_state(state: &str, phase: &str) -> &'static str {
    match (state, phase) {
        ("running", "outcome_unknown") => "Checking commit outcome",
        ("running", "cancellation_pending") => "Finishing current write",
        ("queued", _) => "Queued",
        ("running", _) => "Running",
        ("succeeded", _) => "Complete",
        ("failed", _) => "Failed",
        ("cancelled", _) => "Cancelled",
        _ => "Awaiting task status",
    }
}

fn note_phase(phase: &str) -> &str {
    match phase {
        "preparing" => "Preparing note",
        "metadata" => "Transferring note content",
        "attachments" => "Transferring attachments",
        "applying" => "Applying note",
        "awaiting_commit" => "Awaiting confirmation",
        _ => "Processing note",
    }
}

fn direction_progress(title: &str, progress: &SyncDirectionProgress, connected: bool) -> Html {
    let checking = progress.phase == "checking";
    let discovering = progress.manager.as_ref().is_some_and(|m| !m.scan_complete);
    let comparing = progress
        .manager
        .as_ref()
        .is_some_and(|m| m.pages_checked < m.pages_scanned);
    let notes_total = progress.notes_total.filter(|_| !discovering);
    let resolved = progress
        .notes_equal
        .saturating_add(progress.notes_completed)
        .saturating_add(progress.notes_superseded);
    let value = if checking {
        progress.notes_checked
    } else {
        resolved
    };
    // A known zero-work round is complete; native progress requires a positive max.
    let maximum = notes_total.map(|total| total.max(1));
    let value = notes_total.map(|total| {
        if total == 0 && progress.phase == "idle" {
            1
        } else {
            value.min(total)
        }
    });
    let current_note = progress
        .current_note
        .as_ref()
        .filter(|_| connected && progress.phase == "transferring");
    html! {
        <section class="sync-progress-direction" aria-label={title.to_string()}>
            <h4>{ title }</h4>
            <p class="sync-progress-phase" role="status">{ direction_phase(progress) }</p>
            if let Some(task) = &progress.active_task {
                <div class="sync-progress-note" aria-label={format!("{title} task")}>
                    <p role="status">{ format!("{} · {}", task_operation(&task.operation),
                        task_state(&task.state, &task.phase)) }</p>
                    <p class="system-score-help">{ "Last task update: " }{ sync_timestamp(task.updated_at) }</p>
                    if let Some(error) = &task.error {
                        <p class="error" role="alert">{ error.chars().take(240).collect::<String>() }</p>
                    }
                    <details class="sync-progress-round">
                        <summary>{ "Task details" }</summary><code>{ &task.id }</code>
                    </details>
                </div>
            }
            if let Some(manager) = &progress.manager {
                <div class="sync-progress-note" aria-label={format!("{title} scan")}>
                    <p>{ if manager.mode == "changes" { "Durable changes" } else { "Initial comparison" } }</p>
                    <p>{ format!("{} {} discovered · {} {} compared",
                        manager.pages_scanned, if manager.pages_scanned == 1 { "page" } else { "pages" },
                        manager.pages_checked, if manager.pages_checked == 1 { "page" } else { "pages" }) }</p>
                    <p>{ format!("{} notes discovered · up to {} notes per page", manager.notes_scanned, manager.page_size) }</p>
                    <p>{ if manager.scan_complete { "Source discovery complete" } else { "Total being counted" } }</p>
                    <p class="system-score-help">{ "Last sync progress: " }{ sync_timestamp(manager.last_progress_at) }</p>
                    if let Some(error) = &manager.error {
                        <p class="error" role="alert">{ error.chars().take(240).collect::<String>() }</p>
                    }
                    if let Some(cursor) = &manager.source_cursor {
                        <details class="sync-progress-round">
                            <summary>{ "Scan details" }</summary><code>{ cursor }</code>
                        </details>
                    }
                </div>
            }
            <p class="system-score-help">{
                match progress.basis.as_str() {
                    "remote_verified" => "Compared with GaoNote in this round",
                    "legacy_replay" => "Legacy replay; remote totals unavailable",
                    _ => "Local queue; remote state not verified",
                }
            }</p>
            <progress aria-label={format!("{title} round progress")}
                max={maximum.map(|value| value.to_string())} value={value.map(|value| value.to_string())} />
            <p>{ match notes_total {
                Some(total) if checking => format!("{} of {total} checked", progress.notes_checked),
                Some(total) => format!("{total} notes in this round"),
                None if discovering => format!("Total being counted · {} checked", progress.notes_checked),
                None => format!("Total unknown · {} checked", progress.notes_checked),
            } }</p>
            <ul class="sync-progress-counts">
                <li>{ format!("{} already equal", progress.notes_equal) }</li>
                <li>{ format!("{} planned", progress.notes_planned) }</li>
                <li>{ format!("{} delivered", progress.notes_completed) }</li>
                <li>{ format!("{} superseded", progress.notes_superseded) }</li>
                <li>{ match progress.notes_remaining {
                    Some(remaining) if discovering => format!("{remaining} known remaining · scan in progress"),
                    Some(remaining) if comparing => format!("{remaining} known remaining · comparison in progress"),
                    Some(remaining) => format!("{remaining} remaining"),
                    None if checking => "Remaining being checked".to_string(),
                    None => "Remaining unknown".to_string(),
                } }</li>
                <li>{ format!("{} blocked", progress.blocked_notes) }</li>
            </ul>
            if let Some(note) = current_note {
                <div class="sync-progress-note">
                    <p class="sync-progress-phase" role="status">{ note_phase(&note.phase) }</p>
                    if let Some(title) = &note.title {
                        <p class="sync-progress-note-title">{ title.chars().take(240).collect::<String>() }</p>
                    }
                    <p><code>{ &note.id }</code></p>
                    <p>{ format!("{} / {} attachments · {} / {} bytes", note.attachments_completed,
                        note.attachments_total, note.attachment_bytes_done, note.attachment_bytes_total) }</p>
                    if note.attachments_total > 0 {
                        <progress aria-label={format!("{title} attachment bytes")}
                            max={note.attachment_bytes_total.max(1).to_string()}
                            value={note.attachment_bytes_done.min(note.attachment_bytes_total).to_string()} />
                    }
                </div>
            }
            <details class="sync-progress-round"><summary>{ "Round details" }</summary><code>{ &progress.run_id }</code></details>
        </section>
    }
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
                Interval::new(2_000, move || fetch.emit(()))
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
        <section class="system-section system-sync" aria-labelledby="sync-title">
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
                    if let Some(timestamp) = current.last_attempt_at {
                        <div><dt>{ "Last connection attempt" }</dt><dd>{ sync_timestamp(timestamp) }</dd></div>
                    }
                    if let Some(timestamp) = current.next_retry_at {
                        <div><dt>{ "Next automatic retry" }</dt><dd>{ sync_timestamp(timestamp) }</dd></div>
                    }
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
                if let Some(progress) = &current.progress {
                    <div class="sync-progress">
                        <p class="system-score-help">{ "Last progress sample: " }{ sync_timestamp(progress.updated_at) }</p>
                        if let Some(total) = progress.tracked_notes {
                            <p class="system-score-help">{ format!("Locally tracked at last full check: {total} notes") }</p>
                        }
                        <div class="sync-progress-directions">
                            { direction_progress("Sending to GaoNote", &progress.upload, current.state == "connected") }
                            { direction_progress("Receiving from GaoNote", &progress.download, current.state == "connected") }
                        </div>
                        <p class="system-score-help">{ "Counts belong to each current round. Delivered means processing was confirmed; a newer version may still win." }</p>
                    </div>
                }
                <section class="sync-diagnostics" aria-label="Connection diagnostics">
                    <h4>{ "Connection diagnostics" }</h4>
                    <p class="system-score-help">{ "Recent connection events from this server session, newest first. History resets when Agent Note restarts." }</p>
                    if current.diagnostics.is_empty() {
                        <p class="empty compact">{ "No connection diagnostics recorded in this server session yet." }</p>
                    } else {
                        <ol class="sync-diagnostics-list" tabindex="0" aria-label="Recent connection events">
                            { for current.diagnostics.iter().rev().map(|entry| html! {
                                <li class={classes!("sync-diagnostic", (entry.level == "error").then_some("sync-diagnostic-error"))}>
                                    <div class="sync-diagnostic-meta">
                                        { sync_timestamp(entry.timestamp) }
                                        <span>{ format!("{} · {}", entry.level, entry.stage) }</span>
                                        <code>{ &entry.code }</code>
                                    </div>
                                    <p>{ &entry.message }</p>
                                </li>
                            }) }
                        </ol>
                    }
                </section>
                if !current.blocked_notes.is_empty() {
                    <div class="system-score-error" role="status">
                        <p>{ "These notes need attention. Other notes can continue syncing." }</p>
                        if current.blocked_notes_total > current.blocked_notes.len() as u64 {
                            <p>{ format!("Showing {} of {} notes needing attention.", current.blocked_notes.len(), current.blocked_notes_total) }</p>
                        }
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

#[cfg(test)]
mod progress_tests {
    use super::*;

    fn progress(phase: &str, remaining: Option<u64>) -> SyncDirectionProgress {
        SyncDirectionProgress {
            run_id: "round-1".into(),
            phase: phase.into(),
            basis: "remote_verified".into(),
            notes_total: None,
            notes_checked: 0,
            notes_equal: 0,
            notes_planned: 0,
            notes_completed: 0,
            notes_superseded: 0,
            notes_remaining: remaining,
            blocked_notes: 0,
            current_note: None,
            active_task: None,
            manager: None,
        }
    }

    #[test]
    fn scan_and_uncompared_pages_prevent_false_round_completion() {
        let mut current = progress("idle", Some(0));
        current.manager = Some(crate::api::SyncManagerProgress {
            mode: "initial_scan".into(),
            page_size: 32,
            pages_scanned: 2,
            pages_checked: 1,
            notes_scanned: 64,
            scan_complete: false,
            source_cursor: None,
            last_progress_at: 1_700_000_000_000,
            state: None,
            error: None,
        });
        assert_eq!(direction_phase(&current), "Discovering notes");
        current.manager.as_mut().unwrap().scan_complete = true;
        assert_eq!(direction_phase(&current), "Comparing note pages");
        current.manager.as_mut().unwrap().pages_checked = 2;
        assert_eq!(direction_phase(&current), "Round complete");
        current.manager.as_mut().unwrap().mode = "changes".into();
        assert_eq!(direction_phase(&current), "Watching for changes");
        current.manager.as_mut().unwrap().state = Some("failed".into());
        assert_eq!(direction_phase(&current), "Sync manager failed");
    }

    #[test]
    fn unknown_remaining_is_never_reported_as_complete() {
        assert_eq!(
            direction_phase(&progress("idle", None)),
            "Waiting for notes"
        );
        assert_eq!(
            direction_phase(&progress("checking", None)),
            "Checking notes"
        );
    }

    #[test]
    fn completion_requires_resolved_and_unblocked_work() {
        assert_eq!(
            direction_phase(&progress("idle", Some(0))),
            "Round complete"
        );
        let mut blocked = progress("idle", Some(0));
        blocked.blocked_notes = 1;
        assert_ne!(direction_phase(&blocked), "Round complete");
        assert_ne!(
            direction_phase(&progress("idle", Some(1))),
            "Round complete"
        );
    }

    #[test]
    fn awaiting_commit_is_distinct_from_attachment_transfer() {
        assert_eq!(note_phase("awaiting_commit"), "Awaiting confirmation");
        assert_eq!(note_phase("attachments"), "Transferring attachments");
    }

    #[test]
    fn task_failure_is_visible_without_reporting_a_disconnected_round() {
        let direction: SyncDirectionProgress = serde_json::from_value(serde_json::json!({
            "run_id": "round-1", "phase": "transferring", "basis": "remote_verified",
            "notes_total": 2, "notes_checked": 2, "notes_equal": 0,
            "notes_planned": 2, "notes_completed": 0, "notes_superseded": 0,
            "notes_remaining": 2, "blocked_notes": 1, "current_note": null,
            "active_task": {"id": "node:1", "operation": "blob_end", "state": "failed",
                "phase": "verifying", "updated_at": 1700000000000_i64,
                "error": "Attachment checksum did not match"}
        }))
        .unwrap();
        assert_eq!(direction_phase(&direction), "Task failed");
        assert_eq!(direction.phase, "transferring");
        assert_eq!(direction.notes_completed, 0);
    }

    #[test]
    fn a_running_task_prevents_a_false_round_complete_label() {
        let direction: SyncDirectionProgress = serde_json::from_value(serde_json::json!({
            "run_id": "round-1", "phase": "idle", "basis": "remote_verified",
            "notes_total": 0, "notes_checked": 0, "notes_equal": 0,
            "notes_planned": 0, "notes_completed": 0, "notes_superseded": 0,
            "notes_remaining": 0, "blocked_notes": 0, "current_note": null,
            "active_task": {"id": "node:2", "operation": "inventory_start", "state": "running",
                "phase": "checking", "updated_at": 1700000000000_i64, "error": null}
        }))
        .unwrap();
        assert_eq!(direction_phase(&direction), "Processing task");
    }
}
