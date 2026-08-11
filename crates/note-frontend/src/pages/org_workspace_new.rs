use yew::prelude::*;
use yew_router::prelude::*;

use crate::{
    components::{org_workspace_form::WorkspaceFormMode, OrgWorkspaceForm},
    org::{
        api::{self as org_api, OrgApiError},
        workspace_management::{
            new_workspace_id, CreateWorkspaceBody, WorkspaceDraft, WorkspaceSubmission,
            WorkspaceValidationError,
        },
    },
    routes::Route,
};

fn submit_create(
    body: CreateWorkspaceBody,
    busy: UseStateHandle<bool>,
    error: UseStateHandle<Option<OrgApiError>>,
    navigator: Option<Navigator>,
) {
    busy.set(true);
    error.set(None);
    wasm_bindgen_futures::spawn_local(async move {
        match org_api::create_workspace(&body).await {
            Ok(result) => {
                busy.set(false);
                if let Some(navigator) = navigator {
                    navigator.push(&Route::OrgWorkspace {
                        workspace_id: result.workspace_id,
                    });
                }
            }
            Err(next_error) => {
                busy.set(false);
                error.set(Some(next_error));
            }
        }
    });
}

#[function_component(OrgWorkspaceNewPage)]
pub fn org_workspace_new_page() -> Html {
    let navigator = use_navigator();
    let draft = use_state(WorkspaceDraft::new);
    let validation_errors = use_state(Vec::<WorkspaceValidationError>::new);
    let server_error = use_state(|| None::<OrgApiError>);
    let busy = use_state(|| false);
    let pending = use_state(|| None::<CreateWorkspaceBody>);

    let on_change = {
        let draft = draft.clone();
        let validation_errors = validation_errors.clone();
        Callback::from(move |next: WorkspaceDraft| {
            draft.set(next);
            validation_errors.set(Vec::new());
        })
    };
    let on_submit = {
        let draft = draft.clone();
        let validation_errors = validation_errors.clone();
        let server_error = server_error.clone();
        let busy = busy.clone();
        let pending = pending.clone();
        let navigator = navigator.clone();
        Callback::from(move |_| {
            if *busy {
                return;
            }
            if let Err(errors) = draft.validate() {
                validation_errors.set(errors);
                return;
            }
            validation_errors.set(Vec::new());
            let body = CreateWorkspaceBody::from_draft(
                new_workspace_id(),
                WorkspaceSubmission::new().operation_id,
                &draft,
            );
            pending.set(Some(body.clone()));
            submit_create(body, busy.clone(), server_error.clone(), navigator.clone());
        })
    };
    let on_retry = {
        let pending = pending.clone();
        let busy = busy.clone();
        let server_error = server_error.clone();
        let navigator = navigator.clone();
        Callback::from(move |_| {
            if *busy {
                return;
            }
            if let Some(body) = (*pending).clone() {
                submit_create(body, busy.clone(), server_error.clone(), navigator.clone());
            }
        })
    };
    let on_cancel = {
        let navigator = navigator.clone();
        Callback::from(move |_| {
            if let Some(navigator) = &navigator {
                navigator.push(&Route::Org);
            }
        })
    };

    html! {
        <section class="stack org-workspace-management-page" aria-labelledby="org-workspace-new-title" data-testid="org-workspace-new-page">
            <div class="page-head">
                <div><p class="org-kicker">{ "Workspace configuration" }</p><h2 id="org-workspace-new-title" class="page-title">{ "Create Org workspace" }</h2><p class="page-hint">{ "Start from the engineering policy and adjust the operating model." }</p></div>
            </div>
            <OrgWorkspaceForm
                draft={(*draft).clone()}
                mode={WorkspaceFormMode::Create}
                validation_errors={(*validation_errors).clone()}
                server_error={(*server_error).clone()}
                busy={*busy}
                {on_change}
                {on_submit}
                {on_cancel}
                on_retry={Some(on_retry)}
            />
        </section>
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn create_page_uses_the_shared_form_and_only_the_workspace_create_client() {
        let source = include_str!("org_workspace_new.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("OrgWorkspaceForm"));
        assert!(source.contains("org_api::create_workspace"));
        for forbidden in ["put_document", "create_item", "claim_item", "/mcp"] {
            assert!(!source.contains(forbidden));
        }
    }
}
