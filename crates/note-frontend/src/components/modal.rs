use wasm_bindgen::{closure::Closure, JsCast};
use yew::prelude::*;

const FOCUSABLE_SELECTOR: &str = concat!(
    "a[href]:not([aria-disabled=\"true\"]):not([hidden]),",
    "button:not([disabled]):not([hidden]),",
    "input:not([disabled]):not([type=\"hidden\"]):not([hidden]),",
    "select:not([disabled]):not([hidden]),",
    "textarea:not([disabled]):not([hidden]),",
    "[tabindex]:not([tabindex=\"-1\"]):not([hidden])"
);
const TOPMOST_MODAL_SELECTOR: &str = "[role=\"dialog\"][aria-modal=\"true\"]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTrapAction {
    None,
    FocusPanel,
    FocusFirst,
    FocusLast,
}

fn focus_trap_action(
    key: &str,
    shift_key: bool,
    active_index: Option<usize>,
    focusable_count: usize,
) -> FocusTrapAction {
    if key != "Tab" {
        return FocusTrapAction::None;
    }
    if focusable_count == 0 {
        return FocusTrapAction::FocusPanel;
    }
    match (shift_key, active_index) {
        (true, None | Some(0)) => FocusTrapAction::FocusLast,
        (false, None) => FocusTrapAction::FocusFirst,
        (false, Some(index)) if index + 1 == focusable_count => FocusTrapAction::FocusFirst,
        _ => FocusTrapAction::None,
    }
}

fn element_is_focusable(element: &web_sys::HtmlElement) -> bool {
    element.matches(FOCUSABLE_SELECTOR).unwrap_or(false)
        && element.get_attribute("aria-hidden").as_deref() != Some("true")
        && (element.offset_width() > 0 || element.offset_height() > 0)
}

fn focusable_elements(panel: &web_sys::HtmlElement) -> Vec<web_sys::HtmlElement> {
    let Ok(nodes) = panel.query_selector_all(FOCUSABLE_SELECTOR) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(element_is_focusable)
        .collect()
}

fn is_topmost_dialog_index(panel_index: Option<usize>, dialog_count: usize) -> bool {
    panel_index.is_some_and(|index| dialog_count.checked_sub(1) == Some(index))
}

fn panel_is_topmost(panel: &web_sys::HtmlElement) -> bool {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    let Ok(dialogs) = document.query_selector_all(TOPMOST_MODAL_SELECTOR) else {
        return false;
    };
    let panel: &web_sys::Element = panel.as_ref();
    let panel_index = (0..dialogs.length()).position(|index| {
        dialogs
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            .as_ref()
            .is_some_and(|dialog| dialog == panel)
    });
    is_topmost_dialog_index(panel_index, dialogs.length() as usize)
}

