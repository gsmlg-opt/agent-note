use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ModalProps {
    pub title: String,
    /// Called when the user dismisses the modal (backdrop click or Cancel).
    pub on_close: Callback<()>,
    pub children: Children,
}

/// A centered overlay dialog, styled entirely with app.css (`.app-modal-*`). The classes are
/// deliberately namespaced away from the design system's `.modal-*` classes: duskmoon's
/// `.modal-backdrop::before` is a full-viewport overlay that would paint over and swallow clicks to
/// the panel's controls. Render it conditionally (mount when open).
#[function_component(Modal)]
pub fn modal(props: &ModalProps) -> Html {
    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |e: MouseEvent| {
            // Only dismiss when the backdrop itself is clicked — not a click bubbling up from the
            // panel or its buttons.
            if e.target() == e.current_target() {
                on_close.emit(());
            }
        })
    };

    html! {
        <div class="app-modal-backdrop" onclick={on_backdrop}>
            <div class="app-modal-panel">
                <h2 class="app-modal-title">{ props.title.clone() }</h2>
                <div class="app-modal-body">
                    { for props.children.iter() }
                </div>
            </div>
        </div>
    }
}
