use std::collections::HashSet;

use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_duskmoon::Alert;

use crate::api;
use crate::components::{icons, Modal};
use crate::state::DeletedNoteSummary;

#[derive(Clone, PartialEq)]
enum RestoreTarget {
    Single { id: String, title: String },
    Batch { ids: Vec<String> },
}

impl RestoreTarget {
    fn ids(&self) -> Vec<String> {
        match self {
            Self::Single { id, .. } => vec![id.clone()],
            Self::Batch { ids } => ids.clone(),
        }
    }
}

#[derive(Clone, PartialEq)]
enum DeleteTarget {
    Single { id: String, title: String },
    Batch { ids: Vec<String> },
}

impl DeleteTarget {
    fn ids(&self) -> Vec<String> {
        match self {
            Self::Single { id, .. } => vec![id.clone()],
            Self::Batch { ids } => ids.clone(),
        }
    }

    fn modal_copy(&self) -> (String, String, String) {
        match self {
            Self::Single { title, .. } => (
                "Delete note permanently".to_string(),
                format!(
                    "Permanently delete the note \u{201c}{title}\u{201d}? This cannot be undone."
                ),
                "Delete permanently".to_string(),
            ),
            Self::Batch { ids } => (
                "Delete selected notes permanently".to_string(),
                format!(
                    "Permanently delete {} selected notes? This cannot be undone.",
                    ids.len()
                ),
                format!("Delete {} notes", ids.len()),
            ),
        }
    }

    fn is_batch(&self) -> bool {
        matches!(self, Self::Batch { .. })
    }
}

fn selected_note_ids(notes: &[DeletedNoteSummary], selected: &HashSet<String>) -> Vec<String> {
    notes
        .iter()
        .filter(|note| selected.contains(&note.id))
        .map(|note| note.id.clone())
        .collect()
}

fn reconcile_delete_selection(
    selected: &HashSet<String>,
    attempted: &[String],
    failed: &HashSet<String>,
) -> HashSet<String> {
    let attempted = attempted.iter().map(String::as_str).collect::<HashSet<_>>();
    selected
        .iter()
        .filter(|id| !attempted.contains(id.as_str()) || failed.contains(*id))
        .cloned()
        .collect()
}

fn batch_delete_failure_message(total: usize, failed: usize) -> Option<String> {
    (failed > 0).then(|| format!(
        "Deleted {} of {total} selected notes; {failed} failed. Failed notes still in Trash remain selected.",
        total.saturating_sub(failed)
    ))
}

