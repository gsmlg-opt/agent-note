use note_org::{WorkItem, WorkItemId, WorkItemType};
use std::{collections::BTreeSet, str::FromStr};

pub fn id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

pub fn item<const N: usize>(item_id: &str, state: &str, dependencies: [&str; N]) -> WorkItem {
    WorkItem {
        id: id(item_id),
        item_type: WorkItemType::Task,
        parent_id: None,
        level: 1,
        title: format!("Item {item_id}"),
        state: Some(state.to_string()),
        priority: None,
        tags: BTreeSet::new(),
        scheduled: None,
        deadline: None,
        assignee: None,
        depends_on: dependencies.into_iter().map(id).collect(),
        requires_review: false,
        note_links: vec![],
    }
}

pub fn ready_item() -> WorkItem {
    item("11111111-1111-4111-8111-111111111111", "READY", [])
}
