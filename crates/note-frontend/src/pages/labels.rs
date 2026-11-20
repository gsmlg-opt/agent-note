use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::{Alert, Button, NativeButtonType};

use crate::api;
use crate::components::Modal;

#[derive(Clone, PartialEq)]
enum ModalState {
    Closed,
    Create,
    Edit(String), // holds the (immutable) key being edited
    Delete(String),
    ClearUnused,
}

/// Manage label keys and see how many active notes use each key.
#[function_component(LabelsPage)]
pub fn labels_page() -> Html {
    let labels = use_state(Vec::<api::LabelUsage>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let cleanup_notice = use_state(|| None::<String>);
    let clearing = use_state(|| false);
    let modal = use_state(|| ModalState::Closed);
    // Shared form fields for the create/edit modal.
    let form_key = use_state(String::new);
    let form_desc = use_state(String::new);
    let form_type = use_state(|| "text".to_string());

    // Reloads the catalog; called on mount and after every mutation.
    let reload = {
        let labels = labels.clone();
        let loading = loading.clone();
        let error = error.clone();
        Callback::from(move |_: ()| {
            let labels = labels.clone();
            let loading = loading.clone();
            let error = error.clone();
            loading.set(true);
            error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_label_usage().await {
                    Ok(l) => labels.set(l),
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
        })
    };

    {
        let reload = reload.clone();
        use_effect_with((), move |_| {
            reload.emit(());
            || ()
        });
    }

    let close = {
        let modal = modal.clone();
        let clearing = clearing.clone();
        Callback::from(move |_: ()| {
            if !*clearing {
                modal.set(ModalState::Closed);
            }
        })
    };

    let open_clear_unused = {
        let modal = modal.clone();
        Callback::from(move |_| modal.set(ModalState::ClearUnused))
    };

    let open_create = {
        let modal = modal.clone();
        let form_key = form_key.clone();
        let form_desc = form_desc.clone();
        let form_type = form_type.clone();
        Callback::from(move |_| {
            form_key.set(String::new());
            form_desc.set(String::new());
            form_type.set("text".to_string());
            modal.set(ModalState::Create);
        })
    };

    let on_key_input = {
        let form_key = form_key.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            form_key.set(input.value());
        })
    };
    let on_desc_input = {
        let form_desc = form_desc.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            form_desc.set(input.value());
        })
    };
    let on_type_change = {
        let form_type = form_type.clone();
        Callback::from(move |e: Event| {
            let select: HtmlSelectElement = e.target_unchecked_into();
            form_type.set(select.value());
        })
    };

    let modal_view = match &*modal {
        ModalState::Closed => html! {},
        ModalState::Create => {
            let on_submit = {
                let form_key = form_key.clone();
                let form_desc = form_desc.clone();
                let form_type = form_type.clone();
                let modal = modal.clone();
                let reload = reload.clone();
                let error = error.clone();
                Callback::from(move |e: SubmitEvent| {
                    e.prevent_default();
                    let key = (*form_key).clone();
                    let desc = (*form_desc).clone();
                    let value_type = (*form_type).clone();
                    let modal = modal.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::create_label(&key, &desc, &value_type).await {
                            Ok(()) => {
                                modal.set(ModalState::Closed);
                                reload.emit(());
                            }
                            Err(e) => error.set(Some(e)),
                        }
                    });
                })
            };
            html! {
                <Modal title="New label" on_close={close.clone()}>
                    <form class="stack" onsubmit={on_submit}>
                        <label class="field">
                            <span>{ "Key" }</span>
                            <input class="input input-primary" type="text" placeholder="e.g. project"
                                value={(*form_key).clone()} oninput={on_key_input.clone()} />
                        </label>
                        <label class="field">
                            <span>{ "Description" }</span>
                            <input class="input" type="text" placeholder="What is this label for?"
                                value={(*form_desc).clone()} oninput={on_desc_input.clone()} />
                        </label>
                        <label class="field">
                            <span>{ "Type" }</span>
                            { value_type_select((*form_type).as_str(), on_type_change.clone()) }
                        </label>
                        { modal_actions(&close, html! { <Button native_type={NativeButtonType::Submit} variant={Some("primary".to_string())}>{ "Create label" }</Button> }) }
                    </form>
                </Modal>
            }
        }
        ModalState::Edit(key) => {
            let key = key.clone();
            let on_submit = {
                let key = key.clone();
                let form_desc = form_desc.clone();
                let form_type = form_type.clone();
                let modal = modal.clone();
                let reload = reload.clone();
                let error = error.clone();
                Callback::from(move |e: SubmitEvent| {
                    e.prevent_default();
                    let key = key.clone();
                    let desc = (*form_desc).clone();
                    let value_type = (*form_type).clone();
                    let modal = modal.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::update_label(&key, &desc, &value_type).await {
                            Ok(()) => {
                                modal.set(ModalState::Closed);
                                reload.emit(());
                            }
                            Err(e) => error.set(Some(e)),
                        }
                    });
                })
            };
            html! {
                <Modal title="Edit label" on_close={close.clone()}>
                    <form class="stack" onsubmit={on_submit}>
                        <label class="field">
                            <span>{ "Key" }</span>
                            <input class="input" type="text" value={key.clone()} disabled=true />
                        </label>
                        <label class="field">
                            <span>{ "Description" }</span>
                            <input class="input input-primary" type="text"
                                value={(*form_desc).clone()} oninput={on_desc_input.clone()} />
                        </label>
                        <label class="field">
                            <span>{ "Type" }</span>
                            { value_type_select((*form_type).as_str(), on_type_change.clone()) }
                        </label>
                        { modal_actions(&close, html! { <Button native_type={NativeButtonType::Submit} variant={Some("primary".to_string())}>{ "Save changes" }</Button> }) }
                    </form>
                </Modal>
            }
        }
        ModalState::Delete(key) => {
            let key = key.clone();
            let on_confirm = {
                let key = key.clone();
                let modal = modal.clone();
                let reload = reload.clone();
                let error = error.clone();
                Callback::from(move |_| {
                    let key = key.clone();
                    let modal = modal.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::delete_label(&key).await {
                            Ok(()) => {
                                modal.set(ModalState::Closed);
                                reload.emit(());
                            }
                            Err(e) => error.set(Some(e)),
                        }
                    });
                })
            };
            html! {
                <Modal title="Delete label" on_close={close.clone()}>
                    <p>{ format!("Delete the label \u{201c}{key}\u{201d}? It will be removed from every note that uses it.") }</p>
                    { modal_actions(&close, html! {
                        <button type="button" class="btn btn-error" onclick={on_confirm}>{ "Delete label" }</button>
                    }) }
                </Modal>
            }
        }
        ModalState::ClearUnused => {
            let on_confirm = {
                let modal = modal.clone();
                let reload = reload.clone();
                let error = error.clone();
                let cleanup_notice = cleanup_notice.clone();
                let clearing = clearing.clone();
                Callback::from(move |_| {
                    clearing.set(true);
                    error.set(None);
                    cleanup_notice.set(None);
                    let modal = modal.clone();
                    let reload = reload.clone();
                    let error = error.clone();
                    let cleanup_notice = cleanup_notice.clone();
                    let clearing = clearing.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::clear_unused_labels().await {
                            Ok(count) => {
                                let message = match count {
                                    0 => "No unused labels to clear.".to_string(),
                                    1 => "Cleared 1 unused label.".to_string(),
                                    _ => format!("Cleared {count} unused labels."),
                                };
                                cleanup_notice.set(Some(message));
                                reload.emit(());
                            }
                            Err(err) => error.set(Some(err)),
                        }
                        clearing.set(false);
                        modal.set(ModalState::Closed);
                    });
                })
            };

            html! {
                <Modal title="Clear unused labels" on_close={close.clone()}>
                    <p>{ "Remove label keys with no notes? Labels used by notes in Trash or configured as dashboard categories are kept." }</p>
                    { modal_actions(&close, html! {
                        <button type="button" class="btn btn-error" disabled={*clearing} onclick={on_confirm}>
                            { if *clearing { "Clearing…" } else { "Clear unused labels" } }
                        </button>
                    }) }
                </Modal>
            }
        }
    };

    html! {
        <section class="stack">
            <div class="page-head">
                <h2 class="page-title">{ "Labels" }</h2>
                <div class="label-page-actions">
                    <button type="button" class="btn btn-outline" onclick={open_clear_unused}
                        disabled={*loading || *clearing || !labels.iter().any(|label| label.note_count == Some(0))}>
                        { "Clear unused labels" }
                    </button>
                    <Button variant={Some("primary".to_string())} onclick={open_create}>{ "New label" }</Button>
                </div>
            </div>
            <p class="page-hint">{ "Labels are reusable key = value tags. Descriptions explain what each key is for." }</p>

            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }

            if let Some(message) = &*cleanup_notice {
                <Alert variant={Some("success".to_string())}><span role="status">{ message.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if error.is_none() || !labels.is_empty() {
                <div class="table-scroll">
                    <table class="table table-compact label-table" aria-label="Label keys">
                        <thead>
                            <tr>
                                <th scope="col">{ "Key" }</th>
                                <th scope="col">{ "Type" }</th>
                                <th scope="col">{ "Description" }</th>
                                <th scope="col" class="label-note-count">{ "Active notes" }</th>
                                <th scope="col">{ "Actions" }</th>
                            </tr>
                        </thead>
                        <tbody>
                            if labels.is_empty() {
                                <tr><td colspan="5">{ "No labels yet. Create one, or type a new key when writing a note." }</td></tr>
                            }
                            { for labels.iter().map(|label| {
                                let key = label.key.clone();
                                let desc = label.description.clone();
                                let on_edit = {
                                    let modal = modal.clone();
                                    let form_desc = form_desc.clone();
                                    let form_type = form_type.clone();
                                    let key = key.clone();
                                    let desc = desc.clone();
                                    let value_type = label.value_type.clone();
                                    Callback::from(move |_| {
                                        form_desc.set(desc.clone());
                                        form_type.set(value_type.clone());
                                        modal.set(ModalState::Edit(key.clone()));
                                    })
                                };
                                let on_delete = {
                                    let modal = modal.clone();
                                    let key = key.clone();
                                    Callback::from(move |_| modal.set(ModalState::Delete(key.clone())))
                                };
                                html! {
                                    <tr class="label-row" key={label.key.clone()}>
                                        <th scope="row" class="label-key">{ label.key.clone() }</th>
                                        <td><span class="label-type">{ label.value_type.clone() }</span></td>
                                        <td class="label-desc">{
                                            if label.description.is_empty() { "—".to_string() } else { label.description.clone() }
                                        }</td>
                                        <td class="label-note-count">{
                                            label.note_count.map_or_else(|| "—".to_string(), |count| count.to_string())
                                        }</td>
                                        <td>
                                            <div class="label-row-actions">
                                                <button type="button" class="btn btn-ghost btn-sm" onclick={on_edit}>{ "Edit" }</button>
                                                <button type="button" class="btn btn-ghost btn-sm" onclick={on_delete}>{ "Delete" }</button>
                                            </div>
                                        </td>
                                    </tr>
                                }
                            }) }
                        </tbody>
                    </table>
                </div>
            }

            { modal_view }
        </section>
    }
}

/// Cancel + primary action row for a modal. `close` is the modal's `Callback<()>`; the Cancel
/// button adapts it to the button's `MouseEvent` click.
fn modal_actions(close: &Callback<()>, primary: Html) -> Html {
    let on_cancel = {
        let close = close.clone();
        Callback::from(move |_: MouseEvent| close.emit(()))
    };
    html! {
        <div class="app-modal-actions">
            <button type="button" class="btn btn-ghost" onclick={on_cancel}>{ "Cancel" }</button>
            { primary }
        </div>
    }
}

fn value_type_select(selected: &str, onchange: Callback<Event>) -> Html {
    html! {
        <select class="input" {onchange} value={selected.to_string()}>
            { for ["text", "number", "version", "date", "datetime", "time"].iter().map(|value_type| html! {
                <option value={(*value_type).to_string()}>{ *value_type }</option>
            }) }
        </select>
    }
}
