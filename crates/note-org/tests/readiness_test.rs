mod support;

use chrono::NaiveDate;
use note_org::{
    evaluate_readiness, validate_dependencies, ClaimPolicy, OrgTimestamp, Readiness,
    ReadinessBlocker, ReadinessContext, WorkspacePolicy,
};
use std::collections::BTreeSet;
use support::{id, item, ready_item};

const ONE: &str = "11111111-1111-4111-8111-111111111111";
const TWO: &str = "22222222-2222-4222-8222-222222222222";

fn strings(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn blocked(blocker: ReadinessBlocker) -> Readiness {
    Readiness::Blocked(vec![blocker])
}

#[test]
fn reports_every_blocker_in_declaration_order() {
    let mut item = ready_item();
    item.assignee = Some("agent-a".to_string());
    let policy = WorkspacePolicy::engineering_default();
    let context = ReadinessContext {
        now: 100,
        dependencies_satisfied: false,
        workspace_active: false,
        active_lease: true,
        capacity_available: false,
        actor_id: Some("agent-b".to_string()),
        scheduled_at: Some(101),
        lease_expired_running: false,
    };

    assert_eq!(
        evaluate_readiness(&item, &policy, &context),
        Readiness::Blocked(vec![
            ReadinessBlocker::DependenciesIncomplete,
            ReadinessBlocker::ScheduledForFuture,
            ReadinessBlocker::WorkspaceArchived,
            ReadinessBlocker::ActiveLease,
            ReadinessBlocker::ConcurrencyLimit,
            ReadinessBlocker::AssignedToOtherActor,
        ])
    );
}

#[test]
fn non_executable_state_is_blocked() {
    let mut item = ready_item();
    item.state = Some("BACKLOG".to_string());

    assert_eq!(
        evaluate_readiness(
            &item,
            &WorkspacePolicy::engineering_default(),
            &ReadinessContext::ready_at(100),
        ),
        blocked(ReadinessBlocker::NonExecutableState)
    );
}

#[test]
fn incomplete_dependencies_are_blocked() {
    let context = ReadinessContext {
        dependencies_satisfied: false,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &ready_item(),
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        blocked(ReadinessBlocker::DependenciesIncomplete)
    );
}

#[test]
fn only_a_schedule_strictly_after_now_is_blocked() {
    let policy = WorkspacePolicy::engineering_default();
    let at_boundary = ReadinessContext {
        scheduled_at: Some(100),
        ..ReadinessContext::ready_at(100)
    };
    let future = ReadinessContext {
        scheduled_at: Some(101),
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(&ready_item(), &policy, &at_boundary),
        Readiness::Ready
    );
    assert_eq!(
        evaluate_readiness(&ready_item(), &policy, &future),
        blocked(ReadinessBlocker::ScheduledForFuture)
    );
}

#[test]
fn archived_workspace_is_blocked() {
    let context = ReadinessContext {
        workspace_active: false,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &ready_item(),
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        blocked(ReadinessBlocker::WorkspaceArchived)
    );
}

#[test]
fn active_lease_is_blocked() {
    let context = ReadinessContext {
        active_lease: true,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &ready_item(),
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        blocked(ReadinessBlocker::ActiveLease)
    );
}

#[test]
fn exhausted_concurrency_is_blocked() {
    let context = ReadinessContext {
        capacity_available: false,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &ready_item(),
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        blocked(ReadinessBlocker::ConcurrencyLimit)
    );
}

#[test]
fn explicit_dispatch_requires_assignment() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.claim_policy = ClaimPolicy::ExplicitlyDispatched;

    assert_eq!(
        evaluate_readiness(&ready_item(), &policy, &ReadinessContext::ready_at(100)),
        blocked(ReadinessBlocker::AssignmentRequired)
    );
}

#[test]
fn restricted_assignment_requires_the_assigned_actor() {
    let mut item = ready_item();
    item.assignee = Some("agent-a".to_string());

    for actor_id in [None, Some("agent-b".to_string())] {
        let context = ReadinessContext {
            actor_id,
            ..ReadinessContext::ready_at(100)
        };
        assert_eq!(
            evaluate_readiness(&item, &WorkspacePolicy::engineering_default(), &context,),
            blocked(ReadinessBlocker::AssignedToOtherActor)
        );
    }
}

