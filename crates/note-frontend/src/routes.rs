use yew::prelude::*;
use yew_router::prelude::*;

use crate::pages::{
    DashboardPage, LabelsPage, NewNotePage, NoteEditPage, NoteShowPage, NotesPage, SystemPage,
    TrashPage,
};

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct NotesQueryParams {
    pub current: usize,
    pub page_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
}

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
    #[at("/trash")]
    Trash,
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
        Route::Trash => html! { <TrashPage /> },
        Route::System => html! { <SystemPage /> },
        Route::NotFound => html! { <p class="empty">{ "Page not found." }</p> },
    }
}
