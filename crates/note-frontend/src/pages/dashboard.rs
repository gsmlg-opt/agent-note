use std::{cell::Cell, rc::Rc};

use yew::prelude::*;
use yew_duskmoon::Alert;
use yew_router::prelude::*;

use crate::api;
use crate::routes::{NotesQueryParams, Route, DEFAULT_NOTES_PAGE_SIZE};
use crate::state::LabelFilter;

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
            let active = Rc::new(Cell::new(true));
            let task_active = active.clone();
            wasm_bindgen_futures::spawn_local(async move {
                loading.set(true);
                while task_active.get() {
                    match api::dashboard().await {
                        Ok(next) => {
                            summary.set(Some(next));
                            error.set(None);
                        }
                        Err(e) => error.set(Some(e)),
                    }
                    loading.set(false);
                    gloo_timers::future::TimeoutFuture::new(1_000).await;
                }
            });
            move || active.set(false)
        });
    }

    html! {
        <section class="stack dashboard">
            <div class="page-head">
                <div>
                    <h2 class="page-title">{ "Dashboard" }</h2>
                    <p class="page-hint">{ "Current note inventory, embedding status, category navigation, label usage, and recent updates." }</p>
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
                        <span class="dashboard-metric-label">{ "Embedded notes" }</span>
                        <strong>{ summary.embedded_note_count }</strong>
                        if let Some(note) = &summary.embedding_note {
                            <Link<Route>
                                to={Route::NoteShow { id: note.id.clone() }}
                                classes={classes!("dashboard-metric-detail")}
                            >
                                <span title={format!("Embedding: {} ({})", note.title, note.id)}>
                                    { format!("Embedding: {} ({})", note.title, note.id) }
                                </span>
                            </Link<Route>>
                        } else {
                            <span class="dashboard-metric-detail">{ "No active embedding" }</span>
                        }
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

                if !summary.categories.is_empty() {
                    <div class="dashboard-categories">
                        { for summary.categories.iter().map(|category| html! {
                            <section class="dashboard-panel dashboard-category" key={category.key.clone()}>
                                <div class="dashboard-panel-head">
                                    <div>
                                        <h3>{ category.key.clone() }</h3>
                                        if !category.description.is_empty() {
                                            <p>{ category.description.clone() }</p>
                                        }
                                    </div>
                                </div>
                                if category.values.is_empty() {
                                    <p class="empty compact">{ "No notes in this category." }</p>
                                } else {
                                    <div class="dashboard-category-values">
                                        { for category.values.iter().map(|value| html! {
                                            <Link<Route, NotesQueryParams>
                                                to={Route::Notes}
                                                query={Some(category_notes_query(&category.key, &value.value))}
                                                classes={classes!("chip", "chip-clickable", "chip-primary", "dashboard-category-chip")}
                                            >
                                                { category_chip_text(&value.value, value.count) }
                                            </Link<Route, NotesQueryParams>>
                                        }) }
                                    </div>
                                }
                            </section>
                        }) }
                    </div>
                }

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

fn category_notes_query(key: &str, value: &str) -> NotesQueryParams {
    NotesQueryParams {
        current: 1,
        page_size: DEFAULT_NOTES_PAGE_SIZE,
        search: None,
        labels: api::label_filter_selector(&[LabelFilter {
            key: key.into(),
            operator: "=".into(),
            value: value.into(),
        }]),
    }
}

fn category_chip_text(value: &str, count: usize) -> String {
    let noun = if count == 1 { "note" } else { "notes" };
    format!("{value} · {count} {noun}")
}

#[cfg(test)]
mod tests {
    use yew_router::query::ToQuery;

    use super::{category_chip_text, category_notes_query};
    use crate::routes::DEFAULT_NOTES_PAGE_SIZE;

    #[test]
    fn category_notes_query_uses_an_exact_first_page_filter() {
        let query = category_notes_query("project", "yellow-dog & sigma");

        assert_eq!(query.current, 1);
        assert_eq!(query.page_size, DEFAULT_NOTES_PAGE_SIZE);
        assert_eq!(query.search, None);
        assert_eq!(query.labels.as_deref(), Some("project=yellow-dog & sigma"));

        let serialized = query.to_query().unwrap();
        assert!(serialized.contains("current=1"));
        assert!(serialized.contains(&format!("page_size={DEFAULT_NOTES_PAGE_SIZE}")));
        assert!(serialized.contains("%3D"));
        assert!(serialized.contains("%26"));
        assert!(!serialized.contains("project=yellow-dog & sigma"));
    }

    #[test]
    fn category_chip_text_uses_the_correct_note_noun() {
        assert_eq!(category_chip_text("yellow-dog", 1), "yellow-dog · 1 note");
        assert_eq!(category_chip_text("sigma", 7), "sigma · 7 notes");
    }
}
