use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;
use yew_duskmoon::{Button, Card, Chip};

#[derive(Properties, PartialEq)]
pub struct NoteEditorProps {
    /// Registered (key, description) pairs, shown in the label picker so the
    /// user can see what each label is for (docs/design.md §7).
    pub available_labels: Vec<(String, String)>,
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
}

#[function_component(NoteEditor)]
pub fn note_editor(props: &NoteEditorProps) -> Html {
    let title = use_state(|| props.initial_title.clone());
    let content = use_state(|| props.initial_content.clone());
    // Applied labels the user has added, as (key, value) pairs.
    let labels = use_state(|| props.initial_labels.clone());
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

    let on_content_input = {
        let content = content.clone();
        Callback::from(move |e: InputEvent| {
            let area: HtmlTextAreaElement = e.target_unchecked_into();
            content.set(area.value());
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
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            on_submit.emit(((*title).clone(), (*content).clone(), (*labels).clone()));
        })
    };

    // Description for the currently-selected key, shown as hint text.
    let selected_hint = props
        .available_labels
        .iter()
        .find(|(k, _)| k == &*picker_key)
        .map(|(_, desc)| desc.clone());

    html! {
        <Card title={Some(html! { <span>{ props.card_title.clone() }</span> })} classes={classes!("note-editor")}>
            <form class="stack" onsubmit={on_submit}>
                <label class="field">
                    <span>{ "Title" }</span>
                    <input
                        class="input input-primary"
                        type="text"
                        value={(*title).clone()}
                        oninput={on_title_input}
                    />
                </label>
                <label class="field">
                    <span>{ "Content" }</span>
                    <textarea
                        class="textarea textarea-primary"
                        value={(*content).clone()}
                        oninput={on_content_input}
                    />
                </label>

                <fieldset class="label-picker">
                    <legend>{ "Labels" }</legend>
                    <div class="label-picker-row">
                        // Type a key or pick a registered one — unknown keys are created on save.
                        <input
                            class="input"
                            type="text"
                            list="label-key-options"
                            placeholder="key"
                            value={(*picker_key).clone()}
                            oninput={on_key_input}
                        />
                        <datalist id="label-key-options">
                            { for props.available_labels.iter().map(|(key, _)| html! {
                                <option value={key.clone()} />
                            }) }
                        </datalist>
                        <input
                            class="input"
                            type="text"
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

                <Button variant={Some("primary".to_string())}>
                    <span>{ props.submit_label.clone() }</span>
                </Button>
            </form>
        </Card>
    }
}
