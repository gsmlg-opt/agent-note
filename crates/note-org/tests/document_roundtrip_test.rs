mod support;

use note_org::{parse_document, OrgError, ParseOptions, WorkItemId, WorkItemType};
use std::str::FromStr;

fn options() -> ParseOptions {
    ParseOptions::new([
        "BACKLOG",
        "READY",
        "RUNNING",
        "BLOCKED",
        "REVIEW",
        "DONE",
        "FAILED",
        "CANCELLED",
    ])
}

#[test]
fn stable_work_item_ids_and_types_parse_from_canonical_values() {
    let id = WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    assert_eq!(id.to_string(), "11111111-1111-4111-8111-111111111111");
    assert_eq!(WorkItemType::from_str("task").unwrap(), WorkItemType::Task);
    assert!(WorkItemType::from_str("unknown").is_err());
}

#[test]
fn emacs_document_roundtrips_byte_for_byte() {
    let source = include_str!("fixtures/emacs-rich.org");
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
}

#[test]
fn heading_looking_lines_inside_blocks_remain_opaque() {
    let source = include_str!("fixtures/opaque-constructs.org");
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(document.items().len(), 1);
}

#[test]
fn crlf_source_roundtrips_without_normalization() {
    let source = "* READY CRLF item\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
}

#[test]
fn deferred_metadata_remains_opaque_in_the_source_model() {
    let source = "\
* READY [#A] Deferred metadata :later:
SCHEDULED: not-a-timestamp
DEADLINE: <2026-08-03 Mon +1d>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:ASSIGNEE: agent-alpha
:DEPENDS_ON: not-a-uuid not-a-uuid
:REQUIRES_REVIEW: maybe
:END:
[[agent-note:design:not-a-uuid][Opaque link]].
";
    let document = parse_document(source, &options()).unwrap();
    let item = &document.items()[0];

    assert_eq!(document.source(), source);
    assert_eq!(item.state, None);
    assert_eq!(item.priority, None);
    assert!(item.tags.is_empty());
    assert_eq!(item.scheduled, None);
    assert_eq!(item.deadline, None);
    assert_eq!(item.assignee, None);
    assert!(item.depends_on.is_empty());
    assert!(!item.requires_review);
    assert!(item.note_links.is_empty());
}

#[test]
fn whitespace_only_required_properties_return_parse_errors() {
    let empty_id = "\
* READY Empty ID
:PROPERTIES:
:ID:\x20
:AGENT_NOTE_TYPE: task
:END:
";
    let empty_marker = "\
* READY Empty marker
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE:\x20
:END:
";

    for source in [empty_id, empty_marker] {
        assert!(matches!(
            parse_document(source, &options()),
            Err(OrgError::Parse { .. })
        ));
    }
}

#[test]
fn whitespace_only_unknown_property_remains_losslessly_opaque() {
    let source = "\
* READY Empty custom property
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:CUSTOM:\x20
:END:
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source(), source);
    assert_eq!(document.items().len(), 1);
}

#[test]
fn projected_parent_must_be_an_actual_heading_ancestor() {
    let source = "\
* READY Projected sibling
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
* Ordinary sibling
** READY Projected child
:PROPERTIES:
:ID: 22222222-2222-4222-8222-222222222222
:AGENT_NOTE_TYPE: task
:END:
";

    let document = parse_document(source, &options()).unwrap();
    let child = document
        .item(support::id("22222222-2222-4222-8222-222222222222"))
        .unwrap();

    assert_eq!(child.parent_id, None);
}
