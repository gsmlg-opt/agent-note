mod support;

use note_org::{
    decide_claim, AttemptClosure, AttemptPhase, ClaimDecision, ClaimDecisionError, ClaimKind,
    ClaimRequest, LeaseKind, LeaseStatus, ReadinessBlocker, ReadinessContext, TransitionTarget,
    WorkspacePolicy,
};
use support::ready_item;

fn execution_claim() -> ClaimRequest {
    ClaimRequest {
        lease_kind: LeaseKind::Execution,
        claim_kind: ClaimKind::Claim,
    }
}

fn assert_blocked(result: Result<ClaimDecision, ClaimDecisionError>) {
    assert!(matches!(result, Err(ClaimDecisionError::Blocked(_))));
}

#[test]
fn normal_execution_claim_allocates_the_next_attempt_and_enters_running() {
    let policy = WorkspacePolicy::engineering_default();

    assert_eq!(
        decide_claim(
            &ready_item(),
            &policy,
            &ReadinessContext::ready_at(100),
            execution_claim(),
        ),
        Ok(ClaimDecision {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Claim,
            attempt_closure: AttemptClosure::None,
            transition_target: TransitionTarget::State(policy.running_state),
            allocate_execution_attempt: true,
        })
    );
}

#[test]
fn review_claim_preserves_the_submitted_attempt_and_source_state() {
    let mut item = ready_item();
    let policy = WorkspacePolicy::engineering_default();
    item.state = Some(policy.review_state.clone());

    assert_eq!(
        decide_claim(
            &item,
            &policy,
            &ReadinessContext {
                execution_attempt_count: policy.max_attempts(),
                current_attempt: Some(AttemptPhase::Submitted),
                ..ReadinessContext::ready_at(100)
            },
            ClaimRequest {
                lease_kind: LeaseKind::Review,
                claim_kind: ClaimKind::Claim,
            },
        ),
        Ok(ClaimDecision {
            lease_kind: LeaseKind::Review,
            claim_kind: ClaimKind::Claim,
            attempt_closure: AttemptClosure::None,
            transition_target: TransitionTarget::Unchanged,
            allocate_execution_attempt: false,
        })
    );
}

#[test]
fn expired_execution_reclaim_closes_attempt_and_round_trips_through_recovery() {
    let mut item = ready_item();
    let policy = WorkspacePolicy::engineering_default();
    item.state = Some(policy.running_state.clone());

    assert_eq!(
        decide_claim(
            &item,
            &policy,
            &ReadinessContext {
                lease: LeaseStatus::Expired(LeaseKind::Execution),
                execution_attempt_count: 1,
                current_attempt: Some(AttemptPhase::Running),
                ..ReadinessContext::ready_at(100)
            },
            ClaimRequest {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Reclaim,
            },
        ),
        Ok(ClaimDecision {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Reclaim,
            attempt_closure: AttemptClosure::Expired,
            transition_target: TransitionTarget::Recovery {
                intermediate: policy.lease_expiry_recovery_state,
                final_state: policy.running_state,
            },
            allocate_execution_attempt: true,
        })
    );
}

#[test]
fn failed_retry_allocates_a_new_attempt_without_reclosing_the_old_one() {
    let mut item = ready_item();
    let policy = WorkspacePolicy::engineering_default();
    item.state = Some(policy.failed_state.clone());

    assert_eq!(
        decide_claim(
            &item,
            &policy,
            &ReadinessContext {
                execution_attempt_count: 1,
                current_attempt: Some(AttemptPhase::Terminal),
                ..ReadinessContext::ready_at(100)
            },
            ClaimRequest {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Retry,
            },
        ),
        Ok(ClaimDecision {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Retry,
            attempt_closure: AttemptClosure::None,
            transition_target: TransitionTarget::State(policy.running_state),
            allocate_execution_attempt: true,
        })
    );
}