#[function_component(TrashPage)]
pub fn trash_page() -> Html {
    let notes = use_state(Vec::<DeletedNoteSummary>::new);
    let selected = use_state(HashSet::<String>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let delete_target = use_state(|| None::<DeleteTarget>);
    let restore_target = use_state(|| None::<RestoreTarget>);
    let deleting = use_state(|| false);
    let restoring = use_state(|| false);
    let batch_delete_error = use_state(|| None::<String>);
    let refresh_tick = use_state(|| 0usize);
    let select_all_ref = use_node_ref();

    {
        let notes = notes.clone();
        let selected = selected.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with(*refresh_tick, move |_| {
            loading.set(true);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_deleted_notes().await {
                    Ok(next) => {
                        let visible = next
                            .iter()
                            .map(|note| note.id.clone())
                            .collect::<HashSet<_>>();
                        let mut next_selected = (*selected).clone();
                        next_selected.retain(|id| visible.contains(id));
                        selected.set(next_selected);
                        notes.set(next);
                    }
                    Err(message) => error.set(Some(message)),
                }
                loading.set(false);
            });
            || ()
        });
    }

    {
        let select_all_ref = select_all_ref.clone();
        let selected_count = selected.len();
        let note_count = notes.len();
        use_effect_with(
            (selected_count, note_count),
            move |(selected_count, note_count)| {
                if let Some(input) = select_all_ref.cast::<HtmlInputElement>() {
                    input.set_indeterminate(*selected_count > 0 && *selected_count < *note_count);
                }
                || ()
            },
        );
    }

    let on_refresh = {
        let refresh_tick = refresh_tick.clone();
        let batch_delete_error = batch_delete_error.clone();
        Callback::from(move |_| {
            batch_delete_error.set(None);
            refresh_tick.set((*refresh_tick).saturating_add(1));
        })
    };

    let on_select_all = {
        let notes = notes.clone();
        let selected = selected.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            if input.checked() {
                selected.set(notes.iter().map(|note| note.id.clone()).collect());
            } else {
                selected.set(HashSet::new());
            }
        })
    };

    let open_batch_restore = {
        let notes = notes.clone();
        let selected = selected.clone();
        let restore_target = restore_target.clone();
        Callback::from(move |_| {
            let ids = notes
                .iter()
                .filter(|note| selected.contains(&note.id))
                .map(|note| note.id.clone())
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                restore_target.set(Some(RestoreTarget::Batch { ids }));
            }
        })
    };

    let open_batch_delete = {
        let notes = notes.clone();
        let selected = selected.clone();
        let delete_target = delete_target.clone();
        let error = error.clone();
        let batch_delete_error = batch_delete_error.clone();
        Callback::from(move |_| {
            let ids = selected_note_ids(&notes, &selected);
            error.set(None);
            batch_delete_error.set(None);
            if !ids.is_empty() {
                delete_target.set(Some(DeleteTarget::Batch { ids }));
            }
        })
    };

    let restore_modal = match (*restore_target).clone() {
        None => html! {},
        Some(target) => {
            let ids = target.ids();
            let (modal_title, message, confirm_label) = match &target {
                RestoreTarget::Single { title, .. } => (
                    "Restore note".to_string(),
                    format!("Restore the note \u{201c}{title}\u{201d} to Notes?"),
                    "Restore note".to_string(),
                ),
                RestoreTarget::Batch { ids } => (
                    "Restore selected notes".to_string(),
                    format!("Restore {} selected notes to Notes?", ids.len()),
                    format!("Restore {} notes", ids.len()),
                ),
            };
            let on_close = {
                let restore_target = restore_target.clone();
                let restoring = restoring.clone();
                Callback::from(move |_: ()| {
                    if !*restoring {
                        restore_target.set(None);
                    }
                })
            };
            let on_cancel = {
                let restore_target = restore_target.clone();
                let restoring = restoring.clone();
                Callback::from(move |_| {
                    if !*restoring {
                        restore_target.set(None);
                    }
                })
            };
            let on_confirm = {
                let restore_target = restore_target.clone();
                let restoring = restoring.clone();
                let selected = selected.clone();
                let error = error.clone();
                let refresh_tick = refresh_tick.clone();
                Callback::from(move |_| {
                    if *restoring {
                        return;
                    }
                    restoring.set(true);
                    let restore_target = restore_target.clone();
                    let restoring = restoring.clone();
                    let selected = selected.clone();
                    let error = error.clone();
                    let refresh_tick = refresh_tick.clone();
                    let ids = ids.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::restore_deleted_notes(&ids).await {
                            Ok(()) => {
                                let mut next_selected = (*selected).clone();
                                for id in &ids {
                                    next_selected.remove(id);
                                }
                                selected.set(next_selected);
                            }
                            Err(message) => error.set(Some(message)),
                        }
                        restore_target.set(None);
                        restoring.set(false);
                        refresh_tick.set((*refresh_tick).saturating_add(1));
                    });
                })
            };
            html! {
                <Modal title={modal_title} on_close={on_close}>
                    <p>{ message }</p>
                    <div class="app-modal-actions">
                        <button type="button" class="btn btn-ghost" onclick={on_cancel} disabled={*restoring}>
                            { "Cancel" }
                        </button>
                        <button type="button" class="btn btn-primary" onclick={on_confirm} disabled={*restoring}>
                            { if *restoring { "Restoring...".to_string() } else { confirm_label } }
                        </button>
                    </div>
                </Modal>
            }
        }
    };

    let delete_modal = match (*delete_target).clone() {
        None => html! {},
        Some(target) => {
            let ids = target.ids();
            let is_batch = target.is_batch();
            let (modal_title, message, confirm_label) = target.modal_copy();
            let on_close = {
                let delete_target = delete_target.clone();
                let deleting = deleting.clone();
                Callback::from(move |_: ()| {
                    if !*deleting {
                        delete_target.set(None);
                    }
                })
            };
            let on_cancel = {
                let delete_target = delete_target.clone();
                let deleting = deleting.clone();
                Callback::from(move |_| {
                    if !*deleting {
                        delete_target.set(None);
                    }
                })
            };
            let on_confirm = {
                let delete_target = delete_target.clone();
                let deleting = deleting.clone();
                let error = error.clone();
                let batch_delete_error = batch_delete_error.clone();
                let selected = selected.clone();
                let refresh_tick = refresh_tick.clone();
                Callback::from(move |_| {
                    if *deleting {
                        return;
                    }
                    deleting.set(true);
                    let delete_target = delete_target.clone();
                    let deleting = deleting.clone();
                    let error = error.clone();
                    let batch_delete_error = batch_delete_error.clone();
                    let selected = selected.clone();
                    let refresh_tick = refresh_tick.clone();
                    let ids = ids.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        if is_batch {
                            let mut failed = HashSet::new();
                            for id in &ids {
                                if api::permanently_delete_note(id).await.is_err() {
                                    failed.insert(id.clone());
                                }
                            }
                            selected.set(reconcile_delete_selection(&selected, &ids, &failed));
                            batch_delete_error
                                .set(batch_delete_failure_message(ids.len(), failed.len()));
                            delete_target.set(None);
                            deleting.set(false);
                            refresh_tick.set((*refresh_tick).saturating_add(1));
                        } else {
                            match api::permanently_delete_note(&ids[0]).await {
                                Ok(()) => {
                                    delete_target.set(None);
                                    refresh_tick.set((*refresh_tick).saturating_add(1));
                                }
                                Err(message) => error.set(Some(message)),
                            }
                            deleting.set(false);
                        }
                    });
                })
            };
            html! {
                <Modal title={modal_title} on_close={on_close}>
                    <p>{ message }</p>
                    <div class="app-modal-actions">
                        <button type="button" class="btn btn-ghost" onclick={on_cancel} disabled={*deleting}>
                            { "Cancel" }
                        </button>
                        <button type="button" class="btn btn-error" onclick={on_confirm} disabled={*deleting}>
                            { if *deleting { "Deleting...".to_string() } else { confirm_label } }
                        </button>
                    </div>
                </Modal>
            }
        }
    };

    html! {
        <section class="stack">
            <div class="page-head">
                <div>
                    <h2 class="page-title">{ "Trash" }</h2>
                    <p class="page-hint">{ "Notes in Trash are automatically and permanently deleted after 90 days." }</p>
                </div>
                <button type="button" class="btn btn-ghost btn-icon" title="Refresh" aria-label="Refresh Trash" onclick={on_refresh}>
                    { icons::refresh() }
                </button>
            </div>

            if let Some(message) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
            }

            if let Some(message) = &*batch_delete_error {
                <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading..." }</p>
            } else if notes.is_empty() {
                <p class="empty">{ "Trash is empty." }</p>
            } else {
                <div class="trash-toolbar">
                    <span class="trash-selection-count" role="status">
                        { format!("{} selected", selected.len()) }
                    </span>
                    <div class="row-actions">
                        <button
                            type="button"
                            class="btn btn-primary"
                            disabled={selected.is_empty() || *restoring || *deleting}
                            onclick={open_batch_restore}
                        >
                            { icons::restore() }
                            <span>{ "Restore selected" }</span>
                        </button>
                        <button
                            type="button"
                            class="btn btn-error"
                            disabled={selected.is_empty() || *restoring || *deleting}
                            onclick={open_batch_delete}
                        >
                            { icons::trash() }
                            <span>{ "Delete selected" }</span>
                        </button>
                    </div>
                </div>
                { trash_table(
                    &notes,
                    &selected,
                    &restore_target,
                    &delete_target,
                    &error,
                    &batch_delete_error,
                    &select_all_ref,
                    on_select_all,
                ) }
            }

            { restore_modal }
            { delete_modal }
        </section>
    }
}

