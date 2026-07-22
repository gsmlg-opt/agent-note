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

impl NotesQueryParams {
    pub fn validated(self) -> Option<Self> {
        (self.current >= 1 && matches!(self.page_size, 10 | 30 | 50 | 100 | 1000)).then_some(self)
    }
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

#[cfg(test)]
mod tests {
    use super::NotesQueryParams;

    #[test]
    fn rejects_semantically_invalid_notes_return_context() {
        let query = NotesQueryParams {
            current: 0,
            page_size: 17,
            search: None,
            labels: None,
        };

        assert_eq!(query.validated(), None);
    }

    #[test]
    fn accepts_normalized_notes_return_context() {
        let query = NotesQueryParams {
            current: 3,
            page_size: 30,
            search: Some("release notes".to_string()),
            labels: Some("status=draft".to_string()),
        };

        assert_eq!(query.clone().validated(), Some(query));
    }
}
