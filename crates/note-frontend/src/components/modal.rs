use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ModalProps {
    pub title: String,
    /// Called when the user dismisses the modal (backdrop click or Cancel).
    pub on_close: Callback<()>,
    pub children: Children,
}

/// A centered overlay dialog. Styled with duskmoon's `dialog*` classes; positioning of the
/// full-screen backdrop lives in app.css so it doesn't depend on the design system's JS behavior.
/// Render it conditionally (only mount when open) — there is no internal open/closed state.
#[function_component(Modal)]
pub fn modal(props: &ModalProps) -> Html {
    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };
    // Clicks inside the dialog must not bubble to the backdrop (which would close it).
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        <div class="modal-backdrop" onclick={on_backdrop}>
            <div class="dialog dialog-md modal-panel" onclick={stop}>
                <div class="dialog-header">
                    <h2 class="dialog-title">{ props.title.clone() }</h2>
                </div>
                <div class="dialog-body">
                    { for props.children.iter() }
                </div>
            </div>
        </div>
    }
}
