use note_org::{
    validate_item, validate_policy, validate_transition, ClaimPolicy, PolicyError, TagRule,
    TransitionError, TransitionInput, WorkItem, WorkItemId, WorkItemType, WorkspacePolicy,
};
use std::{collections::BTreeSet, str::FromStr};

fn strings(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn work_item(item_type: WorkItemType, tags: &[&str]) -> WorkItem {
    WorkItem {
        id: WorkItemId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
        item_type,
        parent_id: None,
        level: 1,
        title: "Policy test item".to_string(),
        state: None,
        priority: None,
        tags: strings(tags),
        scheduled: None,
        deadline: None,
        assignee: None,
        depends_on: BTreeSet::new(),
        requires_review: false,
        note_links: Vec::new(),
    }
}

fn transition(
    item_type: WorkItemType,
    from: &str,
    to: &str,
    item_requires_review: bool,
    review_approved: bool,
    dependencies_satisfied: bool,
) -> TransitionInput {
    TransitionInput {
        item_type,
        from: from.to_string(),
        to: to.to_string(),
        item_requires_review,
        review_approved,
        dependencies_satisfied,
    }
}

#[test]
fn default_engineering_policy_has_exact_prd_configuration() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        policy.allowed_types,
        WorkItemType::ALL.into_iter().collect()
    );
    assert_eq!(
        policy.states,
        strings(&[
            "BACKLOG",
            "READY",
            "RUNNING",
            "BLOCKED",
            "REVIEW",
            "DONE",
            "FAILED",
            "CANCELLED",
        ])
    );
    assert_eq!(
        policy.transitions,
        [
            ("BACKLOG", "READY"),
            ("BACKLOG", "CANCELLED"),
            ("READY", "RUNNING"),
            ("READY", "CANCELLED"),
            ("RUNNING", "BLOCKED"),
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
            ("FAILED", "CANCELLED"),
        ]
        .into_iter()
        .map(|(from, to)| (from.to_string(), to.to_string()))
        .collect()
    );
    assert_eq!(policy.initial_state, "BACKLOG");
    assert_eq!(policy.running_state, "RUNNING");
    assert_eq!(policy.executable_states, strings(&["READY"]));
    assert_eq!(policy.review_state, "REVIEW");
    assert_eq!(policy.failed_state, "FAILED");
    assert_eq!(policy.cancelled_state, "CANCELLED");
    assert_eq!(policy.successful_terminal_states, strings(&["DONE"]));
    assert_eq!(policy.terminal_states, strings(&["DONE", "CANCELLED"]));
    assert_eq!(policy.release_state, "READY");
    assert_eq!(policy.review_rejection_state, "READY");
    assert_eq!(policy.lease_expiry_recovery_state, "READY");
    assert!(policy.review_required_types.is_empty());
    assert_eq!(policy.claim_policy, ClaimPolicy::AssignmentRestricted);
    assert_eq!(policy.lease_duration_secs, 900);
    assert_eq!(policy.retry_limit, 2);
    assert_eq!(policy.max_attempts(), 3);
    assert_eq!(policy.concurrency_limit, 4);
    assert!(policy.tag_rules.is_empty());
    assert_eq!(policy.parse_options().states, policy.states);
    assert_eq!(validate_policy(&policy), Ok(()));
}

#[test]
fn every_role_state_must_be_configured() {
    let cases = [
        ("initial", "MISSING"),
        ("running", "MISSING"),
        ("review", "MISSING"),
        ("failed", "MISSING"),
        ("cancelled", "MISSING"),
        ("release", "MISSING"),
        ("review_rejection", "MISSING"),
        ("lease_expiry_recovery", "MISSING"),
    ];

    for (role, state) in cases {
        let mut policy = WorkspacePolicy::engineering_default();
        match role {
            "initial" => policy.initial_state = state.to_string(),
            "running" => policy.running_state = state.to_string(),
            "review" => policy.review_state = state.to_string(),
            "failed" => policy.failed_state = state.to_string(),
            "cancelled" => policy.cancelled_state = state.to_string(),
            "release" => policy.release_state = state.to_string(),
            "review_rejection" => policy.review_rejection_state = state.to_string(),
            "lease_expiry_recovery" => {
                policy.lease_expiry_recovery_state = state.to_string();
            }
            _ => unreachable!(),
        }

        assert_eq!(
            validate_policy(&policy),
            Err(PolicyError::MissingRoleState {
                role: role.to_string(),
                state: state.to_string(),
            })
        );
    }
}

#[test]
fn executable_state_must_be_configured() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.executable_states.insert("MISSING".to_string());

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::MissingRoleState {
            role: "executable".to_string(),
            state: "MISSING".to_string(),
        })
    );
}

