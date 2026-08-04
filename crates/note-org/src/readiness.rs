use crate::{ClaimPolicy, WorkItem, WorkspacePolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseKind {
    Execution,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStatus {
    None,
    Active(LeaseKind),
    Expired(LeaseKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimQueue {
    Execution,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptPhase {
    Running,
    Submitted,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessContext {
    pub now: i64,
    pub dependencies_satisfied: bool,
    pub workspace_active: bool,
    pub lease: LeaseStatus,
    pub capacity_available: bool,
    pub actor_id: Option<String>,
    pub scheduled_at: Option<i64>,
    pub queue: ClaimQueue,
    pub execution_attempt_count: u32,
    pub current_attempt: Option<AttemptPhase>,
}

impl ReadinessContext {
    pub fn ready_at(now: i64) -> Self {
        Self {
            now,
            dependencies_satisfied: true,
            workspace_active: true,
            lease: LeaseStatus::None,
            capacity_available: true,
            actor_id: None,
            scheduled_at: None,
            queue: ClaimQueue::Execution,
            execution_attempt_count: 0,
            current_attempt: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadinessBlocker {
    NonExecutableState,
    DependenciesIncomplete,
    ScheduledForFuture,
    WorkspaceArchived,
    ActiveLease,
    LeaseKindMismatch,
    ConcurrencyLimit,
    RetryLimit,
    AttemptHistoryInconsistent,
    RunningAttemptRequired,
    SubmittedAttemptRequired,
    TerminalAttemptRequired,
    AssignmentRequired,
    AssignedToOtherActor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    RecoveryCandidate,
    Blocked(Vec<ReadinessBlocker>),
}

pub fn evaluate_readiness(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
) -> Readiness {
    let recovery_candidate = context.queue == ClaimQueue::Execution
        && item.state.as_ref() == Some(&policy.running_state)
        && context.lease == LeaseStatus::Expired(LeaseKind::Execution);
    let queue_state_matches = match context.queue {
        ClaimQueue::Execution => item
            .state
            .as_ref()
            .is_some_and(|state| policy.executable_states.contains(state)),
        ClaimQueue::Review => item.state.as_ref() == Some(&policy.review_state),
    };
    let mut blockers = Vec::new();

    if !recovery_candidate && !queue_state_matches {
        blockers.push(ReadinessBlocker::NonExecutableState);
    }
    if !context.dependencies_satisfied {
        blockers.push(ReadinessBlocker::DependenciesIncomplete);
    }
    if context
        .scheduled_at
        .is_some_and(|scheduled_at| scheduled_at > context.now)
    {
        blockers.push(ReadinessBlocker::ScheduledForFuture);
    }
    if !context.workspace_active {
        blockers.push(ReadinessBlocker::WorkspaceArchived);
    }
    match context.lease {
        LeaseStatus::Active(_) => blockers.push(ReadinessBlocker::ActiveLease),
        LeaseStatus::Expired(kind)
            if (context.queue == ClaimQueue::Execution && kind == LeaseKind::Review)
                || (context.queue == ClaimQueue::Review && kind == LeaseKind::Execution) =>
        {
            blockers.push(ReadinessBlocker::LeaseKindMismatch);
        }
        LeaseStatus::None | LeaseStatus::Expired(_) => {}
    }
    if !context.capacity_available {
        blockers.push(ReadinessBlocker::ConcurrencyLimit);
    }
    if context.queue == ClaimQueue::Execution
        && context.execution_attempt_count >= policy.max_attempts()
    {
        blockers.push(ReadinessBlocker::RetryLimit);
    }
    if recovery_candidate {
        if context.execution_attempt_count == 0 {
            blockers.push(ReadinessBlocker::AttemptHistoryInconsistent);
        } else if context.current_attempt != Some(AttemptPhase::Running) {
            blockers.push(ReadinessBlocker::RunningAttemptRequired);
        }
    } else if context.queue == ClaimQueue::Execution && queue_state_matches {
        let attempt_history_is_consistent = match context.execution_attempt_count {
            0 => context.current_attempt.is_none(),
            _ => context.current_attempt == Some(AttemptPhase::Terminal),
        } && context.lease
            != LeaseStatus::Expired(LeaseKind::Execution);
        if !attempt_history_is_consistent {
            blockers.push(ReadinessBlocker::AttemptHistoryInconsistent);
        }
    }
    if context.queue == ClaimQueue::Review && queue_state_matches {
        if context.execution_attempt_count == 0 {
            blockers.push(ReadinessBlocker::AttemptHistoryInconsistent);
        } else if context.current_attempt != Some(AttemptPhase::Submitted) {
            blockers.push(ReadinessBlocker::SubmittedAttemptRequired);
        }
    }

    add_assignment_blockers(item, policy, context, &mut blockers);

    if !blockers.is_empty() {
        Readiness::Blocked(blockers)
    } else if recovery_candidate {
        Readiness::RecoveryCandidate
    } else {
        Readiness::Ready
    }
}

pub(crate) fn evaluate_retry_blockers(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
) -> Vec<ReadinessBlocker> {
    let mut blockers = Vec::new();
    if item.state.as_ref() != Some(&policy.failed_state) {
        blockers.push(ReadinessBlocker::NonExecutableState);
    }
    if !context.dependencies_satisfied {
        blockers.push(ReadinessBlocker::DependenciesIncomplete);
    }
    if context
        .scheduled_at
        .is_some_and(|scheduled_at| scheduled_at > context.now)
    {
        blockers.push(ReadinessBlocker::ScheduledForFuture);
    }
    if !context.workspace_active {
        blockers.push(ReadinessBlocker::WorkspaceArchived);
    }
    if matches!(context.lease, LeaseStatus::Active(_)) {
        blockers.push(ReadinessBlocker::ActiveLease);
    }
    if !context.capacity_available {
        blockers.push(ReadinessBlocker::ConcurrencyLimit);
    }
    if context.execution_attempt_count >= policy.max_attempts() {
        blockers.push(ReadinessBlocker::RetryLimit);
    }
    if item.state.as_ref() == Some(&policy.failed_state) {
        if context.execution_attempt_count == 0 {
            blockers.push(ReadinessBlocker::AttemptHistoryInconsistent);
        } else if context.current_attempt != Some(AttemptPhase::Terminal) {
            blockers.push(ReadinessBlocker::TerminalAttemptRequired);
        }
    }
    add_assignment_blockers(item, policy, context, &mut blockers);
    blockers
}

fn add_assignment_blockers(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
    blockers: &mut Vec<ReadinessBlocker>,
) {
    match policy.claim_policy {
        ClaimPolicy::Open => {}
        ClaimPolicy::AssignmentRestricted => {
            if item.assignee.is_some() && item.assignee.as_deref() != context.actor_id.as_deref() {
                blockers.push(ReadinessBlocker::AssignedToOtherActor);
            }
        }
        ClaimPolicy::ExplicitlyDispatched => match item.assignee.as_deref() {
            None => blockers.push(ReadinessBlocker::AssignmentRequired),
            Some(assignee) if context.actor_id.as_deref() != Some(assignee) => {
                blockers.push(ReadinessBlocker::AssignedToOtherActor);
            }
            Some(_) => {}
        },
    }
}
