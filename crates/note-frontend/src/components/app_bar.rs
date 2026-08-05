use yew::prelude::*;
use yew_router::prelude::*;

use crate::components::icons;
use crate::routes::Route;

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

    html! {
        <header class="app-bar">
            <div class="app-bar-inner">
                <Link<Route> to={Route::Home} classes={classes!("app-brand")}>
                    { "agent-note" }
                </Link<Route>>
                <nav class="app-nav-group" aria-label="Primary navigation">
                    <div class="app-nav">
                        { nav_link(Route::Home, "", "Home", home_active, navigator.clone()) }
                        { nav_link(Route::Notes, "", "Notes", notes_active, navigator.clone()) }
                        { nav_link(Route::Org, "", "Org", org_active, navigator.clone()) }
                        { nav_link(Route::NewNote, "", "New note", new_note_active, navigator.clone()) }
                        { nav_link(Route::Labels, "", "Labels", labels_active, navigator.clone()) }
                        { nav_link(Route::Trash, "", "Trash", trash_active, navigator.clone()) }
                    </div>
                    { nav_link(Route::System, "nav-link-system", "System", system_active, navigator) }
                </nav>
            </div>
        </header>
    }
}

fn is_org_route(route: &Route) -> bool {
    matches!(route, Route::Org | Route::OrgWorkspace { .. })
}

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
    use super::{is_org_route, nav_aria_current};
    use crate::routes::Route;

    #[test]
    fn org_navigation_is_distinct_and_reports_current_page() {
        assert!(is_org_route(&Route::Org));
        assert!(!is_org_route(&Route::Notes));
        assert!(!is_org_route(&Route::System));
        assert_eq!(nav_aria_current(true), Some("page"));
        assert_eq!(nav_aria_current(false), None);
    }
}
