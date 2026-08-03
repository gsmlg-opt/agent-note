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
fn rich_heading_projects_supported_semantics() {
    let source = include_str!("fixtures/emacs-rich.org");
    let document = parse_document(source, &options()).unwrap();
    let item = document
        .item(support::id("11111111-1111-4111-8111-111111111111"))
        .unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(item.item_type, WorkItemType::Issue);
    assert_eq!(item.title, "Ship Org foundation");
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.priority, Some('A'));
    assert_eq!(
        item.tags.iter().map(String::as_str).collect::<Vec<_>>(),
        ["backend", "urgent"]
    );
    assert_eq!(item.assignee.as_deref(), Some("agent-alpha"));
    assert!(item.requires_review);
    assert_eq!(
        item.scheduled
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-01 Sat 09:30>")
    );
    assert_eq!(
        item.deadline
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-03 Mon>")
    );
    assert_eq!(
        item.depends_on
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "22222222-2222-4222-8222-222222222222",
            "33333333-3333-4333-8333-333333333333"
        ]
    );
    assert_eq!(item.note_links.len(), 1);
    assert_eq!(item.note_links[0].purpose, "design");
    assert_eq!(
        item.note_links[0].note_id.to_string(),
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    );
    assert_eq!(item.note_links[0].description, "Design note");
}

#[test]
fn identity_survives_title_and_hierarchy_changes() {
    let old_source = "\
* READY Old root title
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: project
:END:
";
    let new_source = "\
* Ordinary Project
** READY New title
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: project
:END:
";

    let old_document = parse_document(old_source, &options()).unwrap();
    let new_document = parse_document(new_source, &options()).unwrap();

    assert_eq!(old_document.items()[0].title, "Old root title");
    assert_eq!(new_document.items()[0].title, "New title");
    assert_eq!(old_document.items()[0].id, new_document.items()[0].id);
}

#[test]
fn heading_looking_lines_inside_blocks_remain_opaque() {
    let source = include_str!("fixtures/opaque-constructs.org");
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(document.items().len(), 1);
}

#[test]
fn unmatched_block_text_inside_custom_drawers_stays_opaque() {
    let source = "\
:CUSTOM_DRAWER:
#+BEGIN_SRC org
* READY Hidden item
:ID: 22222222-2222-4222-8222-222222222222
:AGENT_NOTE_TYPE: task
[[agent-note:hidden:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb][Hidden link]]
:END:
* READY Visible item
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Visible link]]
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(document.items().len(), 1);
    let item = &document.items()[0];
    assert_eq!(item.id, support::id("11111111-1111-4111-8111-111111111111"));
    assert_eq!(item.note_links.len(), 1);
    assert_eq!(item.note_links[0].purpose, "design");
    assert_eq!(item.note_links[0].description, "Visible link");
}

#[test]
fn spaced_lf_drawer_ends_restore_heading_and_link_scanning() {
    let source = "\
:CUSTOM_DRAWER:
* READY Hidden item
:ID: 22222222-2222-4222-8222-222222222222
:AGENT_NOTE_TYPE: task
[[agent-note:hidden:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb]]
:END:\x20\x20\x20
* READY Visible item
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
:CUSTOM_DRAWER:
[[agent-note:hidden:cccccccc-cccc-4ccc-8ccc-cccccccccccc]]
:END:\x20\x20\x20
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(document.items().len(), 1);
    let item = &document.items()[0];
    assert_eq!(item.id, support::id("11111111-1111-4111-8111-111111111111"));
    assert_eq!(item.note_links.len(), 1);
    assert_eq!(item.note_links[0].purpose, "design");
}

#[test]
fn spaced_crlf_drawer_ends_restore_heading_and_link_scanning() {
    let source = ":CUSTOM_DRAWER:\r\n* READY Hidden item\r\n:ID: 22222222-2222-4222-8222-222222222222\r\n:AGENT_NOTE_TYPE: task\r\n[[agent-note:hidden:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb]]\r\n:END:   \r\n* READY Visible item\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n:CUSTOM_DRAWER:\r\n[[agent-note:hidden:cccccccc-cccc-4ccc-8ccc-cccccccccccc]]\r\n:END:   \r\n[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]\r\n";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(document.items().len(), 1);
    let item = &document.items()[0];
    assert_eq!(item.id, support::id("11111111-1111-4111-8111-111111111111"));
    assert_eq!(item.note_links.len(), 1);
    assert_eq!(item.note_links[0].purpose, "design");
}

#[test]
fn crlf_source_roundtrips_without_normalization() {
    let source = "* READY CRLF item\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
}

#[test]
fn malformed_agent_note_links_and_unknown_properties_remain_opaque() {
    let source = "\
* READY Opaque extensions
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:CUSTOM_FIELD: Keep this opaque value
:END:
[[agent-note:design:not-a-uuid][Opaque link]].
";
    let document = parse_document(source, &options()).unwrap();
    let item = &document.items()[0];

    assert_eq!(document.source(), source);
    assert!(item.note_links.is_empty());
}

