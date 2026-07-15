use yew::prelude::*;
use yew_router::prelude::*;

use crate::pages::{
    DashboardPage, LabelsPage, NewNotePage, NoteEditPage, NoteShowPage, NotesPage, SystemPage,
};

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/notes")]
    Notes,
    #[at("/new")]
    NewNote,
    #[at("/notes/:id/show")]
    NoteShow { id: String },
    #[at("/notes/:id/edit")]
    NoteEdit { id: String },
    #[at("/labels")]
    Labels,
    #[at("/system")]
    System,
    #[not_found]
    #[at("/404")]
    NotFound,
}

pub fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <DashboardPage /> },
        Route::Notes => html! { <NotesPage /> },
        Route::NewNote => html! { <NewNotePage /> },
        Route::NoteShow { id } => html! { <NoteShowPage {id} /> },
        Route::NoteEdit { id } => html! { <NoteEditPage {id} /> },
        Route::Labels => html! { <LabelsPage /> },
        Route::System => html! { <SystemPage /> },
        Route::NotFound => html! { <p class="empty">{ "Page not found." }</p> },
    }
}
