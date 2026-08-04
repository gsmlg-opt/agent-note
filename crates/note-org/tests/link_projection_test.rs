use note_org::{parse_document, ParseOptions, WorkItemId};
use std::str::FromStr;

#[test]
fn projected_links_exclude_opaque_regions_and_keep_source_order() {
    let item_id = WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    let source = "* READY Links\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n:CUSTOM_DRAWER:\r\n[[agent-note:hidden:cccccccc-cccc-4ccc-8ccc-cccccccccccc]]\r\n:END:\r\n[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Design]] and [[agent-note:review:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb]]\r\n";
    let document = parse_document(
        source,
        &ParseOptions::new(["BACKLOG", "READY", "RUNNING", "DONE"]),
    )
    .unwrap();
    let links = &document.item(item_id).unwrap().note_links;

    assert_eq!(links.len(), 2);
    assert_eq!(links[0].purpose, "design");
    assert_eq!(links[0].description, "Design");
    assert_eq!(links[1].purpose, "review");
    assert_eq!(links[1].description, "");
    assert_eq!(document.source(), source);
}
