use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew::virtual_dom::AttrValue;
use yew_duskmoon::{Button, Card, Chip, MarkdownInput};

use crate::state::LabelKey;

#[derive(Properties, PartialEq)]
pub struct NoteEditorProps {
    /// Registered label keys, shown in the label picker so the user can see what each label is for
    /// (docs/design.md §7).
    pub available_labels: Vec<LabelKey>,
    /// Emits (title, content, labels) where labels is a Vec of (key, value).
    pub on_submit: Callback<(String, String, Vec<(String, String)>)>,
    /// Prefill values (used when editing an existing note). Read once at mount.
    #[prop_or_default]
    pub initial_title: String,
    #[prop_or_default]
    pub initial_content: String,
    #[prop_or_default]
    pub initial_labels: Vec<(String, String)>,
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
    let submit_debounce = use_state(|| false);
    let submit_locked = use_mut_ref(|| false);
    // The currently-selected label key and value being staged in the picker.
    let picker_key = use_state(String::new);
    let picker_value = use_state(String::new);

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

    let on_submit = {
        let on_submit = props.on_submit.clone();
        let title = title.clone();
        let content = content.clone();
        let labels = labels.clone();
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
            on_submit.emit(((*title).clone(), (*content).clone(), (*labels).clone()));
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

fn label_value_input_type(value_type: &str) -> &'static str {
    match value_type {
        "number" => "number",
        "date" => "date",
        "datetime" => "datetime-local",
        "time" => "time",
        _ => "text",
    }
}
