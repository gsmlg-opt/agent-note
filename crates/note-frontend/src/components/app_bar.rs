use yew::prelude::*;
use yew_router::prelude::*;

use crate::components::icons;
use crate::routes::Route;

/// Primary application navigation in a full-width header with a centered inner row.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    let route = use_route::<Route>().unwrap_or(Route::NotFound);
    let home_active = route == Route::Home;
    let notes_active = matches!(
        &route,
        Route::Notes | Route::NoteShow { .. } | Route::NoteEdit { .. }
    );
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
                        <Link<Route> to={Route::Home} classes={nav_classes(home_active)}>{ "Home" }</Link<Route>>
                        <Link<Route> to={Route::Notes} classes={nav_classes(notes_active)}>{ "Notes" }</Link<Route>>
                        <Link<Route> to={Route::NewNote} classes={nav_classes(new_note_active)}>{ "New note" }</Link<Route>>
                        <Link<Route> to={Route::Labels} classes={nav_classes(labels_active)}>{ "Labels" }</Link<Route>>
                        <Link<Route> to={Route::Trash} classes={nav_classes(trash_active)}>{ "Trash" }</Link<Route>>
                    </div>
                    <Link<Route>
                        to={Route::System}
                        classes={nav_classes_with(system_active, "nav-link-system")}
                    >
                        { icons::settings() }
                        <span>{ "System" }</span>
                    </Link<Route>>
                </nav>
            </div>
        </header>
    }
}

fn nav_classes(active: bool) -> Classes {
    nav_classes_with(active, "")
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
