#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DependencyError {
    #[error("work item {item_id} depends on missing item {dependency_id}")]
    MissingTarget {
        item_id: crate::WorkItemId,
        dependency_id: crate::WorkItemId,
    },
    #[error("work item {item_id} depends on itself")]
    SelfDependency { item_id: crate::WorkItemId },
    #[error("dependency cycle: {path:?}")]
    Cycle { path: Vec<crate::WorkItemId> },
}
