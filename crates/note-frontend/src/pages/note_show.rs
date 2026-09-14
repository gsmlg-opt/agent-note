use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
use pulldown_cmark_to_cmark::cmark;
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use yew::prelude::*;
use yew_duskmoon::{Alert, Card, Chip, DmMarkdown};
use yew_router::prelude::*;

use crate::api;
use crate::components::Modal;
use crate::export::{
    classify_pdf_error, export_menu_decision, export_menu_should_prevent_default,
    initiate_browser_download, initiate_browser_pdf_download, initiate_captured_markdown_download,
    invalidate_pdf_export, pdf_completion_is_current, pdf_response_is_downloadable,
    prepare_markdown_download, try_start_pdf_export, ClassifiedPdfError, ExportCapabilitiesState,
    ExportMenuDecision, ExportMenuKey, PdfErrorAction, PdfExportError, PdfExportRequest,
};
use crate::routes::{NotesQueryParams, Route};
use crate::state::{stale_retry_blocked, AttachmentContent, NoteSummary};

#[derive(Properties, PartialEq)]
pub struct NoteShowProps {
    pub id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CopyStatus {
    #[default]
    Ready,
    Copied,
    Failed,
}

impl CopyStatus {
    fn action_label(self) -> &'static str {
        match self {
            Self::Ready => "Copy",
            Self::Copied => "Copied",
            Self::Failed => "Copy failed",
        }
    }
}

fn copy_chip_text(id: &str, status: CopyStatus) -> String {
    format!("ID: {id} · {}", status.action_label())
}

fn copy_announcement(status: CopyStatus) -> &'static str {
    match status {
        CopyStatus::Ready => "",
        CopyStatus::Copied => "Note ID copied to clipboard.",
        CopyStatus::Failed => "Unable to copy note ID.",
    }
}

fn content_copy_chip_text(status: CopyStatus) -> String {
    format!("Content · {}", status.action_label())
}

fn content_copy_announcement(status: CopyStatus) -> &'static str {
    match status {
        CopyStatus::Ready => "",
        CopyStatus::Copied => "Note content copied to clipboard.",
        CopyStatus::Failed => "Unable to copy note content.",
    }
}

fn content_copy_payload(content: &str) -> &str {
    content
}

fn next_copy_generation(generation: &mut u64) -> u64 {
    *generation = generation.saturating_add(1);
    *generation
}

fn copy_generation_is_current(current: u64, completed: u64) -> bool {
    current == completed
}

fn note_load_is_current(started_generation: u64, current_generation: u64) -> bool {
    started_generation == current_generation
}

fn next_note_load_generation(generation: &mut u64) -> u64 {
    *generation = generation.saturating_add(1);
    *generation
}

fn delete_confirmation_message(title: &str) -> String {
    format!("Move the note \u{201c}{title}\u{201d} to Trash? You can restore it later.")
}

fn try_start_delete(in_flight: &mut bool) -> bool {
    if *in_flight {
        false
    } else {
        *in_flight = true;
        true
    }
}

fn browser_clipboard() -> Option<web_sys::Clipboard> {
    let navigator = web_sys::window()?.navigator();
    let clipboard = js_sys::Reflect::get(
        navigator.as_ref(),
        &wasm_bindgen::JsValue::from_str("clipboard"),
    )
    .ok()?;

    if clipboard.is_null() || clipboard.is_undefined() {
        None
    } else {
        Some(clipboard.unchecked_into())
    }
}

fn start_capabilities_fetch(
    state: UseStateHandle<ExportCapabilitiesState>,
    generation: Rc<RefCell<u64>>,
    abort_controller: Rc<RefCell<Option<web_sys::AbortController>>>,
) {
    if let Some(previous) = abort_controller.borrow_mut().take() {
        previous.abort();
    }
    let started_generation = {
        let mut current = generation.borrow_mut();
        *current = current.saturating_add(1);
        *current
    };
    state.set(ExportCapabilitiesState::Loading);
    let controller = web_sys::AbortController::new().ok();
    *abort_controller.borrow_mut() = controller.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let signal = controller.as_ref().map(|value| value.signal());
        let result = api::get_export_capabilities(signal.as_ref()).await;
        if *generation.borrow() == started_generation {
            state.set(match result {
                Ok(capabilities) => ExportCapabilitiesState::Available {
                    pdf: capabilities.pdf,
                },
                Err(_) => ExportCapabilitiesState::Failed,
            });
            abort_controller.borrow_mut().take();
        }
    });
}

fn invalidate_active_pdf(
    generation: &Rc<RefCell<u64>>,
    active: &Rc<RefCell<Option<PdfExportRequest>>>,
    abort_controller: &Rc<RefCell<Option<web_sys::AbortController>>>,
) {
    if invalidate_pdf_export(&mut generation.borrow_mut(), &mut active.borrow_mut()) {
        if let Some(controller) = abort_controller.borrow_mut().take() {
            controller.abort();
        }
    }
}

