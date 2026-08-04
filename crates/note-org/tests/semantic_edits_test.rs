use note_org::{
    move_item, parse_document, reparent_item, NewWorkItem, NoteLink, OrgError, ParseOptions,
    PropertyKey, SemanticEdit, WorkItemId, WorkItemType,
};
use std::{collections::BTreeSet, str::FromStr};
use uuid::Uuid;

fn id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

fn options() -> ParseOptions {
    ParseOptions::new(["BACKLOG", "READY", "RUNNING", "DONE"])
}

#[test]
fn transition_changes_only_the_todo_token() {
    let source = "* READY Keep title :tag:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:CUSTOM: keep\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();

    let edited = document
        .apply(SemanticEdit::SetState {
            item_id: id("11111111-1111-4111-8111-111111111111"),
            state: "RUNNING".to_string(),
        })
        .unwrap();

    assert_eq!(edited.source, source.replacen("* READY ", "* RUNNING ", 1));
    assert_eq!(
        edited.changed_items,
        BTreeSet::from([id("11111111-1111-4111-8111-111111111111")])
    );
    assert_eq!(
        parse_document(&edited.source, &options())
            .unwrap()
            .item(id("11111111-1111-4111-8111-111111111111"))
            .unwrap()
            .state
            .as_deref(),
        Some("RUNNING")
    );
}

#[test]
fn transition_inserts_a_missing_configured_state_after_the_heading_marker() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* Keep title :tag:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::SetState {
            item_id,
            state: "READY".to_string(),
        })
        .unwrap();
    assert_eq!(edited.source, source.replacen("* ", "* READY ", 1),);
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));
    let reparsed = parse_document(&edited.source, &options()).unwrap();
    let item = reparsed.item(item_id).unwrap();
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.title, "Keep title");
    assert_eq!(item.tags, BTreeSet::from(["tag".to_string()]));
    assert_eq!(document.source(), source);
    assert!(document
        .apply(SemanticEdit::SetState {
            item_id,
            state: "UNKNOWN".to_string(),
        })
        .is_err());
}

#[test]
fn priority_without_todo_projects_and_edits_independently() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* [#A] Ship\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let item = document.item(item_id).unwrap();
    assert_eq!(item.state, None);
    assert_eq!(item.priority, Some('A'));
    assert_eq!(item.title, "Ship");

    let transitioned = document
        .apply(SemanticEdit::SetState {
            item_id,
            state: "READY".to_string(),
        })
        .unwrap();
    assert_eq!(transitioned.source, source.replacen("* ", "* READY ", 1));

    let reprioritized = document
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: Some('B'),
        })
        .unwrap();
    assert_eq!(reprioritized.source, source.replacen("[#A]", "[#B]", 1));

    let cleared = document
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: None,
        })
        .unwrap();
    assert_eq!(cleared.source, source.replacen(" [#A]", "", 1));

    let without_priority = source.replace(" [#A]", "");
    let inserted = parse_document(&without_priority, &options())
        .unwrap()
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: Some('C'),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        without_priority.replacen("* ", "* [#C] ", 1)
    );
}

#[test]
fn tab_separated_priority_cookies_project_with_and_without_todo() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    for (source, expected_state) in [
        (
            "* READY\t[#A]\tShip\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
            Some("READY"),
        ),
        (
            "* \t[#A]\tShip\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
            None,
        ),
    ] {
        let document = parse_document(source, &options()).unwrap();
        let item = document.item(item_id).unwrap();
        assert_eq!(item.state.as_deref(), expected_state);
        assert_eq!(item.priority, Some('A'));
        assert_eq!(item.title, "Ship");
    }
}

#[test]
fn priority_cookie_requires_a_horizontal_boundary_after_it() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* [#A]Ship\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let item = document.item(item_id).unwrap();

    assert_eq!(item.state, None);
    assert_eq!(item.priority, None);
    assert_eq!(item.title, "[#A]Ship");
}

#[test]
fn priority_cookie_accepts_end_of_line_after_the_cookie() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    for heading in ["* [#A]\n", "* [#A]\t\n"] {
        let source = format!(
            "{heading}:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n"
        );
        let document = parse_document(source, &options()).unwrap();
        let item = document.item(item_id).unwrap();

        assert_eq!(item.state, None);
        assert_eq!(item.priority, Some('A'));
        assert_eq!(item.title, "");
    }
}

#[test]
fn clearing_and_readding_priority_keeps_an_empty_title_heading_valid() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    for line_ending in ["\n", "\r\n"] {
        let source = format!(
            "* [#A]{line_ending}:PROPERTIES:{line_ending}:ID: 11111111-1111-4111-8111-111111111111{line_ending}:AGENT_NOTE_TYPE: task{line_ending}:END:{line_ending}"
        );
        let expected_cleared = source.replacen("* [#A]", "* ", 1);
        let cleared = parse_document(&source, &options())
            .unwrap()
            .apply(SemanticEdit::SetPriority {
                item_id,
                priority: None,
            })
            .unwrap();
        assert_eq!(cleared.source, expected_cleared);
        let cleared_document = parse_document(&cleared.source, &options()).unwrap();
        let cleared_item = cleared_document.item(item_id).unwrap();
        assert_eq!(cleared_item.priority, None);
        assert_eq!(cleared_item.title, "");

        let restored = cleared_document
            .apply(SemanticEdit::SetPriority {
                item_id,
                priority: Some('A'),
            })
            .unwrap();
        assert_eq!(restored.source, source);
        assert_eq!(
            parse_document(&restored.source, &options())
                .unwrap()
                .item(item_id)
                .unwrap()
                .priority,
            Some('A')
        );
    }
}

