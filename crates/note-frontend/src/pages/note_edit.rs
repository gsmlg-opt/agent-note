use yew::prelude::*;
use yew_duskmoon::Alert;
use yew_router::prelude::*;

use crate::api;
use crate::components::NoteEditor;
use crate::routes::{NotesQueryParams, Route};
use crate::state::{LabelKey, NoteAttachment, NoteSummary};

#[derive(Properties, PartialEq)]
pub struct NoteEditProps {
    pub id: String,
}

/// Edit an existing note: fetch it, prefill the editor, and PUT the changes on save.
#[function_component(NoteEditPage)]
pub fn note_edit_page(props: &NoteEditProps) -> Html {
    let navigator = use_navigator().expect("router navigator");
    let notes_query = use_location()
        .and_then(|location| location.query::<NotesQueryParams>().ok())
        .and_then(NotesQueryParams::validated);
    let note = use_state(|| None::<NoteSummary>);
    let available_labels = use_state(Vec::<LabelKey>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let submitting = use_state(|| false);

    {
        let note = note.clone();
        let available_labels = available_labels.clone();
        let loading = loading.clone();
        let error = error.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                // Load the note and the label suggestions together.
                match api::get_note(&id).await {
                    Ok(n) => note.set(Some(n)),
                    Err(e) => error.set(Some(e)),
                }
                if let Ok(keys) = api::list_labels().await {
                    available_labels.set(keys);
                }
                loading.set(false);
            });
            || ()
        });
    }

    let on_submit = {
        let navigator = navigator.clone();
        let error = error.clone();
        let id = props.id.clone();
        let notes_query = notes_query.clone();
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
                let id = id.clone();
                let notes_query = notes_query.clone();
                let submitting = submitting.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match api::update_note(&id, &title, &content, &attachments, &labels).await {
                        Ok(()) => {
                            let route = Route::NoteShow { id };
                            if let Some(query) = notes_query {
                                let _ = navigator.push_with_query(&route, query);
                            } else {
                                navigator.push(&route);
                            }
                        }
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
            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(n) = &*note {
                <div class="note-page-actions">
                    <Link<Route, NotesQueryParams>
                        to={Route::NoteShow { id: n.id.clone() }}
                        query={notes_query.clone()}
                        classes={classes!("btn", "btn-ghost")}
                    >
                        { "Back to note" }
                    </Link<Route, NotesQueryParams>>
                </div>
                <NoteEditor
                    available_labels={(*available_labels).clone()}
                    initial_title={n.title.clone()}
                    initial_content={n.content.clone()}
                    initial_labels={n.labels.clone()}
                    initial_attachments={n.attachments.clone()}
                    attachment_base={Some(format!("/api/notes/{}/attachments", n.id))}
                    card_title="Edit note"
                    submit_label="Save changes"
                    submitting={*submitting}
                    {on_submit}
                />
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
        </section>
    }
}
