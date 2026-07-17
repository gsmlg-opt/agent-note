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
    let backdrop_ref = use_node_ref();
    let panel_ref = use_node_ref();

    {
        let panel_ref = panel_ref.clone();
        use_effect_with((), move |_| {
            if let Some(panel) = panel_ref.cast::<web_sys::HtmlElement>() {
                let _ = panel.focus();
            }
            || ()
        });
    }

    let on_backdrop = {
        let on_close = props.on_close.clone();
        let backdrop_ref = backdrop_ref.clone();
        Callback::from(move |e: MouseEvent| {
            // Only dismiss when the backdrop itself is clicked — not a click bubbling up from the
            // panel or its buttons. Yew delegates events from the app root, so `current_target`
            // does not identify the element whose callback is running.
            let clicked_backdrop = e
                .target_dyn_into::<web_sys::Element>()
                .zip(backdrop_ref.cast::<web_sys::Element>())
                .is_some_and(|(target, backdrop)| target == backdrop);
            if clicked_backdrop {
                on_close.emit(());
            }
        })
    };
    let on_keydown = {
        let on_close = props.on_close.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                on_close.emit(());
            }
        })
    };

    html! {
        <div ref={backdrop_ref} class="app-modal-backdrop" onclick={on_backdrop}>
            <div
                ref={panel_ref}
                class="app-modal-panel"
                role="dialog"
                aria-modal="true"
                aria-labelledby="app-modal-title"
                tabindex="-1"
                onkeydown={on_keydown}
            >
                <h2 id="app-modal-title" class="app-modal-title">{ props.title.clone() }</h2>
                <div class="app-modal-body">
                    { for props.children.iter() }
                </div>
            </div>
        </div>
    }
}
