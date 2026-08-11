use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::org::{
    api::OrgApiError,
    workspace_management::{
        ClaimPolicy, TagRule, WorkspaceDraft, WorkspaceValidationError, WORK_ITEM_TYPES,
    },
};

pub const WORKSPACE_FORM_SECTIONS: [&str; 10] = [
    "Identity",
    "Work types",
    "Workflow states",
    "Transitions",
    "State roles",
    "State groups",
    "Review policy",
    "Dispatch policy",
    "Limits",
    "Tag rules",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceFormMode {
    Create,
    Edit,
}

impl WorkspaceFormMode {
    fn submit_label(self) -> &'static str {
        match self {
            Self::Create => "Create workspace",
            Self::Edit => "Save workspace",
        }
    }
}

#[derive(Properties, Clone, PartialEq)]
pub struct OrgWorkspaceFormProps {
    pub draft: WorkspaceDraft,
    pub mode: WorkspaceFormMode,
    #[prop_or_default]
    pub validation_errors: Vec<WorkspaceValidationError>,
    #[prop_or_default]
    pub server_error: Option<OrgApiError>,
    #[prop_or(false)]
    pub busy: bool,
    pub on_change: Callback<WorkspaceDraft>,
    pub on_submit: Callback<()>,
    pub on_cancel: Callback<()>,
    #[prop_or_default]
    pub on_retry: Option<Callback<()>>,
    #[prop_or_default]
    pub on_reload: Option<Callback<()>>,
}

fn text_input(
    draft: WorkspaceDraft,
    on_change: Callback<WorkspaceDraft>,
    update: impl Fn(&mut WorkspaceDraft, String) + 'static,
) -> Callback<InputEvent> {
    Callback::from(move |event: InputEvent| {
        let input: HtmlInputElement = event.target_unchecked_into();
        let mut next = draft.clone();
        update(&mut next, input.value());
        on_change.emit(next);
    })
}

fn textarea_input(
    draft: WorkspaceDraft,
    on_change: Callback<WorkspaceDraft>,
    update: impl Fn(&mut WorkspaceDraft, String) + 'static,
) -> Callback<InputEvent> {
    Callback::from(move |event: InputEvent| {
        let input: HtmlTextAreaElement = event.target_unchecked_into();
        let mut next = draft.clone();
        update(&mut next, input.value());
        on_change.emit(next);
    })
}

fn select_input(
    draft: WorkspaceDraft,
    on_change: Callback<WorkspaceDraft>,
    update: impl Fn(&mut WorkspaceDraft, String) + 'static,
) -> Callback<Event> {
    Callback::from(move |event: Event| {
        let input: HtmlSelectElement = event.target_unchecked_into();
        let mut next = draft.clone();
        update(&mut next, input.value());
        on_change.emit(next);
    })
}

fn field_error<'a>(errors: &'a [WorkspaceValidationError], field: &str) -> Option<&'a str> {
    errors
        .iter()
        .find(|error| error.field == field)
        .map(|error| error.message.as_str())
}

