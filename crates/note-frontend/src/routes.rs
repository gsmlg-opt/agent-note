use yew::prelude::*;
use yew_router::prelude::*;

use crate::pages::{
    DashboardPage, LabelsPage, NewNotePage, NoteEditPage, NoteShowPage, NotesPage, OrgItemPage,
    OrgWorkspaceNewPage, OrgWorkspacePage, OrgWorkspaceSettingsPage, OrgWorkspacesPage, SystemPage,
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

#[derive(Clone, Debug, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/notes")]
    Notes,
    #[at("/org")]
    Org,
    #[at("/org/new")]
    OrgWorkspaceNew,
    #[at("/org/:workspace_id/settings")]
    OrgWorkspaceSettings { workspace_id: String },
    #[at("/org/:workspace_id")]
    OrgWorkspace { workspace_id: String },
    #[at("/org/:workspace_id/items/:item_id")]
    OrgItem {
        workspace_id: String,
        item_id: String,
    },
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

impl Route {
    pub fn document_title(&self) -> String {
        match self {
            Self::Home => "Home | agent-note".to_owned(),
            Self::Notes => "Notes | agent-note".to_owned(),
            Self::Org => "Org | agent-note".to_owned(),
            Self::OrgWorkspaceNew => "Create Org workspace | agent-note".to_owned(),
            Self::OrgWorkspaceSettings { workspace_id } => {
                format!("Edit Org workspace {workspace_id} | agent-note")
            }
            Self::OrgWorkspace { workspace_id } => {
                format!("Org workspace {workspace_id} | agent-note")
            }
            Self::OrgItem { item_id, .. } => format!("Org item {item_id} | agent-note"),
            Self::NewNote => "New note | agent-note".to_owned(),
            Self::NoteShow { id } => format!("Note {id} | agent-note"),
            Self::NoteEdit { id } => format!("Edit note {id} | agent-note"),
            Self::Labels => "Labels | agent-note".to_owned(),
            Self::Trash => "Trash | agent-note".to_owned(),
            Self::System => "System | agent-note".to_owned(),
            Self::NotFound => "Page not found | agent-note".to_owned(),
        }
    }
}

pub fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <DashboardPage /> },
        Route::Notes => html! { <NotesPage /> },
        Route::Org => html! { <OrgWorkspacesPage /> },
        Route::OrgWorkspaceNew => html! { <OrgWorkspaceNewPage /> },
        Route::OrgWorkspaceSettings { workspace_id } => {
            html! { <OrgWorkspaceSettingsPage {workspace_id} /> }
        }
        Route::OrgWorkspace { workspace_id } => {
            html! { <OrgWorkspacePage {workspace_id} /> }
        }
        Route::OrgItem {
            workspace_id,
            item_id,
        } => html! { <OrgItemPage {workspace_id} {item_id} /> },
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
    use super::{NotesQueryParams, Route};
    use yew_router::Routable;

    #[test]
    fn recognizes_org_directory_as_a_first_class_route() {
        assert_eq!(Route::recognize("/org"), Some(Route::Org));
        assert_eq!(Route::Org.to_path(), "/org");
    }

    #[test]
    fn recognizes_workspace_and_item_context_routes() {
        assert_eq!(
            Route::recognize("/org/workspace-a"),
            Some(Route::OrgWorkspace {
                workspace_id: "workspace-a".into()
            })
        );
        assert_eq!(
            Route::recognize("/org/workspace-a/items/item-1"),
            Some(Route::OrgItem {
                workspace_id: "workspace-a".into(),
                item_id: "item-1".into(),
            })
        );
    }

    #[test]
    fn recognizes_org_workspace_management_routes() {
        assert_eq!(Route::recognize("/org/new"), Some(Route::OrgWorkspaceNew));
        assert_eq!(Route::OrgWorkspaceNew.to_path(), "/org/new");
        assert_eq!(
            Route::recognize("/org/workspace-a/settings"),
            Some(Route::OrgWorkspaceSettings {
                workspace_id: "workspace-a".into(),
            })
        );
        assert_eq!(
            Route::OrgWorkspaceSettings {
                workspace_id: "workspace-a".into(),
            }
            .to_path(),
            "/org/workspace-a/settings"
        );
    }

    #[test]
    fn maps_org_routes_to_specific_document_titles() {
        assert_eq!(Route::Org.document_title(), "Org | agent-note");
        assert_eq!(
            Route::OrgWorkspace {
                workspace_id: "release-ops".into(),
            }
            .document_title(),
            "Org workspace release-ops | agent-note"
        );
        assert_eq!(
            Route::OrgItem {
                workspace_id: "release-ops".into(),
                item_id: "ship-console".into(),
            }
            .document_title(),
            "Org item ship-console | agent-note"
        );
        assert_eq!(Route::Notes.document_title(), "Notes | agent-note");
    }

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
