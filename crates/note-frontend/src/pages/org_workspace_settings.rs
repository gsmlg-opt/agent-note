use yew::prelude::*;
use yew_router::prelude::*;

use crate::{
    components::{org_workspace_form::WorkspaceFormMode, OrgWorkspaceForm},
    org::{
        api::{self as org_api, OrgApiError},
        model::Workspace,
        mutation::MutationSubmission,
        workspace_management::{UpdateWorkspaceBody, WorkspaceDraft, WorkspaceValidationError},
    },
    routes::Route,
};

#[derive(Clone, Debug, PartialEq)]
enum SettingsLoad {
    Loading,
    Ready {
        workspace: Workspace,
        draft: WorkspaceDraft,
    },
    Failed(OrgApiError),
}

fn submit_update(
    workspace_id: String,
    body: UpdateWorkspaceBody,
    busy: UseStateHandle<bool>,
    error: UseStateHandle<Option<OrgApiError>>,
    navigator: Option<Navigator>,
) {
    busy.set(true);
    error.set(None);
    wasm_bindgen_futures::spawn_local(async move {
        match org_api::update_workspace(&workspace_id, &body).await {
            Ok(_) => {
                busy.set(false);
                if let Some(navigator) = navigator {
                    navigator.push(&Route::OrgWorkspace { workspace_id });
                }
            }
            Err(next_error) => {
                busy.set(false);
                error.set(Some(next_error));
            }
        }
    });
}

#[derive(Properties, Clone, PartialEq)]
pub struct OrgWorkspaceSettingsPageProps {
    pub workspace_id: String,
}