#[test]
fn priority_edits_preserve_tab_separators_without_duplicating_cookies() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    for source in [
        "* READY\t[#A]\tShip\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
        "* \t[#A]\tShip\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
    ] {
        let document = parse_document(source, &options()).unwrap();
        let replaced = document
            .apply(SemanticEdit::SetPriority {
                item_id,
                priority: Some('B'),
            })
            .unwrap();
        assert_eq!(replaced.source, source.replacen("[#A]", "[#B]", 1));
        assert_eq!(replaced.source.matches("[#").count(), 1);

        let cleared = document
            .apply(SemanticEdit::SetPriority {
                item_id,
                priority: None,
            })
            .unwrap();
        let expected_cleared = source.replacen("[#A]\t", "", 1);
        assert_eq!(cleared.source, expected_cleared);

        let restored = parse_document(&cleared.source, &options())
            .unwrap()
            .apply(SemanticEdit::SetPriority {
                item_id,
                priority: Some('A'),
            })
            .unwrap();
        assert_eq!(restored.source, source);
        assert_eq!(restored.source.matches("[#").count(), 1);
    }
}

#[test]
fn create_item_uses_the_caller_supplied_id() {
    let document = parse_document("", &options()).unwrap();
    let item_id = id("44444444-4444-4444-8444-444444444444");
    let edited = document
        .apply(SemanticEdit::AppendItem {
            parent_id: None,
            item: NewWorkItem {
                id: item_id,
                item_type: WorkItemType::Task,
                title: "Write parser".to_string(),
                state: "BACKLOG".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        })
        .unwrap();

    assert_eq!(
        edited.source,
        "* BACKLOG Write parser\n:PROPERTIES:\n:ID: 44444444-4444-4444-8444-444444444444\n:AGENT_NOTE_TYPE: task\n:END:\n"
    );
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));
    let reparsed = parse_document(&edited.source, &options()).unwrap();
    let item = reparsed.item(item_id).unwrap();
    assert_eq!(item.id, item_id);
    assert_eq!(item.item_type, WorkItemType::Task);
    assert_eq!(item.parent_id, None);
    assert_eq!(item.level, 1);
    assert_eq!(item.title, "Write parser");
    assert_eq!(item.state.as_deref(), Some("BACKLOG"));
    assert_eq!(item.priority, None);
    assert!(item.tags.is_empty());
}

#[test]
fn append_item_becomes_the_final_child_without_rewriting_surroundings() {
    let parent_id = id("11111111-1111-4111-8111-111111111111");
    let new_id = id("44444444-4444-4444-8444-444444444444");
    let sibling = "* READY Sibling\n:PROPERTIES:\n:ID: 33333333-3333-4333-8333-333333333333\n:AGENT_NOTE_TYPE: project\n:END:\n";
    let source = format!(
        "* READY Parent\n:PROPERTIES:\n:ID: {parent_id}\n:AGENT_NOTE_TYPE: project\n:END:\nParent body.\n** DONE Existing\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\nExisting body.\n{sibling}"
    );
    let document = parse_document(&source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::AppendItem {
            parent_id: Some(parent_id),
            item: NewWorkItem {
                id: new_id,
                item_type: WorkItemType::Review,
                title: "Review parser".to_string(),
                state: "BACKLOG".to_string(),
                priority: Some('B'),
                tags: BTreeSet::from(["beta".to_string(), "alpha".to_string()]),
            },
        })
        .unwrap();
    let rendered = "** BACKLOG [#B] Review parser :alpha:beta:\n:PROPERTIES:\n:ID: 44444444-4444-4444-8444-444444444444\n:AGENT_NOTE_TYPE: review\n:END:\n";
    assert_eq!(
        edited.source,
        source.replacen(sibling, &format!("{rendered}{sibling}"), 1)
    );
    let reparsed = parse_document(edited.source, &options()).unwrap();
    let item = reparsed.item(new_id).unwrap();
    assert_eq!(item.item_type, WorkItemType::Review);
    assert_eq!(item.parent_id, Some(parent_id));
    assert_eq!(item.level, 2);
    assert_eq!(item.title, "Review parser");
    assert_eq!(item.state.as_deref(), Some("BACKLOG"));
    assert_eq!(item.priority, Some('B'));
    assert_eq!(
        item.tags,
        BTreeSet::from(["alpha".to_string(), "beta".to_string()])
    );
    assert_eq!(edited.changed_items, BTreeSet::from([new_id]));

    assert_eq!(
        document.apply(SemanticEdit::AppendItem {
            parent_id: None,
            item: NewWorkItem {
                id: parent_id,
                item_type: WorkItemType::Task,
                title: "Duplicate".to_string(),
                state: "READY".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        }),
        Err(OrgError::DuplicateId(parent_id))
    );
}

#[test]
fn append_item_rejects_an_id_used_by_an_ordinary_heading() {
    let duplicate_id = id("44444444-4444-4444-8444-444444444444");
    let source = "* Ordinary heading\n:PROPERTIES:\n:ID: 44444444-4444-4444-8444-444444444444\n:END:\n* READY Projected\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(
        document.apply(SemanticEdit::AppendItem {
            parent_id: None,
            item: NewWorkItem {
                id: duplicate_id,
                item_type: WorkItemType::Task,
                title: "Duplicate ordinary ID".to_string(),
                state: "READY".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        }),
        Err(OrgError::DuplicateId(duplicate_id))
    );

    let malformed_source = source.replacen("44444444-4444-4444-8444-444444444444", "not-a-uuid", 1);
    let document = parse_document(malformed_source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::AppendItem {
            parent_id: None,
            item: NewWorkItem {
                id: duplicate_id,
                item_type: WorkItemType::Task,
                title: "Valid new ID".to_string(),
                state: "READY".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        })
        .unwrap();
    assert_eq!(edited.changed_items, BTreeSet::from([duplicate_id]));
    let reparsed = parse_document(&edited.source, &options()).unwrap();
    let item = reparsed.item(duplicate_id).unwrap();
    assert_eq!(item.parent_id, None);
    assert_eq!(item.level, 1);
    assert_eq!(item.title, "Valid new ID");
    assert_eq!(item.state.as_deref(), Some("READY"));
}

#[test]
fn append_item_rejects_titles_that_reparse_as_heading_syntax() {
    let document = parse_document("", &options()).unwrap();
    for (item_id, title) in [
        (id("44444444-4444-4444-8444-444444444444"), "[#A] Ship"),
        (
            id("55555555-5555-4555-8555-555555555555"),
            "Ship :internal:",
        ),
    ] {
        assert_eq!(
            document.apply(SemanticEdit::AppendItem {
                parent_id: None,
                item: NewWorkItem {
                    id: item_id,
                    item_type: WorkItemType::Task,
                    title: title.to_string(),
                    state: "READY".to_string(),
                    priority: None,
                    tags: BTreeSet::new(),
                },
            }),
            Err(OrgError::UnsafeEdit(item_id))
        );
        assert_eq!(document.source(), "");
    }
}

#[test]
fn assignment_edits_preserve_every_other_byte() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:ASSIGNEE: old-agent\n:CUSTOM: keep\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();
    let replaced = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("new-agent".to_string()),
        })
        .unwrap();
    assert_eq!(
        replaced.source,
        source.replacen("old-agent", "new-agent", 1)
    );
    assert_eq!(replaced.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&replaced.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .assignee
            .as_deref(),
        Some("new-agent")
    );

    let without_assignee = source.replace(":ASSIGNEE: old-agent\n", "");
    let document = parse_document(&without_assignee, &options()).unwrap();
    let inserted = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("new-agent".to_string()),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        without_assignee.replacen(":END:", ":ASSIGNEE: new-agent\n:END:", 1)
    );
    assert_eq!(inserted.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&inserted.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .assignee
            .as_deref(),
        Some("new-agent")
    );

    let document = parse_document(source, &options()).unwrap();
    let removed = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: None,
        })
        .unwrap();
    assert_eq!(removed.source, without_assignee);
    assert_eq!(removed.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&removed.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .assignee,
        None
    );
    assert!(removed.source.contains(":CUSTOM: keep\n"));
}

