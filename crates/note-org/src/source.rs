use crate::{
    EditedDocument, IndexedItem, IndexedNoteLink, MovedDocuments, NewWorkItem, NoteLink, OrgError,
    OrgTimestamp, ParseOptions, PropertyKey, SemanticEdit, Span, WorkItem, WorkItemId,
    WorkItemType,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};
use uuid::Uuid;

const UNSAFE_SPAN: Span = Span {
    start: usize::MAX,
    end: usize::MAX,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannedHeading {
    level: usize,
    heading: Span,
    subtree: Span,
    section_end: usize,
}

#[derive(Debug)]
struct PropertyOccurrence<'a> {
    key: String,
    accumulated: bool,
    value: &'a str,
    value_span: Span,
    line_start: usize,
}

enum SemanticProperty<'properties, 'source> {
    Missing,
    Single(&'properties PropertyOccurrence<'source>),
    Ambiguous { line_start: usize },
}

enum PropertyDrawer {
    Absent,
    Valid { end: usize },
    Malformed,
}

struct DirectPropertyDrawer {
    contents: Span,
    terminator: Option<usize>,
    nested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDocument {
    source: String,
    items: Vec<WorkItem>,
    index: BTreeMap<WorkItemId, IndexedItem>,
    all_ids: BTreeSet<WorkItemId>,
    #[allow(dead_code)]
    options: ParseOptions,
}

impl OrgDocument {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn items(&self) -> &[WorkItem] {
        &self.items
    }

    pub fn item(&self, id: WorkItemId) -> Option<&WorkItem> {
        self.index.get(&id).map(|entry| &entry.item)
    }

    pub fn apply(&self, edit: SemanticEdit) -> Result<EditedDocument, OrgError> {
        match edit {
            SemanticEdit::SetState { item_id, state } => self.set_state(item_id, &state),
            SemanticEdit::SetProperty {
                item_id,
                key,
                value,
            } => self.set_property(item_id, key, value.as_deref()),
            SemanticEdit::SetScheduled { item_id, value } => {
                self.set_scheduled(item_id, value.as_deref())
            }
            SemanticEdit::SetDeadline { item_id, value } => {
                self.set_deadline(item_id, value.as_deref())
            }
            SemanticEdit::SetTitle { item_id, title } => self.set_title(item_id, &title),
            SemanticEdit::SetPriority { item_id, priority } => self.set_priority(item_id, priority),
            SemanticEdit::SetTags { item_id, tags } => self.set_tags(item_id, &tags),
            SemanticEdit::AddNoteLink { item_id, link } => self.add_note_link(item_id, link),
            SemanticEdit::RemoveNoteLink {
                item_id,
                purpose,
                note_id,
            } => self.remove_note_link(item_id, &purpose, note_id),
            SemanticEdit::AppendItem { parent_id, item } => self.append_item(parent_id, item),
        }
    }