#[test]
fn failed_retry_and_expired_reclaim_stop_at_retry_limit_plus_one() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.retry_limit = 1;
    let context = ReadinessContext {
        execution_attempt_count: 2,
        current_attempt: Some(AttemptPhase::Terminal),
        ..ReadinessContext::ready_at(100)
    };
    let mut failed = ready_item();
    failed.state = Some(policy.failed_state.clone());

    assert_eq!(
        decide_claim(
            &failed,
            &policy,
            &context,
            ClaimRequest {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Retry,
            },
        ),
        Err(ClaimDecisionError::Blocked(vec![
            ReadinessBlocker::RetryLimit
        ]))
    );

    let mut running = ready_item();
    running.state = Some(policy.running_state.clone());
    assert_eq!(
        decide_claim(
            &running,
            &policy,
            &ReadinessContext {
                lease: LeaseStatus::Expired(LeaseKind::Execution),
                current_attempt: Some(AttemptPhase::Running),
                ..context
            },
            ClaimRequest {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Reclaim,
            },
        ),
        Err(ClaimDecisionError::Blocked(vec![
            ReadinessBlocker::RetryLimit
        ]))
    );
}

#[test]
fn zero_retry_limit_allows_only_the_initial_attempt() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.retry_limit = 0;

    assert!(decide_claim(
        &ready_item(),
        &policy,
        &ReadinessContext::ready_at(100),
        execution_claim(),
    )
    .is_ok());
    assert_eq!(
        decide_claim(
            &ready_item(),
            &policy,
            &ReadinessContext {
                execution_attempt_count: 1,
                current_attempt: Some(AttemptPhase::Terminal),
                ..ReadinessContext::ready_at(100)
            },
            execution_claim(),
        ),
        Err(ClaimDecisionError::Blocked(vec![
            ReadinessBlocker::RetryLimit
        ]))
    );
}

#[test]
fn maximum_retry_limit_never_overflows_attempt_budget() {
    let mut policy = WorkspacePolicy::engineering_default();
    policy.retry_limit = u32::MAX;

    assert!(decide_claim(
        &ready_item(),
        &policy,
        &ReadinessContext {
            execution_attempt_count: u32::MAX - 1,
            current_attempt: Some(AttemptPhase::Terminal),
            ..ReadinessContext::ready_at(100)
        },
        execution_claim(),
    )
    .is_ok());
    assert_eq!(
        decide_claim(
            &ready_item(),
            &policy,
            &ReadinessContext {
                execution_attempt_count: u32::MAX,
                current_attempt: Some(AttemptPhase::Terminal),
                ..ReadinessContext::ready_at(100)
            },
            execution_claim(),
        ),
        Err(ClaimDecisionError::Blocked(vec![
            ReadinessBlocker::RetryLimit
        ]))
    );
}

#[test]
fn claim_kind_and_lease_kind_must_match_the_candidate() {
    let policy = WorkspacePolicy::engineering_default();

    for request in [
        ClaimRequest {
            lease_kind: LeaseKind::Review,
            claim_kind: ClaimKind::Claim,
        },
        ClaimRequest {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Reclaim,
        },
        ClaimRequest {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Retry,
        },
    ] {
        assert!(matches!(
            decide_claim(
                &ready_item(),
                &policy,
                &ReadinessContext::ready_at(100),
                request,
            ),
            Err(ClaimDecisionError::Blocked(_))
        ));
    }
}

#[test]
fn reclaim_requires_the_current_running_attempt() {
    let mut item = ready_item();
    let policy = WorkspacePolicy::engineering_default();
    item.state = Some(policy.running_state.clone());

    for current_attempt in [
        None,
        Some(AttemptPhase::Submitted),
        Some(AttemptPhase::Terminal),
    ] {
        assert_eq!(
            decide_claim(
                &item,
                &policy,
                &ReadinessContext {
                    lease: LeaseStatus::Expired(LeaseKind::Execution),
                    execution_attempt_count: 1,
                    current_attempt,
                    ..ReadinessContext::ready_at(100)
                },
                ClaimRequest {
                    lease_kind: LeaseKind::Execution,
                    claim_kind: ClaimKind::Reclaim,
                },
            ),
            Err(ClaimDecisionError::Blocked(vec![
                ReadinessBlocker::RunningAttemptRequired
            ]))
        );
    }
}