#[test]
fn property_edits_insert_only_into_the_direct_drawer() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\nBody.\n:PROPERTIES:\n:ASSIGNEE: hidden-agent\n:CUSTOM: byte-identical\n:END:\n";
    let document = parse_document(source, &options()).unwrap();

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("direct-agent".to_string()),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(":END:", ":ASSIGNEE: direct-agent\n:END:", 1)
    );
    assert!(edited
        .source
        .contains(":ASSIGNEE: hidden-agent\n:CUSTOM: byte-identical\n:END:\n"));
    assert_eq!(
        parse_document(&edited.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .assignee
            .as_deref(),
        Some("direct-agent")
    );
}

#[test]
fn property_drawers_end_before_body_properties_despite_literal_block_markers() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n#+BEGIN_SRC org\n:END:\n#+END_SRC\n:ASSIGNEE: opaque\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.item(item_id).unwrap().assignee, None);

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("direct-agent".to_string()),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(":END:", ":ASSIGNEE: direct-agent\n:END:", 1)
    );
    assert!(edited.source.ends_with("#+END_SRC\n:ASSIGNEE: opaque\n"));
}

#[test]
fn property_edits_preserve_lf_drawer_terminator_trailing_spaces() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:   \nBody.\n";
    let document = parse_document(source, &options()).unwrap();

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("agent".to_string()),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(":END:   \n", ":ASSIGNEE: agent\n:END:   \n", 1)
    );
}

#[test]
fn property_edits_preserve_crlf_drawer_terminator_trailing_spaces() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:   \r\nBody.\r\n";
    let document = parse_document(source, &options()).unwrap();

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("agent".to_string()),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(":END:   \r\n", ":ASSIGNEE: agent\r\n:END:   \r\n", 1,)
    );
}

#[test]
fn property_edits_insert_into_lf_drawers_with_spaced_openers() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\n:PROPERTIES:\x20\x20\x20\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source(), source);
    assert_eq!(document.item(item_id).unwrap().assignee, None);

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("agent".to_string()),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(":END:\n", ":ASSIGNEE: agent\n:END:\n", 1)
    );
}

#[test]
fn property_edits_replace_inside_crlf_drawers_with_spaced_openers() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Assigned\r\n:PROPERTIES:   \r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:ASSIGNEE: old-agent\r\n:END:\r\nBody.\r\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source(), source);
    assert_eq!(
        document.item(item_id).unwrap().assignee.as_deref(),
        Some("old-agent")
    );

    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("new-agent".to_string()),
        })
        .unwrap();

    assert_eq!(edited.source, source.replacen("old-agent", "new-agent", 1));
}