#[test]
fn descriptionless_agent_note_links_project_with_an_empty_description() {
    let source = "\
* READY Links
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]] and [[agent-note:review:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb][Review note]].
";

    let document = parse_document(source, &options()).unwrap();
    let links = &document.items()[0].note_links;

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].purpose, "design");
    assert_eq!(links[0].description, "");
    assert_eq!(links[1].purpose, "review");
    assert_eq!(links[1].description, "Review note");
}

#[test]
fn balanced_brackets_are_valid_in_agent_note_link_descriptions() {
    let source = "\
* READY Link coverage
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
[[agent-note:context:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Coverage [50%] report]]
[[agent-note:context:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb][[nested [ok]][sibling]]]
";

    let document = parse_document(source, &options()).unwrap();
    let links = &document.items()[0].note_links;

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].purpose, "context");
    assert_eq!(links[0].description, "Coverage [50%] report");
    assert_eq!(links[1].description, "[nested [ok]][sibling]");
}

#[test]
fn malformed_outer_links_do_not_hide_later_valid_links() {
    let source = "\
* READY Link recovery
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
[[broken [[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]
";

    let document = parse_document(source, &options()).unwrap();
    let links = &document.items()[0].note_links;

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].purpose, "design");
    assert_eq!(links[0].description, "");
}

#[test]
fn large_malformed_link_prefix_recovers_the_final_valid_link() {
    let mut source = "* READY Link recovery\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n".to_string();
    source.push_str(&"[[x".repeat(20_000));
    source.push_str("[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]\n");

    let document = parse_document(&source, &options()).unwrap();
    let links = &document.items()[0].note_links;

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].purpose, "design");
    assert_eq!(links[0].description, "");
}

#[test]
fn malformed_agent_note_link_grammar_remains_opaque() {
    let source = "\
* READY Opaque links
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
[[agent-note:design]]
[[agent-note::aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:extra]]
[[agent-note:design:not-a-uuid]]
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][desc][extra]]
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][desc[extra]]
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][desc]extra]]
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert!(document.items()[0].note_links.is_empty());
}

#[test]
fn body_property_drawers_do_not_orchestrate_ordinary_headings() {
    let source = "\
* Ordinary heading
Body text keeps the later drawer in the body.
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:ASSIGNEE: hidden-agent
:END:
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert!(document.items().is_empty());
}

#[test]
fn projected_items_use_only_the_direct_property_drawer() {
    let source = "\
* READY Direct metadata
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:ASSIGNEE: direct-agent
:END:
Body text.
:PROPERTIES:
:ASSIGNEE: hidden-agent
:DEPENDS_ON: 22222222-2222-4222-8222-222222222222
:REQUIRES_REVIEW: true
:END:
";

    let document = parse_document(source, &options()).unwrap();
    let item = &document.items()[0];

    assert_eq!(document.source().as_bytes(), source.as_bytes());
    assert_eq!(item.assignee.as_deref(), Some("direct-agent"));
    assert!(item.depends_on.is_empty());
    assert!(!item.requires_review);
}

#[test]
fn agent_note_links_in_opaque_constructs_remain_opaque() {
    let source = "\
* READY Opaque examples
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
# [[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa]]
#+CAPTION: [[agent-note:design:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb]]
#+BEGIN_SRC org
[[agent-note:design:cccccccc-cccc-4ccc-8ccc-cccccccccccc]]
#+END_SRC
:CUSTOM_DRAWER:
[[agent-note:design:dddddddd-dddd-4ddd-8ddd-dddddddddddd]]
:END:
";

    let document = parse_document(source, &options()).unwrap();
    let item = &document.items()[0];

    assert_eq!(document.source(), source);
    assert!(item.note_links.is_empty());
}

fn assert_parse_error(source: &str, expected_line: usize, expected_message: &str) {
    match parse_document(source, &options()) {
        Err(OrgError::Parse { line, message }) => {
            assert_eq!(line, expected_line);
            assert!(
                message.contains(expected_message),
                "expected {message:?} to contain {expected_message:?}"
            );
        }
        other => panic!("expected parse error, got {other:?}"),
    }
}

#[test]
fn malformed_work_item_uuid_reports_its_source_line() {
    let source = "\
* READY Invalid ID
:PROPERTIES:
:ID: not-a-uuid
:AGENT_NOTE_TYPE: task
:END:
";

    assert_parse_error(source, 3, "invalid ID");
}

#[test]
fn duplicate_work_item_ids_report_the_typed_error() {
    let source = "\
* READY First
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
* READY Second
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: issue
:END:
";

    assert!(matches!(
        parse_document(source, &options()),
        Err(OrgError::DuplicateId(id))
            if id == support::id("11111111-1111-4111-8111-111111111111")
    ));
}

#[test]
fn unknown_agent_note_type_reports_its_source_line() {
    let source = "\
* READY Unknown type
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: unknown
:END:
";

    assert_parse_error(source, 4, "unsupported work item type");
}