fn trash_table(
    notes: &[DeletedNoteSummary],
    selected: &UseStateHandle<HashSet<String>>,
    restore_target: &UseStateHandle<Option<RestoreTarget>>,
    delete_target: &UseStateHandle<Option<DeleteTarget>>,
    error: &UseStateHandle<Option<String>>,
    batch_delete_error: &UseStateHandle<Option<String>>,
    select_all_ref: &NodeRef,
    on_select_all: Callback<Event>,
) -> Html {
    let all_selected = !notes.is_empty() && selected.len() == notes.len();
    html! {
        <div class="table-scroll">
            <table class="table note-table">
                <thead>
                    <tr>
                        <th class="col-select">
                            <input
                                ref={select_all_ref.clone()}
                                type="checkbox"
                                class="checkbox checkbox-primary"
                                checked={all_selected}
                                onchange={on_select_all}
                                aria-label="Select all trashed notes"
                            />
                        </th>
                        <th class="col-title">{ "Title" }</th>
                        <th>{ "Labels" }</th>
                        <th class="col-time">{ "Moved to Trash" }</th>
                        <th class="col-actions">{ "Actions" }</th>
                    </tr>
                </thead>
                <tbody>
                    { for notes.iter().map(|note| {
                        let on_select = {
                            let selected = selected.clone();
                            let id = note.id.clone();
                            Callback::from(move |event: Event| {
                                let input: HtmlInputElement = event.target_unchecked_into();
                                let mut next = (*selected).clone();
                                if input.checked() {
                                    next.insert(id.clone());
                                } else {
                                    next.remove(&id);
                                }
                                selected.set(next);
                            })
                        };
                        let on_restore = {
                            let restore_target = restore_target.clone();
                            let id = note.id.clone();
                            let title = note.title.clone();
                            Callback::from(move |_| {
                                restore_target.set(Some(RestoreTarget::Single {
                                    id: id.clone(),
                                    title: title.clone(),
                                }))
                            })
                        };
                        let on_delete = {
                            let delete_target = delete_target.clone();
                            let error = error.clone();
                            let batch_delete_error = batch_delete_error.clone();
                            let id = note.id.clone();
                            let title = note.title.clone();
                            Callback::from(move |_| {
                                error.set(None);
                                batch_delete_error.set(None);
                                delete_target.set(Some(DeleteTarget::Single {
                                    id: id.clone(),
                                    title: title.clone(),
                                }));
                            })
                        };
                        html! {
                            <tr key={note.id.clone()}>
                                <td class="col-select">
                                    <input
                                        type="checkbox"
                                        class="checkbox checkbox-primary"
                                        checked={selected.contains(&note.id)}
                                        onchange={on_select}
                                        aria-label={format!("Select {}", note.title)}
                                    />
                                </td>
                                <td class="col-title trash-note-title">{ note.title.clone() }</td>
                                <td>
                                    <div class="applied-labels">
                                        { for note.labels.iter().map(|(key, value)| html! {
                                            <span class="chip chip-primary note-label-chip" title={format!("{key}: {value}")}>
                                                { format!("{key}: {value}") }
                                            </span>
                                        }) }
                                    </div>
                                </td>
                                <td class="col-time">{ format_timestamp(note.deleted_at) }</td>
                                <td class="col-actions">
                                    <div class="row-actions">
                                        <button
                                            type="button"
                                            class="btn btn-ghost btn-icon"
                                            title="Restore"
                                            aria-label={format!("Restore {}", note.title)}
                                            onclick={on_restore}
                                        >
                                            { icons::restore() }
                                            <span class="sr-only">{ "Restore" }</span>
                                        </button>
                                        <button
                                            type="button"
                                            class="btn btn-ghost btn-icon icon-danger"
                                            title="Delete permanently"
                                            aria-label={format!("Delete {} permanently", note.title)}
                                            onclick={on_delete}
                                        >
                                            { icons::trash() }
                                            <span class="sr-only">{ "Delete permanently" }</span>
                                        </button>
                                    </div>
                                </td>
                            </tr>
                        }
                    }) }
                </tbody>
            </table>
        </div>
    }
}