#[test]
fn dependency_edits_validate_and_render_a_sorted_uuid_set() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Dependencies\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:DEPENDS_ON: 44444444-4444-4444-8444-444444444444\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::DependsOn,
            value: Some(
                "33333333-3333-4333-8333-333333333333   22222222-2222-4222-8222-222222222222"
                    .to_string(),
            ),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen(
            "44444444-4444-4444-8444-444444444444",
            "22222222-2222-4222-8222-222222222222 33333333-3333-4333-8333-333333333333",
            1,
        )
    );
    let reparsed = parse_document(&edited.source, &options()).unwrap();
    assert_eq!(
        reparsed.item(item_id).unwrap().depends_on.clone(),
        BTreeSet::from([
            id("22222222-2222-4222-8222-222222222222"),
            id("33333333-3333-4333-8333-333333333333"),
        ])
    );
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));

    for invalid in [
        "not-a-uuid",
        "22222222-2222-4222-8222-222222222222 22222222-2222-4222-8222-222222222222",
        "11111111-1111-4111-8111-111111111111",
    ] {
        assert!(document
            .apply(SemanticEdit::SetProperty {
                item_id,
                key: PropertyKey::DependsOn,
                value: Some(invalid.to_string()),
            })
            .is_err());
    }
}

#[test]
fn requires_review_accepts_only_exact_boolean_values() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Review\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:REQUIRES_REVIEW: false\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::RequiresReview,
            value: Some("true".to_string()),
        })
        .unwrap();
    assert_eq!(
        edited.source,
        source.replacen(":REQUIRES_REVIEW: false", ":REQUIRES_REVIEW: true", 1)
    );
    assert!(
        parse_document(&edited.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .requires_review
    );
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));
    for invalid in ["TRUE", "yes", ""] {
        assert!(document
            .apply(SemanticEdit::SetProperty {
                item_id,
                key: PropertyKey::RequiresReview,
                value: Some(invalid.to_string()),
            })
            .is_err());
    }
}

#[test]
fn accumulated_assignment_is_opaque_and_unsafe_to_edit() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Ambiguous\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:ASSIGNEE: first\n:ASSIGNEE+: second\n:CUSTOM: keep\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    assert_eq!(document.source(), source);

    assert_eq!(
        document.apply(SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some("replacement".to_string()),
        }),
        Err(OrgError::UnsafeEdit(item_id))
    );
}

#[test]
fn malformed_property_drawers_are_unsafe_to_extend() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    for source in [
        "* READY Unclosed\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:CUSTOM: keep\n",
        "* READY Nested\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:PROPERTIES:\n:CUSTOM: keep\n:END:\n",
    ] {
        let document = parse_document(source, &options()).unwrap();
        assert_eq!(
            document.apply(SemanticEdit::SetProperty {
                item_id,
                key: PropertyKey::Assignee,
                value: Some("agent".to_string()),
            }),
            Err(OrgError::UnsafeEdit(item_id))
        );
    }
}

#[test]
fn scheduled_edits_replace_insert_and_remove_only_the_planning_line() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Scheduled\nSCHEDULED: <2026-08-01 Sat 09:30>\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:CUSTOM: keep\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();
    let replaced = document
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: Some("<2026-08-02 Sun 10:45>".to_string()),
        })
        .unwrap();
    assert_eq!(
        replaced.source,
        source.replacen(
            "SCHEDULED: <2026-08-01 Sat 09:30>\n",
            "SCHEDULED: <2026-08-02 Sun 10:45>\n",
            1,
        )
    );
    assert_eq!(replaced.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&replaced.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .scheduled
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-02 Sun 10:45>")
    );

    let absent = source.replace("SCHEDULED: <2026-08-01 Sat 09:30>\n", "");
    let document = parse_document(&absent, &options()).unwrap();
    let inserted = document
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: Some("<2026-08-03 Mon>".to_string()),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        absent.replacen(
            "* READY Scheduled\n",
            "* READY Scheduled\nSCHEDULED: <2026-08-03 Mon>\n",
            1,
        )
    );
    assert_eq!(inserted.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&inserted.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .scheduled
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-03 Mon>")
    );

    let document = parse_document(source, &options()).unwrap();
    let removed = document
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: None,
        })
        .unwrap();
    assert_eq!(removed.source, absent);
    assert_eq!(removed.changed_items, BTreeSet::from([item_id]));
    assert!(parse_document(&removed.source, &options())
        .unwrap()
        .item(item_id)
        .unwrap()
        .scheduled
        .is_none());
    assert!(document
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: Some("[2026-08-03 Mon]".to_string()),
        })
        .is_err());

    let crlf = source.replace('\n', "\r\n");
    let document = parse_document(&crlf, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: Some("<2026-08-04 Tue>".to_string()),
        })
        .unwrap();
    assert_eq!(
        edited.source,
        crlf.replacen(
            "SCHEDULED: <2026-08-01 Sat 09:30>\r\n",
            "SCHEDULED: <2026-08-04 Tue>\r\n",
            1,
        )
    );
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&edited.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .scheduled
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-04 Tue>")
    );

    let formatted_crlf = crlf.replacen(
        "SCHEDULED: <2026-08-01 Sat 09:30>\r\n",
        "  SCHEDULED: <2026-08-01 Sat 09:30> \t \r\n",
        1,
    );
    let edited = parse_document(&formatted_crlf, &options())
        .unwrap()
        .apply(SemanticEdit::SetScheduled {
            item_id,
            value: Some("<2026-08-05 Wed 11:15>".to_string()),
        })
        .unwrap();
    assert_eq!(
        edited.source,
        formatted_crlf.replacen("<2026-08-01 Sat 09:30>", "<2026-08-05 Wed 11:15>", 1,)
    );
}