#[test]
fn invalid_requires_review_reports_its_source_line() {
    let source = "\
* READY Invalid review flag
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:REQUIRES_REVIEW: yes
:END:
";

    assert_parse_error(source, 5, "REQUIRES_REVIEW");
}

#[test]
fn duplicate_requires_review_is_rejected_at_the_second_occurrence() {
    let source = "\
* READY Duplicate review flag
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:REQUIRES_REVIEW: true
:REQUIRES_REVIEW: false
:END:
";

    assert_parse_error(source, 6, "REQUIRES_REVIEW must occur exactly once");
}

#[test]
fn accumulated_requires_review_is_rejected_at_its_source_line() {
    let source = "\
* READY Accumulated review flag
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:REQUIRES_REVIEW+: true
:END:
";

    assert_parse_error(source, 5, "REQUIRES_REVIEW must occur exactly once");
}

#[test]
fn invalid_dependency_uuid_reports_its_source_line() {
    let source = "\
* READY Invalid dependency
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:DEPENDS_ON: not-a-uuid
:END:
";

    assert_parse_error(source, 5, "dependency ID");
}

#[test]
fn duplicate_depends_on_is_rejected_at_the_second_occurrence() {
    let source = "\
* READY Duplicate dependency property
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:DEPENDS_ON: 22222222-2222-4222-8222-222222222222
:DEPENDS_ON: 11111111-1111-4111-8111-111111111111
:END:
";

    assert_parse_error(source, 6, "DEPENDS_ON must occur exactly once");
}

#[test]
fn accumulated_depends_on_is_rejected_at_its_source_line() {
    let source = "\
* READY Accumulated dependency property
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:DEPENDS_ON+: 11111111-1111-4111-8111-111111111111
:END:
";

    assert_parse_error(source, 5, "DEPENDS_ON must occur exactly once");
}

#[test]
fn duplicate_dependency_ids_report_their_source_line() {
    let source = "\
* READY Duplicate dependency
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:DEPENDS_ON: 22222222-2222-4222-8222-222222222222 22222222-2222-4222-8222-222222222222
:END:
";

    assert_parse_error(source, 5, "duplicate dependency ID");
}

#[test]
fn self_dependency_reports_its_source_line() {
    let source = "\
* READY Self dependency
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:DEPENDS_ON: 11111111-1111-4111-8111-111111111111
:END:
";

    assert_parse_error(source, 5, "depend on itself");
}

#[test]
fn ambiguous_assignee_remains_deferred_and_lossless() {
    let source = "\
* READY Ambiguous assignee
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:ASSIGNEE: agent-alpha
:ASSIGNEE+: agent-beta
:END:
";

    let document = parse_document(source, &options()).unwrap();
    let item = &document.items()[0];

    assert_eq!(document.source(), source);
    assert_eq!(item.assignee, None);
}

#[test]
fn planning_repeaters_are_rejected_for_scheduled_and_deadline() {
    for (keyword, timestamp) in [
        ("SCHEDULED", "<2026-08-01 Sat +1d>"),
        ("DEADLINE", "<2026-08-03 Mon ++1w>"),
    ] {
        let source = format!(
            "\
* READY Repeating planning
{keyword}: {timestamp}
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
"
        );

        assert_parse_error(&source, 2, "repeaters and time ranges are unsupported");
    }
}

#[test]
fn duplicate_planning_rejects_an_invalid_first_value_at_the_second_line() {
    for keyword in ["SCHEDULED", "DEADLINE"] {
        let source = format!(
            "\
* READY Duplicate planning
{keyword}: not-a-timestamp
{keyword}: <2026-08-01 Sat 09:30>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
"
        );

        assert_parse_error(&source, 3, "planning keyword must not be repeated");
    }
}

#[test]
fn duplicate_valid_planning_is_rejected_at_the_second_line() {
    for keyword in ["SCHEDULED", "DEADLINE"] {
        let source = format!(
            "\
* READY Duplicate planning
{keyword}: <2026-08-01 Sat>
{keyword}: <2026-08-02 Sun>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
"
        );

        assert_parse_error(&source, 3, "planning keyword must not be repeated");
    }
}

#[test]
fn planning_time_ranges_are_rejected_for_scheduled_and_deadline() {
    for keyword in ["SCHEDULED", "DEADLINE"] {
        let source = format!(
            "\
* READY Ranged planning
{keyword}: <2026-08-01 Sat 09:30-10:30>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
"
        );

        assert_parse_error(&source, 2, "repeaters and time ranges are unsupported");
    }
}

#[test]
fn planning_and_properties_after_body_content_remain_opaque() {
    let source = "\
* READY Late planning
Normal body content.
SCHEDULED: <2026-08-01 Sat 09:30>
DEADLINE: <2026-08-03 Mon>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
";

    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source(), source);
    assert!(document.items().is_empty());
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