fn format_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        batch_delete_failure_message, reconcile_delete_selection, selected_note_ids, DeleteTarget,
        DeletedNoteSummary,
    };
    use std::collections::HashSet;

    fn note(id: &str) -> DeletedNoteSummary {
        DeletedNoteSummary {
            id: id.to_string(),
            title: format!("Note {id}"),
            labels: vec![],
            created_at: 0,
            updated_at: 0,
            deleted_at: 0,
        }
    }

    #[test]
    fn selected_note_ids_follows_visible_note_order() {
        let notes = vec![note("third"), note("second"), note("first")];
        let selected = HashSet::from(["first".to_string(), "third".to_string()]);

        assert_eq!(selected_note_ids(&notes, &selected), ["third", "first"]);
    }

    #[test]
    fn reconcile_delete_selection_keeps_failed_and_new_selection() {
        let selected = HashSet::from([
            "deleted".to_string(),
            "failed".to_string(),
            "new-selection".to_string(),
        ]);
        let attempted = vec!["deleted".to_string(), "failed".to_string()];
        let failed = HashSet::from(["failed".to_string()]);

        assert_eq!(
            reconcile_delete_selection(&selected, &attempted, &failed),
            HashSet::from(["failed".to_string(), "new-selection".to_string()])
        );
    }

    #[test]
    fn batch_delete_failure_message_reports_failures() {
        assert_eq!(batch_delete_failure_message(5, 0), None);
        assert_eq!(
            batch_delete_failure_message(5, 2),
            Some(
                "Deleted 3 of 5 selected notes; 2 failed. Failed notes still in Trash remain selected."
                    .to_string()
            )
        );
    }

    #[test]
    fn batch_delete_target_has_irreversible_count_copy() {
        let target = DeleteTarget::Batch {
            ids: vec!["one".to_string(), "two".to_string()],
        };

        assert_eq!(target.ids(), ["one", "two"]);
        assert_eq!(
            target.modal_copy(),
            (
                "Delete selected notes permanently".to_string(),
                "Permanently delete 2 selected notes? This cannot be undone.".to_string(),
                "Delete 2 notes".to_string(),
            )
        );
    }
}