fn humanize(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                let mut characters = part.chars();
                characters
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                    .unwrap_or_default()
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn state_select(
    id: &'static str,
    label: &'static str,
    value: &str,
    states: &[String],
    onchange: Callback<Event>,
) -> Html {
    html! {
        <label class="org-workspace-form-field" for={id}>
            <span>{ label }</span>
            <select id={id} class="input" value={value.to_owned()} {onchange}>
                { for states.iter().map(|state| html! {
                    <option value={state.clone()} selected={state == value}>{ state }</option>
                }) }
            </select>
        </label>
    }
}

fn state_group(
    legend: &'static str,
    testid: &'static str,
    states: &[String],
    selected: &[String],
    on_toggle: Callback<(String, bool)>,
) -> Html {
    html! {
        <fieldset class="org-workspace-form-checks" data-testid={testid}>
            <legend>{ legend }</legend>
            { for states.iter().map(|state| {
                let state_value = state.clone();
                let on_toggle = on_toggle.clone();
                html! {
                    <label class="org-workspace-form-check">
                        <input
                            type="checkbox"
                            checked={selected.contains(state)}
                            onchange={Callback::from(move |event: Event| {
                                let input: HtmlInputElement = event.target_unchecked_into();
                                on_toggle.emit((state_value.clone(), input.checked()));
                            })}
                        />
                        <span>{ state }</span>
                    </label>
                }
            }) }
        </fieldset>
    }
}

#[function_component(OrgWorkspaceForm)]
pub fn org_workspace_form(props: &OrgWorkspaceFormProps) -> Html {
    let draft = &props.draft;
    let on_form_submit = {
        let on_submit = props.on_submit.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            on_submit.emit(());
        })
    };
    let on_cancel = {
        let on_cancel = props.on_cancel.clone();
        Callback::from(move |_| on_cancel.emit(()))
    };

    let on_slug = text_input(draft.clone(), props.on_change.clone(), |draft, value| {
        draft.slug = value
    });
    let on_display_name = text_input(draft.clone(), props.on_change.clone(), |draft, value| {
        draft.display_name = value
    });
    let on_description = textarea_input(draft.clone(), props.on_change.clone(), |draft, value| {
        draft.description = value
    });
    let on_timezone = text_input(draft.clone(), props.on_change.clone(), |draft, value| {
        draft.timezone = value
    });

    let role_selects = [
        (
            "initial-state",
            "Initial state",
            draft.policy.initial_state.clone(),
            0_usize,
        ),
        (
            "running-state",
            "Running state",
            draft.policy.running_state.clone(),
            1,
        ),
        (
            "review-state",
            "Review state",
            draft.policy.review_state.clone(),
            2,
        ),
        (
            "failed-state",
            "Failed state",
            draft.policy.failed_state.clone(),
            3,
        ),
        (
            "cancelled-state",
            "Cancelled state",
            draft.policy.cancelled_state.clone(),
            4,
        ),
        (
            "release-state",
            "Release state",
            draft.policy.release_state.clone(),
            5,
        ),
        (
            "review-rejection-state",
            "Review rejection state",
            draft.policy.review_rejection_state.clone(),
            6,
        ),
        (
            "lease-expiry-recovery-state",
            "Lease expiry recovery state",
            draft.policy.lease_expiry_recovery_state.clone(),
            7,
        ),
    ];

    let set_role = |index: usize| {
        select_input(
            draft.clone(),
            props.on_change.clone(),
            move |draft, value| match index {
                0 => draft.policy.initial_state = value,
                1 => draft.policy.running_state = value,
                2 => draft.policy.review_state = value,
                3 => draft.policy.failed_state = value,
                4 => draft.policy.cancelled_state = value,
                5 => draft.policy.release_state = value,
                6 => draft.policy.review_rejection_state = value,
                _ => draft.policy.lease_expiry_recovery_state = value,
            },
        )
    };

    html! {
        <form class="org-workspace-form stack" onsubmit={on_form_submit} data-testid="org-workspace-form" novalidate=true>
            if !props.validation_errors.is_empty() {
                <div class="org-workspace-form-errors" role="alert" tabindex="-1" data-testid="org-workspace-validation-errors">
                    <strong>{ "Review the workspace configuration" }</strong>
                    <ul>{ for props.validation_errors.iter().map(|error| html! {
                        <li><code>{ error.field.clone() }</code>{ format!(": {}", error.message) }</li>
                    }) }</ul>
                </div>
            }
            if let Some(error) = &props.server_error {
                <div class="org-workspace-form-errors" role="alert" data-testid="org-workspace-server-error">
                    <strong>{ error.message.clone() }</strong>
                    <code>{ error.code.clone() }</code>
                    <div class="org-workspace-form-inline-actions">
                        if error.retryable {
                            if let Some(on_retry) = &props.on_retry {
                                <button type="button" class="btn btn-outline" onclick={{ let callback = on_retry.clone(); Callback::from(move |_| callback.emit(())) }}>{ "Retry" }</button>
                            }
                        }
                        if error.code == "stale_revision" {
                            if let Some(on_reload) = &props.on_reload {
                                <button type="button" class="btn btn-outline" onclick={{ let callback = on_reload.clone(); Callback::from(move |_| callback.emit(())) }}>{ "Reload latest" }</button>
                            }
                        }
                    </div>
                </div>
            }

            <section class="org-workspace-form-section" aria-labelledby="workspace-identity-title" data-testid="org-workspace-form-identity">
                <div class="org-workspace-form-section-head"><span>{ "01" }</span><div><h3 id="workspace-identity-title">{ "Identity" }</h3><p>{ "Stable routing and operator-facing workspace metadata." }</p></div></div>
                <div class="org-workspace-form-grid">
                    <label class="org-workspace-form-field" for="workspace-slug"><span>{ "Slug" }</span><input id="workspace-slug" class="input" value={draft.slug.clone()} oninput={on_slug} aria-invalid={field_error(&props.validation_errors, "slug").is_some().to_string()} /></label>
                    <label class="org-workspace-form-field" for="workspace-display-name"><span>{ "Display name" }</span><input id="workspace-display-name" class="input" value={draft.display_name.clone()} oninput={on_display_name} /></label>
                    <label class="org-workspace-form-field org-workspace-form-wide" for="workspace-description"><span>{ "Description" }</span><textarea id="workspace-description" class="input" value={draft.description.clone()} oninput={on_description} /></label>
                    <label class="org-workspace-form-field" for="workspace-timezone"><span>{ "IANA timezone" }</span><input id="workspace-timezone" class="input" value={draft.timezone.clone()} oninput={on_timezone} placeholder="Asia/Shanghai" /></label>
                    <div class="org-workspace-form-field"><span>{ "Policy schema" }</span><output>{ draft.policy_schema_version }</output></div>
                </div>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-types-title" data-testid="org-workspace-form-work-types">
                <div class="org-workspace-form-section-head"><span>{ "02" }</span><div><h3 id="workspace-types-title">{ "Work types" }</h3><p>{ "Types accepted by this workspace policy." }</p></div></div>
                <fieldset class="org-workspace-form-checks"><legend>{ "Allowed work types" }</legend>{ for WORK_ITEM_TYPES.iter().map(|item_type| {
                    let value = (*item_type).to_owned();
                    let base = draft.clone();
                    let on_change = props.on_change.clone();
                    html! { <label class="org-workspace-form-check"><input type="checkbox" checked={draft.policy.allowed_types.iter().any(|current| current == item_type)} onchange={Callback::from(move |event: Event| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = base.clone(); next.toggle_allowed_type(&value, input.checked()); on_change.emit(next); })}/><span>{ humanize(item_type) }</span></label> }
                }) }</fieldset>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-states-title" data-testid="org-workspace-form-states">
                <div class="org-workspace-form-section-head"><span>{ "03" }</span><div><h3 id="workspace-states-title">{ "Workflow states" }</h3><p>{ "Canonical state vocabulary used by Org headings." }</p></div></div>
                <div class="org-workspace-form-rows">{ for draft.policy.states.iter().enumerate().map(|(index, state)| {
                    let base = draft.clone(); let on_change = props.on_change.clone();
                    let remove_base = draft.clone(); let remove_change = props.on_change.clone();
                    html! { <div class="org-workspace-form-row"><input class="input" aria-label={format!("Workflow state {}", index + 1)} value={state.clone()} oninput={Callback::from(move |event: InputEvent| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = base.clone(); next.set_state(index, input.value()); on_change.emit(next); })}/><button type="button" class="btn btn-ghost" onclick={Callback::from(move |_| { let mut next = remove_base.clone(); next.remove_state(index); remove_change.emit(next); })}>{ "Remove" }</button></div> }
                }) }</div>
                <button type="button" class="btn btn-outline" onclick={{ let base = draft.clone(); let on_change = props.on_change.clone(); Callback::from(move |_| { let mut next = base.clone(); next.add_state("NEW_STATE"); on_change.emit(next); }) }}>{ "Add state" }</button>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-transitions-title" data-testid="org-workspace-form-transitions">
                <div class="org-workspace-form-section-head"><span>{ "04" }</span><div><h3 id="workspace-transitions-title">{ "Transitions" }</h3><p>{ "Allowed directional movement between configured states." }</p></div></div>
                <div class="org-workspace-form-rows">{ for draft.policy.transitions.iter().enumerate().map(|(index, (from, to))| {
                    let from_value = from.clone(); let to_value = to.clone();
                    let from_base = draft.clone(); let from_change = props.on_change.clone(); let to_for_from = to.clone();
                    let to_base = draft.clone(); let to_change = props.on_change.clone(); let from_for_to = from.clone();
                    let remove_base = draft.clone(); let remove_change = props.on_change.clone();
                    html! { <div class="org-workspace-form-transition"><select class="input" aria-label={format!("Transition {} from", index + 1)} value={from_value.clone()} onchange={Callback::from(move |event: Event| { let input: HtmlSelectElement = event.target_unchecked_into(); let mut next = from_base.clone(); next.set_transition(index, input.value(), to_for_from.clone()); from_change.emit(next); })}>{ for draft.policy.states.iter().map(|state| html!{<option value={state.clone()} selected={state == &from_value}>{state}</option>}) }</select><span aria-hidden="true">{ "→" }</span><select class="input" aria-label={format!("Transition {} to", index + 1)} value={to_value.clone()} onchange={Callback::from(move |event: Event| { let input: HtmlSelectElement = event.target_unchecked_into(); let mut next = to_base.clone(); next.set_transition(index, from_for_to.clone(), input.value()); to_change.emit(next); })}>{ for draft.policy.states.iter().map(|state| html!{<option value={state.clone()} selected={state == &to_value}>{state}</option>}) }</select><button type="button" class="btn btn-ghost" onclick={Callback::from(move |_| { let mut next = remove_base.clone(); next.remove_transition(index); remove_change.emit(next); })}>{ "Remove" }</button></div> }
                }) }</div>
                <button type="button" class="btn btn-outline" disabled={draft.policy.states.is_empty()} onclick={{ let base = draft.clone(); let on_change = props.on_change.clone(); let first = draft.policy.states.first().cloned().unwrap_or_default(); Callback::from(move |_| { let mut next = base.clone(); next.add_transition(&first, &first); on_change.emit(next); }) }}>{ "Add transition" }</button>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-roles-title" data-testid="org-workspace-form-state-roles">
                <div class="org-workspace-form-section-head"><span>{ "05" }</span><div><h3 id="workspace-roles-title">{ "State roles" }</h3><p>{ "Semantic roles used by execution and recovery workflows." }</p></div></div>
                <div class="org-workspace-form-grid">{ for role_selects.into_iter().map(|(id, label, value, index)| state_select(id, label, &value, &draft.policy.states, set_role(index))) }</div>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-groups-title" data-testid="org-workspace-form-state-groups">
                <div class="org-workspace-form-section-head"><span>{ "06" }</span><div><h3 id="workspace-groups-title">{ "State groups" }</h3><p>{ "Execution eligibility and terminal outcomes." }</p></div></div>
                <div class="org-workspace-form-grid">
                    { state_group("Executable states", "org-executable-states", &draft.policy.states, &draft.policy.executable_states, { let draft = draft.clone(); let on_change = props.on_change.clone(); Callback::from(move |(value, enabled): (String, bool)| { let mut next = draft.clone(); WorkspaceDraft::toggle_policy_value(&mut next.policy.executable_states, &value, enabled); on_change.emit(next); }) }) }
                    { state_group("Successful terminal states", "org-successful-states", &draft.policy.states, &draft.policy.successful_terminal_states, { let draft = draft.clone(); let on_change = props.on_change.clone(); Callback::from(move |(value, enabled): (String, bool)| { let mut next = draft.clone(); WorkspaceDraft::toggle_policy_value(&mut next.policy.successful_terminal_states, &value, enabled); on_change.emit(next); }) }) }
                    { state_group("Terminal states", "org-terminal-states", &draft.policy.states, &draft.policy.terminal_states, { let draft = draft.clone(); let on_change = props.on_change.clone(); Callback::from(move |(value, enabled): (String, bool)| { let mut next = draft.clone(); WorkspaceDraft::toggle_policy_value(&mut next.policy.terminal_states, &value, enabled); on_change.emit(next); }) }) }
                </div>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-review-title" data-testid="org-workspace-form-review-policy">
                <div class="org-workspace-form-section-head"><span>{ "07" }</span><div><h3 id="workspace-review-title">{ "Review policy" }</h3><p>{ "Work types that must pass through review." }</p></div></div>
                <fieldset class="org-workspace-form-checks"><legend>{ "Review-required types" }</legend>{ for draft.policy.allowed_types.iter().map(|item_type| { let value = item_type.clone(); let base = draft.clone(); let on_change = props.on_change.clone(); html!{<label class="org-workspace-form-check"><input type="checkbox" checked={draft.policy.review_required_types.contains(item_type)} onchange={Callback::from(move |event: Event| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = base.clone(); WorkspaceDraft::toggle_policy_value(&mut next.policy.review_required_types, &value, input.checked()); on_change.emit(next); })}/><span>{humanize(item_type)}</span></label>} }) }</fieldset>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-dispatch-title" data-testid="org-workspace-form-dispatch-policy">
                <div class="org-workspace-form-section-head"><span>{ "08" }</span><div><h3 id="workspace-dispatch-title">{ "Dispatch policy" }</h3><p>{ "Claim ownership and cross-workspace agenda behavior." }</p></div></div>
                <div class="org-workspace-form-grid"><label class="org-workspace-form-field" for="workspace-claim-policy"><span>{ "Claim policy" }</span><select id="workspace-claim-policy" class="input" value={draft.policy.claim_policy.as_str()} onchange={select_input(draft.clone(), props.on_change.clone(), |draft, value| { if let Some(policy) = ClaimPolicy::parse(&value) { draft.policy.claim_policy = policy; } })}>{ for ClaimPolicy::ALL.map(|policy| html!{<option value={policy.as_str()} selected={policy == draft.policy.claim_policy}>{humanize(policy.as_str())}</option>}) }</select></label><label class="org-workspace-form-check org-workspace-form-toggle"><input type="checkbox" checked={draft.policy.allow_cross_workspace_agenda} onchange={{ let base = draft.clone(); let on_change = props.on_change.clone(); Callback::from(move |event: Event| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = base.clone(); next.policy.allow_cross_workspace_agenda = input.checked(); on_change.emit(next); }) }}/><span>{ "Allow explicit cross-workspace agenda queries" }</span></label></div>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-limits-title" data-testid="org-workspace-form-limits">
                <div class="org-workspace-form-section-head"><span>{ "09" }</span><div><h3 id="workspace-limits-title">{ "Limits" }</h3><p>{ "Lease, retry, and workspace concurrency controls." }</p></div></div>
                <div class="org-workspace-form-grid"><label class="org-workspace-form-field"><span>{ "Lease duration (seconds)" }</span><input type="number" min="1" class="input" value={draft.policy.lease_duration_secs.to_string()} oninput={text_input(draft.clone(), props.on_change.clone(), |draft, value| draft.policy.lease_duration_secs = value.parse().unwrap_or(0))}/></label><label class="org-workspace-form-field"><span>{ "Retry limit" }</span><input type="number" min="0" class="input" value={draft.policy.retry_limit.to_string()} oninput={text_input(draft.clone(), props.on_change.clone(), |draft, value| draft.policy.retry_limit = value.parse().unwrap_or(0))}/></label><label class="org-workspace-form-field"><span>{ "Concurrency limit" }</span><input type="number" min="1" class="input" value={draft.policy.concurrency_limit.to_string()} oninput={text_input(draft.clone(), props.on_change.clone(), |draft, value| draft.policy.concurrency_limit = value.parse().unwrap_or(0))}/></label></div>
            </section>

            <section class="org-workspace-form-section" aria-labelledby="workspace-tags-title" data-testid="org-workspace-form-tag-rules">
                <div class="org-workspace-form-section-head"><span>{ "10" }</span><div><h3 id="workspace-tags-title">{ "Tag rules" }</h3><p>{ "Comma-separated allowed and required tags per work type." }</p></div></div>
                <div class="org-workspace-form-tag-ledger">{ for draft.policy.allowed_types.iter().map(|item_type| { let rule = draft.policy.tag_rules.get(item_type).cloned().unwrap_or_else(TagRule::default); let allowed = rule.allowed.join(", "); let required = rule.required.join(", "); let allowed_type = item_type.clone(); let required_type = item_type.clone(); let allowed_base = draft.clone(); let allowed_change = props.on_change.clone(); let required_base = draft.clone(); let required_change = props.on_change.clone(); html!{<fieldset><legend>{humanize(item_type)}</legend><label class="org-workspace-form-field"><span>{"Allowed tags"}</span><input class="input" value={allowed} oninput={Callback::from(move |event: InputEvent| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = allowed_base.clone(); let required = next.policy.tag_rules.get(&allowed_type).map(|rule| rule.required.clone()).unwrap_or_default(); next.set_tag_rule(&allowed_type, parse_csv(&input.value()), required); allowed_change.emit(next); })}/></label><label class="org-workspace-form-field"><span>{"Required tags"}</span><input class="input" value={required} oninput={Callback::from(move |event: InputEvent| { let input: HtmlInputElement = event.target_unchecked_into(); let mut next = required_base.clone(); let allowed = next.policy.tag_rules.get(&required_type).map(|rule| rule.allowed.clone()).unwrap_or_default(); next.set_tag_rule(&required_type, allowed, parse_csv(&input.value())); required_change.emit(next); })}/></label></fieldset>} }) }</div>
            </section>

            <div class="org-workspace-form-actions">
                <button type="button" class="btn btn-ghost" onclick={on_cancel} disabled={props.busy}>{ "Cancel" }</button>
                <button type="submit" class="btn btn-primary" disabled={props.busy} data-testid="org-workspace-submit">{ if props.busy { "Saving…" } else { props.mode.submit_label() } }</button>
            </div>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_exposes_every_approved_structured_section() {
        assert_eq!(
            WORKSPACE_FORM_SECTIONS,
            [
                "Identity",
                "Work types",
                "Workflow states",
                "Transitions",
                "State roles",
                "State groups",
                "Review policy",
                "Dispatch policy",
                "Limits",
                "Tag rules",
            ]
        );
        assert_eq!(WorkspaceFormMode::Create.submit_label(), "Create workspace");
        assert_eq!(WorkspaceFormMode::Edit.submit_label(), "Save workspace");
    }

    #[test]
    fn component_contract_accepts_a_controlled_workspace_draft() {
        let view = html! {
            <crate::components::OrgWorkspaceForm
                draft={WorkspaceDraft::new()}
                mode={WorkspaceFormMode::Create}
                on_change={Callback::noop()}
                on_submit={Callback::noop()}
                on_cancel={Callback::noop()}
            />
        };
        assert_ne!(format!("{view:?}"), "");
    }
}
