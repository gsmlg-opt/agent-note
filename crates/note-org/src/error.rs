#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrgError {
    #[error("line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("work item {0} was not found")]
    ItemNotFound(crate::WorkItemId),
    #[error("duplicate work item id {0}")]
    DuplicateId(crate::WorkItemId),
    #[error("semantic edit cannot safely isolate work item {0}")]
    UnsafeEdit(crate::WorkItemId),
    #[error(transparent)]
    Policy(#[from] crate::PolicyError),
    #[error(transparent)]
    Dependency(#[from] crate::DependencyError),
}
