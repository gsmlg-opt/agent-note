#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{role} state {state} is not configured")]
    MissingRoleState { role: String, state: String },
    #[error("transition endpoint {state} is not configured")]
    TransitionStateMissing { state: String },
    #[error("{role} state {state} must be executable")]
    RecoveryStateNotExecutable { role: String, state: String },
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