/// Read-only view of a single note.
#[function_component(NoteShowPage)]
pub fn note_show_page(props: &NoteShowProps) -> Html {
    let navigator = use_navigator().expect("router navigator");
    let notes_query = use_location()
        .and_then(|location| location.query::<NotesQueryParams>().ok())
        .and_then(NotesQueryParams::validated);
    let note = use_state(|| None::<NoteSummary>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let copy_status = use_state(CopyStatus::default);
    let content_copy_status = use_state(CopyStatus::default);
    let content_copy_generation = use_mut_ref(|| 0_u64);
    let load_generation = use_mut_ref(|| 0_u64);
    let export_open = use_state(|| false);
    let export_focused_item = use_state(|| 0_usize);
    let export_announcement = use_state(String::new);
    let export_capabilities = use_state(ExportCapabilitiesState::default);
    let capabilities_generation = use_mut_ref(|| 0_u64);
    let capabilities_abort = use_mut_ref(|| None::<web_sys::AbortController>);
    let pdf_generation = use_mut_ref(|| 0_u64);
    let active_pdf = use_mut_ref(|| None::<PdfExportRequest>);
    let pdf_abort = use_mut_ref(|| None::<web_sys::AbortController>);
    let pdf_pending = use_state(|| false);
    let pdf_error = use_state(|| None::<ClassifiedPdfError>);
    let export_trigger_ref = use_node_ref();
    let markdown_export_ref = use_node_ref();
    let pdf_export_ref = use_node_ref();
    let delete_open = use_state(|| false);
    let delete_pending = use_state(|| false);
    let delete_conflict = use_state(|| None::<api::NoteMutationApiError>);
    let delete_in_flight = use_mut_ref(|| false);

    {
        let export_capabilities = export_capabilities.clone();
        let capabilities_generation = capabilities_generation.clone();
        let capabilities_abort = capabilities_abort.clone();
        use_effect_with((), move |_| {
            start_capabilities_fetch(
                export_capabilities,
                capabilities_generation.clone(),
                capabilities_abort.clone(),
            );
            move || {
                let mut generation = capabilities_generation.borrow_mut();
                *generation = generation.saturating_add(1);
                if let Some(controller) = capabilities_abort.borrow_mut().take() {
                    controller.abort();
                }
            }
        });
    }

    {
        let note = note.clone();
        let loading = loading.clone();
        let error = error.clone();
        let copy_status = copy_status.clone();
        let content_copy_status = content_copy_status.clone();
        let content_copy_generation = content_copy_generation.clone();
        let load_generation = load_generation.clone();
        let export_open = export_open.clone();
        let export_announcement = export_announcement.clone();
        let pdf_generation = pdf_generation.clone();
        let active_pdf = active_pdf.clone();
        let pdf_abort = pdf_abort.clone();
        let pdf_pending = pdf_pending.clone();
        let pdf_error = pdf_error.clone();
        let delete_open = delete_open.clone();
        let delete_pending = delete_pending.clone();
        let delete_in_flight = delete_in_flight.clone();
        let delete_conflict = delete_conflict.clone();
        let id = props.id.clone();
        use_effect_with(props.id.clone(), move |_| {
            let started_generation = {
                let mut generation = load_generation.borrow_mut();
                next_note_load_generation(&mut generation)
            };
            loading.set(true);
            note.set(None);
            error.set(None);
            copy_status.set(CopyStatus::Ready);
            next_copy_generation(&mut content_copy_generation.borrow_mut());
            content_copy_status.set(CopyStatus::Ready);
            export_open.set(false);
            export_announcement.set(String::new());
            invalidate_active_pdf(&pdf_generation, &active_pdf, &pdf_abort);
            pdf_pending.set(false);
            pdf_error.set(None);
            delete_open.set(false);
            delete_pending.set(false);
            *delete_in_flight.borrow_mut() = false;
            delete_conflict.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                let result = api::get_note(&id).await;
                if note_load_is_current(started_generation, *load_generation.borrow()) {
                    match result {
                        Ok(n) => note.set(Some(n)),
                        Err(e) => error.set(Some(e)),
                    }
                    loading.set(false);
                }
            });
            move || {
                invalidate_active_pdf(&pdf_generation, &active_pdf, &pdf_abort);
            }
        });
    }

    {
        let markdown_export_ref = markdown_export_ref.clone();
        let pdf_export_ref = pdf_export_ref.clone();
        use_effect_with(
            (*export_open, *export_focused_item),
            move |(open, focused_item)| {
                if *open {
                    let item_ref = if *focused_item == 0 {
                        &markdown_export_ref
                    } else {
                        &pdf_export_ref
                    };
                    if let Some(item) = item_ref.cast::<web_sys::HtmlElement>() {
                        let _ = item.focus();
                    }
                }
                || ()
            },
        );
    }

    let copy_id = (*note)
        .as_ref()
        .map(|loaded_note| loaded_note.id.clone())
        .unwrap_or_default();
    let copy_content = (*note)
        .as_ref()
        .map(|loaded_note| content_copy_payload(&loaded_note.content).to_string())
        .unwrap_or_default();
    let on_copy_id = {
        let id = copy_id;
        let copy_status = copy_status.clone();
        Callback::from(move |_| {
            let id = id.clone();
            let copy_status = copy_status.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let copied = match browser_clipboard() {
                    Some(clipboard) => JsFuture::from(clipboard.write_text(&id)).await.is_ok(),
                    None => false,
                };
                copy_status.set(if copied {
                    CopyStatus::Copied
                } else {
                    CopyStatus::Failed
                });
            });
        })
    };
    let on_copy_content = {
        let content = copy_content;
        let content_copy_status = content_copy_status.clone();
        let content_copy_generation = content_copy_generation.clone();
        Callback::from(move |_| {
            let content = content.clone();
            let content_copy_status = content_copy_status.clone();
            let request_generation =
                next_copy_generation(&mut content_copy_generation.borrow_mut());
            let content_copy_generation = content_copy_generation.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let copied = match browser_clipboard() {
                    Some(clipboard) => JsFuture::from(clipboard.write_text(&content)).await.is_ok(),
                    None => false,
                };
                if copy_generation_is_current(*content_copy_generation.borrow(), request_generation)
                {
                    content_copy_status.set(if copied {
                        CopyStatus::Copied
                    } else {
                        CopyStatus::Failed
                    });
                }
            });
        })
    };

    let on_pdf_export = {
        let note = note.clone();
        let export_open = export_open.clone();
        let export_announcement = export_announcement.clone();
        let pdf_generation = pdf_generation.clone();
        let active_pdf = active_pdf.clone();
        let pdf_abort = pdf_abort.clone();
        let pdf_pending = pdf_pending.clone();
        let pdf_error = pdf_error.clone();
        Callback::from(move |_: ()| {
            let Some(loaded_note) = (*note).clone() else {
                return;
            };
            let request = {
                let mut active = active_pdf.borrow_mut();
                try_start_pdf_export(
                    &mut active,
                    &loaded_note.id,
                    &loaded_note.title,
                    loaded_note.revision,
                    *pdf_generation.borrow(),
                )
            };
            let Some(request) = request else {
                return;
            };
            let Ok(controller) = web_sys::AbortController::new() else {
                active_pdf.borrow_mut().take();
                pdf_error.set(Some(classify_pdf_error(PdfExportError::transport())));
                return;
            };
            *pdf_abort.borrow_mut() = Some(controller.clone());
            pdf_pending.set(true);
            pdf_error.set(None);
            export_announcement.set("Preparing PDF download…".into());
            export_open.set(false);

            let note = note.clone();
            let export_announcement = export_announcement.clone();
            let pdf_generation = pdf_generation.clone();
            let active_pdf = active_pdf.clone();
            let pdf_abort = pdf_abort.clone();
            let pdf_pending = pdf_pending.clone();
            let pdf_error = pdf_error.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let signal = controller.signal();
                let result = api::get_pdf_export(&request, Some(&signal)).await;
                let is_current = (*note).as_ref().is_some_and(|current_note| {
                    pdf_completion_is_current(
                        &request,
                        *pdf_generation.borrow(),
                        &current_note.id,
                        current_note.revision,
                    )
                });
                if !is_current {
                    return;
                }

                match result {
                    Ok(response)
                        if pdf_response_is_downloadable(
                            &request,
                            response.revision_header.as_deref(),
                            response.content_type.as_deref(),
                        ) =>
                    {
                        match initiate_browser_pdf_download(&request, &response.bytes) {
                            Ok(()) => {
                                export_announcement.set("PDF download initiated.".into());
                                pdf_error.set(None);
                            }
                            Err(_) => {
                                export_announcement.set(String::new());
                                pdf_error
                                    .set(Some(classify_pdf_error(PdfExportError::transport())));
                            }
                        }
                    }
                    Ok(_) => {
                        export_announcement.set(String::new());
                        pdf_error.set(Some(classify_pdf_error(PdfExportError::from_payload(
                            502, "",
                        ))));
                    }
                    Err(error) => {
                        export_announcement.set(String::new());
                        pdf_error.set(Some(classify_pdf_error(error)));
                    }
                }
                active_pdf.borrow_mut().take();
                pdf_abort.borrow_mut().take();
                pdf_pending.set(false);
            });
        })
    };

    let on_retry_capabilities = {
        let export_capabilities = export_capabilities.clone();
        let capabilities_generation = capabilities_generation.clone();
        let capabilities_abort = capabilities_abort.clone();
        Callback::from(move |_| {
            start_capabilities_fetch(
                export_capabilities.clone(),
                capabilities_generation.clone(),
                capabilities_abort.clone(),
            );
        })
    };

    let on_reload_pdf_note = {
        let note = note.clone();
        let error = error.clone();
        let load_generation = load_generation.clone();
        let pdf_generation = pdf_generation.clone();
        let active_pdf = active_pdf.clone();
        let pdf_abort = pdf_abort.clone();
        let pdf_pending = pdf_pending.clone();
        let pdf_error = pdf_error.clone();
        let id = props.id.clone();
        Callback::from(move |_| {
            invalidate_active_pdf(&pdf_generation, &active_pdf, &pdf_abort);
            pdf_pending.set(false);
            pdf_error.set(None);
            let started_generation = {
                let mut generation = load_generation.borrow_mut();
                next_note_load_generation(&mut generation)
            };
            let note = note.clone();
            let error = error.clone();
            let load_generation = load_generation.clone();
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = api::get_note(&id).await;
                if note_load_is_current(started_generation, *load_generation.borrow()) {
                    match result {
                        Ok(latest) => {
                            note.set(Some(latest));
                            error.set(None);
                        }
                        Err(message) => error.set(Some(message)),
                    }
                }
            });
        })
    };

    let loaded_note_state = note.clone();
    let delete_modal = match (*delete_open, (*note).clone()) {
        (true, Some(note)) => {
            let on_close = {
                let delete_open = delete_open.clone();
                let delete_pending = delete_pending.clone();
                Callback::from(move |_: ()| {
                    if !*delete_pending {
                        delete_open.set(false);
                    }
                })
            };
            let on_cancel = {
                let delete_open = delete_open.clone();
                let delete_pending = delete_pending.clone();
                Callback::from(move |_: MouseEvent| {
                    if !*delete_pending {
                        delete_open.set(false);
                    }
                })
            };
            let on_confirm = {
                let delete_open = delete_open.clone();
                let delete_pending = delete_pending.clone();
                let delete_in_flight = delete_in_flight.clone();
                let error = error.clone();
                let navigator = navigator.clone();
                let notes_query = notes_query.clone();
                let id = note.id.clone();
                let expected_revision = note.revision;
                let delete_conflict = delete_conflict.clone();
                Callback::from(move |_: MouseEvent| {
                    if delete_conflict.is_some() {
                        return;
                    }
                    {
                        let mut in_flight = delete_in_flight.borrow_mut();
                        if !try_start_delete(&mut in_flight) {
                            return;
                        }
                    }
                    let delete_open = delete_open.clone();
                    let delete_pending = delete_pending.clone();
                    let delete_in_flight = delete_in_flight.clone();
                    let error = error.clone();
                    let navigator = navigator.clone();
                    let notes_query = notes_query.clone();
                    let id = id.clone();
                    let delete_conflict = delete_conflict.clone();
                    delete_pending.set(true);
                    error.set(None);
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = api::delete_note(&id, expected_revision).await;
                        *delete_in_flight.borrow_mut() = false;
                        delete_pending.set(false);
                        match result {
                            Ok(()) => {
                                delete_open.set(false);
                                if let Some(query) = notes_query {
                                    let _ = navigator.push_with_query(&Route::Notes, query);
                                } else {
                                    navigator.push(&Route::Notes);
                                }
                            }
                            Err(delete_error) => {
                                if delete_error.is_stale_revision() {
                                    delete_conflict.set(Some(delete_error));
                                } else {
                                    error.set(Some(delete_error.to_string()));
                                }
                            }
                        }
                    });
                })
            };
            let on_reload = {
                let note_state = loaded_note_state.clone();
                let delete_conflict = delete_conflict.clone();
                let error = error.clone();
                let load_generation = load_generation.clone();
                let pdf_generation = pdf_generation.clone();
                let active_pdf = active_pdf.clone();
                let pdf_abort = pdf_abort.clone();
                let pdf_pending = pdf_pending.clone();
                let pdf_error = pdf_error.clone();
                let export_announcement = export_announcement.clone();
                let id = note.id.clone();
                Callback::from(move |_| {
                    invalidate_active_pdf(&pdf_generation, &active_pdf, &pdf_abort);
                    pdf_pending.set(false);
                    pdf_error.set(None);
                    export_announcement.set(String::new());
                    let note_state = note_state.clone();
                    let delete_conflict = delete_conflict.clone();
                    let error = error.clone();
                    let load_generation = load_generation.clone();
                    let id = id.clone();
                    let started_generation = {
                        let mut generation = load_generation.borrow_mut();
                        next_note_load_generation(&mut generation)
                    };
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = api::get_note(&id).await;
                        if note_load_is_current(started_generation, *load_generation.borrow()) {
                            match result {
                                Ok(latest) => {
                                    note_state.set(Some(latest));
                                    delete_conflict.set(None);
                                    error.set(None);
                                }
                                Err(message) => error.set(Some(message)),
                            }
                        }
                    });
                })
            };

            html! {
                <Modal title="Move note to Trash" on_close={on_close}>
                    <p>{ delete_confirmation_message(&note.title) }</p>
                    if let Some(delete_error) = &*error {
                        <Alert variant={Some("error".to_string())}>
                            <span>{ delete_error.clone() }</span>
                        </Alert>
                    }
                    if let Some(conflict_error) = &*delete_conflict {
                        <Alert variant={Some("error".to_string())}>
                            <span>{ conflict_error.message.clone() }</span>
                            <span>{ " Reload the note before deciding whether to retry deletion." }</span>
                        </Alert>
                    }
                    <div class="app-modal-actions">
                        if delete_conflict.is_some() {
                            <button type="button" class="btn btn-outline" onclick={on_reload}>
                                { "Reload note" }
                            </button>
                        }
                        <button type="button" class="btn btn-ghost" disabled={*delete_pending} onclick={on_cancel}>
                            { "Cancel" }
                        </button>
                        <button type="button" class="btn btn-error" disabled={stale_retry_blocked(*delete_pending, delete_conflict.is_some())} onclick={on_confirm}>
                            { if *delete_pending { "Moving…" } else { "Move to Trash" } }
                        </button>
                    </div>
                </Modal>
            }
        }
        _ => html! {},
    };

    html! {
        <section class="stack">
            if let Some(err) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ err.clone() }</span></Alert>
            }
            if *loading {
                <p class="loading">{ "Loading…" }</p>
            } else if let Some(n) = (*note).as_ref().filter(|n| n.id == props.id) {
                <Card title={Some(html! { <span>{ n.title.clone() }</span> })}>
                    <div class="note-copy-actions">
                        <button
                            type="button"
                            class="chip chip-clickable chip-primary note-id-copy-chip"
                            aria-label={format!("Copy note ID {}", n.id)}
                            onclick={on_copy_id}
                        >
                            <span class="note-id-copy-text">
                                { copy_chip_text(&n.id, *copy_status) }
                            </span>
                        </button>
                        <span class="sr-only" aria-live="polite" aria-atomic="true">
                            { copy_announcement(*copy_status) }
                        </span>
                        <button
                            type="button"
                            class="chip chip-clickable chip-primary"
                            aria-label="Copy note Markdown content"
                            onclick={on_copy_content}
                        >
                            { content_copy_chip_text(*content_copy_status) }
                        </button>
                        <span class="sr-only" aria-live="polite" aria-atomic="true">
                            { content_copy_announcement(*content_copy_status) }
                        </span>
                    </div>
                    <div class="note-page-actions">
                        <Link<Route, NotesQueryParams>
                            to={Route::Notes}
                            query={notes_query.clone()}
                            classes={classes!("btn","btn-ghost")}
                        >
                            { "Back" }
                        </Link<Route, NotesQueryParams>>
                        <button
                            type="button"
                            class="btn btn-error"
                            onclick={{
                                let delete_open = delete_open.clone();
                                let error = error.clone();
                                Callback::from(move |_| {
                                    error.set(None);
                                    delete_open.set(true);
                                })
                            }}
                        >
                            { "Delete note" }
                        </button>
                        <div class="note-edit-export-actions">
                            <Link<Route, NotesQueryParams>
                                to={Route::NoteEdit { id: n.id.clone() }}
                                query={notes_query.clone()}
                                classes={classes!("btn","btn-primary")}
                            >
                                { "Edit note" }
                            </Link<Route, NotesQueryParams>>
                            <div class="note-export-menu">
                                <button
                                    ref={export_trigger_ref.clone()}
                                    type="button"
                                    class="btn btn-outline"
                                    aria-label="Export note"
                                    aria-haspopup="menu"
                                    aria-expanded={export_open.to_string()}
                                    aria-controls="note-export-menu"
                                    aria-busy={pdf_pending.to_string()}
                                    onclick={{
                                        let export_open = export_open.clone();
                                        let export_focused_item = export_focused_item.clone();
                                        Callback::from(move |_| {
                                            if *export_open {
                                                export_open.set(false);
                                            } else {
                                                export_focused_item.set(0);
                                                export_open.set(true);
                                            }
                                        })
                                    }}
                                    onkeydown={{
                                        let export_open = export_open.clone();
                                        let export_focused_item = export_focused_item.clone();
                                        Callback::from(move |event: KeyboardEvent| {
                                            let key = ExportMenuKey::from_key(&event.key());
                                            if let ExportMenuDecision::OpenAt(item) =
                                                export_menu_decision(false, 0, key)
                                            {
                                                event.prevent_default();
                                                export_focused_item.set(item);
                                                export_open.set(true);
                                            }
                                        })
                                    }}
                                >
                                    { "Export" }
                                    <span aria-hidden="true">{ " ▾" }</span>
                                </button>
                                if *export_open {
                                    <div
                                        id="note-export-menu"
                                        class="note-export-menu-panel"
                                        role="menu"
                                        aria-label="Export note formats"
                                        onkeydown={{
                                            let export_open = export_open.clone();
                                            let export_focused_item = export_focused_item.clone();
                                            let export_trigger_ref = export_trigger_ref.clone();
                                            let export_announcement = export_announcement.clone();
                                            let on_pdf_export = on_pdf_export.clone();
                                            let pdf_available = export_capabilities.pdf_available();
                                            let pdf_pending = *pdf_pending;
                                            let prepared = prepare_markdown_download(
                                                &n.id,
                                                &n.title,
                                                n.revision,
                                                &n.content,
                                            );
                                            let has_attachments = !n.attachments.is_empty();
                                            Callback::from(move |event: KeyboardEvent| {
                                                let key = ExportMenuKey::from_key(&event.key());
                                                if *export_focused_item == 1
                                                    && matches!(key, ExportMenuKey::Enter | ExportMenuKey::Space)
                                                {
                                                    event.prevent_default();
                                                    if pdf_available && !pdf_pending {
                                                        on_pdf_export.emit(());
                                                        export_open.set(false);
                                                        if let Some(trigger) = export_trigger_ref.cast::<web_sys::HtmlElement>() {
                                                            let _ = trigger.focus();
                                                        }
                                                    }
                                                    return;
                                                }
                                                let decision = export_menu_decision(
                                                    true,
                                                    *export_focused_item,
                                                    key,
                                                );
                                                if export_menu_should_prevent_default(decision) {
                                                    event.prevent_default();
                                                }
                                                match decision {
                                                    ExportMenuDecision::Focus(item) => {
                                                        export_focused_item.set(item);
                                                    }
                                                    ExportMenuDecision::CloseWithoutRestoreFocus => {
                                                        export_open.set(false);
                                                    }
                                                    ExportMenuDecision::CloseAndRestoreFocus => {
                                                        export_open.set(false);
                                                        if let Some(trigger) = export_trigger_ref.cast::<web_sys::HtmlElement>() {
                                                            let _ = trigger.focus();
                                                        }
                                                    }
                                                    ExportMenuDecision::DownloadMarkdownAndRestoreFocus => {
                                                        export_announcement.set(
                                                            initiate_captured_markdown_download(
                                                                &prepared,
                                                                has_attachments,
                                                                initiate_browser_download,
                                                            )
                                                        );
                                                        export_open.set(false);
                                                        if let Some(trigger) = export_trigger_ref.cast::<web_sys::HtmlElement>() {
                                                            let _ = trigger.focus();
                                                        }
                                                    }
                                                    ExportMenuDecision::OpenAt(_) | ExportMenuDecision::Stay => {}
                                                }
                                            })
                                        }}
                                    >
                                        <button
                                            ref={markdown_export_ref.clone()}
                                            type="button"
                                            class="note-export-menu-item"
                                            role="menuitem"
                                            tabindex="-1"
                                            aria-disabled={(!export_capabilities.markdown_available()).to_string()}
                                            onclick={{
                                                let export_open = export_open.clone();
                                                let export_trigger_ref = export_trigger_ref.clone();
                                                let export_announcement = export_announcement.clone();
                                                let prepared = prepare_markdown_download(
                                                    &n.id,
                                                    &n.title,
                                                    n.revision,
                                                    &n.content,
                                                );
                                                let has_attachments = !n.attachments.is_empty();
                                                Callback::from(move |_| {
                                                    export_announcement.set(
                                                        initiate_captured_markdown_download(
                                                            &prepared,
                                                            has_attachments,
                                                            initiate_browser_download,
                                                        )
                                                    );
                                                    export_open.set(false);
                                                    if let Some(trigger) = export_trigger_ref.cast::<web_sys::HtmlElement>() {
                                                        let _ = trigger.focus();
                                                    }
                                                })
                                            }}
                                        >
                                            <span>{ "Markdown (.md)" }</span>
                                            <small>{ "Original source" }</small>
                                        </button>
                                        <button
                                            ref={pdf_export_ref.clone()}
                                            type="button"
                                            class={classes!(
                                                "note-export-menu-item",
                                                (!export_capabilities.pdf_available() || *pdf_pending)
                                                    .then_some("note-export-menu-item-disabled"),
                                            )}
                                            role="menuitem"
                                            tabindex="-1"
                                            aria-disabled={(!export_capabilities.pdf_available() || *pdf_pending).to_string()}
                                            aria-busy={pdf_pending.to_string()}
                                            aria-describedby="note-export-pdf-description"
                                            onclick={{
                                                let on_pdf_export = on_pdf_export.clone();
                                                let export_trigger_ref = export_trigger_ref.clone();
                                                let actionable = export_capabilities.pdf_available() && !*pdf_pending;
                                                Callback::from(move |_| {
                                                    if actionable {
                                                        on_pdf_export.emit(());
                                                        if let Some(trigger) = export_trigger_ref.cast::<web_sys::HtmlElement>() {
                                                            let _ = trigger.focus();
                                                        }
                                                    }
                                                })
                                            }}
                                        >
                                            <span>{ "PDF (.pdf)" }</span>
                                            <small id="note-export-pdf-description">
                                                {
                                                    if *pdf_pending {
                                                        "Preparing PDF…"
                                                    } else {
                                                        export_capabilities.pdf_description()
                                                    }
                                                }
                                            </small>
                                        </button>
                                    </div>
                                }
                            </div>
                        </div>
                    </div>
                    if !export_announcement.is_empty() {
                        <p class="note-export-status" role="status" aria-live="polite" aria-atomic="true">
                            { (*export_announcement).clone() }
                        </p>
                    }
                    if matches!(*export_capabilities, ExportCapabilitiesState::Failed) {
                        <div class="note-export-feedback" role="status">
                            <span>{ "PDF availability could not be loaded. Markdown export remains available." }</span>
                            <button type="button" class="btn btn-ghost btn-sm" onclick={on_retry_capabilities}>
                                { "Retry availability" }
                            </button>
                        </div>
                    }
                    if let Some(pdf_failure) = &*pdf_error {
                        <div class="note-export-feedback note-export-feedback-error" role="alert">
                            <span>{ pdf_failure.message.clone() }</span>
                            { match pdf_failure.action {
                                PdfErrorAction::ReloadNote => html! {
                                    <button type="button" class="btn btn-outline btn-sm" onclick={on_reload_pdf_note.clone()}>
                                        { "Reload note" }
                                    </button>
                                },
                                PdfErrorAction::Retry => html! {
                                    <button
                                        type="button"
                                        class="btn btn-outline btn-sm"
                                        onclick={{
                                            let on_pdf_export = on_pdf_export.clone();
                                            Callback::from(move |_| on_pdf_export.emit(()))
                                        }}
                                    >
                                        { "Retry PDF export" }
                                    </button>
                                },
                                PdfErrorAction::None => html! {},
                            }}
                        </div>
                    }
                    if !n.labels.is_empty() {
                        <div class="applied-labels">
                            { for n.labels.iter().map(|(k, v)| html! {
                                <Chip variant={Some("primary".to_string())}>
                                    <span>{ format!("{k}: {v}") }</span>
                                </Chip>
                            }) }
                        </div>
                    }
                    <DmMarkdown
                        markdown={AttrValue::from(note_markdown(n))}
                        base_url={Some(format!("/api/notes/{}/attachments", n.id))}
                    />
                    if !n.attachments.is_empty() {
                        <section class="attachment-view-section">
                            <h3>{ "Attachments" }</h3>
                            <div class="attachment-view-list">
                                { for n.attachments.iter().map(|attachment| {
                                    let attachment_base = format!("/api/notes/{}/attachments", n.id);
                                    let download_url = api::attachment_url(&attachment_base, &attachment.path);
                                    html! {
                                        <section class="attachment-view-item" key={attachment.id.clone()}>
                                            <div class="attachment-view-header">
                                                <strong>{ attachment.path.clone() }</strong>
                                                <span>{ attachment.mime.clone() }</span>
                                            </div>
                                            if !attachment.description.is_empty() {
                                                <p class="attachment-description">{ attachment.description.clone() }</p>
                                            }
                                            { match &attachment.content {
                                                AttachmentContent::Text(content) => html! {
                                                    <pre>{ content.clone() }</pre>
                                                },
                                                AttachmentContent::Base64(_) => html! {
                                                    <div class="attachment-binary-actions">
                                                        <span>{ "Binary attachment" }</span>
                                                        <a class="btn btn-outline" href={download_url} download="">{ "Download" }</a>
                                                    </div>
                                                },
                                            } }
                                        </section>
                                    }
                                }) }
                            </div>
                        </section>
                    }
                </Card>
            } else {
                <p class="empty">{ "Note not found." }</p>
            }
            { delete_modal }
        </section>
    }
}

