mod api;
mod components;
mod org;
mod pages;
mod routes;
mod state;

use yew::prelude::*;
use yew_router::prelude::*;

use components::AppBar;
use routes::{switch, Route};

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <AppBar />
            <main class="app">
                <Switch<Route> render={switch} />
            </main>
        </BrowserRouter>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
