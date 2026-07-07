use yew::prelude::*;
use yew_router::prelude::*;

use crate::routes::Route;

/// Top navigation bar shown on every page. Uses duskmoon's `navbar` styling; the wordmark links
/// home and the three destinations map to the app's routes.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    html! {
        <header class="navbar app-bar">
            <div class="navbar-start">
                <Link<Route> to={Route::Notes} classes={classes!("app-brand")}>
                    { "agent-note" }
                </Link<Route>>
            </div>
            <nav class="navbar-end app-nav">
                <Link<Route> to={Route::Notes} classes={classes!("nav-link")}>{ "Notes" }</Link<Route>>
                <Link<Route> to={Route::NewNote} classes={classes!("nav-link")}>{ "New note" }</Link<Route>>
                <Link<Route> to={Route::Labels} classes={classes!("nav-link")}>{ "Labels" }</Link<Route>>
            </nav>
        </header>
    }
}
