use std::collections::HashMap;

use yew::prelude::*;
use yew_duskmoon::Alert;
use yew_router::prelude::*;

use crate::api;
use crate::routes::Route;
use crate::state::{LabelKey, NoteSummary};

#[function_component(DashboardPage)]
pub fn dashboard_page() -> Html {
    let notes = use_state(Vec::<NoteSummary>::new);
    let labels = use_state(Vec::<LabelKey>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    {
        let notes = notes.clone();
        let labels = labels.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                loading.set(true);
                error.set(None);
                let loaded_notes = api::list_notes_filtered(&[]).await;
                let loaded_labels = api::list_labels().await;
                match (loaded_notes, loaded_labels) {
                    (Ok(note_list), Ok(label_list)) => {
                        notes.set(note_list);
                        labels.set(label_list);
                    }
                    (Err(e), _) | (_, Err(e)) => error.set(Some(e)),
                }
                loading.set(false);
            });
            || ()
        });
    }

    let mut label_counts = HashMap::<String, usize>::new();
    for note in notes.iter() {
        for (key, _) in &note.labels {
            *label_counts.entry(key.clone()).or_insert(0) += 1;
        }
    }
    let mut label_rows = labels
        .iter()
        .map(|label| {
            (
                label.key.clone(),
                label.value_type.clone(),
                label.description.clone(),
                *label_counts.get(&label.key).unwrap_or(&0),
            )
        })
        .collect::<Vec<_>>();
    label_rows.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));

    let mut recent = (*notes).clone();
    recent.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    recent.truncate(5);

    let latest_update = recent
        .first()
        .map(|note| format_timestamp(note.updated_at))
        .unwrap_or_else(|| "-".to_string());

    html! {
        <section class="stack dashboard">
            <div class="page-head">
                <div>
                    <h2 class="page-title">{ "Dashboard" }</h2>
                    <p class="page-hint">{ "Current note inventory, label usage, and recent updates." }</p>
                </div>
                <Link<Route> to={Route::Notes} classes={classes!("btn", "btn-primary")}>{ "Open notes" }</Link<Route>>
            </div>

            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading..." }</p>
            } else {
                <div class="dashboard-metrics">
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Notes" }</span>
                        <strong>{ notes.len() }</strong>
                    </div>
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Labels" }</span>
                        <strong>{ labels.len() }</strong>
                    </div>
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Last update" }</span>
                        <strong>{ latest_update }</strong>
                    </div>
                </div>

                <div class="dashboard-grid">
                    <section class="dashboard-panel">
                        <div class="dashboard-panel-head">
                            <h3>{ "Labels" }</h3>
                            <Link<Route> to={Route::Labels} classes={classes!("btn", "btn-ghost", "btn-sm")}>{ "Manage" }</Link<Route>>
                        </div>
                        if label_rows.is_empty() {
                            <p class="empty compact">{ "No labels yet." }</p>
                        } else {
                            <ul class="dashboard-labels">
                                { for label_rows.iter().take(12).map(|(key, value_type, description, count)| html! {
                                    <li class="dashboard-label-row" key={key.clone()}>
                                        <div class="dashboard-label-main">
                                            <span class="label-key">{ key.clone() }</span>
                                            <span class="label-type">{ value_type.clone() }</span>
                                            if !description.is_empty() {
                                                <span class="dashboard-label-desc">{ description.clone() }</span>
                                            }
                                        </div>
                                        <span class="dashboard-label-count">{ format!("{count} notes") }</span>
                                    </li>
                                }) }
                            </ul>
                        }
                    </section>

                    <section class="dashboard-panel">
                        <div class="dashboard-panel-head">
                            <h3>{ "Recent Updates" }</h3>
                            <Link<Route> to={Route::Notes} classes={classes!("btn", "btn-ghost", "btn-sm")}>{ "View all" }</Link<Route>>
                        </div>
                        if recent.is_empty() {
                            <p class="empty compact">{ "No notes yet." }</p>
                        } else {
                            <ul class="dashboard-updates">
                                { for recent.iter().map(|note| html! {
                                    <li class="dashboard-update-row" key={note.id.clone()}>
                                        <Link<Route> to={Route::NoteShow { id: note.id.clone() }} classes={classes!("dashboard-update-title")}>
                                            { note.title.clone() }
                                        </Link<Route>>
                                        <span>{ format_timestamp(note.updated_at) }</span>
                                    </li>
                                }) }
                            </ul>
                        }
                    </section>
                </div>
            }
        </section>
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
