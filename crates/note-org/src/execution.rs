use crate::{
    evaluate_readiness, evaluate_retry_blockers, validate_transition, ClaimQueue, LeaseKind,
    Readiness, ReadinessBlocker, ReadinessContext, TransitionError, TransitionInput, WorkItem,
    WorkspacePolicy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    Claim,
    Reclaim,
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimRequest {
    pub lease_kind: LeaseKind,
    pub claim_kind: ClaimKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptClosure {
    None,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionTarget {
    Unchanged,
    State(String),
    Recovery {
        intermediate: String,
        final_state: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimDecision {
    pub lease_kind: LeaseKind,
    pub claim_kind: ClaimKind,
    pub attempt_closure: AttemptClosure,
    pub transition_target: TransitionTarget,
    pub allocate_execution_attempt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimDecisionError {
    Blocked(Vec<ReadinessBlocker>),
    Transition(TransitionError),
}

pub fn decide_claim(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
    request: ClaimRequest,
) -> Result<ClaimDecision, ClaimDecisionError> {
    match (request.lease_kind, request.claim_kind) {
        (LeaseKind::Execution, ClaimKind::Claim) => {
            let mut claim_context = context.clone();
            claim_context.queue = ClaimQueue::Execution;
            require_readiness(
                evaluate_readiness(item, policy, &claim_context),
                Readiness::Ready,
            )?;
            validate_edge(
                item,
                policy,
                context,
                item_state(item),
                &policy.running_state,
            )?;
            Ok(ClaimDecision {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Claim,
                attempt_closure: AttemptClosure::None,
                transition_target: TransitionTarget::State(policy.running_state.clone()),
                allocate_execution_attempt: true,
            })
        }
        (LeaseKind::Review, ClaimKind::Claim) => {
            let mut claim_context = context.clone();
            claim_context.queue = ClaimQueue::Review;
            require_readiness(
                evaluate_readiness(item, policy, &claim_context),
                Readiness::Ready,
            )?;
            Ok(ClaimDecision {
                lease_kind: LeaseKind::Review,
                claim_kind: ClaimKind::Claim,
                attempt_closure: AttemptClosure::None,
                transition_target: TransitionTarget::Unchanged,
                allocate_execution_attempt: false,
            })
        }
        (LeaseKind::Execution, ClaimKind::Reclaim) => {
            let mut claim_context = context.clone();
            claim_context.queue = ClaimQueue::Execution;
            require_readiness(
                evaluate_readiness(item, policy, &claim_context),
                Readiness::RecoveryCandidate,
            )?;
            validate_edge(
                item,
                policy,
                context,
                &policy.running_state,
                &policy.lease_expiry_recovery_state,
            )?;
            validate_edge(
                item,
                policy,
                context,
                &policy.lease_expiry_recovery_state,
                &policy.running_state,
            )?;
            Ok(ClaimDecision {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Reclaim,
                attempt_closure: AttemptClosure::Expired,
                transition_target: TransitionTarget::Recovery {
                    intermediate: policy.lease_expiry_recovery_state.clone(),
                    final_state: policy.running_state.clone(),
                },
                allocate_execution_attempt: true,
            })
        }
        (LeaseKind::Execution, ClaimKind::Retry) => {
            let blockers = evaluate_retry_blockers(item, policy, context);
            if !blockers.is_empty() {
                return Err(ClaimDecisionError::Blocked(blockers));
            }
            validate_edge(
                item,
                policy,
                context,
                &policy.failed_state,
                &policy.running_state,
            )?;
            Ok(ClaimDecision {
                lease_kind: LeaseKind::Execution,
                claim_kind: ClaimKind::Retry,
                attempt_closure: AttemptClosure::None,
                transition_target: TransitionTarget::State(policy.running_state.clone()),
                allocate_execution_attempt: true,
            })
        }
        (LeaseKind::Review, ClaimKind::Reclaim | ClaimKind::Retry) => {
            Err(ClaimDecisionError::Blocked(vec![
                ReadinessBlocker::NonExecutableState,
            ]))
        }
    }
}

fn require_readiness(actual: Readiness, expected: Readiness) -> Result<(), ClaimDecisionError> {
    if actual == expected {
        return Ok(());
    }
    Err(ClaimDecisionError::Blocked(match actual {
        Readiness::Blocked(blockers) => blockers,
        Readiness::Ready | Readiness::RecoveryCandidate => {
            vec![ReadinessBlocker::NonExecutableState]
        }
    }))
}

fn validate_edge(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
    from: &str,
    to: &str,
) -> Result<(), ClaimDecisionError> {
    validate_transition(
        policy,
        &TransitionInput {
            item_type: item.item_type,
            from: from.to_string(),
            to: to.to_string(),
            item_requires_review: item.requires_review,
            review_approved: false,
            dependencies_satisfied: context.dependencies_satisfied,
        },
    )
    .map_err(ClaimDecisionError::Transition)
}

fn item_state(item: &WorkItem) -> &str {
    item.state.as_deref().unwrap_or_default()
}