#[test]
fn deadline_edits_replace_insert_and_clear_only_the_planning_line() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Deadline\r\nSCHEDULED: <2026-08-01 Sat 09:30>\r\nDEADLINE: <2026-08-02 Sun>\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:CUSTOM: keep\r\n:END:\r\nBody.\r\n";
    let document = parse_document(source, &options()).unwrap();

    let replaced = document
        .apply(SemanticEdit::SetDeadline {
            item_id,
            value: Some("<2026-08-03 Mon 10:45>".to_string()),
        })
        .unwrap();
    assert_eq!(
        replaced.source,
        source.replacen(
            "DEADLINE: <2026-08-02 Sun>\r\n",
            "DEADLINE: <2026-08-03 Mon 10:45>\r\n",
            1,
        )
    );
    assert_eq!(replaced.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&replaced.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .deadline
            .as_ref()
            .map(|timestamp| timestamp.raw.as_str()),
        Some("<2026-08-03 Mon 10:45>")
    );

    let without_deadline = source.replace("DEADLINE: <2026-08-02 Sun>\r\n", "");
    let inserted = parse_document(&without_deadline, &options())
        .unwrap()
        .apply(SemanticEdit::SetDeadline {
            item_id,
            value: Some("<2026-08-04 Tue>".to_string()),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        without_deadline.replacen(
            "SCHEDULED: <2026-08-01 Sat 09:30>\r\n",
            "SCHEDULED: <2026-08-01 Sat 09:30>\r\nDEADLINE: <2026-08-04 Tue>\r\n",
            1,
        )
    );

    let cleared = document
        .apply(SemanticEdit::SetDeadline {
            item_id,
            value: None,
        })
        .unwrap();
    assert_eq!(cleared.source, without_deadline);
    assert!(parse_document(&cleared.source, &options())
        .unwrap()
        .item(item_id)
        .unwrap()
        .deadline
        .is_none());

    let formatted_crlf = source.replacen(
        "DEADLINE: <2026-08-02 Sun>\r\n",
        " \tDEADLINE: <2026-08-02 Sun>   \r\n",
        1,
    );
    let edited = parse_document(&formatted_crlf, &options())
        .unwrap()
        .apply(SemanticEdit::SetDeadline {
            item_id,
            value: Some("<2026-08-06 Thu 12:30>".to_string()),
        })
        .unwrap();
    assert_eq!(
        edited.source,
        formatted_crlf.replacen("<2026-08-02 Sun>", "<2026-08-06 Thu 12:30>", 1,)
    );
}

#[test]
fn title_and_priority_edits_touch_only_their_heading_tokens() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY [#A] Old title :keep:\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\nBody bytes stay.\r\n";
    let document = parse_document(source, &options()).unwrap();

    let titled = document
        .apply(SemanticEdit::SetTitle {
            item_id,
            title: "New title".to_string(),
        })
        .unwrap();
    assert_eq!(titled.source, source.replacen("Old title", "New title", 1));
    let titled_item = parse_document(&titled.source, &options())
        .unwrap()
        .item(item_id)
        .unwrap()
        .clone();
    assert_eq!(titled_item.title, "New title");
    assert_eq!(titled_item.priority, Some('A'));
    assert_eq!(titled_item.tags, BTreeSet::from(["keep".to_string()]));

    let reprioritized = document
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: Some('B'),
        })
        .unwrap();
    assert_eq!(reprioritized.source, source.replacen("[#A]", "[#B]", 1));

    let cleared = document
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: None,
        })
        .unwrap();
    assert_eq!(cleared.source, source.replacen(" [#A]", "", 1));

    let without_priority = source.replace(" [#A]", "");
    let inserted = parse_document(&without_priority, &options())
        .unwrap()
        .apply(SemanticEdit::SetPriority {
            item_id,
            priority: Some('C'),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        without_priority.replacen("* READY", "* READY [#C]", 1)
    );
    assert_eq!(
        parse_document(&inserted.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .priority,
        Some('C')
    );
}

#[test]
fn title_insertion_preserves_empty_heading_token_boundaries() {
    let cases = [
        ("* READY\n", "* READY New title\n"),
        ("* [#A]\r\n", "* [#A] New title\r\n"),
        ("* READY [#B]\n", "* READY [#B] New title\n"),
        ("* READY :keep:\r\n", "* READY New title :keep:\r\n"),
    ];
    for (heading, expected_heading) in cases {
        let item_id = id("11111111-1111-4111-8111-111111111111");
        let line_ending = if heading.ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let source = format!(
            "{heading}:PROPERTIES:{line_ending}:ID: 11111111-1111-4111-8111-111111111111{line_ending}:AGENT_NOTE_TYPE: task{line_ending}:END:{line_ending}"
        );
        let document = parse_document(&source, &options()).unwrap();
        assert_eq!(document.item(item_id).unwrap().title, "");

        let edited = document
            .apply(SemanticEdit::SetTitle {
                item_id,
                title: "New title".to_string(),
            })
            .unwrap();
        assert_eq!(
            edited.source,
            source.replacen(heading, expected_heading, 1),
            "failed heading {heading:?}"
        );
        let item = parse_document(&edited.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .clone();
        assert_eq!(item.title, "New title");
        assert_eq!(
            item.priority,
            document.item(item_id).unwrap().priority,
            "priority changed for {heading:?}"
        );
        assert_eq!(
            item.tags,
            document.item(item_id).unwrap().tags,
            "tags changed for {heading:?}"
        );
    }
}

#[test]
fn heading_edits_reject_values_that_reparse_as_other_heading_syntax() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Safe\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();

    assert!(matches!(
        document.apply(SemanticEdit::SetTitle {
            item_id,
            title: "Ship :secret:".to_string(),
        }),
        Err(OrgError::UnsafeEdit(id)) if id == item_id
    ));
    assert!(matches!(
        document.apply(SemanticEdit::SetTitle {
            item_id,
            title: "\nInjected".to_string(),
        }),
        Err(OrgError::Parse { .. })
    ));
    assert!(matches!(
        document.apply(SemanticEdit::SetPriority {
            item_id,
            priority: Some('1'),
        }),
        Err(OrgError::Parse { .. })
    ));
}

#[test]
fn typed_agent_note_link_edits_preserve_opaque_and_unrelated_bytes() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let note_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let source = "* READY Links\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n#+BEGIN_SRC org\r\n[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][opaque]]\r\n#+END_SRC\r\nBody stays.\r\n";
    let document = parse_document(source, &options()).unwrap();
    let link = NoteLink {
        purpose: "design".to_string(),
        note_id,
        description: "Design note".to_string(),
    };

    let added = document
        .apply(SemanticEdit::AddNoteLink {
            item_id,
            link: link.clone(),
        })
        .unwrap();
    let rendered = "[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Design note]]\r\n";
    assert_eq!(added.source, format!("{source}{rendered}"));
    assert_eq!(added.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&added.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .note_links,
        vec![link]
    );

    let removed = parse_document(&added.source, &options())
        .unwrap()
        .apply(SemanticEdit::RemoveNoteLink {
            item_id,
            purpose: "design".to_string(),
            note_id,
        })
        .unwrap();
    assert_eq!(removed.source, source);
    assert!(removed.source.contains("[opaque]"));
    assert!(parse_document(&removed.source, &options())
        .unwrap()
        .item(item_id)
        .unwrap()
        .note_links
        .is_empty());
}

