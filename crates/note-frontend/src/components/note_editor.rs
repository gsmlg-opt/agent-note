use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct NoteEditorProps {
    /// Registered (key, description) pairs, shown in the label picker so the
    /// user can see what each label is for (docs/design.md §7).
    pub available_labels: Vec<(String, String)>,
    /// Emits (title, content, labels) where labels is a Vec of (key, value).
    pub on_submit: Callback<(String, String, Vec<(String, String)>)>,
}

#[function_component(NoteEditor)]
pub fn note_editor(props: &NoteEditorProps) -> Html {
    let title = use_state(String::new);
    let content = use_state(String::new);
    // Applied labels the user has added, as (key, value) pairs.
    let labels = use_state(Vec::<(String, String)>::new);
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

    let on_key_change = {
        let picker_key = picker_key.clone();
        Callback::from(move |e: Event| {
            let select: HtmlSelectElement = e.target_unchecked_into();
            picker_key.set(select.value());
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
        <form class="card note-editor" onsubmit={on_submit}>
            <h2>{ "New Note" }</h2>
            <label class="field">
                <span>{ "Title" }</span>
                <input
                    class="input"
                    type="text"
                    value={(*title).clone()}
                    oninput={on_title_input}
                />
            </label>
            <label class="field">
                <span>{ "Content" }</span>
                <textarea
                    class="textarea"
                    value={(*content).clone()}
                    oninput={on_content_input}
                />
            </label>

            <fieldset class="label-picker">
                <legend>{ "Labels" }</legend>
                <div class="label-picker-row">
                    <select class="input" onchange={on_key_change}>
                        <option value="" selected={picker_key.is_empty()}>{ "Select a label" }</option>
                        { for props.available_labels.iter().map(|(key, desc)| {
                            html! {
                                <option value={key.clone()} title={desc.clone()}>
                                    { key.clone() }
                                </option>
                            }
                        }) }
                    </select>
                    <input
                        class="input"
                        type="text"
                        placeholder="value"
                        value={(*picker_value).clone()}
                        oninput={on_value_input}
                    />
                    <button type="button" class="button" onclick={on_add_label}>
                        { "Add label" }
                    </button>
                </div>
                if let Some(hint) = selected_hint {
                    <p class="label-hint">{ hint }</p>
                }
                <div class="applied-labels">
                    { for labels.iter().map(|(key, value)| {
                        html! {
                            <span class="tag">{ format!("{key}: {value}") }</span>
                        }
                    }) }
                </div>
            </fieldset>

            <button type="submit" class="button button-primary">{ "Save note" }</button>
        </form>
    }
}
