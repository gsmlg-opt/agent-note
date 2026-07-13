use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;
use yew::virtual_dom::AttrValue;
use yew_duskmoon::{Button, Card, Chip, MarkdownInput};

use crate::state::{LabelKey, NoteAttachment};

type NoteEditorSubmit = (String, String, Vec<(String, String)>, Vec<NoteAttachment>);

#[derive(Properties, PartialEq)]
pub struct NoteEditorProps {
    /// Registered label keys, shown in the label picker so the user can see what each label is for
    /// (docs/design.md §7).
    pub available_labels: Vec<LabelKey>,
    /// Emits (title, content, labels, attachments).
    pub on_submit: Callback<NoteEditorSubmit>,
    /// Prefill values (used when editing an existing note). Read once at mount.
    #[prop_or_default]
    pub initial_title: String,
    #[prop_or_default]
    pub initial_content: String,
    #[prop_or_default]
    pub initial_labels: Vec<(String, String)>,
    #[prop_or_default]
    pub initial_attachments: Vec<NoteAttachment>,
    #[prop_or_else(|| "New note".to_string())]
    pub card_title: String,
    #[prop_or_else(|| "Save note".to_string())]
    pub submit_label: String,
    #[prop_or_default]
    pub submitting: bool,
}

#[function_component(NoteEditor)]
pub fn note_editor(props: &NoteEditorProps) -> Html {
    let title = use_state(|| props.initial_title.clone());
    let content = use_state(|| props.initial_content.clone());
    // Applied labels the user has added, as (key, value) pairs.
    let labels = use_state(|| props.initial_labels.clone());
    let attachments = use_state(|| props.initial_attachments.clone());
    let submit_debounce = use_state(|| false);
    let submit_locked = use_mut_ref(|| false);
    // The currently-selected label key and value being staged in the picker.
    let picker_key = use_state(String::new);
    let picker_value = use_state(String::new);
    let attachment_id = use_state(String::new);
    let attachment_path = use_state(|| "./".to_string());
    let attachment_mime = use_state(|| "text/plain".to_string());
    let attachment_description = use_state(String::new);
    let attachment_content = use_state(String::new);

    let on_title_input = {
        let title = title.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            title.set(input.value());
        })
    };

    let on_content_change = {
        let content = content.clone();
        Callback::from(move |next: AttrValue| {
            content.set(next.to_string());
        })
    };

    let on_key_input = {
        let picker_key = picker_key.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            picker_key.set(input.value());
        })
    };

    let on_value_input = {
        let picker_value = picker_value.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            picker_value.set(input.value());
        })
    };

    let on_attachment_id_input = {
        let attachment_id = attachment_id.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            attachment_id.set(input.value());
        })
    };

    let on_attachment_path_input = {
        let attachment_path = attachment_path.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            attachment_path.set(input.value());
        })
    };

    let on_attachment_mime_input = {
        let attachment_mime = attachment_mime.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            attachment_mime.set(input.value());
        })
    };

    let on_attachment_description_input = {
        let attachment_description = attachment_description.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            attachment_description.set(input.value());
        })
    };

    let on_attachment_content_input = {
        let attachment_content = attachment_content.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlTextAreaElement = e.target_unchecked_into();
            attachment_content.set(input.value());
        })
    };

    let on_add_label = {
        let labels = labels.clone();
        let picker_key = picker_key.clone();
        let picker_value = picker_value.clone();
        Callback::from(move |_| {
            let key = (*picker_key).clone();
            let value = (*picker_value).clone();
            if key.is_empty() {
                return;
            }
            let mut next = (*labels).clone();
            next.push((key, value));
            labels.set(next);
            picker_value.set(String::new());
        })
    };

    let on_add_attachment = {
        let attachments = attachments.clone();
        let attachment_id = attachment_id.clone();
        let attachment_path = attachment_path.clone();
        let attachment_mime = attachment_mime.clone();
        let attachment_description = attachment_description.clone();
        let attachment_content = attachment_content.clone();
        Callback::from(move |_| {
            let id = attachment_id.trim().to_string();
            let path = attachment_path.trim().to_string();
            let mime = attachment_mime.trim().to_string();
            if id.is_empty() || path.is_empty() || mime.is_empty() {
                return;
            }
            let mut next = (*attachments).clone();
            next.push(NoteAttachment {
                id,
                path,
                mime,
                description: (*attachment_description).clone(),
                content: attachment_content.to_string(),
            });
            attachments.set(next);
            attachment_id.set(String::new());
            attachment_path.set("./".to_string());
            attachment_mime.set("text/plain".to_string());
            attachment_description.set(String::new());
            attachment_content.set(String::new());
        })
    };

    let on_submit = {
        let on_submit = props.on_submit.clone();
        let title = title.clone();
        let content = content.clone();
        let labels = labels.clone();
        let attachments = attachments.clone();
        let submit_debounce = submit_debounce.clone();
        let submit_locked = submit_locked.clone();
        let submitting = props.submitting;
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            if submitting || *submit_locked.borrow() {
                return;
            }
            *submit_locked.borrow_mut() = true;
            submit_debounce.set(true);
            on_submit.emit((
                (*title).clone(),
                (*content).clone(),
                (*labels).clone(),
                (*attachments).clone(),
            ));
        })
    };

    {
        let submit_debounce = submit_debounce.clone();
        let submit_locked = submit_locked.clone();
        let submitting = props.submitting;
        use_effect_with(submitting, move |is_submitting| {
            if !*is_submitting {
                *submit_locked.borrow_mut() = false;
                submit_debounce.set(false);
            }
            || ()
        });
    }

    let submit_disabled = props.submitting || *submit_debounce;

    // Description for the currently-selected key, shown as hint text.
    let selected_hint = props
        .available_labels
        .iter()
        .find(|label| label.key.as_str() == (*picker_key).as_str())
        .map(|label| {
            if label.description.is_empty() {
                label.value_type.clone()
            } else {
                format!("{} - {}", label.description, label.value_type)
            }
        });
    let selected_value_type = props
        .available_labels
        .iter()
        .find(|label| label.key.as_str() == (*picker_key).as_str())
        .map(|label| label.value_type.as_str())
        .unwrap_or("text");

    html! {
        <Card title={Some(html! { <span>{ props.card_title.clone() }</span> })} classes={classes!("note-editor")}>
            <form class="stack" onsubmit={on_submit}>
                <div class="note-page-actions">
                    <Button
                        variant={Some("primary".to_string())}
                        disabled={submit_disabled}
                        loading={submit_disabled}
                    >
                        <span>{
                            if submit_disabled {
                                "Saving...".to_string()
                            } else {
                                props.submit_label.clone()
                            }
                        }</span>
                    </Button>
                </div>
                <label class="field">
                    <span>{ "Title" }</span>
                    <input
                        class="input input-primary"
                        type="text"
                        value={(*title).clone()}
                        oninput={on_title_input}
                    />
                </label>

                <fieldset class="label-picker">
                    <legend>{ "Labels" }</legend>
                    <div class="label-picker-row">
                        // Type a key or pick a registered one — unknown keys are created on save.
                        <input
                            class="input label-key-input"
                            type="text"
                            list="label-key-options"
                            placeholder="key"
                            value={(*picker_key).clone()}
                            oninput={on_key_input}
                        />
                        <datalist id="label-key-options">
                            { for props.available_labels.iter().map(|label| html! {
                                <option value={label.key.clone()} />
                            }) }
                        </datalist>
                        <span class="label-eq">{ "=" }</span>
                        <input
                            class="input label-value-input"
                            type={label_value_input_type(selected_value_type)}
                            placeholder="value"
                            value={(*picker_value).clone()}
                            oninput={on_value_input}
                        />
                        // Native <button type="button"> so it never submits the form
                        // (yew-duskmoon's Button renders a submit-type <button>).
                        <button type="button" class="btn btn-outline" onclick={on_add_label}>
                            { "Add label" }
                        </button>
                    </div>
                    if let Some(hint) = selected_hint {
                        <p class="label-hint">{ hint }</p>
                    }
                    <div class="applied-labels">
                        { for labels.iter().map(|(key, value)| {
                            html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{key}: {value}") }</span>
                                </Chip>
                            }
                        }) }
                    </div>
                </fieldset>

                <fieldset class="attachment-picker">
                    <legend>{ "Attachments" }</legend>
                    <div class="attachment-picker-grid">
                        <input
                            class="input"
                            type="text"
                            placeholder="id"
                            value={(*attachment_id).clone()}
                            oninput={on_attachment_id_input}
                        />
                        <input
                            class="input"
                            type="text"
                            placeholder="./meta.json"
                            value={(*attachment_path).clone()}
                            oninput={on_attachment_path_input}
                        />
                        <input
                            class="input"
                            type="text"
                            placeholder="application/json"
                            value={(*attachment_mime).clone()}
                            oninput={on_attachment_mime_input}
                        />
                        <input
                            class="input attachment-description-input"
                            type="text"
                            placeholder="description"
                            value={(*attachment_description).clone()}
                            oninput={on_attachment_description_input}
                        />
                        <textarea
                            class="input attachment-content-input"
                            placeholder="attachment content"
                            value={(*attachment_content).clone()}
                            oninput={on_attachment_content_input}
                        />
                        <button type="button" class="btn btn-outline" onclick={on_add_attachment}>
                            { "Add attachment" }
                        </button>
                    </div>
                    <div class="attachment-list">
                        { for attachments.iter().enumerate().map(|(idx, attachment)| {
                            let on_id_input = attachment_input_callback(&attachments, idx, AttachmentField::Id);
                            let on_path_input = attachment_input_callback(&attachments, idx, AttachmentField::Path);
                            let on_mime_input = attachment_input_callback(&attachments, idx, AttachmentField::Mime);
                            let on_description_input = attachment_input_callback(&attachments, idx, AttachmentField::Description);
                            let on_content_input = {
                                let attachments = attachments.clone();
                                Callback::from(move |e: InputEvent| {
                                    let input: HtmlTextAreaElement = e.target_unchecked_into();
                                    let mut next = (*attachments).clone();
                                    if let Some(attachment) = next.get_mut(idx) {
                                        attachment.content = input.value();
                                    }
                                    attachments.set(next);
                                })
                            };
                            let on_remove = {
                                let attachments = attachments.clone();
                                Callback::from(move |_| {
                                    let mut next = (*attachments).clone();
                                    if idx < next.len() {
                                        next.remove(idx);
                                    }
                                    attachments.set(next);
                                })
                            };
                            html! {
                                <div class="attachment-item" key={attachment.id.clone()}>
                                    <div class="attachment-item-grid">
                                        <input class="input" type="text" value={attachment.id.clone()} oninput={on_id_input} />
                                        <input class="input" type="text" value={attachment.path.clone()} oninput={on_path_input} />
                                        <input class="input" type="text" value={attachment.mime.clone()} oninput={on_mime_input} />
                                        <input class="input" type="text" value={attachment.description.clone()} oninput={on_description_input} />
                                        <button type="button" class="btn btn-outline" onclick={on_remove}>{ "Remove" }</button>
                                    </div>
                                    <textarea
                                        class="input attachment-content-input"
                                        value={attachment.content.clone()}
                                        oninput={on_content_input}
                                    />
                                </div>
                            }
                        }) }
                    </div>
                </fieldset>

                <div class="field">
                    <span>{ "Content" }</span>
                    <MarkdownInput
                        class="note-content-input"
                        variant={Some("primary".to_string())}
                        value={Some(AttrValue::from((*content).clone()))}
                        placeholder="Write markdown..."
                        on_change={on_content_change}
                    />
                </div>
            </form>
        </Card>
    }
}

#[derive(Clone, Copy)]
enum AttachmentField {
    Id,
    Path,
    Mime,
    Description,
}

fn attachment_input_callback(
    attachments: &UseStateHandle<Vec<NoteAttachment>>,
    idx: usize,
    field: AttachmentField,
) -> Callback<InputEvent> {
    let attachments = attachments.clone();
    Callback::from(move |e: InputEvent| {
        let input: HtmlInputElement = e.target_unchecked_into();
        let mut next = (*attachments).clone();
        if let Some(attachment) = next.get_mut(idx) {
            match field {
                AttachmentField::Id => attachment.id = input.value(),
                AttachmentField::Path => attachment.path = input.value(),
                AttachmentField::Mime => attachment.mime = input.value(),
                AttachmentField::Description => attachment.description = input.value(),
            }
        }
        attachments.set(next);
    })
}

fn label_value_input_type(value_type: &str) -> &'static str {
    match value_type {
        "number" => "number",
        "date" => "date",
        "datetime" => "datetime-local",
        "time" => "time",
        _ => "text",
    }
}
