use yew::prelude::*;
use yew_router::prelude::*;

use crate::routes::Route;

/// Top navigation bar shown on every page: a full-width surface with a centered inner row (matching
/// the page content column). The wordmark links home; the three links map to the app's routes.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    html! {
        <header class="app-bar">
            <div class="app-bar-inner">
                <Link<Route> to={Route::Notes} classes={classes!("app-brand")}>
                    { "agent-note" }
                </Link<Route>>
                <nav class="app-nav">
                    <Link<Route> to={Route::Notes} classes={classes!("nav-link")}>{ "Notes" }</Link<Route>>
                    <Link<Route> to={Route::NewNote} classes={classes!("nav-link")}>{ "New note" }</Link<Route>>
                    <Link<Route> to={Route::Labels} classes={classes!("nav-link")}>{ "Labels" }</Link<Route>>
                </nav>
            </div>
        </header>
    }
}
