use base64::{engine::general_purpose::STANDARD, Engine as _};
use gloo_file::futures::read_as_bytes;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;
use yew::virtual_dom::AttrValue;
use yew_duskmoon::{Button, Card, Chip, MarkdownInput};

use crate::{
    api::attachment_url,
    state::{AttachmentContent, LabelKey, NoteAttachment},
};

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
    #[prop_or_default]
    pub attachment_base: Option<String>,
    #[prop_or_else(|| "New note".to_string())]
    pub card_title: String,
    #[prop_or_else(|| "Save note".to_string())]
    pub submit_label: String,
    #[prop_or_default]
    pub submitting: bool,
    /// Prevent submission without showing an in-flight loading state.
    #[prop_or_default]
    pub submit_blocked: bool,
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
    let editing_label_index = use_state(|| None::<usize>);
    let attachment_path = use_state(String::new);
    let attachment_mime = use_state(|| "text/plain".to_string());
    let attachment_description = use_state(String::new);
    let attachment_content = use_state(|| AttachmentContent::Text(String::new()));
    let attachment_file_reading = use_state(|| false);
    let attachment_file_error = use_state(|| None::<String>);

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
            let input: HtmlTextAreaElement = e.target_unchecked_into();
            attachment_description.set(input.value());
        })
    };

    let on_attachment_content_input = {
        let attachment_content = attachment_content.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlTextAreaElement = e.target_unchecked_into();
            attachment_content.set(AttachmentContent::Text(input.value()));
        })
    };

    let on_attachment_file_change = {
        let attachment_path = attachment_path.clone();
        let attachment_mime = attachment_mime.clone();
        let attachment_content = attachment_content.clone();
        let attachment_file_reading = attachment_file_reading.clone();
        let attachment_file_error = attachment_file_error.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            let Some(file) = input.files().and_then(|files| files.get(0)) else {
                return;
            };
            input.set_value("");

            attachment_path.set(attachment_path_for_file(&file.name()));
            attachment_mime.set(attachment_mime_for_file(&file.type_()));
            attachment_file_error.set(None);
            attachment_file_reading.set(true);

            let file = gloo_file::Blob::from(file);
            let attachment_content = attachment_content.clone();
            let attachment_file_reading = attachment_file_reading.clone();
            let attachment_file_error = attachment_file_error.clone();
            spawn_local(async move {
                match read_as_bytes(&file).await {
                    Ok(bytes) => attachment_content.set(attachment_content_for_bytes(bytes)),
                    Err(error) => {
                        attachment_file_error.set(Some(format!("Unable to read file: {error}")))
                    }
                }
                attachment_file_reading.set(false);
            });
        })
    };

    let on_apply_label = {
        let labels = labels.clone();
        let picker_key = picker_key.clone();
        let picker_value = picker_value.clone();
        let editing_label_index = editing_label_index.clone();
        Callback::from(move |_| {
            let key = (*picker_key).clone();
            let value = (*picker_value).clone();
            if key.is_empty() {
                return;
            }
            let mut next = (*labels).clone();
            if let Some(index) = *editing_label_index {
                if !replace_label_at(&mut next, index, key, value) {
                    return;
                }
                editing_label_index.set(None);
            } else {
                next.push((key, value));
            }
            labels.set(next);
            picker_value.set(String::new());
        })
    };

    let on_add_attachment = {
        let attachments = attachments.clone();
        let attachment_path = attachment_path.clone();
        let attachment_mime = attachment_mime.clone();
        let attachment_description = attachment_description.clone();
        let attachment_content = attachment_content.clone();
        let attachment_file_error = attachment_file_error.clone();
        Callback::from(move |_| {
            let path = attachment_path.trim().to_string();
            let mime = attachment_mime.trim().to_string();
            if path.is_empty() || path == "." || path == "./" || mime.is_empty() {
                return;
            }
            let mut next = (*attachments).clone();
            let id = next_attachment_id(&next);
            next.push(NoteAttachment {
                id,
                path,
                mime,
                description: (*attachment_description).clone(),
                content: (*attachment_content).clone(),
            });
            attachments.set(next);
            attachment_path.set(String::new());
            attachment_mime.set("text/plain".to_string());
            attachment_description.set(String::new());
            attachment_content.set(AttachmentContent::Text(String::new()));
            attachment_file_error.set(None);
        })
    };

    let on_submit = {
        let on_submit = props.on_submit.clone();
        let title = title.clone();
        let content = content.clone();
        let labels = labels.clone();
        let attachments = attachments.clone();
        let attachment_file_reading = attachment_file_reading.clone();
        let submit_debounce = submit_debounce.clone();
        let submit_locked = submit_locked.clone();
        let submitting = props.submitting || props.submit_blocked;
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            if submitting || *attachment_file_reading || *submit_locked.borrow() {
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

    let submit_disabled =
        props.submitting || props.submit_blocked || *submit_debounce || *attachment_file_reading;
    let attachment_add_disabled = *attachment_file_reading
        || attachment_path.trim().is_empty()
        || attachment_path.trim() == "."
        || attachment_path.trim() == "./"
        || attachment_mime.trim().is_empty();

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
                        loading={props.submitting || *submit_debounce || *attachment_file_reading}
                    >
                        <span>{
                            if props.submitting || *submit_debounce {
                                "Saving...".to_string()
                            } else if *attachment_file_reading {
                                "Reading file...".to_string()
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
                        <button type="button" class="btn btn-outline" onclick={on_apply_label}>
                            { if editing_label_index.is_some() { "Update label" } else { "Add label" } }
                        </button>
                    </div>
                    if let Some(hint) = selected_hint {
                        <p class="label-hint">{ hint }</p>
                    }
                    <div class="applied-labels">
                        { for labels.iter().enumerate().map(|(index, (key, value))| {
                            let on_edit = {
                                let picker_key = picker_key.clone();
                                let picker_value = picker_value.clone();
                                let editing_label_index = editing_label_index.clone();
                                let key = key.clone();
                                let value = value.clone();
                                Callback::from(move |_| {
                                    picker_key.set(key.clone());
                                    picker_value.set(value.clone());
                                    editing_label_index.set(Some(index));
                                })
                            };
                            let on_remove = {
                                let labels = labels.clone();
                                let picker_key = picker_key.clone();
                                let picker_value = picker_value.clone();
                                let editing_label_index = editing_label_index.clone();
                                Callback::from(move |_| {
                                    let mut next = (*labels).clone();
                                    if !remove_label_at(&mut next, index) {
                                        return;
                                    }
                                    let previous_editing_index = *editing_label_index;
                                    let next_editing_index = editing_index_after_removal(
                                        previous_editing_index,
                                        index,
                                    );
                                    labels.set(next);
                                    editing_label_index.set(next_editing_index);
                                    if previous_editing_index == Some(index) {
                                        picker_key.set(String::new());
                                        picker_value.set(String::new());
                                    }
                                })
                            };
                            html! {
                                <Chip key={index} variant={Some("primary".to_string())}>
                                    <span class="applied-label-content">
                                        <span>{ format!("{key}: {value}") }</span>
                                        <span class="applied-label-actions">
                                            <button
                                                type="button"
                                                class="applied-label-action"
                                                aria-label={format!("Edit label {key}")}
                                                onclick={on_edit}
                                            >
                                                { "Edit" }
                                            </button>
                                            <button
                                                type="button"
                                                class="applied-label-action"
                                                aria-label={format!("Remove label {key}")}
                                                onclick={on_remove}
                                            >
                                                { "Remove" }
                                            </button>
                                        </span>
                                    </span>
                                </Chip>
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

                <fieldset class="attachment-picker">
                    <legend>{ "Attachments" }</legend>
                    <div class="attachment-picker-grid">
                        <label class="field attachment-path-field">
                            <span>{ "Path" }</span>
                            <input
                                class="input"
                                type="text"
                                placeholder="./meta.json"
                                value={(*attachment_path).clone()}
                                disabled={*attachment_file_reading}
                                oninput={on_attachment_path_input}
                            />
                        </label>
                        <label class="field attachment-mime-field">
                            <span>{ "MIME type" }</span>
                            <input
                                class="input"
                                type="text"
                                placeholder="application/json"
                                value={(*attachment_mime).clone()}
                                disabled={*attachment_file_reading}
                                oninput={on_attachment_mime_input}
                            />
                        </label>
                        <label class="field attachment-description-field">
                            <span>{ "Description" }</span>
                            <textarea
                                class="input attachment-description-input"
                                placeholder="Description"
                                value={(*attachment_description).clone()}
                                disabled={*attachment_file_reading}
                                oninput={on_attachment_description_input}
                            />
                        </label>
                        <label class="field attachment-file-field">
                            <span>{ "File" }</span>
                            <input
                                class="input attachment-file-input"
                                type="file"
                                disabled={*attachment_file_reading}
                                onchange={on_attachment_file_change}
                            />
                        </label>
                        { match &*attachment_content {
                            AttachmentContent::Text(content) => html! {
                                <label class="field attachment-content-field">
                                    <span>{ "Content" }</span>
                                    <textarea
                                        class="input attachment-content-input"
                                        placeholder="Attachment content"
                                        value={content.clone()}
                                        disabled={*attachment_file_reading}
                                        oninput={on_attachment_content_input.clone()}
                                    />
                                </label>
                            },
                            AttachmentContent::Base64(_) => html! {
                                <div class="field attachment-content-field">
                                    <span>{ "Content" }</span>
                                    <p class="attachment-binary-status">{ "Binary attachment" }</p>
                                </div>
                            },
                        } }
                        <button
                            type="button"
                            class="btn btn-outline attachment-add-button"
                            disabled={attachment_add_disabled}
                            onclick={on_add_attachment}
                        >
                            { if *attachment_file_reading { "Reading..." } else { "Add attachment" } }
                        </button>
                    </div>
                    if let Some(error) = (*attachment_file_error).as_ref() {
                        <p class="attachment-file-error" role="alert">{ error.clone() }</p>
                    }
                    <div class="attachment-list">
                        { for attachments.iter().enumerate().map(|(idx, attachment)| {
                            let on_path_input = attachment_input_callback(&attachments, idx, AttachmentField::Path);
                            let on_mime_input = attachment_input_callback(&attachments, idx, AttachmentField::Mime);
                            let on_description_input = {
                                let attachments = attachments.clone();
                                Callback::from(move |e: InputEvent| {
                                    let input: HtmlTextAreaElement = e.target_unchecked_into();
                                    let mut next = (*attachments).clone();
                                    if let Some(attachment) = next.get_mut(idx) {
                                        attachment.description = input.value();
                                    }
                                    attachments.set(next);
                                })
                            };
                            let on_content_input = {
                                let attachments = attachments.clone();
                                Callback::from(move |e: InputEvent| {
                                    let input: HtmlTextAreaElement = e.target_unchecked_into();
                                    let mut next = (*attachments).clone();
                                    if let Some(attachment) = next.get_mut(idx) {
                                        if let AttachmentContent::Text(content) = &mut attachment.content {
                                            *content = input.value();
                                        }
                                    }
                                    attachments.set(next);
                                })
                            };
                            let download_url = props.attachment_base.as_ref().and_then(|base| {
                                props.initial_attachments.iter().find(|initial| {
                                    initial.id == attachment.id
                                        && initial.path == attachment.path
                                        && initial.content == attachment.content
                                }).map(|_| attachment_url(base, &attachment.path))
                            });
                            let on_file_change = attachment_file_callback(
                                &attachments,
                                attachment.id.clone(),
                                &attachment_file_reading,
                                &attachment_file_error,
                            );
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
                                        <label class="field attachment-path-field">
                                            <span>{ "Path" }</span>
                                            <input class="input" type="text" value={attachment.path.clone()} disabled={*attachment_file_reading} oninput={on_path_input} />
                                        </label>
                                        <label class="field attachment-mime-field">
                                            <span>{ "MIME type" }</span>
                                            <input class="input" type="text" value={attachment.mime.clone()} disabled={*attachment_file_reading} oninput={on_mime_input} />
                                        </label>
                                        <label class="field attachment-description-field">
                                            <span>{ "Description" }</span>
                                            <textarea class="input attachment-description-input" value={attachment.description.clone()} disabled={*attachment_file_reading} oninput={on_description_input} />
                                        </label>
                                        <label class="field attachment-file-field">
                                            <span>{ "File" }</span>
                                            <input
                                                class="input attachment-file-input"
                                                type="file"
                                                disabled={*attachment_file_reading}
                                                onchange={on_file_change}
                                            />
                                        </label>
                                        { match &attachment.content {
                                            AttachmentContent::Text(content) => html! {
                                                <label class="field attachment-content-field">
                                                    <span>{ "Content" }</span>
                                                    <textarea
                                                        class="input attachment-content-input"
                                                        value={content.clone()}
                                                        disabled={*attachment_file_reading}
                                                        oninput={on_content_input}
                                                    />
                                                </label>
                                            },
                                            AttachmentContent::Base64(_) => html! {
                                                <div class="field attachment-content-field">
                                                    <span>{ "Content" }</span>
                                                    <p class="attachment-binary-status">{ "Binary attachment" }</p>
                                                    if let Some(url) = download_url {
                                                        <a class="btn btn-ghost" href={url} download="">{ "Download" }</a>
                                                    }
                                                </div>
                                            },
                                        } }
                                        <button type="button" class="btn btn-outline attachment-remove-button" disabled={*attachment_file_reading} onclick={on_remove}>
                                            { "Remove" }
                                        </button>
                                    </div>
                                </div>
                            }
                        }) }
                    </div>
                </fieldset>
            </form>
        </Card>
    }
}

#[derive(Clone, Copy)]
enum AttachmentField {
    Path,
    Mime,
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
                AttachmentField::Path => attachment.path = input.value(),
                AttachmentField::Mime => attachment.mime = input.value(),
            }
        }
        attachments.set(next);
    })
}

fn attachment_file_callback(
    attachments: &UseStateHandle<Vec<NoteAttachment>>,
    attachment_id: String,
    attachment_file_reading: &UseStateHandle<bool>,
    attachment_file_error: &UseStateHandle<Option<String>>,
) -> Callback<Event> {
    let attachments = attachments.clone();
    let attachment_file_reading = attachment_file_reading.clone();
    let attachment_file_error = attachment_file_error.clone();
    Callback::from(move |e: Event| {
        let input: HtmlInputElement = e.target_unchecked_into();
        let Some(file) = input.files().and_then(|files| files.get(0)) else {
            return;
        };
        input.set_value("");

        attachment_file_error.set(None);
        attachment_file_reading.set(true);
        let path = attachment_path_for_file(&file.name());
        let mime = attachment_mime_for_file(&file.type_());
        let file = gloo_file::Blob::from(file);
        let attachments = attachments.clone();
        let attachment_id = attachment_id.clone();
        let attachment_file_reading = attachment_file_reading.clone();
        let attachment_file_error = attachment_file_error.clone();
        spawn_local(async move {
            match read_as_bytes(&file).await {
                Ok(bytes) => {
                    let mut next = (*attachments).clone();
                    if let Some(attachment) = next
                        .iter_mut()
                        .find(|attachment| attachment.id == attachment_id)
                    {
                        attachment.path = path;
                        attachment.mime = mime;
                        attachment.content = attachment_content_for_bytes(bytes);
                    }
                    attachments.set(next);
                }
                Err(error) => {
                    attachment_file_error.set(Some(format!("Unable to read file: {error}")))
                }
            }
            attachment_file_reading.set(false);
        });
    })
}

fn next_attachment_id(attachments: &[NoteAttachment]) -> String {
    let mut sequence = 1;
    loop {
        let candidate = format!("attachment-{sequence}");
        if attachments
            .iter()
            .all(|attachment| attachment.id != candidate)
        {
            return candidate;
        }
        sequence += 1;
    }
}

fn attachment_path_for_file(file_name: &str) -> String {
    let file_name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    format!("./{file_name}")
}

fn attachment_mime_for_file(mime: &str) -> String {
    if mime.trim().is_empty() {
        "text/plain".to_string()
    } else {
        mime.to_string()
    }
}

fn attachment_content_for_bytes(bytes: Vec<u8>) -> AttachmentContent {
    match String::from_utf8(bytes) {
        Ok(content) => AttachmentContent::Text(content),
        Err(error) => AttachmentContent::Base64(STANDARD.encode(error.into_bytes())),
    }
}

fn replace_label_at(
    labels: &mut [(String, String)],
    index: usize,
    key: String,
    value: String,
) -> bool {
    let Some(label) = labels.get_mut(index) else {
        return false;
    };
    *label = (key, value);
    true
}

fn remove_label_at(labels: &mut Vec<(String, String)>, index: usize) -> bool {
    if index >= labels.len() {
        return false;
    }
    labels.remove(index);
    true
}

fn editing_index_after_removal(
    editing_index: Option<usize>,
    removed_index: usize,
) -> Option<usize> {
    match editing_index {
        Some(index) if index == removed_index => None,
        Some(index) if index > removed_index => Some(index - 1),
        editing_index => editing_index,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_markdown_textarea_uses_native_content_sizing() {
        let app_css = include_str!("../../app.css");
        assert!(app_css.contains(
            ".note-content-input .markdown-input-field {\n    field-sizing: content;\n}"
        ));
    }

    #[test]
    fn editing_a_draft_label_replaces_its_key_and_value_in_place() {
        let mut labels = vec![
            ("status".to_string(), "draft".to_string()),
            ("priority".to_string(), "1".to_string()),
        ];

        assert!(replace_label_at(
            &mut labels,
            0,
            "stage".to_string(),
            "review".to_string()
        ));
        assert_eq!(
            labels,
            vec![
                ("stage".to_string(), "review".to_string()),
                ("priority".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn removing_a_draft_label_removes_only_the_target() {
        let mut labels = vec![
            ("status".to_string(), "draft".to_string()),
            ("priority".to_string(), "1".to_string()),
        ];

        assert!(remove_label_at(&mut labels, 0));
        assert_eq!(labels, vec![("priority".to_string(), "1".to_string())]);
    }

    #[test]
    fn invalid_label_indices_leave_the_draft_unchanged() {
        let initial = vec![
            ("status".to_string(), "draft".to_string()),
            ("priority".to_string(), "1".to_string()),
        ];
        let mut labels = initial.clone();

        assert!(!replace_label_at(
            &mut labels,
            2,
            "stage".to_string(),
            "review".to_string()
        ));
        assert_eq!(labels, initial);
        assert!(!remove_label_at(&mut labels, 2));
        assert_eq!(labels, initial);
    }

    #[test]
    fn removing_a_label_reconciles_the_current_edit_index() {
        assert_eq!(editing_index_after_removal(Some(2), 0), Some(1));
        assert_eq!(editing_index_after_removal(Some(1), 1), None);
        assert_eq!(editing_index_after_removal(Some(0), 2), Some(0));
        assert_eq!(editing_index_after_removal(None, 0), None);
    }

    fn attachment(id: &str) -> NoteAttachment {
        NoteAttachment {
            id: id.to_string(),
            path: "./file.txt".to_string(),
            mime: "text/plain".to_string(),
            description: String::new(),
            content: AttachmentContent::Text(String::new()),
        }
    }

    #[test]
    fn generates_the_next_available_attachment_id() {
        let attachments = vec![attachment("attachment-1"), attachment("custom-id")];

        assert_eq!(next_attachment_id(&attachments), "attachment-2");
    }

    #[test]
    fn derives_safe_file_defaults() {
        assert_eq!(attachment_path_for_file("folder\\notes.md"), "./notes.md");
        assert_eq!(attachment_mime_for_file(""), "text/plain");
        assert_eq!(attachment_mime_for_file("text/markdown"), "text/markdown");
    }

    #[test]
    fn keeps_utf8_files_as_text_and_encodes_binary_files() {
        assert_eq!(
            attachment_content_for_bytes(b"hello".to_vec()),
            AttachmentContent::Text("hello".to_string())
        );
        assert_eq!(
            attachment_content_for_bytes(vec![0xff, 0x00]),
            AttachmentContent::Base64("/wA=".to_string())
        );
    }
}
