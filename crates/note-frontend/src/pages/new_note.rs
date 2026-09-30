use yew::prelude::*;
use yew_duskmoon::Alert;
use yew_router::prelude::*;

use crate::api;
use crate::components::NoteEditor;
use crate::routes::Route;
use crate::state::{LabelFilter, LabelKey, NoteAttachment};

fn initial_labels_from_filters(filters: &[LabelFilter]) -> Vec<(String, String)> {
    let mut labels = Vec::new();
    let mut conflicting = std::collections::HashSet::new();
    for filter in filters {
        if !matches!(filter.operator.as_str(), "=" | "==")
            || (filter.operator == "=" && filter.value.is_empty())
        {
            continue;
        }
        if let Some((_, value)) = labels.iter().find(|(key, _)| key == &filter.key) {
            if value != &filter.value {
                conflicting.insert(filter.key.as_str());
            }
        } else {
            labels.push((filter.key.clone(), filter.value.clone()));
        }
    }
    labels.retain(|(key, _)| !conflicting.contains(key.as_str()));
    labels
}

/// Dedicated page for composing a note. On save it returns to the note list.
#[function_component(NewNotePage)]
pub fn new_note_page() -> Html {
    let navigator = use_navigator().expect("router navigator");
    let initial_labels = use_location()
        .map(|location| crate::pages::notes::parse_notes_query(location.query_str()))
        .map(|state| initial_labels_from_filters(&state.labels))
        .unwrap_or_default();
    // Registered label keys used to populate the label-key suggestions.
    let available_labels = use_state(Vec::<LabelKey>::new);
    let error = use_state(|| None::<String>);
    let submitting = use_state(|| false);

    {
        let available_labels = available_labels.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(keys) = api::list_labels().await {
                    available_labels.set(keys);
                }
            });
            || ()
        });
    }

    let on_submit = {
        let navigator = navigator.clone();
        let error = error.clone();
        let submitting = submitting.clone();
        Callback::from(
            move |(title, content, labels, attachments): (
                String,
                String,
                Vec<(String, String)>,
                Vec<NoteAttachment>,
            )| {
                if *submitting {
                    return;
                }
                submitting.set(true);
                error.set(None);
                let navigator = navigator.clone();
                let error = error.clone();
                let submitting = submitting.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match api::save_note(&title, &content, &attachments, &labels).await {
                        Ok(_) => navigator.push(&Route::Notes),
                        Err(e) => {
                            error.set(Some(e));
                            submitting.set(false);
                        }
                    }
                });
            },
        )
    };

    html! {
        <section class="stack">
            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }
            <NoteEditor
                available_labels={(*available_labels).clone()}
                {initial_labels}
                submitting={*submitting}
                {on_submit}
            />
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::notes::parse_notes_query;

    #[test]
    fn new_note_prefills_only_concrete_equality_labels_from_url() {
        let selector =
            "status=Draft&~project==%20Agent%20Note%20&~empty==&archived&count>=2&name^=Ag";
        let state = parse_notes_query(&format!("labels={}", urlencoding::encode(selector)));

        assert_eq!(
            initial_labels_from_filters(&state.labels),
            vec![
                ("status".into(), "Draft".into()),
                ("project".into(), " Agent Note ".into()),
                ("empty".into(), "".into()),
            ]
        );
    }

    #[test]
    fn new_note_deduplicates_same_key_and_omits_conflicting_values() {
        let selector = "status=ready&status=ready&team=one&team=two&~empty==&~empty==";
        let state = parse_notes_query(&format!("labels={}", urlencoding::encode(selector)));

        assert_eq!(
            initial_labels_from_filters(&state.labels),
            vec![
                ("status".into(), "ready".into()),
                ("empty".into(), "".into())
            ]
        );
    }
}
