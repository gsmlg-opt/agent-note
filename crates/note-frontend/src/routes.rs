use yew::prelude::*;
use yew_router::prelude::*;

use crate::pages::{LabelsPage, NewNotePage, NotesPage};

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Notes,
    #[at("/new")]
    NewNote,
    #[at("/labels")]
    Labels,
    #[not_found]
    #[at("/404")]
    NotFound,
}

pub fn switch(route: Route) -> Html {
    match route {
        Route::Notes => html! { <NotesPage /> },
        Route::NewNote => html! { <NewNotePage /> },
        Route::Labels => html! { <LabelsPage /> },
        Route::NotFound => html! { <p class="empty">{ "Page not found." }</p> },
    }
}