#[test]
fn retry_requires_the_previous_attempt_to_be_terminal() {
    let mut item = ready_item();
    let policy = WorkspacePolicy::engineering_default();
    item.state = Some(policy.failed_state.clone());

    for current_attempt in [
        None,
        Some(AttemptPhase::Running),
        Some(AttemptPhase::Submitted),
    ] {
        assert_eq!(
            decide_claim(
                &item,
                &policy,
                &ReadinessContext {
                    execution_attempt_count: 1,
                    current_attempt,
                    ..ReadinessContext::ready_at(100)
                },
                ClaimRequest {
                    lease_kind: LeaseKind::Execution,
                    claim_kind: ClaimKind::Retry,
                },
            ),
            Err(ClaimDecisionError::Blocked(vec![
                ReadinessBlocker::TerminalAttemptRequired
            ]))
        );
    }
}

#[test]
fn initial_execution_claim_requires_zero_attempts_and_no_current_phase() {
    let policy = WorkspacePolicy::engineering_default();

    for current_attempt in [
        Some(AttemptPhase::Running),
        Some(AttemptPhase::Submitted),
        Some(AttemptPhase::Terminal),
    ] {
        assert_blocked(decide_claim(
            &ready_item(),
            &policy,
            &ReadinessContext {
                current_attempt,
                ..ReadinessContext::ready_at(100)
            },
            execution_claim(),
        ));
    }
}

#[test]
fn later_execution_claim_requires_a_terminal_current_attempt() {
    let policy = WorkspacePolicy::engineering_default();

    for current_attempt in [
        None,
        Some(AttemptPhase::Running),
        Some(AttemptPhase::Submitted),
    ] {
        assert_blocked(decide_claim(
            &ready_item(),
            &policy,
            &ReadinessContext {
                execution_attempt_count: 1,
                current_attempt,
                ..ReadinessContext::ready_at(100)
            },
            execution_claim(),
        ));
    }

    assert!(decide_claim(
        &ready_item(),
        &policy,
        &ReadinessContext {
            execution_attempt_count: 1,
            current_attempt: Some(AttemptPhase::Terminal),
            ..ReadinessContext::ready_at(100)
        },
        execution_claim(),
    )
    .is_ok());
}

#[test]
fn review_reclaim_and_retry_require_an_existing_execution_attempt() {
    let policy = WorkspacePolicy::engineering_default();

    let mut review = ready_item();
    review.state = Some(policy.review_state.clone());
    assert_blocked(decide_claim(
        &review,
        &policy,
        &ReadinessContext {
            current_attempt: Some(AttemptPhase::Submitted),
            ..ReadinessContext::ready_at(100)
        },
        ClaimRequest {
            lease_kind: LeaseKind::Review,
            claim_kind: ClaimKind::Claim,
        },
    ));

    let mut running = ready_item();
    running.state = Some(policy.running_state.clone());
    assert_blocked(decide_claim(
        &running,
        &policy,
        &ReadinessContext {
            lease: LeaseStatus::Expired(LeaseKind::Execution),
            current_attempt: Some(AttemptPhase::Running),
            ..ReadinessContext::ready_at(100)
        },
        ClaimRequest {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Reclaim,
        },
    ));

    let mut failed = ready_item();
    failed.state = Some(policy.failed_state.clone());
    assert_blocked(decide_claim(
        &failed,
        &policy,
        &ReadinessContext {
            current_attempt: Some(AttemptPhase::Terminal),
            ..ReadinessContext::ready_at(100)
        },
        ClaimRequest {
            lease_kind: LeaseKind::Execution,
            claim_kind: ClaimKind::Retry,
        },
    ));
}
