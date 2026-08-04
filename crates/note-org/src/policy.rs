use crate::{ParseOptions, WorkItem, WorkItemType};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{role} state {state} is not configured")]
    MissingRoleState { role: String, state: String },
    #[error("transition endpoint {state} is not configured")]
    TransitionStateMissing { state: String },
    #[error("{role} state {state} must be executable")]
    RecoveryStateNotExecutable { role: String, state: String },
    #[error("required {role} transition {from} -> {to} is not configured")]
    RequiredTransitionMissing {
        role: String,
        from: String,
        to: String,
    },
    #[error("successful state {state} must be terminal")]
    SuccessfulStateNotTerminal { state: String },
    #[error("concurrency limit must be greater than zero")]
    ZeroConcurrency,
    #[error("lease duration must be greater than zero")]
    ZeroLeaseDuration,
    #[error("required tag {tag} is not allowed for {item_type:?}")]
    RequiredTagNotAllowed {
        item_type: crate::WorkItemType,
        tag: String,
    },
    #[error("work item type {0:?} is not allowed")]
    TypeNotAllowed(crate::WorkItemType),
    #[error("tag {tag} is not allowed for {item_type:?}")]
    TagNotAllowed {
        item_type: crate::WorkItemType,
        tag: String,
    },
    #[error("required tag {tag} is missing for {item_type:?}")]
    RequiredTagMissing {
        item_type: crate::WorkItemType,
        tag: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TagRule {
    pub allowed: BTreeSet<String>,
    pub required: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPolicy {
    Open,
    AssignmentRestricted,
    ExplicitlyDispatched,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspacePolicy {
    #[serde(default)]
    pub allow_cross_workspace_agenda: bool,
    pub allowed_types: BTreeSet<WorkItemType>,
    pub states: BTreeSet<String>,
    pub transitions: BTreeSet<(String, String)>,
    pub initial_state: String,
    pub running_state: String,
    pub executable_states: BTreeSet<String>,
    pub review_state: String,
    pub failed_state: String,
    pub cancelled_state: String,
    pub successful_terminal_states: BTreeSet<String>,
    pub terminal_states: BTreeSet<String>,
    pub release_state: String,
    pub review_rejection_state: String,
    pub lease_expiry_recovery_state: String,
    pub review_required_types: BTreeSet<WorkItemType>,
    pub claim_policy: ClaimPolicy,
    pub lease_duration_secs: u64,
    pub retry_limit: u32,
    pub concurrency_limit: usize,
    pub tag_rules: BTreeMap<WorkItemType, TagRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionInput {
    pub item_type: WorkItemType,
    pub from: String,
    pub to: String,
    pub item_requires_review: bool,
    pub review_approved: bool,
    pub dependencies_satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("work item type {0:?} is not allowed")]
    TypeNotAllowed(WorkItemType),
    #[error("transition {from} -> {to} is not allowed")]
    NotAllowed { from: String, to: String },
    #[error("dependencies are incomplete")]
    DependenciesIncomplete,
    #[error("review is required before {0}")]
    ReviewRequired(String),
}

impl WorkspacePolicy {
    pub fn engineering_default() -> Self {
        Self {
            allow_cross_workspace_agenda: false,
            allowed_types: WorkItemType::ALL.into_iter().collect(),
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
            .into_iter()
            .map(str::to_string)
            .collect(),
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
            .into_iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect(),
            initial_state: "BACKLOG".to_string(),
            running_state: "RUNNING".to_string(),
            executable_states: BTreeSet::from(["READY".to_string()]),
            review_state: "REVIEW".to_string(),
            failed_state: "FAILED".to_string(),
            cancelled_state: "CANCELLED".to_string(),
            successful_terminal_states: BTreeSet::from(["DONE".to_string()]),
            terminal_states: BTreeSet::from(["DONE".to_string(), "CANCELLED".to_string()]),
            release_state: "READY".to_string(),
            review_rejection_state: "READY".to_string(),
            lease_expiry_recovery_state: "READY".to_string(),
            review_required_types: BTreeSet::new(),
            claim_policy: ClaimPolicy::AssignmentRestricted,
            lease_duration_secs: 900,
            retry_limit: 2,
            concurrency_limit: 4,
            tag_rules: BTreeMap::new(),
        }
    }

    pub fn max_attempts(&self) -> u32 {
        self.retry_limit.saturating_add(1)
    }

    pub fn parse_options(&self) -> ParseOptions {
        ParseOptions::new(self.states.iter().cloned())
    }
}

pub fn validate_policy(policy: &WorkspacePolicy) -> Result<(), PolicyError> {
    let role_states = [
        ("initial", &policy.initial_state),
        ("running", &policy.running_state),
        ("review", &policy.review_state),
        ("failed", &policy.failed_state),
        ("cancelled", &policy.cancelled_state),
        ("release", &policy.release_state),
        ("review_rejection", &policy.review_rejection_state),
        ("lease_expiry_recovery", &policy.lease_expiry_recovery_state),
    ];
    for (role, state) in role_states {
        if !policy.states.contains(state) {
            return Err(PolicyError::MissingRoleState {
                role: role.to_string(),
                state: state.clone(),
            });
        }
    }

    for (role, states) in [
        ("executable", &policy.executable_states),
        ("successful_terminal", &policy.successful_terminal_states),
        ("terminal", &policy.terminal_states),
    ] {
        for state in states {
            if !policy.states.contains(state) {
                return Err(PolicyError::MissingRoleState {
                    role: role.to_string(),
                    state: state.clone(),
                });
            }
        }
    }

    for (from, to) in &policy.transitions {
        for state in [from, to] {
            if !policy.states.contains(state) {
                return Err(PolicyError::TransitionStateMissing {
                    state: state.clone(),
                });
            }
        }
    }

    for (role, state) in [
        ("release", &policy.release_state),
        ("review_rejection", &policy.review_rejection_state),
        ("lease_expiry_recovery", &policy.lease_expiry_recovery_state),
    ] {
        if !policy.executable_states.contains(state) {
            return Err(PolicyError::RecoveryStateNotExecutable {
                role: role.to_string(),
                state: state.clone(),
            });
        }
    }

    for (role, from, to) in [
        ("release", &policy.running_state, &policy.release_state),
        (
            "review_rejection",
            &policy.review_state,
            &policy.review_rejection_state,
        ),
        (
            "lease_expiry",
            &policy.running_state,
            &policy.lease_expiry_recovery_state,
        ),
        ("retry", &policy.failed_state, &policy.running_state),
        (
            "lease_expiry_recovery",
            &policy.lease_expiry_recovery_state,
            &policy.running_state,
        ),
    ] {
        require_transition(policy, role, from, to)?;
    }
    for state in &policy.executable_states {
        require_transition(policy, "claim", state, &policy.running_state)?;
    }

    for state in &policy.successful_terminal_states {
        if !policy.terminal_states.contains(state) {
            return Err(PolicyError::SuccessfulStateNotTerminal {
                state: state.clone(),
            });
        }
    }

    if policy.concurrency_limit == 0 {
        return Err(PolicyError::ZeroConcurrency);
    }
    if policy.lease_duration_secs == 0 {
        return Err(PolicyError::ZeroLeaseDuration);
    }

    for item_type in &policy.review_required_types {
        if !policy.allowed_types.contains(item_type) {
            return Err(PolicyError::TypeNotAllowed(*item_type));
        }
    }

    for (item_type, rule) in &policy.tag_rules {
        if !policy.allowed_types.contains(item_type) {
            return Err(PolicyError::TypeNotAllowed(*item_type));
        }
        if !rule.allowed.is_empty() {
            for tag in &rule.required {
                if !rule.allowed.contains(tag) {
                    return Err(PolicyError::RequiredTagNotAllowed {
                        item_type: *item_type,
                        tag: tag.clone(),
                    });
                }
            }
        }
    }

    Ok(())
}

fn require_transition(
    policy: &WorkspacePolicy,
    role: &str,
    from: &str,
    to: &str,
) -> Result<(), PolicyError> {
    if !policy
        .transitions
        .contains(&(from.to_string(), to.to_string()))
    {
        return Err(PolicyError::RequiredTransitionMissing {
            role: role.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    Ok(())
}

pub fn validate_item(policy: &WorkspacePolicy, item: &WorkItem) -> Result<(), PolicyError> {
    if !policy.allowed_types.contains(&item.item_type) {
        return Err(PolicyError::TypeNotAllowed(item.item_type));
    }

    if let Some(rule) = policy.tag_rules.get(&item.item_type) {
        if !rule.allowed.is_empty() {
            for tag in &item.tags {
                if !rule.allowed.contains(tag) {
                    return Err(PolicyError::TagNotAllowed {
                        item_type: item.item_type,
                        tag: tag.clone(),
                    });
                }
            }
        }
        for tag in &rule.required {
            if !item.tags.contains(tag) {
                return Err(PolicyError::RequiredTagMissing {
                    item_type: item.item_type,
                    tag: tag.clone(),
                });
            }
        }
    }

    Ok(())
}

pub fn validate_transition(
    policy: &WorkspacePolicy,
    input: &TransitionInput,
) -> Result<(), TransitionError> {
    if !policy.allowed_types.contains(&input.item_type) {
        return Err(TransitionError::TypeNotAllowed(input.item_type));
    }
    if !policy
        .transitions
        .contains(&(input.from.clone(), input.to.clone()))
    {
        return Err(TransitionError::NotAllowed {
            from: input.from.clone(),
            to: input.to.clone(),
        });
    }
    if !input.dependencies_satisfied
        && (policy.executable_states.contains(&input.to)
            || policy.successful_terminal_states.contains(&input.to))
    {
        return Err(TransitionError::DependenciesIncomplete);
    }
    let requires_review =
        input.item_requires_review || policy.review_required_types.contains(&input.item_type);
    let successful = policy.successful_terminal_states.contains(&input.to);
    if successful {
        let leaving_review_for_success = input.from == policy.review_state;
        if (requires_review && !leaving_review_for_success)
            || (leaving_review_for_success && !input.review_approved)
        {
            return Err(TransitionError::ReviewRequired(input.to.clone()));
        }
    }

    Ok(())
}
