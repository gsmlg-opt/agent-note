use note_org::{WorkItemId, WorkItemType};
use std::str::FromStr;

#[test]
fn stable_work_item_ids_and_types_parse_from_canonical_values() {
    let id = WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    assert_eq!(id.to_string(), "11111111-1111-4111-8111-111111111111");
    assert_eq!(WorkItemType::from_str("task").unwrap(), WorkItemType::Task);
    assert!(WorkItemType::from_str("unknown").is_err());
}