fn note_markdown(note: &NoteSummary) -> String {
    let attachment_base = format!("/api/notes/{}/attachments", note.id);
    rewrite_attachment_urls(&note.content, &attachment_base)
}

// WORKAROUND(upstream): duskmoon-dev/yew-duskmoon-ui#8
fn rewrite_attachment_urls(markdown: &str, attachment_base: &str) -> String {
    let (front_matter, body) = split_front_matter(markdown);
    let events = Parser::new_ext(body, markdown_options()).map(|event| match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: rewrite_url_needing_encoding(dest_url, attachment_base),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: rewrite_url_needing_encoding(dest_url, attachment_base),
            title,
            id,
        }),
        Event::Html(html) => Event::Html(rewrite_relative_html_urls(html, attachment_base)),
        Event::InlineHtml(html) => {
            Event::InlineHtml(rewrite_relative_html_urls(html, attachment_base))
        }
        event => event,
    });

    let mut rewritten = String::from(front_matter);
    cmark(events, &mut rewritten).expect("parsed Markdown events must serialize");
    rewritten
}

fn split_front_matter(markdown: &str) -> (&str, &str) {
    let bom_len = if markdown.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let Some(opening_end) = markdown[bom_len..].find('\n').map(|end| bom_len + end) else {
        return ("", markdown);
    };
    if markdown[bom_len..opening_end].trim_end_matches('\r') != "---" {
        return ("", markdown);
    }

    let mut cursor = opening_end + 1;
    while cursor < markdown.len() {
        let line_end = markdown[cursor..]
            .find('\n')
            .map(|end| cursor + end)
            .unwrap_or(markdown.len());
        let line = markdown[cursor..line_end].trim_end_matches('\r');
        if matches!(line, "---" | "...") {
            let body_start = (line_end + 1).min(markdown.len());
            return markdown.split_at(body_start);
        }
        cursor = line_end + 1;
    }

    ("", markdown)
}

