mod api;
mod components;
mod org;
mod pages;
mod routes;
mod state;
pub mod theme;

use yew::prelude::*;
use yew_router::prelude::*;

use components::AppBar;
use routes::{switch, Route};
use theme::{apply_theme, get_saved_theme};

#[function_component(App)]
fn app() -> Html {
    use_effect_with((), |_| {
        apply_theme(get_saved_theme());
        || ()
    });

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
    apply_theme(get_saved_theme());
    yew::Renderer::<App>::new().render();
}
