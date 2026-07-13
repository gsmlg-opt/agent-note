use yew::prelude::*;
use yew_duskmoon::Alert;
use yew_router::prelude::*;

use crate::api;
use crate::routes::Route;

#[function_component(DashboardPage)]
pub fn dashboard_page() -> Html {
    let summary = use_state(|| None::<api::DashboardSummary>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);

    {
        let summary = summary.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                loading.set(true);
                error.set(None);
                match api::dashboard().await {
                    Ok(next) => summary.set(Some(next)),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
            || ()
        });
    }

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
            } else if let Some(summary) = &*summary {
                <div class="dashboard-metrics">
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Notes" }</span>
                        <strong>{ summary.note_count }</strong>
                    </div>
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Labels" }</span>
                        <strong>{ summary.label_count }</strong>
                    </div>
                    <div class="dashboard-metric">
                        <span class="dashboard-metric-label">{ "Last update" }</span>
                        <strong>{ summary.last_updated_at.map(format_timestamp).unwrap_or_else(|| "-".to_string()) }</strong>
                    </div>
                </div>

                <div class="dashboard-grid">
                    <section class="dashboard-panel">
                        <div class="dashboard-panel-head">
                            <h3>{ "Labels" }</h3>
                            <Link<Route> to={Route::Labels} classes={classes!("btn", "btn-ghost", "btn-sm")}>{ "Manage" }</Link<Route>>
                        </div>
                        if summary.labels.is_empty() {
                            <p class="empty compact">{ "No labels yet." }</p>
                        } else {
                            <ul class="dashboard-labels">
                                { for summary.labels.iter().take(12).map(|label| html! {
                                    <li class="dashboard-label-row" key={label.key.clone()}>
                                        <div class="dashboard-label-main">
                                            <span class="label-key">{ label.key.clone() }</span>
                                            <span class="label-type">{ label.value_type.clone() }</span>
                                            if !label.description.is_empty() {
                                                <span class="dashboard-label-desc">{ label.description.clone() }</span>
                                            }
                                        </div>
                                        <span class="dashboard-label-count">{ format!("{} notes", label.count) }</span>
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
                        if summary.recent_updates.is_empty() {
                            <p class="empty compact">{ "No notes yet." }</p>
                        } else {
                            <ul class="dashboard-updates">
                                { for summary.recent_updates.iter().map(|note| html! {
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
            } else {
                <p class="empty">{ "Dashboard is unavailable." }</p>
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
