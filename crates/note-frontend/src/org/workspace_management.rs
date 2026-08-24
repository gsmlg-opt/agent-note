use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::mutation::MutationEnvelope;

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

impl ClaimPolicy {
    pub const ALL: [Self; 3] = [
        Self::Open,
        Self::AssignmentRestricted,
        Self::ExplicitlyDispatched,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::AssignmentRestricted => "assignment_restricted",
            Self::ExplicitlyDispatched => "explicitly_dispatched",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "assignment_restricted" => Some(Self::AssignmentRestricted),
            "explicitly_dispatched" => Some(Self::ExplicitlyDispatched),
            _ => None,
        }
    }
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

    pub fn toggle_allowed_type(&mut self, item_type: &str, enabled: bool) {
        if enabled {
            if WORK_ITEM_TYPES.contains(&item_type)
                && !self
                    .policy
                    .allowed_types
                    .iter()
                    .any(|value| value == item_type)
            {
                self.policy.allowed_types.push(item_type.into());
                self.policy.allowed_types.sort_by_key(|value| {
                    WORK_ITEM_TYPES
                        .iter()
                        .position(|candidate| candidate == value)
                        .unwrap_or(usize::MAX)
                });
            }
        } else {
            self.policy.allowed_types.retain(|value| value != item_type);
            self.policy
                .review_required_types
                .retain(|value| value != item_type);
            self.policy.tag_rules.remove(item_type);
        }
    }

    pub fn add_state(&mut self, state: &str) {
        let state = state.trim();
        if !state.is_empty() && !self.policy.states.iter().any(|value| value == state) {
            self.policy.states.push(state.into());
        }
    }

    pub fn set_state(&mut self, index: usize, state: String) {
        if let Some(current) = self.policy.states.get_mut(index) {
            let previous = std::mem::replace(current, state.clone());
            for (from, to) in &mut self.policy.transitions {
                if *from == previous {
                    *from = state.clone();
                }
                if *to == previous {
                    *to = state.clone();
                }
            }
            for values in [
                &mut self.policy.executable_states,
                &mut self.policy.successful_terminal_states,
                &mut self.policy.terminal_states,
            ] {
                for value in values {
                    if *value == previous {
                        *value = state.clone();
                    }
                }
            }
            for value in [
                &mut self.policy.initial_state,
                &mut self.policy.running_state,
                &mut self.policy.review_state,
                &mut self.policy.failed_state,
                &mut self.policy.cancelled_state,
                &mut self.policy.release_state,
                &mut self.policy.review_rejection_state,
                &mut self.policy.lease_expiry_recovery_state,
            ] {
                if *value == previous {
                    *value = state.clone();
                }
            }
        }
    }

    pub fn remove_state(&mut self, index: usize) {
        if index >= self.policy.states.len() {
            return;
        }
        let removed = self.policy.states.remove(index);
        self.policy
            .transitions
            .retain(|(from, to)| from != &removed && to != &removed);
        for values in [
            &mut self.policy.executable_states,
            &mut self.policy.successful_terminal_states,
            &mut self.policy.terminal_states,
        ] {
            values.retain(|value| value != &removed);
        }
    }

    pub fn add_transition(&mut self, from: &str, to: &str) {
        let transition = (from.to_owned(), to.to_owned());
        if !self.policy.transitions.contains(&transition) {
            self.policy.transitions.push(transition);
        }
    }

    pub fn set_transition(&mut self, index: usize, from: String, to: String) {
        if let Some(transition) = self.policy.transitions.get_mut(index) {
            *transition = (from, to);
        }
    }

    pub fn remove_transition(&mut self, index: usize) {
        if index < self.policy.transitions.len() {
            self.policy.transitions.remove(index);
        }
    }

    pub fn toggle_policy_value(values: &mut Vec<String>, value: &str, enabled: bool) {
        if enabled && !values.iter().any(|current| current == value) {
            values.push(value.into());
        } else if !enabled {
            values.retain(|current| current != value);
        }
    }

    pub fn set_tag_rule(&mut self, item_type: &str, allowed: Vec<String>, required: Vec<String>) {
        if allowed.is_empty() && required.is_empty() {
            self.policy.tag_rules.remove(item_type);
        } else {
            self.policy
                .tag_rules
                .insert(item_type.into(), TagRule { allowed, required });
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub workspace_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
}

impl CreateWorkspaceBody {
    pub fn from_draft(workspace_id: String, operation_id: String, draft: &WorkspaceDraft) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            workspace_id,
            slug: draft.slug.clone(),
            display_name: draft.display_name.clone(),
            description: draft.description.clone(),
            timezone: draft.timezone.clone(),
            policy_schema_version: draft.policy_schema_version,
            policy: draft.policy.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkspaceBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub expected_revision: i64,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    pub policy: WorkspacePolicy,
}

impl UpdateWorkspaceBody {
    pub fn from_draft(
        operation_id: String,
        expected_revision: i64,
        draft: &WorkspaceDraft,
    ) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            expected_revision,
            slug: draft.slug.clone(),
            display_name: draft.display_name.clone(),
            description: draft.description.clone(),
            timezone: draft.timezone.clone(),
            policy_schema_version: draft.policy_schema_version,
            policy: draft.policy.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveWorkspaceBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub expected_revision: i64,
}

impl ArchiveWorkspaceBody {
    pub fn new(operation_id: String, expected_revision: i64) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            expected_revision,
        }
    }
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
    use crate::org::mutation::{new_id, MutationSubmission, WEB_ACTOR_ID};
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

    #[test]
    fn workspace_mutation_bodies_match_the_existing_rest_contract() {
        let draft = valid_draft();
        let create = CreateWorkspaceBody::from_draft(
            "10000000-0000-4000-8000-000000000001".into(),
            "20000000-0000-4000-8000-000000000001".into(),
            &draft,
        );
        let update = UpdateWorkspaceBody::from_draft(
            "20000000-0000-4000-8000-000000000002".into(),
            7,
            &draft,
        );
        let archive = ArchiveWorkspaceBody::new("20000000-0000-4000-8000-000000000003".into(), 8);

        let create_json = serde_json::to_value(create).unwrap();
        assert_eq!(create_json["schema_version"], 1);
        assert_eq!(create_json["actor_id"], WEB_ACTOR_ID);
        assert_eq!(
            create_json["workspace_id"],
            "10000000-0000-4000-8000-000000000001"
        );
        assert_eq!(create_json["slug"], draft.slug);
        assert_eq!(
            create_json["policy"],
            serde_json::to_value(&draft.policy).unwrap()
        );

        let update_json = serde_json::to_value(update).unwrap();
        assert_eq!(update_json["actor_id"], WEB_ACTOR_ID);
        assert_eq!(update_json["expected_revision"], 7);
        assert_eq!(
            update_json["operation_id"],
            "20000000-0000-4000-8000-000000000002"
        );

        let archive_json = serde_json::to_value(archive).unwrap();
        assert_eq!(archive_json["actor_id"], WEB_ACTOR_ID);
        assert_eq!(archive_json["expected_revision"], 8);
        assert_eq!(archive_json.as_object().unwrap().len(), 4);
    }

    #[test]
    fn submission_identity_is_a_valid_uuid_and_is_reused_for_retry() {
        let submission = MutationSubmission::new();
        assert!(uuid::Uuid::parse_str(&submission.operation_id).is_ok());
        assert_eq!(submission.retry(), submission);
        assert_ne!(MutationSubmission::new(), submission);
        assert!(uuid::Uuid::parse_str(&new_id()).is_ok());
    }

    #[test]
    fn structured_form_mutations_preserve_unrelated_policy_fields() {
        let mut draft = valid_draft();
        let original = draft.policy.clone();

        draft.toggle_allowed_type("incident", false);
        assert!(!draft.policy.allowed_types.contains(&"incident".into()));
        assert_eq!(draft.policy.states, original.states);
        assert_eq!(draft.policy.transitions, original.transitions);

        draft.add_state("VERIFY");
        draft.add_transition("REVIEW", "VERIFY");
        assert!(draft.policy.states.contains(&"VERIFY".into()));
        assert!(draft
            .policy
            .transitions
            .contains(&("REVIEW".into(), "VERIFY".into())));

        draft.set_tag_rule(
            "task",
            vec!["ops".into(), "release".into()],
            vec!["release".into()],
        );
        assert_eq!(draft.policy.tag_rules["task"].required, ["release"]);
        assert_eq!(
            draft.policy.lease_duration_secs,
            original.lease_duration_secs
        );
        assert_eq!(draft.policy.claim_policy, original.claim_policy);
    }
}