fn rewrite_url_needing_encoding<'a>(url: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    let Some(path) = url.strip_prefix("./") else {
        return url;
    };

    let encoded = api::attachment_url(attachment_base, path);
    let unencoded = format!("{}/{path}", attachment_base.trim_end_matches('/'));
    if encoded == unencoded {
        url
    } else {
        CowStr::Boxed(encoded.into_boxed_str())
    }
}

fn rewrite_relative_html_urls<'a>(html: CowStr<'a>, attachment_base: &str) -> CowStr<'a> {
    if !html.contains("href=\"./") && !html.contains("src=\"./") {
        return html;
    }

    let rewritten = rewrite_relative_html_attribute(&html, "href", attachment_base);
    CowStr::Boxed(
        rewrite_relative_html_attribute(&rewritten, "src", attachment_base).into_boxed_str(),
    )
}

fn rewrite_relative_html_attribute(html: &str, attribute: &str, attachment_base: &str) -> String {
    let marker = format!(r#"{attribute}="./"#);
    let mut remaining = html;
    let mut rewritten = String::with_capacity(html.len());

    while let Some(marker_start) = remaining.find(&marker) {
        let path_start = marker_start + marker.len();
        let Some(path_end) = remaining[path_start..].find('"') else {
            break;
        };
        let path_end = path_start + path_end;
        rewritten.push_str(&remaining[..marker_start]);
        rewritten.push_str(attribute);
        rewritten.push_str("=\"");
        rewritten.push_str(&api::attachment_url(
            attachment_base,
            &remaining[path_start..path_end],
        ));
        remaining = &remaining[path_end..];
    }

    rewritten.push_str(remaining);
    rewritten
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

#[cfg(test)]
mod tests {
    use super::{
        content_copy_announcement, content_copy_chip_text, content_copy_payload, copy_announcement,
        copy_chip_text, copy_generation_is_current, delete_confirmation_message,
        next_copy_generation, next_note_load_generation, note_load_is_current,
        rewrite_attachment_urls, try_start_delete, CopyStatus,
    };
    use yew_duskmoon::{render_markdown_to_html_with_options, DmMarkdownOptions};

    const BASE: &str = "/api/notes/note-1/attachments";

    #[test]
    fn delete_confirmation_explains_that_the_note_can_be_restored() {
        assert_eq!(
            delete_confirmation_message("Release notes"),
            "Move the note \u{201c}Release notes\u{201d} to Trash? You can restore it later."
        );
    }

    #[test]
    fn delete_request_gate_allows_only_one_in_flight_request() {
        let mut in_flight = false;

        assert!(try_start_delete(&mut in_flight));
        assert!(!try_start_delete(&mut in_flight));

        in_flight = false;
        assert!(try_start_delete(&mut in_flight));
    }

    #[test]
    fn copy_chip_initially_offers_to_copy_the_canonical_id() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Ready),
            "ID: note-123 · Copy"
        );
        assert_eq!(copy_announcement(CopyStatus::Ready), "");
    }

    #[test]
    fn copy_chip_reports_success() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Copied),
            "ID: note-123 · Copied"
        );
        assert_eq!(
            copy_announcement(CopyStatus::Copied),
            "Note ID copied to clipboard."
        );
    }

    #[test]
    fn copy_chip_reports_failure() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Failed),
            "ID: note-123 · Copy failed"
        );
        assert_eq!(
            copy_announcement(CopyStatus::Failed),
            "Unable to copy note ID."
        );
    }

    #[test]
    fn content_copy_chip_starts_ready_and_preserves_raw_markdown() {
        let markdown = "# Heading\n\n[attachment](./report.pdf)\n";

        assert_eq!(content_copy_chip_text(CopyStatus::Ready), "Content · Copy");
        assert_eq!(content_copy_payload(markdown), markdown);
        assert_eq!(content_copy_announcement(CopyStatus::Ready), "");
    }

    #[test]
    fn content_copy_chip_reports_success_independently_from_copy_id() {
        assert_eq!(
            copy_chip_text("note-123", CopyStatus::Ready),
            "ID: note-123 · Copy"
        );
        assert_eq!(
            content_copy_chip_text(CopyStatus::Copied),
            "Content · Copied"
        );
        assert_eq!(
            content_copy_announcement(CopyStatus::Copied),
            "Note content copied to clipboard."
        );
    }

    #[test]
    fn content_copy_chip_reports_failure() {
        assert_eq!(
            content_copy_chip_text(CopyStatus::Failed),
            "Content · Copy failed"
        );
        assert_eq!(
            content_copy_announcement(CopyStatus::Failed),
            "Unable to copy note content."
        );
    }

    #[test]
    fn content_copy_generations_ignore_stale_completions() {
        let mut generation = 0;

        let first = next_copy_generation(&mut generation);
        assert_eq!(first, 1);
        assert!(copy_generation_is_current(generation, first));

        let second = next_copy_generation(&mut generation);
        assert_eq!(second, 2);
        assert!(!copy_generation_is_current(generation, first));
        assert!(copy_generation_is_current(generation, second));

        let navigation = next_copy_generation(&mut generation);
        assert_eq!(navigation, 3);
        assert!(!copy_generation_is_current(generation, second));
    }

    #[test]
    fn note_load_generation_rejects_late_initial_and_conflict_reload_completions() {
        let mut generation = 6;
        let initial = next_note_load_generation(&mut generation);
        assert!(note_load_is_current(initial, generation));

        let older_reload = next_note_load_generation(&mut generation);
        let newer_reload = next_note_load_generation(&mut generation);

        assert!(!note_load_is_current(initial, generation));
        assert!(!note_load_is_current(older_reload, generation));
        assert!(note_load_is_current(newer_reload, generation));
    }

    #[test]
    fn rewrites_relative_link_image_and_html_urls() {
        let markdown = r#"[meta](./meta.json "metadata")

![diagram][diagram]

[diagram]: ./images/diagram.svg

<a href="./raw.txt">raw</a>
<img src="./raw.svg">"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);
        let html = render_markdown_to_html_with_options(
            &rewritten,
            DmMarkdownOptions {
                base_url: Some(BASE.to_string()),
                ..DmMarkdownOptions::default()
            },
        );

        assert!(rewritten.contains("[meta](./meta.json \"metadata\")"));
        assert!(rewritten.contains("[diagram]: ./images/diagram.svg"));
        assert!(rewritten.contains("href=\"/api/notes/note-1/attachments/raw.txt\""));
        assert!(rewritten.contains("src=\"/api/notes/note-1/attachments/raw.svg\""));
        assert!(html.contains("href=\"/api/notes/note-1/attachments/meta.json\""));
        assert!(html.contains("src=\"/api/notes/note-1/attachments/images/diagram.svg\""));
    }

    #[test]
    fn percent_encodes_reserved_characters_in_attachment_paths() {
        let markdown = r#"[report](./reports/progress#1?value=50%.txt)

<img src="./images/status #1?value=50%.png">"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);

        assert!(rewritten
            .contains("/api/notes/note-1/attachments/reports/progress%231%3Fvalue%3D50%25.txt"));
        assert!(rewritten.contains(
            "src=\"/api/notes/note-1/attachments/images/status%20%231%3Fvalue%3D50%25.png\""
        ));
    }

    #[test]
    fn preserves_other_urls_code_and_front_matter() {
        let markdown = r#"---
title: Example
---

[external](https://example.com)
[root](/docs)
[anchor](#top)
[mail](mailto:test@example.com)

`./inline.txt`

```text
./code.txt
```"#;

        let rewritten = rewrite_attachment_urls(markdown, BASE);

        assert!(rewritten.starts_with("---\ntitle: Example\n---"));
        assert!(rewritten.contains("https://example.com"));
        assert!(rewritten.contains("/docs"));
        assert!(rewritten.contains("#top"));
        assert!(rewritten.contains("mailto:test@example.com"));
        assert!(rewritten.contains("`./inline.txt`"));
        assert!(rewritten.contains("./code.txt"));
    }

    #[test]
    fn preserves_inline_css_colors_for_dm_markdown() {
        let rewritten = rewrite_attachment_urls(
            "| Token | Value |\n| --- | --- |\n| `primary` | `#0065FF` |",
            BASE,
        );
        let html = render_markdown_to_html_with_options(
            &rewritten,
            DmMarkdownOptions {
                base_url: Some(BASE.to_string()),
                ..DmMarkdownOptions::default()
            },
        );

        assert!(html.contains(r#"class="dm-color-code""#));
        assert!(html.contains(r#"class="dm-color-chip""#));
        assert!(html.contains("background-color:#0065FF;"));
        assert!(html.contains("display:inline-flex"));
    }
}
