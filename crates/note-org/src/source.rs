use crate::{
    IndexedItem, NoteLink, OrgError, OrgTimestamp, ParseOptions, Span, WorkItem, WorkItemId,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDocument {
    source: String,
    items: Vec<WorkItem>,
    index: BTreeMap<WorkItemId, IndexedItem>,
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
}

pub fn parse_document(
    source: impl Into<String>,
    options: &ParseOptions,
) -> Result<OrgDocument, OrgError> {
    let source = source.into();
    let headings = scan_headings(&source);
    project_document(source, headings, options.clone())
}

fn scan_headings(source: &str) -> Vec<ScannedHeading> {
    let mut headings = Vec::new();
    let mut block_stack = Vec::<String>::new();
    let mut opaque_drawer = false;
    let mut offset = 0;

    for line in source.split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();

        if let Some(kind) = block_begin(trimmed) {
            block_stack.push(kind.to_string());
            offset += line.len();
            continue;
        }

        if let Some(kind) = block_end(trimmed) {
            if block_stack
                .last()
                .is_some_and(|open| open.eq_ignore_ascii_case(kind))
            {
                block_stack.pop();
            }
            offset += line.len();
            continue;
        }

        if !block_stack.is_empty() {
            offset += line.len();
            continue;
        }

        if opaque_drawer {
            if trimmed.eq_ignore_ascii_case(":END:") {
                opaque_drawer = false;
            }
            offset += line.len();
            continue;
        }

        if let Some(name) = drawer_start(trimmed) {
            if !name.eq_ignore_ascii_case("PROPERTIES") && !name.eq_ignore_ascii_case("END") {
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
    let mut projected_ancestors = Vec::<(usize, WorkItemId)>::new();

    for heading in headings {
        while projected_ancestors
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            projected_ancestors.pop();
        }

        let properties = parse_properties(&source, heading.heading.end, heading.section_end);
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
        let note_links = parse_note_links(&source, heading.heading.end, heading.section_end);

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
            state: parsed_heading.state_span,
            properties: property_spans(&properties),
            planning,
            tags: parsed_heading.tags_span,
        };
        items.push(item);
        index.insert(id, indexed);
        projected_ancestors.push((heading.level, id));
    }

    Ok(OrgDocument {
        source,
        items,
        index,
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
    tags_span: Option<Span>,
}

fn parse_heading(source: &str, heading: &ScannedHeading, options: &ParseOptions) -> ParsedHeading {
    let line = line_content(&source[heading.heading.start..heading.heading.end]);
    let mut cursor = heading.level + 1;
    let mut state = None;
    let mut priority = None;
    let mut state_span = None;

    if let Some((token_start, token_end)) = next_token(line, cursor) {
        let token = &line[token_start..token_end];
        if options.states.contains(token) {
            state = Some(token.to_string());
            state_span = Some(Span {
                start: heading.heading.start + token_start,
                end: heading.heading.start + token_end,
            });
            cursor = token_end;
            cursor = skip_ascii_spaces(line, cursor);
            let bytes = line.as_bytes();
            if bytes[cursor..].starts_with(b"[#")
                && bytes.get(cursor + 3) == Some(&b']')
                && bytes.get(cursor + 2).is_some_and(u8::is_ascii_uppercase)
            {
                priority = Some(bytes[cursor + 2] as char);
                cursor += 4;
            }
        }
    }

    cursor = skip_ascii_spaces(line, cursor);
    let content_end = line.trim_end_matches([' ', '\t']).len();
    let tags_span = trailing_tags(line, cursor, content_end).map(|(start, end)| Span {
        start: heading.heading.start + start,
        end: heading.heading.start + end,
    });
    let title_end = tags_span
        .map(|span| span.start - heading.heading.start)
        .unwrap_or(content_end);
    let title = line[cursor..title_end].trim().to_string();
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

fn parse_note_links(source: &str, start: usize, end: usize) -> Vec<NoteLink> {
    let mut links = Vec::new();
    let mut block_stack = Vec::<String>::new();
    let mut drawer = false;

    for line in source[start..end].split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();
        if let Some(kind) = block_begin(trimmed) {
            block_stack.push(kind.to_string());
            continue;
        }
        if let Some(kind) = block_end(trimmed) {
            if block_stack
                .last()
                .is_some_and(|open| open.eq_ignore_ascii_case(kind))
            {
                block_stack.pop();
            }
            continue;
        }
        if !block_stack.is_empty() {
            continue;
        }
        if drawer {
            if trimmed.eq_ignore_ascii_case(":END:") {
                drawer = false;
            }
            continue;
        }
        if drawer_start(trimmed).is_some_and(|name| !name.eq_ignore_ascii_case("END")) {
            drawer = true;
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        parse_note_links_in_line(content, &mut links);
    }

    links
}

fn parse_note_links_in_line(line: &str, links: &mut Vec<NoteLink>) {
    let mut remaining = line;
    while let Some(link_start) = remaining.find("[[") {
        remaining = &remaining[link_start + 2..];
        let Some(target_end) = remaining.find("][") else {
            break;
        };
        let target = &remaining[..target_end];
        remaining = &remaining[target_end + 2..];
        let Some(description_end) = remaining.find("]]") else {
            break;
        };
        let description = &remaining[..description_end];
        remaining = &remaining[description_end + 2..];

        let mut parts = target.split(':');
        let (Some("agent-note"), Some(purpose), Some(note_id), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if purpose.is_empty() {
            continue;
        }
        let Ok(note_id) = Uuid::parse_str(note_id) else {
            continue;
        };
        links.push(NoteLink {
            purpose: purpose.to_string(),
            note_id,
            description: description.to_string(),
        });
    }
}

fn parse_properties<'a>(source: &'a str, start: usize, end: usize) -> Vec<PropertyOccurrence<'a>> {
    let mut properties = Vec::new();
    let mut in_drawer = false;
    let mut opaque_drawer = false;
    let mut block_stack = Vec::<String>::new();
    let mut offset = start;

    for line in source[start..end].split_inclusive('\n') {
        let content = line_content(line);
        let trimmed = content.trim_start();
        if let Some(kind) = block_begin(trimmed) {
            block_stack.push(kind.to_string());
            offset += line.len();
            continue;
        }
        if let Some(kind) = block_end(trimmed) {
            if block_stack
                .last()
                .is_some_and(|open| open.eq_ignore_ascii_case(kind))
            {
                block_stack.pop();
            }
            offset += line.len();
            continue;
        }
        if !block_stack.is_empty() {
            offset += line.len();
            continue;
        }
        if opaque_drawer {
            if trimmed.eq_ignore_ascii_case(":END:") {
                opaque_drawer = false;
            }
            offset += line.len();
            continue;
        }
        if !in_drawer {
            if trimmed.eq_ignore_ascii_case(":PROPERTIES:") {
                in_drawer = true;
            } else if drawer_start(trimmed).is_some_and(|name| !name.eq_ignore_ascii_case("END")) {
                opaque_drawer = true;
            }
            offset += line.len();
            continue;
        }
        if trimmed.eq_ignore_ascii_case(":END:") {
            in_drawer = false;
            offset += line.len();
            continue;
        }
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
    let start = skip_ascii_spaces(line, start);
    if start == line.len() {
        return None;
    }
    let end = line[start..]
        .find(|character: char| character.is_ascii_whitespace())
        .map_or(line.len(), |length| start + length);
    Some((start, end))
}

fn skip_ascii_spaces(line: &str, mut offset: usize) -> usize {
    while line.as_bytes().get(offset) == Some(&b' ') {
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
    use super::{parse_heading, scan_headings};
    use crate::ParseOptions;

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
}