#[test]
fn successful_state_must_be_configured() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy
        .successful_terminal_states
        .insert("MISSING".to_string());

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::MissingRoleState {
            role: "successful_terminal".to_string(),
            state: "MISSING".to_string(),
        })
    );
}

#[test]
fn terminal_state_must_be_configured() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.terminal_states.insert("MISSING".to_string());

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::MissingRoleState {
            role: "terminal".to_string(),
            state: "MISSING".to_string(),
        })
    );
}

#[test]
fn every_transition_endpoint_must_be_configured() {
    for transition in [
        ("MISSING".to_string(), "READY".to_string()),
        ("READY".to_string(), "MISSING".to_string()),
    ] {
        let mut policy = WorkspacePolicy::engineering_default();
        policy.transitions.insert(transition);

        assert_eq!(
            validate_policy(&policy),
            Err(PolicyError::TransitionStateMissing {
                state: "MISSING".to_string(),
            })
        );
    }
}

#[test]
fn release_and_recovery_states_must_be_executable() {
    let cases = [
        ("release", "BACKLOG"),
        ("review_rejection", "BACKLOG"),
        ("lease_expiry_recovery", "BACKLOG"),
    ];

    for (role, state) in cases {
        let mut policy = WorkspacePolicy::engineering_default();
        match role {
            "release" => policy.release_state = state.to_string(),
            "review_rejection" => policy.review_rejection_state = state.to_string(),
            "lease_expiry_recovery" => {
                policy.lease_expiry_recovery_state = state.to_string();
            }
            _ => unreachable!(),
        }

        assert_eq!(
            validate_policy(&policy),
            Err(PolicyError::RecoveryStateNotExecutable {
                role: role.to_string(),
                state: state.to_string(),
            })
        );
    }
}

#[test]
fn successful_state_must_be_terminal() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.terminal_states.remove("DONE");

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::SuccessfulStateNotTerminal {
            state: "DONE".to_string(),
        })
    );
}

#[test]
fn concurrency_limit_must_be_nonzero() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.concurrency_limit = 0;

    assert_eq!(validate_policy(&policy), Err(PolicyError::ZeroConcurrency));
}

#[test]
fn lease_duration_must_be_nonzero() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.lease_duration_secs = 0;

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::ZeroLeaseDuration)
    );
}

#[test]
fn tag_rule_type_must_be_allowed_by_policy() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allowed_types.remove(&WorkItemType::Incident);
    policy.tag_rules.insert(
        WorkItemType::Incident,
        TagRule {
            allowed: BTreeSet::new(),
            required: BTreeSet::new(),
        },
    );

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::TypeNotAllowed(WorkItemType::Incident))
    );
}

#[test]
fn review_required_type_must_be_allowed_by_policy() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allowed_types.remove(&WorkItemType::Incident);
    policy.review_required_types.insert(WorkItemType::Incident);

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::TypeNotAllowed(WorkItemType::Incident))
    );
}

#[test]
fn required_tag_must_be_in_nonempty_allowed_set() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.tag_rules.insert(
        WorkItemType::Task,
        TagRule {
            allowed: strings(&["backend"]),
            required: strings(&["urgent"]),
        },
    );

    assert_eq!(
        validate_policy(&policy),
        Err(PolicyError::RequiredTagNotAllowed {
            item_type: WorkItemType::Task,
            tag: "urgent".to_string(),
        })
    );
}

#[test]
fn empty_allowed_tag_set_is_unrestricted() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.tag_rules.insert(
        WorkItemType::Task,
        TagRule {
            allowed: BTreeSet::new(),
            required: strings(&["urgent"]),
        },
    );

    assert_eq!(validate_policy(&policy), Ok(()));
    assert_eq!(
        validate_item(
            &policy,
            &work_item(WorkItemType::Task, &["urgent", "backend"])
        ),
        Ok(())
    );
}

#[test]
fn item_type_must_be_allowed() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allowed_types.remove(&WorkItemType::Incident);

    assert_eq!(
        validate_item(&policy, &work_item(WorkItemType::Incident, &[])),
        Err(PolicyError::TypeNotAllowed(WorkItemType::Incident))
    );
}

#[test]
fn item_tag_must_be_allowed() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.tag_rules.insert(
        WorkItemType::Task,
        TagRule {
            allowed: strings(&["backend"]),
            required: BTreeSet::new(),
        },
    );

    assert_eq!(
        validate_item(&policy, &work_item(WorkItemType::Task, &["frontend"])),
        Err(PolicyError::TagNotAllowed {
            item_type: WorkItemType::Task,
            tag: "frontend".to_string(),
        })
    );
}