#[test]
fn deadline_metadata_does_not_affect_readiness() {
    let mut item = ready_item();
    item.deadline = Some(OrgTimestamp {
        raw: "<2000-01-01 Sat>".to_string(),
        local: NaiveDate::from_ymd_opt(2000, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    });

    assert_eq!(
        evaluate_readiness(
            &item,
            &WorkspacePolicy::engineering_default(),
            &ReadinessContext::ready_at(100),
        ),
        Readiness::Ready
    );
}

#[test]
fn executable_states_are_policy_configured() {
    let mut item = ready_item();
    item.state = Some("QUEUED".to_string());
    let mut policy = WorkspacePolicy::engineering_default();
    policy.states.insert("QUEUED".to_string());
    policy.executable_states = strings(&["QUEUED"]);

    assert_eq!(
        evaluate_readiness(&item, &policy, &ReadinessContext::ready_at(100)),
        Readiness::Ready
    );
}

#[test]
fn configured_successful_states_flow_from_dependency_graph_to_readiness() {
    let dependent = item(ONE, "READY", [TWO]);
    let prerequisite = item(TWO, "ARCHIVED", []);
    let graph =
        validate_dependencies(&[dependent.clone(), prerequisite], &strings(&["ARCHIVED"])).unwrap();
    let context = ReadinessContext {
        dependencies_satisfied: graph.dependencies_satisfied(id(ONE)),
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &dependent,
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        Readiness::Ready
    );
}

#[test]
fn open_claim_policy_ignores_assignment() {
    let mut item = ready_item();
    item.assignee = Some("agent-a".to_string());
    let mut policy = WorkspacePolicy::engineering_default();
    policy.claim_policy = ClaimPolicy::Open;
    let context = ReadinessContext {
        actor_id: Some("agent-b".to_string()),
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(&item, &policy, &context),
        Readiness::Ready
    );
}

#[test]
fn assignment_restricted_allows_unassigned_or_matching_actor() {
    let policy = WorkspacePolicy::engineering_default();
    assert_eq!(
        evaluate_readiness(&ready_item(), &policy, &ReadinessContext::ready_at(100)),
        Readiness::Ready
    );

    let mut assigned = ready_item();
    assigned.assignee = Some("agent-a".to_string());
    let matching = ReadinessContext {
        actor_id: Some("agent-a".to_string()),
        ..ReadinessContext::ready_at(100)
    };
    assert_eq!(
        evaluate_readiness(&assigned, &policy, &matching),
        Readiness::Ready
    );
}

#[test]
fn explicit_dispatch_requires_the_matching_actor_when_assigned() {
    let mut item = ready_item();
    item.assignee = Some("agent-a".to_string());
    let mut policy = WorkspacePolicy::engineering_default();
    policy.claim_policy = ClaimPolicy::ExplicitlyDispatched;

    for actor_id in [None, Some("agent-b".to_string())] {
        let context = ReadinessContext {
            actor_id,
            ..ReadinessContext::ready_at(100)
        };
        assert_eq!(
            evaluate_readiness(&item, &policy, &context),
            blocked(ReadinessBlocker::AssignedToOtherActor)
        );
    }

    let matching = ReadinessContext {
        actor_id: Some("agent-a".to_string()),
        ..ReadinessContext::ready_at(100)
    };
    assert_eq!(
        evaluate_readiness(&item, &policy, &matching),
        Readiness::Ready
    );
}

#[test]
fn expired_running_item_is_recovery_candidate_despite_its_lease() {
    let mut item = ready_item();
    item.state = Some("RUNNING".to_string());
    let context = ReadinessContext {
        active_lease: true,
        lease_expired_running: true,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(&item, &WorkspacePolicy::engineering_default(), &context,),
        Readiness::RecoveryCandidate
    );
}

#[test]
fn expired_running_item_with_other_blockers_reports_only_those_blockers() {
    let mut item = ready_item();
    item.state = Some("RUNNING".to_string());
    let context = ReadinessContext {
        dependencies_satisfied: false,
        active_lease: true,
        lease_expired_running: true,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(&item, &WorkspacePolicy::engineering_default(), &context,),
        blocked(ReadinessBlocker::DependenciesIncomplete)
    );
}

#[test]
fn expired_flag_does_not_make_other_states_recovery_candidates() {
    let context = ReadinessContext {
        lease_expired_running: true,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(
            &ready_item(),
            &WorkspacePolicy::engineering_default(),
            &context,
        ),
        Readiness::Ready
    );
}

#[test]
fn running_without_expiry_must_be_executable_to_be_ready() {
    let mut item = ready_item();
    item.state = Some("RUNNING".to_string());
    let mut policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        evaluate_readiness(&item, &policy, &ReadinessContext::ready_at(100)),
        blocked(ReadinessBlocker::NonExecutableState)
    );

    policy.executable_states.insert("RUNNING".to_string());
    assert_eq!(
        evaluate_readiness(&item, &policy, &ReadinessContext::ready_at(100)),
        Readiness::Ready
    );
}
