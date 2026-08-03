use crate::{ClaimPolicy, WorkItem, WorkspacePolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessContext {
    pub now: i64,
    pub dependencies_satisfied: bool,
    pub workspace_active: bool,
    pub active_lease: bool,
    pub capacity_available: bool,
    pub actor_id: Option<String>,
    pub scheduled_at: Option<i64>,
    pub lease_expired_running: bool,
}

impl ReadinessContext {
    pub fn ready_at(now: i64) -> Self {
        Self {
            now,
            dependencies_satisfied: true,
            workspace_active: true,
            active_lease: false,
            capacity_available: true,
            actor_id: None,
            scheduled_at: None,
            lease_expired_running: false,
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
    ConcurrencyLimit,
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
    let recovery_candidate =
        item.state.as_ref() == Some(&policy.running_state) && context.lease_expired_running;
    let mut blockers = Vec::new();

    if !recovery_candidate
        && !item
            .state
            .as_ref()
            .is_some_and(|state| policy.executable_states.contains(state))
    {
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
    if context.active_lease && !recovery_candidate {
        blockers.push(ReadinessBlocker::ActiveLease);
    }
    if !context.capacity_available {
        blockers.push(ReadinessBlocker::ConcurrencyLimit);
    }

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

    if !blockers.is_empty() {
        Readiness::Blocked(blockers)
    } else if recovery_candidate {
        Readiness::RecoveryCandidate
    } else {
        Readiness::Ready
    }
}
