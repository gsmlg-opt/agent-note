use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::{Alert, Button};

use crate::api;
use crate::components::Modal;
use crate::state::LabelKey;

#[derive(Clone, PartialEq)]
enum ModalState {
    Closed,
    Create,
    Edit(String), // holds the (immutable) key being edited
    Delete(String),
}

/// Labels management: list the catalog, create/edit via a modal form, delete via a confirm modal.
#[function_component(LabelsPage)]
pub fn labels_page() -> Html {
    let labels = use_state(Vec::<LabelKey>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
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
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_labels().await {
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
        Callback::from(move |_: ()| modal.set(ModalState::Closed))
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
                        { modal_actions(&close, html! { <Button variant={Some("primary".to_string())}>{ "Create label" }</Button> }) }
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
                        { modal_actions(&close, html! { <Button variant={Some("primary".to_string())}>{ "Save changes" }</Button> }) }
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
    };

    html! {
        <section class="stack">
            <div class="page-head">
                <h2 class="page-title">{ "Labels" }</h2>
                <Button variant={Some("primary".to_string())} onclick={open_create}>{ "New label" }</Button>
            </div>
            <p class="page-hint">{ "Labels are reusable key = value tags. Descriptions explain what each key is for." }</p>

            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if labels.is_empty() {
                <p class="empty">{ "No labels yet. Create one, or just type a new key when writing a note." }</p>
            } else {
                <ul class="label-list">
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
                            <li class="label-row" key={label.key.clone()}>
                                <div class="label-row-main">
                                    <span class="label-key">{ label.key.clone() }</span>
                                    <span class="label-type">{ label.value_type.clone() }</span>
                                    <span class="label-desc">{
                                        if label.description.is_empty() { "—".to_string() } else { label.description.clone() }
                                    }</span>
                                </div>
                                <div class="label-row-actions">
                                    <button type="button" class="btn btn-ghost btn-sm" onclick={on_edit}>{ "Edit" }</button>
                                    <button type="button" class="btn btn-ghost btn-sm" onclick={on_delete}>{ "Delete" }</button>
                                </div>
                            </li>
                        }
                    }) }
                </ul>
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