#[function_component(OrgWorkspaceSettingsPage)]
pub fn org_workspace_settings_page(props: &OrgWorkspaceSettingsPageProps) -> Html {
    let navigator = use_navigator();
    let load = use_state(|| SettingsLoad::Loading);
    let validation_errors = use_state(Vec::<WorkspaceValidationError>::new);
    let server_error = use_state(|| None::<OrgApiError>);
    let busy = use_state(|| false);
    let pending = use_state(|| None::<UpdateWorkspaceBody>);
    let reload_tick = use_state(|| 0_u64);

    {
        let workspace_id = props.workspace_id.clone();
        let load = load.clone();
        let reload = *reload_tick;
        use_effect_with((workspace_id.clone(), reload), move |_| {
            load.set(SettingsLoad::Loading);
            wasm_bindgen_futures::spawn_local(async move {
                match org_api::get_workspace(&workspace_id).await {
                    Ok(workspace) => {
                        let draft = WorkspaceDraft::from_workspace(&workspace);
                        load.set(SettingsLoad::Ready { workspace, draft });
                    }
                    Err(error) => load.set(SettingsLoad::Failed(error)),
                }
            });
            || ()
        });
    }

    let on_cancel = {
        let navigator = navigator.clone();
        let workspace_id = props.workspace_id.clone();
        Callback::from(move |(): ()| {
            if let Some(navigator) = &navigator {
                navigator.push(&Route::OrgWorkspace {
                    workspace_id: workspace_id.clone(),
                });
            }
        })
    };
    let on_reload = {
        let reload_tick = reload_tick.clone();
        let server_error = server_error.clone();
        let pending = pending.clone();
        Callback::from(move |(): ()| {
            server_error.set(None);
            pending.set(None);
            reload_tick.set((*reload_tick).saturating_add(1));
        })
    };

    match &*load {
        SettingsLoad::Loading => html! {
            <section class="stack org-workspace-management-page" data-testid="org-workspace-settings-loading"><h2 class="page-title">{ "Loading workspace settings" }</h2><p class="loading">{ "Loading the latest workspace revision…" }</p></section>
        },
        SettingsLoad::Failed(error) => html! {
            <section class="stack org-workspace-management-page" data-testid="org-workspace-settings-error"><h2 class="page-title">{ "Workspace settings unavailable" }</h2><div class="org-workspace-form-errors" role="alert"><strong>{ error.message.clone() }</strong><code>{ error.code.clone() }</code></div><button type="button" class="btn btn-outline" onclick={{ let callback = on_reload.clone(); Callback::from(move |_| callback.emit(())) }}>{ "Retry load" }</button></section>
        },
        SettingsLoad::Ready { workspace, .. } if workspace.archived_at.is_some() => html! {
            <section class="stack org-workspace-management-page" data-testid="org-workspace-settings-archived"><p class="org-kicker">{ "Archived workspace" }</p><h2 class="page-title">{ workspace.display_name.clone() }</h2><div class="org-workspace-form-errors" role="alert"><strong>{ "Archived workspaces cannot be edited" }</strong><span>{ "Workspace history and operations remain readable." }</span></div><button type="button" class="btn btn-outline" onclick={{ let callback = on_cancel.clone(); Callback::from(move |_| callback.emit(())) }}>{ "Back to workspace" }</button></section>
        },
        SettingsLoad::Ready { workspace, draft } => {
            let on_change = {
                let load = load.clone();
                let workspace = workspace.clone();
                let validation_errors = validation_errors.clone();
                Callback::from(move |draft: WorkspaceDraft| {
                    validation_errors.set(Vec::new());
                    load.set(SettingsLoad::Ready {
                        workspace: workspace.clone(),
                        draft,
                    });
                })
            };
            let on_submit = {
                let draft = draft.clone();
                let revision = workspace.revision;
                let workspace_id = workspace.id.clone();
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
                    let body = UpdateWorkspaceBody::from_draft(
                        MutationSubmission::new().operation_id,
                        revision,
                        &draft,
                    );
                    pending.set(Some(body.clone()));
                    submit_update(
                        workspace_id.clone(),
                        body,
                        busy.clone(),
                        server_error.clone(),
                        navigator.clone(),
                    );
                })
            };
            let on_retry = {
                let workspace_id = workspace.id.clone();
                let pending = pending.clone();
                let busy = busy.clone();
                let server_error = server_error.clone();
                let navigator = navigator.clone();
                Callback::from(move |_| {
                    if *busy {
                        return;
                    }
                    if let Some(body) = (*pending).clone() {
                        submit_update(
                            workspace_id.clone(),
                            body,
                            busy.clone(),
                            server_error.clone(),
                            navigator.clone(),
                        );
                    }
                })
            };
            html! {
                <section class="stack org-workspace-management-page" aria-labelledby="org-workspace-settings-title" data-testid="org-workspace-settings-page">
                    <div class="page-head"><div><p class="org-kicker">{ "Workspace configuration" }</p><h2 id="org-workspace-settings-title" class="page-title">{ format!("Edit {}", workspace.display_name) }</h2><p class="page-hint">{ format!("Revision {} · policy schema {}", workspace.revision, workspace.policy_schema_version) }</p></div></div>
                    <OrgWorkspaceForm
                        draft={draft.clone()}
                        mode={WorkspaceFormMode::Edit}
                        validation_errors={(*validation_errors).clone()}
                        server_error={(*server_error).clone()}
                        busy={*busy}
                        {on_change}
                        {on_submit}
                        on_cancel={on_cancel}
                        on_retry={Some(on_retry)}
                        on_reload={Some(on_reload)}
                    />
                </section>
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::workspace_management::WorkspacePolicy;

    fn workspace(archived: bool) -> Workspace {
        Workspace {
            id: "10000000-0000-4000-8000-000000000001".into(),
            slug: "delivery".into(),
            display_name: "Delivery".into(),
            description: "Release operations".into(),
            timezone: "Asia/Shanghai".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
            revision: 4,
            archived_at: archived.then_some(1),
        }
    }

    #[test]
    fn settings_state_preserves_revision_and_marks_archived_workspaces() {
        let active = workspace(false);
        let draft = WorkspaceDraft::from_workspace(&active);
        let state = SettingsLoad::Ready {
            workspace: active.clone(),
            draft: draft.clone(),
        };
        assert!(matches!(state, SettingsLoad::Ready { workspace, .. } if workspace.revision == 4));
        assert!(workspace(true).archived_at.is_some());
        assert_eq!(draft.policy, active.policy);
    }
}