#[test]
fn added_agent_note_link_uses_the_target_sections_local_line_ending() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let note_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let source = "Mixed preamble.\r\n* READY Local LF\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\nBody stays.\n";
    let edited = parse_document(source, &options())
        .unwrap()
        .apply(SemanticEdit::AddNoteLink {
            item_id,
            link: NoteLink {
                purpose: "design".to_string(),
                note_id,
                description: "Design note".to_string(),
            },
        })
        .unwrap();

    assert_eq!(
        edited.source,
        format!(
            "{source}[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Design note]]\n"
        )
    );
}

#[test]
fn typed_agent_note_link_edits_reject_invalid_or_ambiguous_targets() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let note_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let source = "* READY Links\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][one]] and [[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][two]]\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(
        document.apply(SemanticEdit::RemoveNoteLink {
            item_id,
            purpose: "design".to_string(),
            note_id,
        }),
        Err(OrgError::UnsafeEdit(item_id))
    );
    for link in [
        NoteLink {
            purpose: "bad:purpose".to_string(),
            note_id,
            description: "safe".to_string(),
        },
        NoteLink {
            purpose: "design".to_string(),
            note_id,
            description: "unsafe\nline".to_string(),
        },
        NoteLink {
            purpose: "design".to_string(),
            note_id,
            description: "unbalanced ] bracket".to_string(),
        },
    ] {
        assert!(matches!(
            document.apply(SemanticEdit::AddNoteLink { item_id, link }),
            Err(OrgError::Parse { .. })
        ));
    }
}

#[test]
fn tag_edits_touch_only_the_sorted_heading_suffix() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY [#A] Keep title :zeta:alpha:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();
    let edited = document
        .apply(SemanticEdit::SetTags {
            item_id,
            tags: BTreeSet::from(["gamma".to_string(), "beta".to_string()]),
        })
        .unwrap();
    assert_eq!(
        edited.source,
        source.replacen(":zeta:alpha:", ":beta:gamma:", 1)
    );
    assert_eq!(edited.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&edited.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .tags
            .clone(),
        BTreeSet::from(["beta".to_string(), "gamma".to_string()])
    );

    let absent = source.replace(" :zeta:alpha:", "");
    let document = parse_document(&absent, &options()).unwrap();
    let inserted = document
        .apply(SemanticEdit::SetTags {
            item_id,
            tags: BTreeSet::from(["solo".to_string()]),
        })
        .unwrap();
    assert_eq!(
        inserted.source,
        absent.replacen("Keep title\n", "Keep title :solo:\n", 1)
    );
    assert_eq!(inserted.changed_items, BTreeSet::from([item_id]));
    assert_eq!(
        parse_document(&inserted.source, &options())
            .unwrap()
            .item(item_id)
            .unwrap()
            .tags,
        BTreeSet::from(["solo".to_string()])
    );

    let document = parse_document(source, &options()).unwrap();
    let removed = document
        .apply(SemanticEdit::SetTags {
            item_id,
            tags: BTreeSet::new(),
        })
        .unwrap();
    assert_eq!(removed.source, absent);
    assert_eq!(removed.changed_items, BTreeSet::from([item_id]));
    let item = parse_document(&removed.source, &options())
        .unwrap()
        .item(item_id)
        .unwrap()
        .clone();
    assert!(item.tags.is_empty());
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.priority, Some('A'));
    assert_eq!(item.title, "Keep title");
}

#[test]
fn tag_removal_rejects_exposing_a_title_suffix_as_semantic_tags() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* READY Ship :internal: :actual:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let item = document.item(item_id).unwrap();
    assert_eq!(item.title, "Ship :internal:");
    assert_eq!(item.tags, BTreeSet::from(["actual".to_string()]));

    assert_eq!(
        document.apply(SemanticEdit::SetTags {
            item_id,
            tags: BTreeSet::new(),
        }),
        Err(OrgError::UnsafeEdit(item_id))
    );
    assert_eq!(document.source(), source);
}