#[test]
fn required_item_tag_must_be_present() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.tag_rules.insert(
        WorkItemType::Task,
        TagRule {
            allowed: strings(&["backend", "urgent"]),
            required: strings(&["urgent"]),
        },
    );

    assert_eq!(
        validate_item(&policy, &work_item(WorkItemType::Task, &["backend"])),
        Err(PolicyError::RequiredTagMissing {
            item_type: WorkItemType::Task,
            tag: "urgent".to_string(),
        })
    );
}

#[test]
fn retry_limit_counts_retries_after_the_first_attempt() {
    for retry_limit in [0, 1, 2, 9] {
        let mut policy = WorkspacePolicy::engineering_default();
        policy.retry_limit = retry_limit;

        assert_eq!(
            (1..=policy.max_attempts()).collect::<Vec<_>>(),
            (1..=retry_limit + 1).collect::<Vec<_>>()
        );
    }

    let mut policy = WorkspacePolicy::engineering_default();
    policy.retry_limit = u32::MAX;
    assert_eq!(policy.max_attempts(), u32::MAX);
}

#[test]
fn configured_transition_is_allowed() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "READY", "RUNNING", false, false, true),
        ),
        Ok(())
    );
}

#[test]
fn unconfigured_transition_is_rejected() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "BACKLOG", "DONE", false, false, true),
        ),
        Err(TransitionError::NotAllowed {
            from: "BACKLOG".to_string(),
            to: "DONE".to_string(),
        })
    );
}

#[test]
fn transition_item_type_must_be_allowed() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.allowed_types.remove(&WorkItemType::Incident);

    assert_eq!(
        validate_transition(
            &policy,
            &transition(
                WorkItemType::Incident,
                "READY",
                "RUNNING",
                false,
                false,
                true,
            ),
        ),
        Err(TransitionError::TypeNotAllowed(WorkItemType::Incident))
    );
}

#[test]
fn dependencies_must_be_complete_before_entering_executable_state() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "BACKLOG", "READY", false, false, false),
        ),
        Err(TransitionError::DependenciesIncomplete)
    );
}

#[test]
fn dependencies_must_be_complete_before_entering_successful_state() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "RUNNING", "DONE", false, false, false),
        ),
        Err(TransitionError::DependenciesIncomplete)
    );
}

#[test]
fn incomplete_dependencies_do_not_block_irrelevant_transition() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(
                WorkItemType::Task,
                "RUNNING",
                "BLOCKED",
                false,
                false,
                false
            ),
        ),
        Ok(())
    );
}

#[test]
fn required_review_blocks_direct_completion() {
    let policy = WorkspacePolicy::engineering_default();

    let error = validate_transition(
        &policy,
        &transition(WorkItemType::Task, "RUNNING", "DONE", true, false, true),
    )
    .unwrap_err();

    assert_eq!(error, TransitionError::ReviewRequired("DONE".to_string()));
    assert_eq!(error.to_string(), "review is required before DONE");
}

#[test]
fn type_review_requirement_cannot_be_bypassed_by_item_flag() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.review_required_types.insert(WorkItemType::Task);

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "RUNNING", "DONE", false, false, true),
        ),
        Err(TransitionError::ReviewRequired("DONE".to_string()))
    );
}

#[test]
fn item_review_requirement_strengthens_policy() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "RUNNING", "DONE", true, false, true),
        ),
        Err(TransitionError::ReviewRequired("DONE".to_string()))
    );
}

#[test]
fn approval_cannot_bypass_required_review_state() {
    for (item_requires_review, type_requires_review) in [(true, false), (false, true)] {
        let mut policy = WorkspacePolicy::engineering_default();
        if type_requires_review {
            policy.review_required_types.insert(WorkItemType::Task);
        }

        assert_eq!(
            validate_transition(
                &policy,
                &transition(
                    WorkItemType::Task,
                    "RUNNING",
                    "DONE",
                    item_requires_review,
                    true,
                    true,
                ),
            ),
            Err(TransitionError::ReviewRequired("DONE".to_string()))
        );
    }
}

#[test]
fn leaving_review_for_success_requires_approval() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "REVIEW", "DONE", false, false, true),
        ),
        Err(TransitionError::ReviewRequired("DONE".to_string()))
    );
}

#[test]
fn approved_review_allows_configured_completion_edge() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.review_required_types.insert(WorkItemType::Task);

    assert_eq!(
        validate_transition(
            &policy,
            &transition(WorkItemType::Task, "REVIEW", "DONE", false, true, true),
        ),
        Ok(())
    );
}
