use crate::{WorkItem, WorkItemId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DependencyError {
    #[error("duplicate work item id {item_id}")]
    DuplicateItem { item_id: crate::WorkItemId },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    edges: BTreeMap<WorkItemId, BTreeSet<WorkItemId>>,
    successful: BTreeSet<WorkItemId>,
}

impl DependencyGraph {
    pub fn dependencies_satisfied(&self, item_id: WorkItemId) -> bool {
        self.edges
            .get(&item_id)
            .is_some_and(|dependencies| dependencies.is_subset(&self.successful))
    }
}

pub fn validate_dependencies(
    items: &[WorkItem],
    successful_states: &BTreeSet<String>,
) -> Result<DependencyGraph, DependencyError> {
    let mut items_by_id = BTreeMap::new();
    let mut duplicate_ids = BTreeSet::new();
    for item in items {
        match items_by_id.entry(item.id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(item);
            }
            std::collections::btree_map::Entry::Occupied(_) => {
                duplicate_ids.insert(item.id);
            }
        }
    }
    if let Some(item_id) = duplicate_ids.first() {
        return Err(DependencyError::DuplicateItem { item_id: *item_id });
    }

    for (item_id, item) in &items_by_id {
        if item.depends_on.contains(item_id) {
            return Err(DependencyError::SelfDependency { item_id: *item_id });
        }
    }

    for (item_id, item) in &items_by_id {
        for dependency_id in &item.depends_on {
            if !items_by_id.contains_key(dependency_id) {
                return Err(DependencyError::MissingTarget {
                    item_id: *item_id,
                    dependency_id: *dependency_id,
                });
            }
        }
    }

    let edges: BTreeMap<_, _> = items_by_id
        .iter()
        .map(|(item_id, item)| (*item_id, item.depends_on.clone()))
        .collect();
    reject_cycles(&edges)?;

    let successful = items_by_id
        .values()
        .filter(|item| {
            item.state
                .as_ref()
                .is_some_and(|state| successful_states.contains(state))
        })
        .map(|item| item.id)
        .collect();

    Ok(DependencyGraph { edges, successful })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    White,
    Gray,
    Black,
}

struct VisitFrame<'a> {
    item_id: WorkItemId,
    dependencies: std::collections::btree_set::Iter<'a, WorkItemId>,
}

fn reject_cycles(
    edges: &BTreeMap<WorkItemId, BTreeSet<WorkItemId>>,
) -> Result<(), DependencyError> {
    let mut colors: BTreeMap<_, _> = edges
        .keys()
        .map(|item_id| (*item_id, Color::White))
        .collect();
    let mut active_path = Vec::new();
    let mut active_indices = BTreeMap::new();

    for root_id in edges.keys() {
        if colors[root_id] != Color::White {
            continue;
        }

        colors.insert(*root_id, Color::Gray);
        active_indices.insert(*root_id, 0);
        active_path.push(*root_id);
        let mut frames = vec![VisitFrame {
            item_id: *root_id,
            dependencies: edges[root_id].iter(),
        }];

        while !frames.is_empty() {
            let dependency_id = frames
                .last_mut()
                .and_then(|frame| frame.dependencies.next().copied());

            if let Some(dependency_id) = dependency_id {
                match colors[&dependency_id] {
                    Color::White => {
                        colors.insert(dependency_id, Color::Gray);
                        active_indices.insert(dependency_id, active_path.len());
                        active_path.push(dependency_id);
                        frames.push(VisitFrame {
                            item_id: dependency_id,
                            dependencies: edges[&dependency_id].iter(),
                        });
                    }
                    Color::Gray => {
                        let cycle_start = active_indices[&dependency_id];
                        return Err(DependencyError::Cycle {
                            path: canonical_cycle(&active_path[cycle_start..]),
                        });
                    }
                    Color::Black => {}
                }
            } else {
                let completed = frames.pop().expect("DFS frame stack is non-empty").item_id;
                let active = active_path
                    .pop()
                    .expect("each DFS frame has an active path entry");
                debug_assert_eq!(active, completed);
                active_indices.remove(&completed);
                colors.insert(completed, Color::Black);
            }
        }
    }

    Ok(())
}

fn canonical_cycle(cycle: &[WorkItemId]) -> Vec<WorkItemId> {
    let minimum_index = cycle
        .iter()
        .enumerate()
        .min_by_key(|(_, item_id)| *item_id)
        .map(|(index, _)| index)
        .expect("dependency cycles are non-empty");
    let mut canonical = cycle[minimum_index..].to_vec();
    canonical.extend_from_slice(&cycle[..minimum_index]);
    canonical.push(canonical[0]);
    canonical
}
