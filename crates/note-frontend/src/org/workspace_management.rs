use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const WORK_ITEM_TYPES: [&str; 9] = [
    "project",
    "epic",
    "issue",
    "task",
    "subtask",
    "review",
    "approval",
    "incident",
    "milestone",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPolicy {
    Open,
    AssignmentRestricted,
    ExplicitlyDispatched,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagRule {
    pub allowed: Vec<String>,
    pub required: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub allow_cross_workspace_agenda: bool,
    pub allowed_types: Vec<String>,
    pub states: Vec<String>,
    pub transitions: Vec<(String, String)>,
    pub initial_state: String,
    pub running_state: String,
    pub executable_states: Vec<String>,
    pub review_state: String,
    pub failed_state: String,
    pub cancelled_state: String,
    pub successful_terminal_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub release_state: String,
    pub review_rejection_state: String,
    pub lease_expiry_recovery_state: String,
    pub review_required_types: Vec<String>,
    pub claim_policy: ClaimPolicy,
    pub lease_duration_secs: u64,
    pub retry_limit: u32,
    pub concurrency_limit: usize,
    pub tag_rules: BTreeMap<String, TagRule>,
}

impl WorkspacePolicy {
    pub fn engineering_default() -> Self {
        Self {
            allow_cross_workspace_agenda: false,
            allowed_types: WORK_ITEM_TYPES.map(str::to_owned).to_vec(),
            states: [
                "BACKLOG",
                "READY",
                "RUNNING",
                "BLOCKED",
                "REVIEW",
                "DONE",
                "FAILED",
                "CANCELLED",
            ]
            .map(str::to_owned)
            .to_vec(),
            transitions: [
                ("BACKLOG", "READY"),
                ("BACKLOG", "CANCELLED"),
                ("READY", "RUNNING"),
                ("READY", "CANCELLED"),
                ("RUNNING", "BLOCKED"),
                ("RUNNING", "READY"),
                ("RUNNING", "REVIEW"),
                ("RUNNING", "DONE"),
                ("RUNNING", "FAILED"),
                ("RUNNING", "CANCELLED"),
                ("BLOCKED", "READY"),
                ("BLOCKED", "CANCELLED"),
                ("REVIEW", "DONE"),
                ("REVIEW", "READY"),
                ("REVIEW", "CANCELLED"),
                ("FAILED", "READY"),
                ("FAILED", "RUNNING"),
                ("FAILED", "CANCELLED"),
            ]
            .map(|(from, to)| (from.to_owned(), to.to_owned()))
            .to_vec(),
            initial_state: "BACKLOG".into(),
            running_state: "RUNNING".into(),
            executable_states: vec!["READY".into()],
            review_state: "REVIEW".into(),
            failed_state: "FAILED".into(),
            cancelled_state: "CANCELLED".into(),
            successful_terminal_states: vec!["DONE".into()],
            terminal_states: vec!["DONE".into(), "CANCELLED".into()],
            release_state: "READY".into(),
            review_rejection_state: "READY".into(),
            lease_expiry_recovery_state: "READY".into(),
            review_required_types: Vec::new(),
            claim_policy: ClaimPolicy::AssignmentRestricted,
            lease_duration_secs: 900,
            retry_limit: 2,
            concurrency_limit: 4,
            tag_rules: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDraft {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
}

impl Default for WorkspaceDraft {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceDraft {
    pub fn new() -> Self {
        Self {
            slug: String::new(),
            display_name: String::new(),
            description: String::new(),
            timezone: "UTC".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
        }
    }

    pub fn from_workspace(workspace: &super::model::Workspace) -> Self {
        Self {
            slug: workspace.slug.clone(),
            display_name: workspace.display_name.clone(),
            description: workspace.description.clone(),
            timezone: workspace.timezone.clone(),
            policy_schema_version: workspace.policy_schema_version,
            policy: workspace.policy.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), Vec<WorkspaceValidationError>> {
        let mut errors = Vec::new();
        required(&mut errors, "slug", &self.slug);
        required(&mut errors, "display_name", &self.display_name);
        required(&mut errors, "timezone", &self.timezone);
        if self.timezone.parse::<chrono_tz::Tz>().is_err() {
            push_error(&mut errors, "timezone", "Use a valid IANA timezone");
        }
        if self.policy_schema_version != 1 {
            push_error(
                &mut errors,
                "policy_schema_version",
                "Only policy schema version 1 is supported",
            );
        }

        validate_policy(&self.policy, &mut errors);
        errors.is_empty().then_some(()).ok_or(errors)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceValidationError {
    pub field: String,
    pub message: String,
}

fn required(errors: &mut Vec<WorkspaceValidationError>, field: &str, value: &str) {
    if value.trim().is_empty() {
        push_error(errors, field, "This field is required");
    }
}

fn push_error(errors: &mut Vec<WorkspaceValidationError>, field: &str, message: &str) {
    errors.push(WorkspaceValidationError {
        field: field.into(),
        message: message.into(),
    });
}

fn duplicates(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(|value| value.trim())
        .any(|value| value.is_empty() || !seen.insert(value))
}

fn validate_policy(policy: &WorkspacePolicy, errors: &mut Vec<WorkspaceValidationError>) {
    if policy.allowed_types.is_empty() || duplicates(&policy.allowed_types) {
        push_error(
            errors,
            "policy.allowed_types",
            "Select unique supported work types",
        );
    }
    if policy
        .allowed_types
        .iter()
        .any(|value| !WORK_ITEM_TYPES.contains(&value.as_str()))
    {
        push_error(
            errors,
            "policy.allowed_types",
            "An unsupported work type is selected",
        );
    }
    if policy.states.is_empty() || duplicates(&policy.states) {
        push_error(
            errors,
            "policy.states",
            "State names must be non-empty and unique",
        );
    }
    let states = policy
        .states
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut transitions = BTreeSet::new();
    if policy.transitions.iter().any(|(from, to)| {
        from.trim().is_empty()
            || to.trim().is_empty()
            || !states.contains(from.as_str())
            || !states.contains(to.as_str())
            || !transitions.insert((from.as_str(), to.as_str()))
    }) {
        push_error(
            errors,
            "policy.transitions",
            "Transitions must be unique and reference configured states",
        );
    }

    for (field, state) in [
        ("policy.initial_state", &policy.initial_state),
        ("policy.running_state", &policy.running_state),
        ("policy.review_state", &policy.review_state),
        ("policy.failed_state", &policy.failed_state),
        ("policy.cancelled_state", &policy.cancelled_state),
        ("policy.release_state", &policy.release_state),
        (
            "policy.review_rejection_state",
            &policy.review_rejection_state,
        ),
        (
            "policy.lease_expiry_recovery_state",
            &policy.lease_expiry_recovery_state,
        ),
    ] {
        if !states.contains(state.as_str()) {
            push_error(errors, field, "Select a configured workflow state");
        }
    }

    for (field, values) in [
        ("policy.executable_states", &policy.executable_states),
        (
            "policy.successful_terminal_states",
            &policy.successful_terminal_states,
        ),
        ("policy.terminal_states", &policy.terminal_states),
    ] {
        if duplicates(values) || values.iter().any(|value| !states.contains(value.as_str())) {
            push_error(errors, field, "Select unique configured workflow states");
        }
    }
    let executable = policy
        .executable_states
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for (field, state) in [
        ("policy.release_state", &policy.release_state),
        (
            "policy.review_rejection_state",
            &policy.review_rejection_state,
        ),
        (
            "policy.lease_expiry_recovery_state",
            &policy.lease_expiry_recovery_state,
        ),
    ] {
        if !executable.contains(state.as_str()) {
            push_error(errors, field, "Recovery states must be executable");
        }
    }
    let terminal = policy
        .terminal_states
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if policy
        .successful_terminal_states
        .iter()
        .any(|state| !terminal.contains(state.as_str()))
    {
        push_error(
            errors,
            "policy.successful_terminal_states",
            "Successful states must also be terminal",
        );
    }
    if policy.lease_duration_secs == 0 {
        push_error(
            errors,
            "policy.lease_duration_secs",
            "Lease duration must be greater than zero",
        );
    }
    if policy.concurrency_limit == 0 {
        push_error(
            errors,
            "policy.concurrency_limit",
            "Concurrency limit must be greater than zero",
        );
    }
    if duplicates(&policy.review_required_types)
        || policy
            .review_required_types
            .iter()
            .any(|value| !policy.allowed_types.contains(value))
    {
        push_error(
            errors,
            "policy.review_required_types",
            "Review-required types must be unique allowed work types",
        );
    }
    for (item_type, rule) in &policy.tag_rules {
        if !policy.allowed_types.contains(item_type) {
            push_error(
                errors,
                &format!("policy.tag_rules.{item_type}"),
                "Tag rules require an allowed work type",
            );
        }
        if duplicates(&rule.allowed) {
            push_error(
                errors,
                &format!("policy.tag_rules.{item_type}.allowed"),
                "Allowed tags must be non-empty and unique",
            );
        }
        if duplicates(&rule.required)
            || rule
                .required
                .iter()
                .any(|required| !rule.allowed.contains(required))
        {
            push_error(
                errors,
                &format!("policy.tag_rules.{item_type}.required"),
                "Required tags must be unique allowed tags",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::model::Workspace;
    use std::collections::BTreeMap;

    fn valid_draft() -> WorkspaceDraft {
        WorkspaceDraft {
            slug: "release-operations".into(),
            display_name: "Release operations".into(),
            description: "Coordinates production releases".into(),
            timezone: "Asia/Shanghai".into(),
            policy_schema_version: 1,
            policy: WorkspacePolicy::engineering_default(),
        }
    }

    #[test]
    fn engineering_default_is_a_complete_valid_policy() {
        let policy = WorkspacePolicy::engineering_default();
        assert_eq!(policy.allowed_types, WORK_ITEM_TYPES.map(str::to_owned));
        assert_eq!(policy.states.len(), 8);
        assert_eq!(policy.initial_state, "BACKLOG");
        assert!(policy
            .transitions
            .contains(&("READY".into(), "RUNNING".into())));
        assert_eq!(policy.claim_policy, ClaimPolicy::AssignmentRestricted);
        assert_eq!(policy.lease_duration_secs, 900);
        assert_eq!(policy.retry_limit, 2);
        assert_eq!(policy.concurrency_limit, 4);
        assert!(policy.tag_rules.is_empty());
        assert!(valid_draft().validate().is_ok());
    }

    #[test]
    fn validation_reports_identity_transition_role_limit_and_tag_errors() {
        let mut draft = valid_draft();
        draft.slug.clear();
        draft.timezone = "not/a-zone".into();
        draft.policy.states.push("READY".into());
        draft
            .policy
            .transitions
            .push(("MISSING".into(), "DONE".into()));
        draft.policy.initial_state = "UNKNOWN".into();
        draft.policy.concurrency_limit = 0;
        draft.policy.tag_rules.insert(
            "task".into(),
            TagRule {
                allowed: vec!["ops".into()],
                required: vec!["release".into()],
            },
        );

        let errors = draft.validate().unwrap_err();
        for field in [
            "slug",
            "timezone",
            "policy.states",
            "policy.transitions",
            "policy.initial_state",
            "policy.concurrency_limit",
            "policy.tag_rules.task.required",
        ] {
            assert!(
                errors.iter().any(|error| error.field == field),
                "missing validation error for {field}: {errors:?}"
            );
        }
    }

    #[test]
    fn non_default_policy_round_trips_without_losing_fields() {
        let mut policy = WorkspacePolicy::engineering_default();
        policy.allow_cross_workspace_agenda = true;
        policy.claim_policy = ClaimPolicy::ExplicitlyDispatched;
        policy.review_required_types = vec!["approval".into(), "review".into()];
        policy.tag_rules = BTreeMap::from([(
            "incident".into(),
            TagRule {
                allowed: vec!["sev1".into(), "sev2".into()],
                required: vec!["sev1".into()],
            },
        )]);

        let json = serde_json::to_value(&policy).unwrap();
        let decoded: WorkspacePolicy = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, policy);
    }

    #[test]
    fn stored_workspace_round_trips_into_an_edit_draft_without_loss() {
        let mut policy = WorkspacePolicy::engineering_default();
        policy.retry_limit = 8;
        let workspace = Workspace {
            id: "10000000-0000-4000-8000-000000000001".into(),
            slug: "production".into(),
            display_name: "Production".into(),
            description: "Production operations".into(),
            timezone: "Europe/London".into(),
            policy_schema_version: 1,
            policy: policy.clone(),
            revision: 12,
            archived_at: None,
        };

        let draft = WorkspaceDraft::from_workspace(&workspace);
        assert_eq!(draft.slug, workspace.slug);
        assert_eq!(draft.display_name, workspace.display_name);
        assert_eq!(draft.description, workspace.description);
        assert_eq!(draft.timezone, workspace.timezone);
        assert_eq!(draft.policy_schema_version, 1);
        assert_eq!(draft.policy, policy);
    }
}
