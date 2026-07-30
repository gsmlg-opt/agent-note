use note_org::{
    move_item, parse_document, reparent_item, NewWorkItem, OrgError, ParseOptions, PropertyKey,
    SemanticEdit, WorkItemId, WorkItemType,
};
use std::{collections::BTreeSet, str::FromStr};

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
fn state_insertion_rejects_reinterpreting_a_priority_like_title() {
    let item_id = id("11111111-1111-4111-8111-111111111111");
    let source = "* [#A] Ship\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
    let document = parse_document(source, &options()).unwrap();
    let item = document.item(item_id).unwrap();
    assert_eq!(item.state, None);
    assert_eq!(item.priority, None);
    assert_eq!(item.title, "[#A] Ship");

    assert_eq!(
        document.apply(SemanticEdit::SetState {
            item_id,
            state: "READY".to_string(),
        }),
        Err(OrgError::UnsafeEdit(item_id))
    );
    assert_eq!(document.source(), source);
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
