use yew::prelude::*;

use crate::org::model::Document;

const DOCUMENT_COLUMNS: [&str; 4] = ["File", "Revision", "Status", "Actions"];

#[derive(Debug, PartialEq, Eq)]
struct DocumentRowModel<'a> {
    path: &'a str,
    actions: Vec<&'static str>,
}

impl DocumentRowModel<'_> {
    fn action_name(&self, action: &str) -> String {
        format!("{action} {}", self.path)
    }
}

fn document_row_model(document: &Document, workspace_archived: bool) -> DocumentRowModel<'_> {
    let actions = if workspace_archived {
        vec![]
    } else if document.archived_at.is_some() {
        vec!["Rename", "Restore"]
    } else {
        vec!["Rename", "Archive"]
    };
    DocumentRowModel {
        path: &document.path,
        actions,
    }
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgDocumentTableProps {
    pub documents: Vec<Document>,
    pub workspace_archived: bool,
    pub on_rename: Callback<Document>,
    pub on_archive: Callback<Document>,
    pub on_restore: Callback<Document>,
}

#[function_component(OrgDocumentTable)]
pub fn org_document_table(props: &OrgDocumentTableProps) -> Html {
    html! {
        <div class="org-ledger-scroll org-files-scroll" tabindex="0">
            <table class="org-ledger org-files-ledger">
                <caption>{ "Org workspace files. Use the table actions to manage file lifecycle." }</caption>
                <thead>
                    <tr>
                        { for DOCUMENT_COLUMNS.iter().map(|column| html! { <th scope="col">{ column }</th> }) }
                    </tr>
                </thead>
                <tbody>
                    { for props.documents.iter().map(|document| document_row(document, props)) }
                </tbody>
            </table>
        </div>
    }
}

fn document_row(document: &Document, props: &OrgDocumentTableProps) -> Html {
    let model = document_row_model(document, props.workspace_archived);
    let archived = document.archived_at.is_some();
    let rename = {
        let callback = props.on_rename.clone();
        let document = document.clone();
        Callback::from(move |_| callback.emit(document.clone()))
    };
    let archive = {
        let callback = props.on_archive.clone();
        let document = document.clone();
        Callback::from(move |_| callback.emit(document.clone()))
    };
    let restore = {
        let callback = props.on_restore.clone();
        let document = document.clone();
        Callback::from(move |_| callback.emit(document.clone()))
    };

    html! {
        <tr class={classes!("org-file-row", archived.then_some("is-archived"))}>
            <th scope="row">
                <span class="org-file-identity">
                    <strong>{ document.path.clone() }</strong>
                    <span>{ document.id.clone() }</span>
                </span>
            </th>
            <td class="org-file-revision">{ document.revision }</td>
            <td>
                <span class={classes!("org-files-status", archived.then_some("is-archived"))}>
                    { if archived { "Archived" } else { "Active" } }
                </span>
            </td>
            <td>
                <div class="org-files-row-actions">
                    if model.actions.contains(&"Rename") {
                        <button type="button" class="btn btn-outline" aria-label={model.action_name("Rename")} onclick={rename}>{ "Rename" }</button>
                    }
                    if model.actions.contains(&"Archive") {
                        <button type="button" class="btn btn-outline org-files-archive-button" aria-label={model.action_name("Archive")} onclick={archive}>{ "Archive" }</button>
                    }
                    if model.actions.contains(&"Restore") {
                        <button type="button" class="btn btn-outline" aria-label={model.action_name("Restore")} onclick={restore}>{ "Restore" }</button>
                    }
                </div>
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::model::Document;

    fn document(archived: bool) -> Document {
        Document {
            id: "40000000-0000-4000-8000-000000000001".into(),
            path: "roadmap/release.org".into(),
            revision: 7,
            archived_at: archived.then_some(1),
        }
    }

    #[test]
    fn table_contract_has_file_revision_status_and_actions_columns() {
        assert_eq!(DOCUMENT_COLUMNS, ["File", "Revision", "Status", "Actions"]);
        let source = include_str!("org_document_table.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(
            source.contains("Org workspace files. Use the table actions to manage file lifecycle.")
        );
        assert!(source.contains("tabindex=\"0\""));
    }

    #[test]
    fn row_actions_follow_document_and_workspace_lifecycle() {
        assert_eq!(
            document_row_model(&document(false), false).actions,
            vec!["Rename", "Archive"]
        );
        assert_eq!(
            document_row_model(&document(true), false).actions,
            vec!["Rename", "Restore"]
        );
        assert!(document_row_model(&document(false), true)
            .actions
            .is_empty());
        assert!(document_row_model(&document(true), true).actions.is_empty());
    }

    #[test]
    fn every_action_name_includes_the_exact_file_path() {
        let active_document = document(false);
        let active = document_row_model(&active_document, false);
        assert_eq!(active.action_name("Rename"), "Rename roadmap/release.org");
        assert_eq!(active.action_name("Archive"), "Archive roadmap/release.org");

        let archived_document = document(true);
        let archived = document_row_model(&archived_document, false);
        assert_eq!(
            archived.action_name("Restore"),
            "Restore roadmap/release.org"
        );
    }
}
