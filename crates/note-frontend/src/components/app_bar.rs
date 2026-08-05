use yew::prelude::*;
use yew_router::prelude::*;

use crate::components::icons;
use crate::routes::Route;

const PRIMARY_NAV_LABELS: [&str; 7] = [
    "Home", "Notes", "Org", "New note", "Labels", "Trash", "System",
];

/// Primary application navigation in a full-width header with a centered inner row.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    let route = use_route::<Route>().unwrap_or(Route::NotFound);
    let navigator = use_navigator();
    let home_active = route == Route::Home;
    let notes_active = matches!(
        &route,
        Route::Notes | Route::NoteShow { .. } | Route::NoteEdit { .. }
    );
    let org_active = is_org_route(&route);
    let new_note_active = route == Route::NewNote;
    let labels_active = route == Route::Labels;
    let trash_active = route == Route::Trash;
    let system_active = route == Route::System;

    {
        let document_title = route.document_title();
        use_effect_with(document_title, move |title| {
            set_document_title(title);
            || ()
        });
    }

    html! {
        <header class="app-bar">
            <div class="app-bar-inner">
                <Link<Route> to={Route::Home} classes={classes!("app-brand")}>
                    { "agent-note" }
                </Link<Route>>
                <nav class="app-nav-group" aria-label="Primary navigation">
                    <div class="app-nav">
                        { nav_link(Route::Home, "", PRIMARY_NAV_LABELS[0], home_active, navigator.clone()) }
                        { nav_link(Route::Notes, "", PRIMARY_NAV_LABELS[1], notes_active, navigator.clone()) }
                        { nav_link(Route::Org, "", PRIMARY_NAV_LABELS[2], org_active, navigator.clone()) }
                        { nav_link(Route::NewNote, "", PRIMARY_NAV_LABELS[3], new_note_active, navigator.clone()) }
                        { nav_link(Route::Labels, "", PRIMARY_NAV_LABELS[4], labels_active, navigator.clone()) }
                        { nav_link(Route::Trash, "", PRIMARY_NAV_LABELS[5], trash_active, navigator.clone()) }
                    </div>
                    { nav_link(Route::System, "nav-link-system", PRIMARY_NAV_LABELS[6], system_active, navigator) }
                </nav>
            </div>
        </header>
    }
}

fn is_org_route(route: &Route) -> bool {
    matches!(
        route,
        Route::Org | Route::OrgWorkspace { .. } | Route::OrgItem { .. }
    )
}

#[cfg(target_arch = "wasm32")]
fn set_document_title(title: &str) {
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        document.set_title(title);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn set_document_title(_title: &str) {}

fn nav_aria_current(active: bool) -> Option<&'static str> {
    active.then_some("page")
}

fn nav_link(
    destination: Route,
    extra_class: &'static str,
    label: &'static str,
    active: bool,
    navigator: Option<Navigator>,
) -> Html {
    let href = destination.to_path();
    let destination_for_click = destination.clone();
    let onclick = Callback::from(move |event: MouseEvent| {
        if event.button() == 0
            && !event.alt_key()
            && !event.ctrl_key()
            && !event.meta_key()
            && !event.shift_key()
        {
            if let Some(navigator) = &navigator {
                event.prevent_default();
                navigator.push(&destination_for_click);
            }
        }
    });
    let content = if destination == Route::System {
        html! { <>{ icons::settings() }<span>{ label }</span></> }
    } else {
        html! { label }
    };

    html! {
        <a
            {href}
            {onclick}
            class={nav_classes_with(active, extra_class)}
            aria-current={nav_aria_current(active)}
        >
            { content }
        </a>
    }
}

fn nav_classes_with(active: bool, extra: &'static str) -> Classes {
    let mut classes = classes!("nav-link");
    if active {
        classes.push("is-active");
    }
    if !extra.is_empty() {
        classes.push(extra);
    }
    classes
}

#[cfg(test)]
mod tests {
    use super::{is_org_route, nav_aria_current, PRIMARY_NAV_LABELS};
    use crate::routes::Route;

    #[test]
    fn org_navigation_is_distinct_and_reports_current_page() {
        assert!(is_org_route(&Route::Org));
        assert!(is_org_route(&Route::OrgItem {
            workspace_id: "workspace-a".into(),
            item_id: "item-1".into(),
        }));
        assert!(!is_org_route(&Route::Notes));
        assert!(!is_org_route(&Route::System));
        assert_eq!(nav_aria_current(true), Some("page"));
        assert_eq!(nav_aria_current(false), None);
    }

    #[test]
    fn mobile_navigation_keeps_every_target_visible_and_focusable() {
        assert_eq!(
            PRIMARY_NAV_LABELS,
            ["Home", "Notes", "Org", "New note", "Labels", "Trash", "System"]
        );

        let css = include_str!("../../app.css");
        assert!(css.contains(".app-nav-group .nav-link:focus-visible"));
        assert!(css.contains("grid-template-columns: 1fr"));
        assert!(css.contains(".app-nav {\n        display: contents;"));
        assert!(!css.contains(".app-nav-group .nav-link-system span {"));
    }
}