#[test]
fn move_item_transfers_the_exact_subtree_as_the_target_parents_last_child() {
    let moved_id = id("11111111-1111-4111-8111-111111111111");
    let child_id = id("22222222-2222-4222-8222-222222222222");
    let target_parent_id = id("44444444-4444-4444-8444-444444444444");
    let source = "SOURCE PREAMBLE\n* READY Move me\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\nMove body.\n#+BEGIN_SRC org\n*** READY Not a real heading\n#+END_SRC\n:CUSTOM_DRAWER:\n*** READY Not a drawer heading\n:END:\n** DONE Real child\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\nChild body.\n* READY Stay\n:PROPERTIES:\n:ID: 33333333-3333-4333-8333-333333333333\n:AGENT_NOTE_TYPE: task\n:END:\nStay body.\n";
    let target_sibling = "* READY Target sibling\n:PROPERTIES:\n:ID: 66666666-6666-4666-8666-666666666666\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let target = format!(
        "TARGET PREAMBLE\n* READY Target parent\n:PROPERTIES:\n:ID: {target_parent_id}\n:AGENT_NOTE_TYPE: project\n:END:\nTarget body.\n** DONE Existing child\n:PROPERTIES:\n:ID: 55555555-5555-4555-8555-555555555555\n:AGENT_NOTE_TYPE: task\n:END:\nExisting body.\n{target_sibling}"
    );
    let source_document = parse_document(source, &options()).unwrap();
    let target_document = parse_document(&target, &options()).unwrap();
    let moved = move_item(
        &source_document,
        &target_document,
        moved_id,
        Some(target_parent_id),
    )
    .unwrap();

    let expected_source = source.replacen(
        "* READY Move me\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\nMove body.\n#+BEGIN_SRC org\n*** READY Not a real heading\n#+END_SRC\n:CUSTOM_DRAWER:\n*** READY Not a drawer heading\n:END:\n** DONE Real child\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\nChild body.\n",
        "",
        1,
    );
    let expected_subtree = "** READY Move me\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\nMove body.\n#+BEGIN_SRC org\n*** READY Not a real heading\n#+END_SRC\n:CUSTOM_DRAWER:\n*** READY Not a drawer heading\n:END:\n*** DONE Real child\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\nChild body.\n";
    assert_eq!(moved.source, expected_source);
    assert_eq!(
        moved.target,
        target.replacen(
            target_sibling,
            &format!("{expected_subtree}{target_sibling}"),
            1,
        )
    );
    assert!(moved.target.contains("*** READY Not a real heading\n"));
    assert!(moved.target.contains("*** READY Not a drawer heading\n"));
    let reparsed = parse_document(moved.target, &options()).unwrap();
    let root = reparsed.item(moved_id).unwrap();
    let child = reparsed.item(child_id).unwrap();
    assert_eq!((root.level, root.parent_id), (2, Some(target_parent_id)));
    assert_eq!((child.level, child.parent_id), (3, Some(moved_id)));
    assert!(reparsed.source().contains("Move body.\n"));
    assert!(reparsed.source().contains("Child body.\n"));
    let reparsed_source = parse_document(&moved.source, &options()).unwrap();
    assert!(reparsed_source.item(moved_id).is_none());
    assert!(reparsed_source.item(child_id).is_none());
    assert!(reparsed_source
        .item(id("33333333-3333-4333-8333-333333333333"))
        .is_some());

    let moved_to_eof = move_item(
        &source_document,
        &parse_document("", &options()).unwrap(),
        moved_id,
        None,
    )
    .unwrap();
    assert_eq!(moved_to_eof.source, expected_source);
    assert_eq!(
        moved_to_eof.target,
        expected_subtree
            .replacen("** READY Move me", "* READY Move me", 1)
            .replacen("*** DONE Real child", "** DONE Real child", 1)
    );
    let reparsed_source = parse_document(&moved_to_eof.source, &options()).unwrap();
    assert!(reparsed_source.item(moved_id).is_none());
    assert!(reparsed_source.item(child_id).is_none());
    let reparsed_target = parse_document(&moved_to_eof.target, &options()).unwrap();
    assert_eq!(
        (
            reparsed_target.item(moved_id).unwrap().level,
            reparsed_target.item(moved_id).unwrap().parent_id
        ),
        (1, None)
    );
    assert_eq!(
        (
            reparsed_target.item(child_id).unwrap().level,
            reparsed_target.item(child_id).unwrap().parent_id,
        ),
        (2, Some(moved_id))
    );
}

#[test]
fn move_item_rejects_collisions_with_ordinary_heading_ids() {
    let moved_id = id("11111111-1111-4111-8111-111111111111");
    let descendant_id = id("22222222-2222-4222-8222-222222222222");
    let ordinary_id = id("44444444-4444-4444-8444-444444444444");
    let projected_source = "* READY Root\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\n** READY Child\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let source_document = parse_document(projected_source, &options()).unwrap();

    for duplicate_id in [moved_id, descendant_id] {
        let target = format!("* Ordinary target\n:PROPERTIES:\n:ID: {duplicate_id}\n:END:\n");
        assert_eq!(
            move_item(
                &source_document,
                &parse_document(target, &options()).unwrap(),
                moved_id,
                None,
            ),
            Err(OrgError::DuplicateId(duplicate_id))
        );
    }

    let source_with_ordinary = format!(
        "* READY Root\n:PROPERTIES:\n:ID: {moved_id}\n:AGENT_NOTE_TYPE: project\n:END:\n** Ordinary child\n:PROPERTIES:\n:ID: {ordinary_id}\n:END:\n"
    );
    let source_document = parse_document(source_with_ordinary, &options()).unwrap();
    for target in [
        format!("* Ordinary target\n:PROPERTIES:\n:ID: {ordinary_id}\n:END:\n"),
        format!(
            "* READY Projected target\n:PROPERTIES:\n:ID: {ordinary_id}\n:AGENT_NOTE_TYPE: task\n:END:\n"
        ),
    ] {
        assert_eq!(
            move_item(
                &source_document,
                &parse_document(target, &options()).unwrap(),
                moved_id,
                None,
            ),
            Err(OrgError::DuplicateId(ordinary_id))
        );
    }

    let malformed_target = parse_document(
        "* Ordinary target\n:PROPERTIES:\n:ID: not-a-uuid\n:END:\n",
        &options(),
    )
    .unwrap();
    assert!(move_item(&source_document, &malformed_target, moved_id, None).is_ok());
}