fn handle_modal_keydown(
    event: &web_sys::KeyboardEvent,
    panel: &web_sys::HtmlElement,
    on_close: &Callback<()>,
) {
    if !panel_is_topmost(panel) {
        return;
    }
    if event.key() == "Escape" {
        event.prevent_default();
        on_close.emit(());
        return;
    }
    if event.key() != "Tab" {
        return;
    }
    let focusable = focusable_elements(panel);
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
    let active_index = focusable.iter().position(|element| {
        let element: &web_sys::Element = element.as_ref();
        active.as_ref().is_some_and(|active| active == element)
    });
    let target = match focus_trap_action(
        &event.key(),
        event.shift_key(),
        active_index,
        focusable.len(),
    ) {
        FocusTrapAction::None => None,
        FocusTrapAction::FocusPanel => Some(panel),
        FocusTrapAction::FocusFirst => focusable.first(),
        FocusTrapAction::FocusLast => focusable.last(),
    };
    if let Some(target) = target {
        event.prevent_default();
        let _ = target.focus();
    }
}

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
            let opener = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.active_element())
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
            if let Some(panel) = panel_ref.cast::<web_sys::HtmlElement>() {
                let _ = panel.focus();
            }
            move || {
                if let Some(opener) = opener {
                    if opener.is_connected() && element_is_focusable(&opener) {
                        let _ = opener.focus();
                    }
                }
            }
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

    {
        let panel_ref = panel_ref.clone();
        use_effect_with(props.on_close.clone(), move |on_close| {
            let listener = web_sys::window()
                .and_then(|window| window.document())
                .map(|document| {
                    let panel_ref = panel_ref.clone();
                    let on_close = on_close.clone();
                    let listener =
                        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event| {
                            if let Some(panel) = panel_ref.cast::<web_sys::HtmlElement>() {
                                handle_modal_keydown(&event, &panel, &on_close);
                            }
                        });
                    let registered = document
                        .add_event_listener_with_callback(
                            "keydown",
                            listener.as_ref().unchecked_ref(),
                        )
                        .is_ok();
                    (document, listener, registered)
                });
            move || {
                if let Some((document, listener, registered)) = listener {
                    if registered {
                        let _ = document.remove_event_listener_with_callback(
                            "keydown",
                            listener.as_ref().unchecked_ref(),
                        );
                    }
                }
            }
        });
    }

    html! {
        <div ref={backdrop_ref} class="app-modal-backdrop" onclick={on_backdrop}>
            <div
                ref={panel_ref}
                class="app-modal-panel"
                role="dialog"
                aria-modal="true"
                aria-labelledby="app-modal-title"
                tabindex="-1"
            >
                <h2 id="app-modal-title" class="app-modal-title">{ props.title.clone() }</h2>
                <div class="app-modal-body">
                    { for props.children.iter() }
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_key_wraps_at_focusable_endpoints_and_from_the_panel() {
        assert_eq!(
            focus_trap_action("Tab", false, Some(2), 3),
            FocusTrapAction::FocusFirst
        );
        assert_eq!(
            focus_trap_action("Tab", true, Some(0), 3),
            FocusTrapAction::FocusLast
        );
        assert_eq!(
            focus_trap_action("Tab", false, None, 3),
            FocusTrapAction::FocusFirst
        );
        assert_eq!(
            focus_trap_action("Tab", true, None, 3),
            FocusTrapAction::FocusLast
        );
        assert_eq!(
            focus_trap_action("Tab", false, Some(1), 3),
            FocusTrapAction::None
        );
        assert_eq!(
            focus_trap_action("Tab", true, Some(1), 3),
            FocusTrapAction::None
        );
        assert_eq!(
            focus_trap_action("Enter", false, Some(2), 3),
            FocusTrapAction::None
        );
        assert_eq!(
            focus_trap_action("Tab", false, None, 0),
            FocusTrapAction::FocusPanel
        );
        assert_eq!(
            focus_trap_action("Tab", true, None, 0),
            FocusTrapAction::FocusPanel
        );
        assert_eq!(
            focus_trap_action("Enter", false, None, 0),
            FocusTrapAction::None
        );
    }

    #[test]
    fn only_the_last_mounted_modal_is_topmost() {
        assert!(is_topmost_dialog_index(Some(0), 1));
        assert!(is_topmost_dialog_index(Some(1), 2));
        assert!(!is_topmost_dialog_index(Some(0), 2));
        assert!(!is_topmost_dialog_index(None, 2));
        assert!(!is_topmost_dialog_index(Some(0), 0));
    }

    #[test]
    fn modal_source_captures_and_restores_the_opener_and_scopes_focusables() {
        let source = include_str!("modal.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let capture = source.find("document.active_element()").unwrap();
        let panel_focus = source.find("panel.focus()").unwrap();
        assert!(capture < panel_focus);
        assert!(source.contains("if opener.is_connected() && element_is_focusable(&opener)"));
        assert!(source.contains("let _ = opener.focus()"));
        assert!(source.contains("panel.query_selector_all(FOCUSABLE_SELECTOR)"));
        assert!(source.contains("FocusTrapAction::FocusPanel => Some(panel)"));
        assert!(source.contains("add_event_listener_with_callback("));
        assert!(source.contains("remove_event_listener_with_callback("));
        assert!(!source.contains("onkeydown={on_keydown}"));
        assert!(source.contains("query_selector_all(TOPMOST_MODAL_SELECTOR)"));
        let handler = source.split("fn handle_modal_keydown").nth(1).unwrap();
        assert!(
            handler.find("if !panel_is_topmost(panel)").unwrap()
                < handler.find("event.key() == \"Escape\"").unwrap()
        );
        assert!(source.contains("event.key() == \"Escape\""));
        assert!(source.contains("event.prevent_default()"));
        assert!(source.contains("clicked_backdrop"));
    }
}
