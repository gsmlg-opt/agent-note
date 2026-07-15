mod api;
mod components;
mod pages;
mod routes;
mod state;

use yew::prelude::*;
use yew_router::prelude::*;

use components::{icons, AppBar};
use routes::{switch, Route};

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <AppShell />
        </BrowserRouter>
    }
}

#[function_component(AppShell)]
fn app_shell() -> Html {
    let route = use_route::<Route>().unwrap_or(Route::NotFound);
    let system_active = route == Route::System;
    let page_name = match route {
        Route::Home => "Dashboard",
        Route::Notes => "Notes",
        Route::NewNote => "New note",
        Route::NoteShow { .. } => "Note",
        Route::NoteEdit { .. } => "Edit note",
        Route::Labels => "Labels",
        Route::System => "System",
        Route::NotFound => "Not found",
    };

    html! {
        <div class="app-shell">
            <AppBar />
            <div class="app-main">
                <header class="app-topbar">
                    <div class="app-topbar-inner">
                        <nav class="breadcrumb" aria-label="Breadcrumb">
                            <Link<Route> to={Route::Home}>{ "agent-note" }</Link<Route>>
                            <span aria-hidden="true">{ "/" }</span>
                            <span aria-current="page">{ page_name }</span>
                        </nav>
                        <Link<Route>
                            to={Route::System}
                            classes={classes!("app-topbar-system", system_active.then_some("is-active"))}
                        >
                            { icons::settings() }
                            <span>{ "System" }</span>
                        </Link<Route>>
                    </div>
                </header>
                <main class="app">
                    <Switch<Route> render={switch} />
                </main>
            </div>
        </div>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