#[test]
fn reparent_item_moves_the_subtree_to_the_target_parents_final_child() {
    let moved_id = id("22222222-2222-4222-8222-222222222222");
    let descendant_id = id("33333333-3333-4333-8333-333333333333");
    let target_parent_id = id("55555555-5555-4555-8555-555555555555");
    let trailing = "* DONE Trailing\n:PROPERTIES:\n:ID: 77777777-7777-4777-8777-777777777777\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let moved_subtree = "** READY Move\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: epic\n:END:\nMove body.\n*** DONE Descendant\n:PROPERTIES:\n:ID: 33333333-3333-4333-8333-333333333333\n:AGENT_NOTE_TYPE: task\n:END:\nDescendant body.\n";
    let source = format!(
        "* READY Origin\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\nOrigin body.\n{moved_subtree}* READY Destination\n:PROPERTIES:\n:ID: 44444444-4444-4444-8444-444444444444\n:AGENT_NOTE_TYPE: project\n:END:\nDestination body.\n** READY Target parent\n:PROPERTIES:\n:ID: {target_parent_id}\n:AGENT_NOTE_TYPE: epic\n:END:\nTarget body.\n*** DONE Existing\n:PROPERTIES:\n:ID: 66666666-6666-4666-8666-666666666666\n:AGENT_NOTE_TYPE: task\n:END:\nExisting body.\n{trailing}"
    );
    let document = parse_document(&source, &options()).unwrap();
    let edited = reparent_item(&document, moved_id, Some(target_parent_id)).unwrap();
    let adjusted = moved_subtree
        .replacen("** READY Move", "*** READY Move", 1)
        .replacen("*** DONE Descendant", "**** DONE Descendant", 1);
    let without_moved = source.replacen(moved_subtree, "", 1);
    assert_eq!(
        edited.source,
        without_moved.replacen(trailing, &format!("{adjusted}{trailing}"), 1)
    );
    assert_eq!(
        edited.changed_items,
        BTreeSet::from([moved_id, descendant_id])
    );
    let reparsed = parse_document(edited.source, &options()).unwrap();
    assert_eq!(
        (
            reparsed.item(moved_id).unwrap().level,
            reparsed.item(moved_id).unwrap().parent_id,
        ),
        (3, Some(target_parent_id))
    );
    assert_eq!(
        (
            reparsed.item(descendant_id).unwrap().level,
            reparsed.item(descendant_id).unwrap().parent_id,
        ),
        (4, Some(moved_id))
    );
    assert!(reparsed.source().contains("Move body.\n"));
    assert!(reparsed.source().contains("Descendant body.\n"));

    assert_eq!(
        reparent_item(&document, moved_id, Some(descendant_id)),
        Err(OrgError::UnsafeEdit(moved_id))
    );
}

#[test]
fn public_edits_return_errors_for_unknown_ids_and_invalid_requests() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let unknown = id("99999999-9999-4999-8999-999999999999");
    let source = "* READY Valid\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(
        document.apply(SemanticEdit::SetState {
            item_id: unknown,
            state: "READY".to_string(),
        }),
        Err(OrgError::ItemNotFound(unknown))
    );
    assert_eq!(
        reparent_item(&document, unknown, None),
        Err(OrgError::ItemNotFound(unknown))
    );
    assert_eq!(
        move_item(&document, &document, unknown, None),
        Err(OrgError::ItemNotFound(unknown))
    );
    assert_eq!(
        move_item(
            &document,
            &parse_document("", &options()).unwrap(),
            item_id,
            Some(unknown)
        ),
        Err(OrgError::ItemNotFound(unknown))
    );
    assert_eq!(
        reparent_item(&document, item_id, Some(unknown)),
        Err(OrgError::ItemNotFound(unknown))
    );

    for edit in [
        SemanticEdit::SetState {
            item_id,
            state: "UNKNOWN".to_string(),
        },
        SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::Assignee,
            value: Some(" \t".to_string()),
        },
        SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::DependsOn,
            value: Some("invalid".to_string()),
        },
        SemanticEdit::SetProperty {
            item_id,
            key: PropertyKey::RequiresReview,
            value: Some("yes".to_string()),
        },
        SemanticEdit::SetScheduled {
            item_id,
            value: Some("<2026-99-99 Nope>".to_string()),
        },
    ] {
        assert!(matches!(document.apply(edit), Err(OrgError::Parse { .. })));
    }

    for item in [
        NewWorkItem {
            id: unknown,
            item_type: WorkItemType::Task,
            title: "".to_string(),
            state: "READY".to_string(),
            priority: None,
            tags: BTreeSet::new(),
        },
        NewWorkItem {
            id: unknown,
            item_type: WorkItemType::Task,
            title: "Valid".to_string(),
            state: "UNKNOWN".to_string(),
            priority: None,
            tags: BTreeSet::new(),
        },
        NewWorkItem {
            id: unknown,
            item_type: WorkItemType::Task,
            title: "Valid".to_string(),
            state: "READY".to_string(),
            priority: Some('1'),
            tags: BTreeSet::new(),
        },
        NewWorkItem {
            id: unknown,
            item_type: WorkItemType::Task,
            title: "Valid".to_string(),
            state: "READY".to_string(),
            priority: None,
            tags: BTreeSet::from(["bad tag".to_string()]),
        },
    ] {
        assert!(matches!(
            document.apply(SemanticEdit::AppendItem {
                parent_id: None,
                item,
            }),
            Err(OrgError::Parse { .. })
        ));
    }
}
