use wasm_bindgen::JsCast;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ModalProps {
    pub title: String,
    /// Called when the user dismisses the modal (backdrop click or Cancel).
    pub on_close: Callback<()>,
    pub children: Children,
}

const FOCUSABLE_SELECTOR: &str = "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusWrapTarget {
    Panel,
    First,
    Last,
    None,
}

fn focus_wrap_target(
    focusable_count: usize,
    active_index: Option<usize>,
    shift_pressed: bool,
) -> FocusWrapTarget {
    if focusable_count == 0 {
        return FocusWrapTarget::Panel;
    }

    match active_index {
        None if shift_pressed => FocusWrapTarget::Last,
        None => FocusWrapTarget::First,
        Some(0) if shift_pressed => FocusWrapTarget::Last,
        Some(index) if index + 1 == focusable_count => FocusWrapTarget::First,
        _ => FocusWrapTarget::None,
    }
}

fn focusable_descendants(panel: &web_sys::HtmlElement) -> Vec<web_sys::HtmlElement> {
    panel
        .query_selector_all(FOCUSABLE_SELECTOR)
        .ok()
        .into_iter()
        .flat_map(|nodes| (0..nodes.length()).filter_map(move |index| nodes.item(index)))
        .filter_map(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        .collect()
}

fn active_focusable_index(focusables: &[web_sys::HtmlElement]) -> Option<usize> {
    let active = web_sys::window()?.document()?.active_element()?;
    let same_element = |focusable: &web_sys::HtmlElement| {
        let focusable: &web_sys::Element = focusable.as_ref();
        focusable == &active
    };

    if focusables.first().is_some_and(same_element) {
        return Some(0);
    }
    if focusables.last().is_some_and(same_element) {
        return Some(focusables.len().saturating_sub(1));
    }

    focusables.iter().position(same_element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_from_last_focusable_wraps_to_first() {
        assert_eq!(focus_wrap_target(3, Some(2), false), FocusWrapTarget::First);
    }

    #[test]
    fn shift_tab_from_first_focusable_wraps_to_last() {
        assert_eq!(focus_wrap_target(3, Some(0), true), FocusWrapTarget::Last);
    }

    #[test]
    fn middle_focusable_does_not_intercept_tab() {
        assert_eq!(focus_wrap_target(3, Some(1), false), FocusWrapTarget::None);
        assert_eq!(focus_wrap_target(3, Some(1), true), FocusWrapTarget::None);
    }

    #[test]
    fn empty_panel_keeps_focus_on_the_panel_and_single_control_wraps_safely() {
        assert_eq!(focus_wrap_target(0, None, false), FocusWrapTarget::Panel);
        assert_eq!(focus_wrap_target(0, None, true), FocusWrapTarget::Panel);
        assert_eq!(focus_wrap_target(1, Some(0), false), FocusWrapTarget::First);
        assert_eq!(focus_wrap_target(1, Some(0), true), FocusWrapTarget::Last);
    }
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
        let panel_ref = panel_ref.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                on_close.emit(());
                return;
            }
            if event.key() != "Tab" {
                return;
            }
            let Some(panel) = panel_ref.cast::<web_sys::HtmlElement>() else {
                return;
            };
            let focusables = focusable_descendants(&panel);
            let target = focus_wrap_target(
                focusables.len(),
                active_focusable_index(&focusables),
                event.shift_key(),
            );
            match target {
                FocusWrapTarget::Panel => {
                    event.prevent_default();
                    let _ = panel.focus();
                }
                FocusWrapTarget::First => {
                    event.prevent_default();
                    if let Some(first) = focusables.first() {
                        let _ = first.focus();
                    }
                }
                FocusWrapTarget::Last => {
                    event.prevent_default();
                    if let Some(last) = focusables.last() {
                        let _ = last.focus();
                    }
                }
                FocusWrapTarget::None => {}
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