    fn set_state(&self, item_id: WorkItemId, state: &str) -> Result<EditedDocument, OrgError> {
        if !self.options.states.contains(state) {
            return Err(parse_error(
                &self.source,
                0,
                format!("state {state} is not configured"),
            ));
        }
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        let (span, replacement) = match indexed.state {
            Some(span) => {
                ensure_contained_span(&self.source, span, indexed.heading, item_id)?;
                (span, state.to_string())
            }
            None => {
                let marker_end = indexed
                    .item
                    .level
                    .checked_add(1)
                    .ok_or(OrgError::UnsafeEdit(item_id))?;
                let offset =
                    checked_offset(&self.source, indexed.heading.start, marker_end, item_id)?;
                if offset > indexed.heading.end {
                    return Err(OrgError::UnsafeEdit(item_id));
                }
                (
                    Span {
                        start: offset,
                        end: offset,
                    },
                    format!("{state} "),
                )
            }
        };
        let source = replace_span(&self.source, span, &replacement);
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        expected.state = Some(state.to_string());
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn set_property(
        &self,
        item_id: WorkItemId,
        key: PropertyKey,
        value: Option<&str>,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        let (key_name, normalized) = match key {
            PropertyKey::Assignee => {
                if value
                    .is_some_and(|value| value.trim().is_empty() || value.contains(['\r', '\n']))
                {
                    return Err(parse_error(
                        &self.source,
                        indexed.heading.start,
                        "ASSIGNEE must be one nonempty line",
                    ));
                }
                ("ASSIGNEE", value.map(str::to_string))
            }
            PropertyKey::DependsOn => {
                let normalized = match value {
                    Some(value) => {
                        let mut dependencies = BTreeSet::new();
                        for raw_id in value.split_whitespace() {
                            let dependency = WorkItemId::from_str(raw_id).map_err(|_| {
                                parse_error(
                                    &self.source,
                                    indexed.heading.start,
                                    "DEPENDS_ON contains an invalid dependency ID",
                                )
                            })?;
                            if dependency == item_id {
                                return Err(parse_error(
                                    &self.source,
                                    indexed.heading.start,
                                    "work item cannot depend on itself",
                                ));
                            }
                            if !dependencies.insert(dependency) {
                                return Err(parse_error(
                                    &self.source,
                                    indexed.heading.start,
                                    "DEPENDS_ON contains a duplicate dependency ID",
                                ));
                            }
                        }
                        Some(
                            dependencies
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    }
                    None => None,
                };
                ("DEPENDS_ON", normalized)
            }
            PropertyKey::RequiresReview => {
                if value.is_some_and(|value| !matches!(value, "true" | "false")) {
                    return Err(parse_error(
                        &self.source,
                        indexed.heading.start,
                        "REQUIRES_REVIEW must be true or false",
                    ));
                }
                ("REQUIRES_REVIEW", value.map(str::to_string))
            }
        };
        let value = normalized.as_deref();
        let source = match (indexed.properties.get(key_name).copied(), value) {
            (Some(span), Some(value)) => {
                ensure_contained_span(&self.source, span, indexed.subtree, item_id)?;
                replace_span(&self.source, span, value)
            }
            (Some(span), None) => {
                ensure_contained_span(&self.source, span, indexed.subtree, item_id)?;
                let line = whole_line_span(&self.source, span, item_id)?;
                ensure_contained_span(&self.source, line, indexed.subtree, item_id)?;
                replace_span(&self.source, line, "")
            }
            (None, Some(value)) => {
                let line_ending = preferred_line_ending(&self.source);
                match property_drawer(&self.source, indexed, item_id)? {
                    PropertyDrawer::Valid { end: offset } => {
                        let span = Span {
                            start: offset,
                            end: offset,
                        };
                        ensure_editable_span(&self.source, span, item_id)?;
                        replace_span(
                            &self.source,
                            span,
                            &format!(":{key_name}: {value}{line_ending}"),
                        )
                    }
                    PropertyDrawer::Absent => {
                        let offset = planning_insertion_offset(&self.source, indexed, item_id)?;
                        let span = Span {
                            start: offset,
                            end: offset,
                        };
                        ensure_editable_span(&self.source, span, item_id)?;
                        replace_span(
                            &self.source,
                            span,
                            &format!(
                                ":PROPERTIES:{line_ending}:{key_name}: {value}{line_ending}:END:{line_ending}"
                            ),
                        )
                    }
                    PropertyDrawer::Malformed => return Err(OrgError::UnsafeEdit(item_id)),
                }
            }
            (None, None) => self.source.clone(),
        };
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn set_scheduled(
        &self,
        item_id: WorkItemId,
        value: Option<&str>,
    ) -> Result<EditedDocument, OrgError> {
        self.set_planning(item_id, "SCHEDULED", value)
    }

    fn set_deadline(
        &self,
        item_id: WorkItemId,
        value: Option<&str>,
    ) -> Result<EditedDocument, OrgError> {
        self.set_planning(item_id, "DEADLINE", value)
    }

    fn set_planning(
        &self,
        item_id: WorkItemId,
        key: &str,
        value: Option<&str>,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        if let Some(value) = value {
            parse_planning_timestamp(value).map_err(|message| {
                parse_error(
                    &self.source,
                    indexed.heading.start,
                    format!("{key}: {message}"),
                )
            })?;
        }
        let source = match (indexed.planning.get(key).copied(), value) {
            (Some(span), Some(value)) => {
                ensure_contained_span(&self.source, span, indexed.subtree, item_id)?;
                let value_span = planning_value_span(&self.source, span, key, item_id)?;
                replace_span(&self.source, value_span, value)
            }
            (Some(span), None) => {
                ensure_contained_span(&self.source, span, indexed.subtree, item_id)?;
                replace_span(&self.source, span, "")
            }
            (None, Some(value)) => {
                let offset = if key == "SCHEDULED" {
                    indexed.heading.end
                } else {
                    planning_insertion_offset(&self.source, indexed, item_id)?
                };
                let span = Span {
                    start: offset,
                    end: offset,
                };
                ensure_editable_span(&self.source, span, item_id)?;
                let line_ending = span_line_ending(&self.source, indexed.heading, item_id)?;
                replace_span(&self.source, span, &format!("{key}: {value}{line_ending}"))
            }
            (None, None) => self.source.clone(),
        };
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        let parsed = value
            .map(parse_planning_timestamp)
            .transpose()
            .map_err(|_| OrgError::UnsafeEdit(item_id))?;
        if key == "SCHEDULED" {
            expected.scheduled = parsed;
        } else {
            expected.deadline = parsed;
        }
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn set_title(&self, item_id: WorkItemId, title: &str) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        if title.trim().is_empty() || title != title.trim() || title.contains(['\r', '\n']) {
            return Err(parse_error(
                &self.source,
                indexed.heading.start,
                "work item title must be one nonempty trimmed line",
            ));
        }
        ensure_contained_span(&self.source, indexed.title, indexed.heading, item_id)?;
        let replacement = if indexed.title.start == indexed.title.end {
            let before = self.source[..indexed.title.start].chars().next_back();
            let after = self.source[indexed.title.end..].chars().next();
            let mut replacement = String::with_capacity(title.len() + 2);
            if !matches!(before, None | Some(' ' | '\t' | '\r' | '\n')) {
                replacement.push(' ');
            }
            replacement.push_str(title);
            if !matches!(after, None | Some(' ' | '\t' | '\r' | '\n')) {
                replacement.push(' ');
            }
            replacement
        } else {
            title.to_string()
        };
        let source = replace_span(&self.source, indexed.title, &replacement);
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        expected.title = title.to_string();
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn set_priority(
        &self,
        item_id: WorkItemId,
        priority: Option<char>,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        if priority.is_some_and(|value| !value.is_ascii_uppercase()) {
            return Err(parse_error(
                &self.source,
                indexed.heading.start,
                "work item priority must be an uppercase ASCII letter",
            ));
        }
        let source = match (indexed.priority, priority) {
            (Some(span), Some(priority)) => {
                ensure_contained_span(&self.source, span, indexed.heading, item_id)?;
                replace_span(&self.source, span, &format!("[#{priority}]"))
            }
            (Some(span), None) => {
                ensure_contained_span(&self.source, span, indexed.heading, item_id)?;
                let bytes = self.source.as_bytes();
                let removal = if bytes
                    .get(span.end)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                {
                    Span {
                        start: span.start,
                        end: span.end + 1,
                    }
                } else {
                    let marker_content_start = indexed
                        .heading
                        .start
                        .checked_add(indexed.item.level)
                        .and_then(|offset| offset.checked_add(1))
                        .ok_or(OrgError::UnsafeEdit(item_id))?;
                    let start = if span.start == marker_content_start {
                        span.start
                    } else {
                        span.start
                            .checked_sub(1)
                            .filter(|offset| {
                                bytes
                                    .get(*offset)
                                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                            })
                            .unwrap_or(span.start)
                    };
                    Span {
                        start,
                        end: span.end,
                    }
                };
                ensure_contained_span(&self.source, removal, indexed.heading, item_id)?;
                replace_span(&self.source, removal, "")
            }
            (None, Some(priority)) => {
                ensure_contained_span(&self.source, indexed.title, indexed.heading, item_id)?;
                let offset = indexed.title.start;
                let preceding = offset
                    .checked_sub(1)
                    .and_then(|offset| self.source.as_bytes().get(offset))
                    .copied();
                let separator = if preceding == Some(b'\t') { '\t' } else { ' ' };
                let has_leading_separator =
                    preceding.is_some_and(|byte| matches!(byte, b' ' | b'\t'));
                let heading = self
                    .source
                    .get(indexed.heading.start..indexed.heading.end)
                    .ok_or(OrgError::UnsafeEdit(item_id))?;
                let meaningful_end = indexed.heading.start
                    + line_content(heading).trim_end_matches([' ', '\t']).len();
                let trailing_separator = if offset < meaningful_end {
                    separator.to_string()
                } else {
                    String::new()
                };
                let replacement = if has_leading_separator {
                    format!("[#{priority}]{trailing_separator}")
                } else {
                    format!("{separator}[#{priority}]{trailing_separator}")
                };
                let insertion = Span {
                    start: offset,
                    end: offset,
                };
                ensure_contained_span(&self.source, insertion, indexed.heading, item_id)?;
                replace_span(&self.source, insertion, &replacement)
            }
            (None, None) => self.source.clone(),
        };
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        expected.priority = priority;
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn add_note_link(
        &self,
        item_id: WorkItemId,
        link: NoteLink,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        validate_note_link(&self.source, indexed.heading.start, &link)?;
        if indexed.note_links.iter().any(|existing| {
            existing.link.purpose == link.purpose && existing.link.note_id == link.note_id
        }) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        let line_ending = local_section_line_ending(&self.source, indexed, item_id)?;
        let rendered = render_note_link(&link, line_ending);
        let insertion = insertion_with_boundaries(
            &self.source,
            indexed.section.end,
            &rendered,
            line_ending,
            item_id,
        )?;
        let source = replace_span(
            &self.source,
            Span {
                start: indexed.section.end,
                end: indexed.section.end,
            },
            &insertion,
        );
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        expected.note_links.push(link);
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn remove_note_link(
        &self,
        item_id: WorkItemId,
        purpose: &str,
        note_id: Uuid,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        let matches = indexed
            .note_links
            .iter()
            .filter(|entry| entry.link.purpose == purpose && entry.link.note_id == note_id)
            .collect::<Vec<_>>();
        let [matched] = matches.as_slice() else {
            return Err(OrgError::UnsafeEdit(item_id));
        };
        ensure_contained_span(&self.source, matched.span, indexed.section, item_id)?;
        let line = whole_line_span(&self.source, matched.span, item_id)?;
        let link_source = self
            .source
            .get(matched.span.start..matched.span.end)
            .ok_or(OrgError::UnsafeEdit(item_id))?;
        let line_source = self
            .source
            .get(line.start..line.end)
            .ok_or(OrgError::UnsafeEdit(item_id))?;
        let removal = if line_content(line_source).trim() == link_source {
            line
        } else {
            matched.span
        };
        ensure_contained_span(&self.source, removal, indexed.section, item_id)?;
        let source = replace_span(&self.source, removal, "");
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        let position = expected
            .note_links
            .iter()
            .position(|link| link.purpose == purpose && link.note_id == note_id)
            .ok_or(OrgError::UnsafeEdit(item_id))?;
        expected.note_links.remove(position);
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn set_tags(
        &self,
        item_id: WorkItemId,
        tags: &BTreeSet<String>,
    ) -> Result<EditedDocument, OrgError> {
        let indexed = self
            .index
            .get(&item_id)
            .ok_or(OrgError::ItemNotFound(item_id))?;
        validate_indexed_item(&self.source, indexed, item_id)?;
        if !valid_tags(tags) {
            return Err(parse_error(
                &self.source,
                indexed.heading.start,
                "work item tags are invalid",
            ));
        }
        let rendered = render_tags(tags);
        let source = match (indexed.tags, rendered.as_deref()) {
            (Some(span), Some(tags)) => {
                ensure_contained_span(&self.source, span, indexed.heading, item_id)?;
                replace_span(&self.source, span, tags)
            }
            (Some(span), None) => {
                ensure_contained_span(&self.source, span, indexed.heading, item_id)?;
                let start = span
                    .start
                    .checked_sub(1)
                    .filter(|offset| self.source.as_bytes().get(*offset) == Some(&b' '))
                    .unwrap_or(span.start);
                let removal = Span {
                    start,
                    end: span.end,
                };
                ensure_contained_span(&self.source, removal, indexed.heading, item_id)?;
                replace_span(&self.source, removal, "")
            }
            (None, Some(tags)) => {
                let heading = self
                    .source
                    .get(indexed.heading.start..indexed.heading.end)
                    .ok_or(OrgError::UnsafeEdit(item_id))?;
                let offset = checked_offset(
                    &self.source,
                    indexed.heading.start,
                    line_content(heading).len(),
                    item_id,
                )?;
                let insertion = Span {
                    start: offset,
                    end: offset,
                };
                ensure_contained_span(&self.source, insertion, indexed.heading, item_id)?;
                replace_span(&self.source, insertion, &format!(" {tags}"))
            }
            (None, None) => self.source.clone(),
        };
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item_id))?;
        let mut expected = indexed.item.clone();
        expected.tags = tags.clone();
        if reparsed.item(item_id) != Some(&expected) {
            return Err(OrgError::UnsafeEdit(item_id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item_id]),
        })
    }

    fn append_item(
        &self,
        parent_id: Option<WorkItemId>,
        item: NewWorkItem,
    ) -> Result<EditedDocument, OrgError> {
        if self.all_ids.contains(&item.id) {
            return Err(OrgError::DuplicateId(item.id));
        }
        validate_new_item(&item, &self.options, &self.source)?;
        let (level, offset) = match parent_id {
            Some(parent_id) => {
                let parent = self
                    .index
                    .get(&parent_id)
                    .ok_or(OrgError::ItemNotFound(parent_id))?;
                validate_indexed_item(&self.source, parent, parent_id)?;
                (
                    parent
                        .item
                        .level
                        .checked_add(1)
                        .ok_or(OrgError::UnsafeEdit(parent_id))?,
                    parent.subtree.end,
                )
            }
            None => (1, self.source.len()),
        };
        let span = Span {
            start: offset,
            end: offset,
        };
        ensure_editable_span(&self.source, span, item.id)?;
        let line_ending = preferred_line_ending(&self.source);
        let rendered = render_new_item(&item, level, line_ending);
        let insertion =
            insertion_with_boundaries(&self.source, offset, &rendered, line_ending, item.id)?;
        let source = replace_span(&self.source, span, &insertion);
        let reparsed =
            parse_document(&source, &self.options).map_err(|_| OrgError::UnsafeEdit(item.id))?;
        let Some(created) = reparsed.item(item.id) else {
            return Err(OrgError::UnsafeEdit(item.id));
        };
        if created.item_type != item.item_type
            || created.parent_id != parent_id
            || created.level != level
            || created.title != item.title
            || created.state.as_deref() != Some(item.state.as_str())
            || created.priority != item.priority
            || created.tags != item.tags
        {
            return Err(OrgError::UnsafeEdit(item.id));
        }
        Ok(EditedDocument {
            source,
            changed_items: BTreeSet::from([item.id]),
        })
    }
}

fn whole_line_span(source: &str, contained: Span, item_id: WorkItemId) -> Result<Span, OrgError> {
    ensure_editable_span(source, contained, item_id)?;
    let before = source
        .get(..contained.start)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let after = source
        .get(contained.end..)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let start = before
        .rfind('\n')
        .map(|offset| offset.checked_add(1).ok_or(OrgError::UnsafeEdit(item_id)))
        .transpose()?
        .unwrap_or(0);
    let end = match after.find('\n') {
        Some(offset) => contained
            .end
            .checked_add(offset)
            .and_then(|end| end.checked_add(1))
            .ok_or(OrgError::UnsafeEdit(item_id))?,
        None => source.len(),
    };
    let span = Span { start, end };
    ensure_editable_span(source, span, item_id)?;
    Ok(span)
}

fn property_drawer(
    source: &str,
    indexed: &IndexedItem,
    item_id: WorkItemId,
) -> Result<PropertyDrawer, OrgError> {
    validate_indexed_item(source, indexed, item_id)?;
    let Some(id_span) = indexed.properties.get("ID").copied() else {
        return Ok(PropertyDrawer::Absent);
    };
    ensure_contained_span(source, id_span, indexed.subtree, item_id)?;
    let scanned = scan_headings(source)
        .into_iter()
        .find(|heading| heading.heading.start == indexed.heading.start)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let section = Span {
        start: indexed.heading.end,
        end: scanned.section_end,
    };
    ensure_contained_span(source, section, indexed.subtree, item_id)?;
    let Some(drawer) = direct_property_drawer(source, section.start, section.end) else {
        return Ok(PropertyDrawer::Absent);
    };
    if drawer.nested {
        return Ok(PropertyDrawer::Malformed);
    }
    if id_span.start < drawer.contents.start || id_span.end > drawer.contents.end {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    match drawer.terminator {
        Some(end) => Ok(PropertyDrawer::Valid { end }),
        None => Ok(PropertyDrawer::Malformed),
    }
}

fn planning_insertion_offset(
    source: &str,
    indexed: &IndexedItem,
    item_id: WorkItemId,
) -> Result<usize, OrgError> {
    validate_indexed_item(source, indexed, item_id)?;
    let mut offset = indexed.heading.end;
    for span in indexed.planning.values().copied() {
        ensure_contained_span(source, span, indexed.subtree, item_id)?;
        offset = offset.max(span.end);
    }
    let insertion = Span {
        start: offset,
        end: offset,
    };
    ensure_contained_span(source, insertion, indexed.subtree, item_id)?;
    Ok(offset)
}

fn validate_new_item(
    item: &NewWorkItem,
    options: &ParseOptions,
    source: &str,
) -> Result<(), OrgError> {
    if !options.states.contains(&item.state) {
        return Err(parse_error(
            source,
            0,
            format!("state {} is not configured", item.state),
        ));
    }
    if item.title.trim().is_empty() || item.title.contains(['\r', '\n']) {
        return Err(parse_error(
            source,
            0,
            "work item title must be one nonempty line",
        ));
    }
    if item
        .priority
        .is_some_and(|priority| !priority.is_ascii_uppercase())
    {
        return Err(parse_error(
            source,
            0,
            "work item priority must be an uppercase ASCII letter",
        ));
    }
    if !valid_tags(&item.tags) {
        return Err(parse_error(source, 0, "work item tags are invalid"));
    }
    Ok(())
}

fn valid_tags(tags: &BTreeSet<String>) -> bool {
    !tags.iter().any(|tag| {
        tag.is_empty() || tag.contains(':') || tag.bytes().any(|byte| byte.is_ascii_whitespace())
    })
}

fn render_new_item(item: &NewWorkItem, level: usize, line_ending: &str) -> String {
    let priority = item
        .priority
        .map(|value| format!(" [#{value}]"))
        .unwrap_or_default();
    let tags = render_tags(&item.tags)
        .map(|value| format!(" {value}"))
        .unwrap_or_default();
    let item_type = work_item_type_name(item.item_type);
    format!(
        "{} {}{} {}{}{}:PROPERTIES:{}:ID: {}{}:AGENT_NOTE_TYPE: {}{}:END:{}",
        "*".repeat(level),
        item.state,
        priority,
        item.title,
        tags,
        line_ending,
        line_ending,
        item.id,
        line_ending,
        item_type,
        line_ending,
        line_ending,
    )
}

fn work_item_type_name(item_type: WorkItemType) -> &'static str {
    match item_type {
        WorkItemType::Project => "project",
        WorkItemType::Epic => "epic",
        WorkItemType::Issue => "issue",
        WorkItemType::Task => "task",
        WorkItemType::Subtask => "subtask",
        WorkItemType::Review => "review",
        WorkItemType::Approval => "approval",
        WorkItemType::Incident => "incident",
        WorkItemType::Milestone => "milestone",
    }
}

fn preferred_line_ending(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn local_section_line_ending(
    source: &str,
    indexed: &IndexedItem,
    item_id: WorkItemId,
) -> Result<&'static str, OrgError> {
    ensure_contained_span(source, indexed.section, indexed.subtree, item_id)?;
    let section = source
        .get(indexed.section.start..indexed.section.end)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    if let Some(newline) = section.rfind('\n') {
        return Ok(
            if newline > 0 && section.as_bytes().get(newline - 1) == Some(&b'\r') {
                "\r\n"
            } else {
                "\n"
            },
        );
    }
    span_line_ending(source, indexed.heading, item_id)
}

fn span_line_ending(
    source: &str,
    span: Span,
    item_id: WorkItemId,
) -> Result<&'static str, OrgError> {
    ensure_editable_span(source, span, item_id)?;
    let value = source
        .get(span.start..span.end)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    Ok(if value.ends_with("\r\n") {
        "\r\n"
    } else if value.ends_with('\n') {
        "\n"
    } else {
        preferred_line_ending(source)
    })
}

fn insertion_with_boundaries(
    source: &str,
    offset: usize,
    insertion: &str,
    line_ending: &str,
    item_id: WorkItemId,
) -> Result<String, OrgError> {
    let span = Span {
        start: offset,
        end: offset,
    };
    ensure_editable_span(source, span, item_id)?;
    let before = source.get(..offset).ok_or(OrgError::UnsafeEdit(item_id))?;
    let after = source.get(offset..).ok_or(OrgError::UnsafeEdit(item_id))?;
    let needs_before = offset > 0 && !before.ends_with('\n');
    let needs_after =
        offset < source.len() && !insertion.ends_with('\n') && !after.starts_with('\n');
    let capacity = insertion
        .len()
        .checked_add(if needs_before { line_ending.len() } else { 0 })
        .and_then(|capacity| capacity.checked_add(if needs_after { line_ending.len() } else { 0 }))
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let mut result = String::with_capacity(capacity);
    if needs_before {
        result.push_str(line_ending);
    }
    result.push_str(insertion);
    if needs_after {
        result.push_str(line_ending);
    }
    Ok(result)
}

fn render_tags(tags: &BTreeSet<String>) -> Option<String> {
    (!tags.is_empty()).then(|| format!(":{}:", tags.iter().cloned().collect::<Vec<_>>().join(":")))
}

fn ensure_editable_span(source: &str, span: Span, item_id: WorkItemId) -> Result<(), OrgError> {
    if span == UNSAFE_SPAN
        || span.start > span.end
        || span.end > source.len()
        || !source.is_char_boundary(span.start)
        || !source.is_char_boundary(span.end)
    {
        Err(OrgError::UnsafeEdit(item_id))
    } else {
        Ok(())
    }
}

fn ensure_contained_span(
    source: &str,
    span: Span,
    container: Span,
    item_id: WorkItemId,
) -> Result<(), OrgError> {
    ensure_editable_span(source, container, item_id)?;
    ensure_editable_span(source, span, item_id)?;
    if span.start < container.start || span.end > container.end {
        Err(OrgError::UnsafeEdit(item_id))
    } else {
        Ok(())
    }
}

fn validate_indexed_item(
    source: &str,
    indexed: &IndexedItem,
    item_id: WorkItemId,
) -> Result<(), OrgError> {
    ensure_editable_span(source, indexed.heading, item_id)?;
    ensure_editable_span(source, indexed.subtree, item_id)?;
    if indexed.heading.start != indexed.subtree.start
        || indexed.heading.end > indexed.subtree.end
        || source
            .get(indexed.heading.start..indexed.heading.end)
            .and_then(|heading| heading_level(line_content(heading)))
            != Some(indexed.item.level)
    {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    let scanned = scan_headings(source)
        .into_iter()
        .find(|heading| heading.heading.start == indexed.heading.start)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let section = Span {
        start: scanned.heading.end,
        end: scanned.section_end,
    };
    if indexed.item.id != item_id
        || scanned.heading != indexed.heading
        || scanned.subtree != indexed.subtree
        || indexed.section != section
    {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    let properties = parse_properties(source, scanned.heading.end, scanned.section_end);
    let ids = occurrences(&properties, "ID");
    let markers = occurrences(&properties, "AGENT_NOTE_TYPE");
    if ids.len() != 1
        || ids[0].accumulated
        || WorkItemId::from_str(ids[0].value).ok() != Some(item_id)
        || markers.len() != 1
        || markers[0].accumulated
        || WorkItemType::from_str(markers[0].value).ok() != Some(indexed.item.item_type)
    {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    Ok(())
}

fn checked_offset(
    source: &str,
    start: usize,
    length: usize,
    item_id: WorkItemId,
) -> Result<usize, OrgError> {
    let offset = start
        .checked_add(length)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let insertion = Span {
        start: offset,
        end: offset,
    };
    ensure_editable_span(source, insertion, item_id)?;
    Ok(offset)
}

fn replace_span(source: &str, span: Span, replacement: &str) -> String {
    let mut result =
        String::with_capacity(source.len() - (span.end - span.start) + replacement.len());
    result.push_str(&source[..span.start]);
    result.push_str(replacement);
    result.push_str(&source[span.end..]);
    result
}

pub fn parse_document(
    source: impl Into<String>,
    options: &ParseOptions,
) -> Result<OrgDocument, OrgError> {
    let source = source.into();
    let headings = scan_headings(&source);
    project_document(source, headings, options.clone())
}

pub fn move_item(
    source: &OrgDocument,
    target: &OrgDocument,
    item_id: WorkItemId,
    target_parent: Option<WorkItemId>,
) -> Result<MovedDocuments, OrgError> {
    let indexed = source
        .index
        .get(&item_id)
        .ok_or(OrgError::ItemNotFound(item_id))?;
    validate_indexed_item(&source.source, indexed, item_id)?;
    projected_ids_in_span(source, indexed.subtree, item_id)?;
    let fragment = source
        .source
        .get(indexed.subtree.start..indexed.subtree.end)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let moved_ids = valid_heading_ids(fragment, item_id)?;
    if let Some(duplicate) = moved_ids.intersection(&target.all_ids).next() {
        return Err(OrgError::DuplicateId(*duplicate));
    }
    let (target_level, target_offset) = match target_parent {
        Some(parent_id) => {
            let parent = target
                .index
                .get(&parent_id)
                .ok_or(OrgError::ItemNotFound(parent_id))?;
            validate_indexed_item(&target.source, parent, parent_id)?;
            (
                parent
                    .item
                    .level
                    .checked_add(1)
                    .ok_or(OrgError::UnsafeEdit(item_id))?,
                parent.subtree.end,
            )
        }
        None => (1, target.source.len()),
    };
    let adjusted = adjust_heading_levels(fragment, indexed.item.level, target_level, item_id)?;
    let target_span = Span {
        start: target_offset,
        end: target_offset,
    };
    ensure_editable_span(&target.source, target_span, item_id)?;
    let insertion = insertion_with_boundaries(
        &target.source,
        target_offset,
        &adjusted,
        preferred_line_ending(&target.source),
        item_id,
    )?;

    Ok(MovedDocuments {
        source: replace_span(&source.source, indexed.subtree, ""),
        target: replace_span(&target.source, target_span, &insertion),
    })
}

pub fn reparent_item(
    document: &OrgDocument,
    item_id: WorkItemId,
    target_parent: Option<WorkItemId>,
) -> Result<EditedDocument, OrgError> {
    let indexed = document
        .index
        .get(&item_id)
        .ok_or(OrgError::ItemNotFound(item_id))?;
    validate_indexed_item(&document.source, indexed, item_id)?;
    let moved_ids = projected_ids_in_span(document, indexed.subtree, item_id)?;
    let (target_level, original_offset) = match target_parent {
        Some(parent_id) => {
            let parent = document
                .index
                .get(&parent_id)
                .ok_or(OrgError::ItemNotFound(parent_id))?;
            validate_indexed_item(&document.source, parent, parent_id)?;
            if indexed.subtree.start <= parent.heading.start
                && parent.heading.start < indexed.subtree.end
            {
                return Err(OrgError::UnsafeEdit(item_id));
            }
            (
                parent
                    .item
                    .level
                    .checked_add(1)
                    .ok_or(OrgError::UnsafeEdit(item_id))?,
                parent.subtree.end,
            )
        }
        None => (1, document.source.len()),
    };
    let fragment = document
        .source
        .get(indexed.subtree.start..indexed.subtree.end)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let adjusted = adjust_heading_levels(fragment, indexed.item.level, target_level, item_id)?;
    let cut = replace_span(&document.source, indexed.subtree, "");
    let cut_length = indexed.subtree.end - indexed.subtree.start;
    let target_offset = if original_offset >= indexed.subtree.end {
        original_offset
            .checked_sub(cut_length)
            .ok_or(OrgError::UnsafeEdit(item_id))?
    } else if original_offset <= indexed.subtree.start {
        original_offset
    } else {
        return Err(OrgError::UnsafeEdit(item_id));
    };
    let insertion_span = Span {
        start: target_offset,
        end: target_offset,
    };
    ensure_editable_span(&cut, insertion_span, item_id)?;
    let insertion = insertion_with_boundaries(
        &cut,
        target_offset,
        &adjusted,
        preferred_line_ending(&document.source),
        item_id,
    )?;
    Ok(EditedDocument {
        source: replace_span(&cut, insertion_span, &insertion),
        changed_items: moved_ids,
    })
}

fn projected_ids_in_span(
    document: &OrgDocument,
    span: Span,
    root_id: WorkItemId,
) -> Result<BTreeSet<WorkItemId>, OrgError> {
    ensure_editable_span(&document.source, span, root_id)?;
    let mut moved_ids = BTreeSet::new();
    for (id, indexed) in &document.index {
        validate_indexed_item(&document.source, indexed, *id)?;
        if span.start <= indexed.heading.start && indexed.heading.start < span.end {
            ensure_contained_span(&document.source, indexed.heading, span, *id)?;
            ensure_contained_span(&document.source, indexed.subtree, span, *id)?;
            moved_ids.insert(*id);
        }
    }
    Ok(moved_ids)
}

fn valid_heading_ids(source: &str, item_id: WorkItemId) -> Result<BTreeSet<WorkItemId>, OrgError> {
    let mut ids = BTreeSet::new();
    for heading in scan_headings(source) {
        ensure_editable_span(source, heading.heading, item_id)?;
        ensure_editable_span(source, heading.subtree, item_id)?;
        let section = Span {
            start: heading.heading.end,
            end: heading.section_end,
        };
        ensure_contained_span(source, section, heading.subtree, item_id)?;
        let properties = parse_properties(source, section.start, section.end);
        for entry in occurrences(&properties, "ID")
            .into_iter()
            .filter(|entry| !entry.accumulated)
        {
            ensure_contained_span(source, entry.value_span, section, item_id)?;
            if let Ok(id) = WorkItemId::from_str(entry.value) {
                ids.insert(id);
            }
        }
    }
    Ok(ids)
}

fn adjust_heading_levels(
    fragment: &str,
    old_root_level: usize,
    new_root_level: usize,
    item_id: WorkItemId,
) -> Result<String, OrgError> {
    let increasing = new_root_level >= old_root_level;
    let delta = new_root_level.abs_diff(old_root_level);
    let mut adjusted = fragment.to_string();
    for heading in scan_headings(fragment).into_iter().rev() {
        let new_level = if increasing {
            heading.level.checked_add(delta)
        } else {
            heading.level.checked_sub(delta)
        }
        .filter(|level| *level > 0)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
        let stars = Span {
            start: heading.heading.start,
            end: heading
                .heading
                .start
                .checked_add(heading.level)
                .ok_or(OrgError::UnsafeEdit(item_id))?,
        };
        ensure_editable_span(&adjusted, stars, item_id)?;
        adjusted = replace_span(&adjusted, stars, &"*".repeat(new_level));
    }
    Ok(adjusted)
}

fn scan_headings(source: &str) -> Vec<ScannedHeading> {
    let mut headings = Vec::new();
    let mut block_stack = Vec::<String>::new();
    let mut opaque_drawer = false;
    let mut offset = 0;

    for line in source.split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();

        if !block_stack.is_empty() {
            if let Some(kind) = block_begin(trimmed) {
                block_stack.push(kind.to_string());
            } else if let Some(kind) = block_end(trimmed) {
                if block_stack
                    .last()
                    .is_some_and(|open| open.eq_ignore_ascii_case(kind))
                {
                    block_stack.pop();
                }
            }
            offset += line.len();
            continue;
        }

        if opaque_drawer {
            if is_drawer_end(content) {
                opaque_drawer = false;
            }
            offset += line.len();
            continue;
        }

        if let Some(kind) = block_begin(trimmed) {
            block_stack.push(kind.to_string());
            offset += line.len();
            continue;
        }

        if let Some(name) = drawer_start(trimmed) {
            if !name.eq_ignore_ascii_case("END") {
                opaque_drawer = true;
                offset += line.len();
                continue;
            }
        }

        if let Some(level) = heading_level(content) {
            headings.push(ScannedHeading {
                level,
                heading: Span {
                    start: offset,
                    end: offset + line.len(),
                },
                subtree: Span {
                    start: offset,
                    end: source.len(),
                },
                section_end: source.len(),
            });
        }

        offset += line.len();
    }

    for index in 0..headings.len() {
        headings[index].section_end = headings
            .get(index + 1)
            .map_or(source.len(), |heading| heading.heading.start);
        headings[index].subtree.end = headings[index + 1..]
            .iter()
            .find(|heading| heading.level <= headings[index].level)
            .map_or(source.len(), |heading| heading.heading.start);
    }

    headings
}

fn project_document(
    source: String,
    headings: Vec<ScannedHeading>,
    options: ParseOptions,
) -> Result<OrgDocument, OrgError> {
    let mut items = Vec::new();
    let mut index = BTreeMap::new();
    let mut all_ids = BTreeSet::new();
    let mut projected_ancestors = Vec::<(usize, WorkItemId)>::new();

    for heading in headings {
        while projected_ancestors
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            projected_ancestors.pop();
        }

        let properties = parse_properties(&source, heading.heading.end, heading.section_end);
        all_ids.extend(
            occurrences(&properties, "ID")
                .into_iter()
                .filter(|entry| !entry.accumulated)
                .filter_map(|entry| WorkItemId::from_str(entry.value).ok()),
        );
        let marker = occurrences(&properties, "AGENT_NOTE_TYPE");
        if marker.is_empty() {
            continue;
        }
        if marker.len() != 1 || marker[0].accumulated {
            return Err(parse_error(
                &source,
                marker[0].line_start,
                "AGENT_NOTE_TYPE must occur exactly once",
            ));
        }

        let ids = occurrences(&properties, "ID");
        if ids.len() != 1 || ids.first().is_some_and(|entry| entry.accumulated) {
            let offset = ids
                .first()
                .map_or(heading.heading.start, |entry| entry.line_start);
            return Err(parse_error(
                &source,
                offset,
                "orchestrated heading must contain exactly one ID",
            ));
        }

        let id = WorkItemId::from_str(ids[0].value).map_err(|_| {
            parse_error(
                &source,
                ids[0].line_start,
                "orchestrated heading has an invalid ID",
            )
        })?;
        if index.contains_key(&id) {
            return Err(OrgError::DuplicateId(id));
        }

        let item_type = WorkItemType::from_str(marker[0].value)
            .map_err(|message| parse_error(&source, marker[0].line_start, message))?;
        let parsed_heading = parse_heading(&source, &heading, &options);
        let planning = planning_spans(&source, heading.heading.end, heading.section_end)?;
        let scheduled = parse_planning(&source, &planning, "SCHEDULED")?;
        let deadline = parse_planning(&source, &planning, "DEADLINE")?;
        let assignee = match semantic_property(&properties, "ASSIGNEE") {
            SemanticProperty::Single(entry) => Some(entry.value.to_string()),
            SemanticProperty::Missing | SemanticProperty::Ambiguous { .. } => None,
        };
        let requires_review = match semantic_property(&properties, "REQUIRES_REVIEW") {
            SemanticProperty::Single(entry) => match entry.value {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(parse_error(
                        &source,
                        entry.line_start,
                        "REQUIRES_REVIEW must be true or false",
                    ));
                }
            },
            SemanticProperty::Missing => false,
            SemanticProperty::Ambiguous { line_start } => {
                return Err(parse_error(
                    &source,
                    line_start,
                    "REQUIRES_REVIEW must occur exactly once without accumulation",
                ));
            }
        };
        let depends_on = parse_dependencies(&source, &properties, id)?;
        let indexed_note_links =
            parse_note_links(&source, heading.heading.end, heading.section_end);
        let note_links = indexed_note_links
            .iter()
            .map(|entry| entry.link.clone())
            .collect();

        let parent_id = projected_ancestors.last().map(|(_, id)| *id);

        let item = WorkItem {
            id,
            item_type,
            parent_id,
            level: heading.level,
            title: parsed_heading.title,
            state: parsed_heading.state,
            priority: parsed_heading.priority,
            tags: parsed_heading.tags,
            scheduled,
            deadline,
            assignee,
            depends_on,
            requires_review,
            note_links,
        };

        let indexed = IndexedItem {
            item: item.clone(),
            heading: heading.heading,
            subtree: heading.subtree,
            section: Span {
                start: heading.heading.end,
                end: heading.section_end,
            },
            state: parsed_heading.state_span,
            title: parsed_heading.title_span,
            priority: parsed_heading.priority_span,
            properties: property_spans(&properties),
            planning,
            tags: parsed_heading.tags_span,
            note_links: indexed_note_links,
        };
        items.push(item);
        index.insert(id, indexed);
        projected_ancestors.push((heading.level, id));
    }

    Ok(OrgDocument {
        source,
        items,
        index,
        all_ids,
        options,
    })
}

#[derive(Debug)]
struct ParsedHeading {
    title: String,
    state: Option<String>,
    priority: Option<char>,
    tags: BTreeSet<String>,
    state_span: Option<Span>,
    title_span: Span,
    priority_span: Option<Span>,
    tags_span: Option<Span>,
}

fn parse_heading(source: &str, heading: &ScannedHeading, options: &ParseOptions) -> ParsedHeading {
    let line = line_content(&source[heading.heading.start..heading.heading.end]);
    let mut cursor = heading.level + 1;
    let mut state = None;
    let mut priority = None;
    let mut state_span = None;
    let mut priority_span = None;

    if let Some((token_start, token_end)) = next_token(line, cursor) {
        let token = &line[token_start..token_end];
        if options.states.contains(token) {
            state = Some(token.to_string());
            state_span = Some(Span {
                start: heading.heading.start + token_start,
                end: heading.heading.start + token_end,
            });
            cursor = token_end;
        }
    }

    cursor = skip_horizontal_whitespace(line, cursor);
    let bytes = line.as_bytes();
    if bytes[cursor..].starts_with(b"[#")
        && bytes.get(cursor + 3) == Some(&b']')
        && bytes.get(cursor + 2).is_some_and(u8::is_ascii_uppercase)
        && matches!(bytes.get(cursor + 4), None | Some(b' ' | b'\t'))
    {
        priority = Some(bytes[cursor + 2] as char);
        priority_span = Some(Span {
            start: heading.heading.start + cursor,
            end: heading.heading.start + cursor + 4,
        });
        cursor += 4;
    }
    cursor = skip_horizontal_whitespace(line, cursor);
    let content_end = line
        .trim_end_matches([' ', '\t'])
        .len()
        .max(heading.level + 1);
    cursor = cursor.min(content_end);
    let tags_span = trailing_tags(line, cursor, content_end).map(|(start, end)| Span {
        start: heading.heading.start + start,
        end: heading.heading.start + end,
    });
    let title_end = tags_span
        .map(|span| span.start - heading.heading.start)
        .unwrap_or(content_end);
    let raw_title = &line[cursor..title_end];
    let title_start = cursor + raw_title.len() - raw_title.trim_start().len();
    let title_end = cursor + raw_title.trim_end().len();
    let title_span = Span {
        start: heading.heading.start + title_start,
        end: heading.heading.start + title_end,
    };
    let title = line[title_start..title_end].to_string();
    let tags = tags_span
        .map(|span| {
            line[span.start - heading.heading.start..span.end - heading.heading.start]
                .trim_matches(':')
                .split(':')
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    ParsedHeading {
        title,
        state,
        priority,
        tags,
        state_span,
        title_span,
        priority_span,
        tags_span,
    }
}

fn semantic_property<'source, 'properties>(
    properties: &'properties [PropertyOccurrence<'source>],
    key: &str,
) -> SemanticProperty<'properties, 'source> {
    let entries = occurrences(properties, key);
    if entries.is_empty() {
        SemanticProperty::Missing
    } else if entries.len() == 1 && !entries[0].accumulated {
        SemanticProperty::Single(entries[0])
    } else {
        let ambiguous = if entries[0].accumulated {
            entries[0]
        } else {
            entries[1]
        };
        SemanticProperty::Ambiguous {
            line_start: ambiguous.line_start,
        }
    }
}

fn parse_dependencies(
    source: &str,
    properties: &[PropertyOccurrence<'_>],
    item_id: WorkItemId,
) -> Result<BTreeSet<WorkItemId>, OrgError> {
    let entry = match semantic_property(properties, "DEPENDS_ON") {
        SemanticProperty::Missing => return Ok(BTreeSet::new()),
        SemanticProperty::Single(entry) => entry,
        SemanticProperty::Ambiguous { line_start } => {
            return Err(parse_error(
                source,
                line_start,
                "DEPENDS_ON must occur exactly once without accumulation",
            ));
        }
    };
    let mut dependencies = BTreeSet::new();
    for value in entry.value.split_whitespace() {
        let dependency = WorkItemId::from_str(value).map_err(|_| {
            parse_error(
                source,
                entry.line_start,
                "DEPENDS_ON contains an invalid dependency ID",
            )
        })?;
        if dependency == item_id {
            return Err(parse_error(
                source,
                entry.line_start,
                "work item cannot depend on itself",
            ));
        }
        if !dependencies.insert(dependency) {
            return Err(parse_error(
                source,
                entry.line_start,
                "DEPENDS_ON contains a duplicate dependency ID",
            ));
        }
    }
    Ok(dependencies)
}

fn parse_planning(
    source: &str,
    planning: &BTreeMap<String, Span>,
    key: &str,
) -> Result<Option<OrgTimestamp>, OrgError> {
    let Some(span) = planning.get(key) else {
        return Ok(None);
    };
    let line = line_content(&source[span.start..span.end]).trim_start();
    let raw = line
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix(':'))
        .map(str::trim)
        .unwrap_or_default();
    parse_planning_timestamp(raw)
        .map(Some)
        .map_err(|message| parse_error(source, span.start, format!("{key}: {message}")))
}

fn parse_planning_timestamp(raw: &str) -> Result<OrgTimestamp, String> {
    let inner = raw
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or_else(|| "planning timestamp must be active Org syntax".to_string())?;
    if inner.contains('+')
        || inner.contains("++")
        || inner.contains(".+")
        || inner.matches('-').count() > 2
    {
        return Err("repeaters and time ranges are unsupported".to_string());
    }
    let parts = inner.split_whitespace().collect::<Vec<_>>();
    let value = match parts.as_slice() {
        [date, _weekday] => format!("{date} 00:00"),
        [date, _weekday, time] => format!("{date} {time}"),
        _ => return Err("unsupported planning timestamp".to_string()),
    };
    let local = chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M")
        .map_err(|_| "invalid planning timestamp".to_string())?;
    Ok(OrgTimestamp {
        raw: raw.to_string(),
        local,
    })
}

fn parse_note_links(source: &str, start: usize, end: usize) -> Vec<IndexedNoteLink> {
    let mut links = Vec::new();
    let mut block_stack = Vec::<String>::new();
    let mut drawer = false;
    let mut offset = start;

    for line in source[start..end].split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();
        if !block_stack.is_empty() {
            if let Some(kind) = block_begin(trimmed) {
                block_stack.push(kind.to_string());
            } else if let Some(kind) = block_end(trimmed) {
                if block_stack
                    .last()
                    .is_some_and(|open| open.eq_ignore_ascii_case(kind))
                {
                    block_stack.pop();
                }
            }
            offset += line.len();
            continue;
        }
        if drawer {
            if is_drawer_end(content) {
                drawer = false;
            }
            offset += line.len();
            continue;
        }
        if let Some(kind) = block_begin(trimmed) {
            block_stack.push(kind.to_string());
            offset += line.len();
            continue;
        }
        if drawer_start(trimmed).is_some_and(|name| !name.eq_ignore_ascii_case("END")) {
            drawer = true;
            offset += line.len();
            continue;
        }
        if trimmed.starts_with('#') {
            offset += line.len();
            continue;
        }
        parse_note_links_in_line(content, offset, &mut links);
        offset += line.len();
    }

    links
}

fn parse_note_links_in_line(line: &str, base_offset: usize, links: &mut Vec<IndexedNoteLink>) {
    enum State {
        Searching,
        Target {
            outer_start: usize,
            start: usize,
        },
        Description {
            outer_start: usize,
            target_start: usize,
            target_end: usize,
            description_start: usize,
            depth: usize,
        },
        Discarding,
    }

    let bytes = line.as_bytes();
    let mut state = State::Searching;
    let mut cursor = 0;
    while cursor < bytes.len() {
        let next = bytes.get(cursor + 1).copied();
        match &mut state {
            State::Searching => {
                if bytes[cursor] == b'[' && next == Some(b'[') {
                    state = State::Target {
                        outer_start: cursor,
                        start: cursor + 2,
                    };
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
            State::Target { outer_start, start } => {
                if bytes[cursor] == b'[' && next == Some(b'[') {
                    *outer_start = cursor;
                    *start = cursor + 2;
                    cursor += 2;
                } else if bytes[cursor] == b']' && next == Some(b']') {
                    if let Some((purpose, note_id)) = parse_note_link_target(&line[*start..cursor])
                    {
                        links.push(IndexedNoteLink {
                            link: NoteLink {
                                purpose: purpose.to_string(),
                                note_id,
                                description: String::new(),
                            },
                            span: Span {
                                start: base_offset + *outer_start,
                                end: base_offset + cursor + 2,
                            },
                        });
                    }
                    state = State::Searching;
                    cursor += 2;
                } else if bytes[cursor] == b']' && next == Some(b'[') {
                    if parse_note_link_target(&line[*start..cursor]).is_some() {
                        state = State::Description {
                            outer_start: *outer_start,
                            target_start: *start,
                            target_end: cursor,
                            description_start: cursor + 2,
                            depth: 0,
                        };
                    } else {
                        state = State::Discarding;
                    }
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
            State::Description {
                outer_start,
                target_start,
                target_end,
                description_start,
                depth,
            } => match bytes[cursor] {
                b'[' => {
                    *depth += 1;
                    cursor += 1;
                }
                b']' if *depth > 0 => {
                    *depth -= 1;
                    cursor += 1;
                }
                b']' if next == Some(b']') => {
                    if let Some((purpose, note_id)) =
                        parse_note_link_target(&line[*target_start..*target_end])
                    {
                        links.push(IndexedNoteLink {
                            link: NoteLink {
                                purpose: purpose.to_string(),
                                note_id,
                                description: line[*description_start..cursor].to_string(),
                            },
                            span: Span {
                                start: base_offset + *outer_start,
                                end: base_offset + cursor + 2,
                            },
                        });
                    }
                    state = State::Searching;
                    cursor += 2;
                }
                b']' => {
                    state = State::Discarding;
                    cursor += 1;
                }
                _ => cursor += 1,
            },
            State::Discarding => {
                if bytes[cursor] == b'[' && next == Some(b'[') {
                    state = State::Target {
                        outer_start: cursor,
                        start: cursor + 2,
                    };
                    cursor += 2;
                } else if bytes[cursor] == b']' && next == Some(b']') {
                    state = State::Searching;
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
        }
    }
}

fn parse_note_link_target(target: &str) -> Option<(&str, Uuid)> {
    if target.contains(['[', ']']) {
        return None;
    }

    let mut parts = target.split(':');
    let (Some("agent-note"), Some(purpose), Some(note_id), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if purpose.is_empty() {
        return None;
    }
    let note_id = Uuid::parse_str(note_id).ok()?;
    Some((purpose, note_id))
}

fn validate_note_link(source: &str, offset: usize, link: &NoteLink) -> Result<(), OrgError> {
    if link.purpose.is_empty()
        || link.purpose.contains([':', '[', ']', '\r', '\n'])
        || link.purpose.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(parse_error(
            source,
            offset,
            "agent-note link purpose is invalid",
        ));
    }
    if link.description.contains(['\r', '\n']) {
        return Err(parse_error(
            source,
            offset,
            "agent-note link description must be one line",
        ));
    }
    let mut depth = 0usize;
    for byte in link.description.bytes() {
        if byte == b'[' {
            depth += 1;
        } else if byte == b']' {
            depth = depth.checked_sub(1).ok_or_else(|| {
                parse_error(
                    source,
                    offset,
                    "agent-note link description has unbalanced brackets",
                )
            })?;
        }
    }
    if depth != 0 {
        return Err(parse_error(
            source,
            offset,
            "agent-note link description has unbalanced brackets",
        ));
    }
    Ok(())
}

fn render_note_link(link: &NoteLink, line_ending: &str) -> String {
    let target = format!("agent-note:{}:{}", link.purpose, link.note_id);
    if link.description.is_empty() {
        format!("[[{target}]]{line_ending}")
    } else {
        format!("[[{target}][{}]]{line_ending}", link.description)
    }
}

fn parse_properties<'a>(source: &'a str, start: usize, end: usize) -> Vec<PropertyOccurrence<'a>> {
    let mut properties = Vec::new();
    let Some(drawer) = direct_property_drawer(source, start, end) else {
        return properties;
    };
    let mut offset = drawer.contents.start;

    for line in source[drawer.contents.start..drawer.contents.end].split_inclusive('\n') {
        let content = line_content(line);
        if let Some((raw_key, value_start, value_end)) = property_parts(content) {
            let accumulated = raw_key.ends_with('+');
            let key = raw_key
                .strip_suffix('+')
                .unwrap_or(raw_key)
                .to_ascii_uppercase();
            properties.push(PropertyOccurrence {
                key,
                accumulated,
                value: &content[value_start..value_end],
                value_span: Span {
                    start: offset + value_start,
                    end: offset + value_end,
                },
                line_start: offset,
            });
        }
        offset += line.len();
    }

    properties
}

fn direct_property_drawer(source: &str, start: usize, end: usize) -> Option<DirectPropertyDrawer> {
    let drawer_start = direct_property_drawer_start(source, start, end)?;
    let opening_line = source[drawer_start..end].split_inclusive('\n').next()?;
    let contents_start = drawer_start + opening_line.len();
    let mut offset = contents_start;
    let mut nested = false;

    for line in source[contents_start..end].split_inclusive('\n') {
        let content = line_content(line);
        if is_drawer_end(content) {
            return Some(DirectPropertyDrawer {
                contents: Span {
                    start: contents_start,
                    end: offset,
                },
                terminator: Some(offset),
                nested,
            });
        }
        if is_properties_drawer_start(content) {
            nested = true;
        }
        offset += line.len();
    }

    Some(DirectPropertyDrawer {
        contents: Span {
            start: contents_start,
            end,
        },
        terminator: None,
        nested,
    })
}

fn direct_property_drawer_start(source: &str, start: usize, end: usize) -> Option<usize> {
    let mut offset = start;
    for line in source[start..end].split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();
        if trimmed.starts_with("SCHEDULED:") || trimmed.starts_with("DEADLINE:") {
            offset += line.len();
            continue;
        }
        return is_properties_drawer_start(content).then_some(offset);
    }
    None
}

fn is_properties_drawer_start(line_content: &str) -> bool {
    drawer_start(line_content.trim_start())
        .is_some_and(|name| name.eq_ignore_ascii_case("PROPERTIES"))
}

fn is_drawer_end(line_content: &str) -> bool {
    line_content.trim().eq_ignore_ascii_case(":END:")
}

fn occurrences<'a, 'b>(
    properties: &'b [PropertyOccurrence<'a>],
    key: &str,
) -> Vec<&'b PropertyOccurrence<'a>> {
    properties.iter().filter(|entry| entry.key == key).collect()
}

fn property_spans(properties: &[PropertyOccurrence<'_>]) -> BTreeMap<String, Span> {
    let mut grouped = BTreeMap::<String, Vec<&PropertyOccurrence<'_>>>::new();
    for property in properties {
        grouped
            .entry(property.key.clone())
            .or_default()
            .push(property);
    }
    grouped
        .into_iter()
        .map(|(key, entries)| {
            let base_count = entries.iter().filter(|entry| !entry.accumulated).count();
            let unsafe_to_edit = base_count > 1 || entries.iter().any(|entry| entry.accumulated);
            let span = if unsafe_to_edit {
                UNSAFE_SPAN
            } else {
                entries[0].value_span
            };
            (key, span)
        })
        .collect()
}

fn planning_spans(
    source: &str,
    start: usize,
    end: usize,
) -> Result<BTreeMap<String, Span>, OrgError> {
    let mut spans = BTreeMap::new();
    let mut offset = start;

    for line in source[start..end].split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();
        let key = if trimmed.starts_with("SCHEDULED:") {
            "SCHEDULED"
        } else if trimmed.starts_with("DEADLINE:") {
            "DEADLINE"
        } else {
            break;
        };
        if spans.contains_key(key) {
            return Err(parse_error(
                source,
                offset,
                format!("{key} planning keyword must not be repeated"),
            ));
        }
        spans.insert(
            key.to_string(),
            Span {
                start: offset,
                end: offset + line.len(),
            },
        );
        offset += line.len();
    }

    Ok(spans)
}

fn planning_value_span(
    source: &str,
    line_span: Span,
    key: &str,
    item_id: WorkItemId,
) -> Result<Span, OrgError> {
    ensure_editable_span(source, line_span, item_id)?;
    let line = source
        .get(line_span.start..line_span.end)
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    let content = line_content(line);
    let trimmed = content.trim_start();
    let key_start = content.len() - trimmed.len();
    let value_region_start = key_start
        .checked_add(key.len())
        .and_then(|offset| offset.checked_add(1))
        .ok_or(OrgError::UnsafeEdit(item_id))?;
    if !trimmed.starts_with(key)
        || trimmed.as_bytes().get(key.len()) != Some(&b':')
        || value_region_start > content.len()
    {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    let value_region = &content[value_region_start..];
    let value_start = value_region_start + value_region.len() - value_region.trim_start().len();
    let value_end = value_region_start + value_region.trim_end().len();
    if value_start >= value_end {
        return Err(OrgError::UnsafeEdit(item_id));
    }
    let span = Span {
        start: line_span.start + value_start,
        end: line_span.start + value_end,
    };
    ensure_contained_span(source, span, line_span, item_id)?;
    Ok(span)
}

fn line_content(line: &str) -> &str {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .strip_suffix('\r')
        .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line))
}

fn block_begin(line: &str) -> Option<&str> {
    block_directive(line, "#+BEGIN_")
}

fn block_end(line: &str) -> Option<&str> {
    block_directive(line, "#+END_")
}

fn block_directive<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let upper = line.to_ascii_uppercase();
    let suffix = upper.strip_prefix(prefix)?;
    let kind_end = suffix
        .find(|character: char| character.is_ascii_whitespace())
        .unwrap_or(suffix.len());
    let kind = &suffix[..kind_end];
    if matches!(kind, "SRC" | "EXAMPLE" | "EXPORT") {
        let start = prefix.len();
        Some(&line[start..start + kind.len()])
    } else {
        None
    }
}

fn drawer_start(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let name = line.strip_prefix(':')?.strip_suffix(':')?;
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        Some(name)
    } else {
        None
    }
}

fn heading_level(line: &str) -> Option<usize> {
    let level = line.bytes().take_while(|byte| *byte == b'*').count();
    (level > 0 && line.as_bytes().get(level) == Some(&b' ')).then_some(level)
}

fn next_token(line: &str, start: usize) -> Option<(usize, usize)> {
    let start = skip_horizontal_whitespace(line, start);
    if start == line.len() {
        return None;
    }
    let end = line[start..]
        .find(|character: char| character.is_ascii_whitespace())
        .map_or(line.len(), |length| start + length);
    Some((start, end))
}

fn skip_horizontal_whitespace(line: &str, mut offset: usize) -> usize {
    while line
        .as_bytes()
        .get(offset)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        offset += 1;
    }
    offset
}

fn trailing_tags(line: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let remaining = &line[start..end];
    let candidate_start = if remaining.starts_with(':') {
        start
    } else {
        remaining.rfind(' ').map(|index| start + index + 1)?
    };
    let candidate = &line[candidate_start..end];
    if candidate.len() >= 3
        && candidate.starts_with(':')
        && candidate.ends_with(':')
        && candidate
            .trim_matches(':')
            .split(':')
            .all(|tag| !tag.is_empty() && !tag.bytes().any(|byte| byte.is_ascii_whitespace()))
    {
        Some((candidate_start, end))
    } else {
        None
    }
}

fn property_parts(line: &str) -> Option<(&str, usize, usize)> {
    let leading = line.len() - line.trim_start().len();
    let rest = &line[leading..];
    let after_colon = rest.strip_prefix(':')?;
    let key_end = after_colon.find(':')?;
    let key = &after_colon[..key_end];
    if key.is_empty() {
        return None;
    }
    let value_end = line.trim_end_matches([' ', '\t']).len();
    let mut value_start = leading + 1 + key_end + 1;
    while value_start < value_end
        && (line.as_bytes().get(value_start) == Some(&b' ')
            || line.as_bytes().get(value_start) == Some(&b'\t'))
    {
        value_start += 1;
    }
    Some((key, value_start, value_end))
}

fn parse_error(source: &str, offset: usize, message: impl Into<String>) -> OrgError {
    OrgError::Parse {
        line: source[..offset.min(source.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        move_item, parse_document, parse_heading, reparent_item, scan_headings, Span, UNSAFE_SPAN,
    };
    use crate::{
        NewWorkItem, OrgError, ParseOptions, PropertyKey, SemanticEdit, WorkItemId, WorkItemType,
    };
    use std::{collections::BTreeSet, str::FromStr};

    fn id() -> WorkItemId {
        WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap()
    }

    fn options() -> ParseOptions {
        ParseOptions::new(["BACKLOG", "READY", "RUNNING", "DONE"])
    }

    fn editable_document() -> super::OrgDocument {
        parse_document(
            "* READY Safe :tag:\nSCHEDULED: <2026-08-01 Sat>\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:ASSIGNEE: agent\n:END:\n",
            &options(),
        )
        .unwrap()
    }

    fn assert_unsafe(result: Result<crate::EditedDocument, OrgError>) {
        assert_eq!(result, Err(OrgError::UnsafeEdit(id())));
    }

    #[test]
    fn scans_lf_heading_and_subtree_spans() {
        let source = "preamble\n* READY One\nbody\n** DONE Child\nchild body\n* DONE Two\n";
        let headings = scan_headings(source);

        assert_eq!(headings.len(), 3);
        assert_eq!(
            &source[headings[0].heading.start..headings[0].heading.end],
            "* READY One\n"
        );
        assert_eq!(
            &source[headings[0].subtree.start..headings[0].subtree.end],
            "* READY One\nbody\n** DONE Child\nchild body\n"
        );
    }

    #[test]
    fn scans_crlf_without_changing_byte_offsets() {
        let source = "* READY One\r\nbody\r\n* DONE Two\r\n";
        let headings = scan_headings(source);

        assert_eq!(headings.len(), 2);
        assert_eq!(
            &source[headings[0].heading.start..headings[0].heading.end],
            "* READY One\r\n"
        );
        assert_eq!(
            &source[headings[0].subtree.start..headings[0].subtree.end],
            "* READY One\r\nbody\r\n"
        );
    }

    #[test]
    fn nested_blocks_keep_heading_looking_lines_opaque() {
        let source = "\
#+BEGIN_SRC org
#+BEGIN_EXAMPLE
* DONE Hidden in nested block
#+END_EXAMPLE
#+END_SRC
* READY Visible
";
        let headings = scan_headings(source);

        assert_eq!(headings.len(), 1);
        assert_eq!(
            &source[headings[0].heading.start..headings[0].heading.end],
            "* READY Visible\n"
        );
    }

    #[test]
    fn marked_fake_headings_inside_opaque_constructs_are_not_scanned() {
        let source = "\
#+BEGIN_SRC org
* READY Hidden block item
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
#+END_SRC
:CUSTOM_DRAWER:
* READY Hidden drawer item
:PROPERTIES:
:ID: 22222222-2222-4222-8222-222222222222
:AGENT_NOTE_TYPE: task
:END:
:END:
* READY Visible item
";
        let headings = scan_headings(source);

        assert_eq!(headings.len(), 1);
        assert_eq!(
            &source[headings[0].heading.start..headings[0].heading.end],
            "* READY Visible item\n"
        );
    }

    #[test]
    fn scans_final_heading_line_without_newline() {
        let source = "preamble\n* READY Final";
        let headings = scan_headings(source);

        assert_eq!(headings.len(), 1);
        assert_eq!(
            &source[headings[0].heading.start..headings[0].heading.end],
            "* READY Final"
        );
        assert_eq!(
            &source[headings[0].subtree.start..headings[0].subtree.end],
            "* READY Final"
        );
    }

    #[test]
    fn indexes_tag_span_when_tags_are_all_remaining_heading_content() {
        let source = "* READY :urgent:\n";
        let heading = &scan_headings(source)[0];
        let parsed = parse_heading(source, heading, &ParseOptions::new(["READY"]));
        let tags = parsed.tags_span.unwrap();

        assert_eq!(&source[tags.start..tags.end], ":urgent:");
    }

    #[test]
    fn corrupt_semantic_edit_spans_return_unsafe_errors_without_panicking() {
        let mut document = editable_document();
        document.index.get_mut(&id()).unwrap().state = Some(UNSAFE_SPAN);
        assert_unsafe(document.apply(SemanticEdit::SetState {
            item_id: id(),
            state: "RUNNING".to_string(),
        }));

        let mut document = editable_document();
        document
            .index
            .get_mut(&id())
            .unwrap()
            .properties
            .insert("ASSIGNEE".to_string(), UNSAFE_SPAN);
        assert_unsafe(document.apply(SemanticEdit::SetProperty {
            item_id: id(),
            key: PropertyKey::Assignee,
            value: Some("other".to_string()),
        }));

        let mut document = editable_document();
        document
            .index
            .get_mut(&id())
            .unwrap()
            .planning
            .insert("SCHEDULED".to_string(), UNSAFE_SPAN);
        assert_unsafe(document.apply(SemanticEdit::SetScheduled {
            item_id: id(),
            value: Some("<2026-08-02 Sun>".to_string()),
        }));

        let mut document = editable_document();
        document.index.get_mut(&id()).unwrap().tags = Some(UNSAFE_SPAN);
        assert_unsafe(document.apply(SemanticEdit::SetTags {
            item_id: id(),
            tags: BTreeSet::from(["other".to_string()]),
        }));

        let source = "* READY Café :tag:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
        let mut document = parse_document(source, &options()).unwrap();
        let inside_multibyte_character = source.find('é').unwrap() + 1;
        document.index.get_mut(&id()).unwrap().tags = Some(Span {
            start: inside_multibyte_character,
            end: inside_multibyte_character,
        });
        assert_unsafe(document.apply(SemanticEdit::SetTags {
            item_id: id(),
            tags: BTreeSet::from(["other".to_string()]),
        }));

        let source = "* Missing state\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n";
        let mut document = parse_document(source, &options()).unwrap();
        document.index.get_mut(&id()).unwrap().item.level = usize::MAX;
        assert_unsafe(document.apply(SemanticEdit::SetState {
            item_id: id(),
            state: "READY".to_string(),
        }));

        let mut document = parse_document(source, &options()).unwrap();
        document.index.get_mut(&id()).unwrap().heading.end = usize::MAX;
        assert_unsafe(document.apply(SemanticEdit::SetScheduled {
            item_id: id(),
            value: Some("<2026-08-02 Sun>".to_string()),
        }));

        let mut document = editable_document();
        document.index.get_mut(&id()).unwrap().heading = Span {
            start: usize::MAX,
            end: usize::MAX,
        };
        assert_unsafe(document.apply(SemanticEdit::SetScheduled {
            item_id: id(),
            value: None,
        }));

        let mut document = editable_document();
        document.index.get_mut(&id()).unwrap().subtree = UNSAFE_SPAN;
        assert_unsafe(document.apply(SemanticEdit::SetProperty {
            item_id: id(),
            key: PropertyKey::Assignee,
            value: None,
        }));
        assert_eq!(
            move_item(
                &document,
                &parse_document("", &options()).unwrap(),
                id(),
                None
            ),
            Err(OrgError::UnsafeEdit(id()))
        );
        assert_unsafe(reparent_item(&document, id(), None));

        let mut document = editable_document();
        document.index.get_mut(&id()).unwrap().subtree = UNSAFE_SPAN;
        assert_unsafe(document.apply(SemanticEdit::AppendItem {
            parent_id: Some(id()),
            item: NewWorkItem {
                id: WorkItemId::from_str("22222222-2222-4222-8222-222222222222").unwrap(),
                item_type: WorkItemType::Task,
                title: "Child".to_string(),
                state: "READY".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        }));
    }

    #[test]
    fn corrupt_descendant_indices_make_move_and_reparent_unsafe() {
        let child_id = WorkItemId::from_str("22222222-2222-4222-8222-222222222222").unwrap();
        let target_id = WorkItemId::from_str("33333333-3333-4333-8333-333333333333").unwrap();
        let source = "* READY Root\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: project\n:END:\n** READY Child\n:PROPERTIES:\n:ID: 22222222-2222-4222-8222-222222222222\n:AGENT_NOTE_TYPE: task\n:END:\n* READY Target\n:PROPERTIES:\n:ID: 33333333-3333-4333-8333-333333333333\n:AGENT_NOTE_TYPE: project\n:END:\n";

        let mut document = parse_document(source, &options()).unwrap();
        document.index.get_mut(&child_id).unwrap().heading = UNSAFE_SPAN;
        assert_eq!(
            move_item(
                &document,
                &parse_document("", &options()).unwrap(),
                id(),
                None,
            ),
            Err(OrgError::UnsafeEdit(child_id))
        );

        let mut document = parse_document(source, &options()).unwrap();
        document.index.get_mut(&child_id).unwrap().heading = UNSAFE_SPAN;
        assert_eq!(
            reparent_item(&document, id(), Some(target_id)),
            Err(OrgError::UnsafeEdit(child_id))
        );
    }
}
