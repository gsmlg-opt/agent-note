use std::collections::BTreeMap;

pub fn compute_tag(content: &str) -> String {
    let mut hash = 0x811c9dc5u32;
    for b in content.as_bytes() {
        hash = (hash ^ *b as u32).wrapping_mul(0x01000193);
    }
    format!("{hash:08x}")
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditOp {
    Swap {
        from: usize,
        to: usize,
        lines: Vec<String>,
    },
    Delete {
        from: usize,
        to: usize,
    },
    InsertBefore {
        line: usize,
        lines: Vec<String>,
    },
    InsertAfter {
        line: usize,
        lines: Vec<String>,
    },
    InsertHead {
        lines: Vec<String>,
    },
    InsertTail {
        lines: Vec<String>,
    },
}

pub fn apply_line_edits(content: &str, ops: &[EditOp]) -> Result<String, String> {
    if ops.is_empty() {
        return Err("edits must contain at least one operation".into());
    }

    let lines = content.split('\n').collect::<Vec<&str>>();
    let n = lines.len();

    validate_ops(ops, n)?;

    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut before: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut after: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut replacements = BTreeMap::new();

    for op in ops {
        match op {
            EditOp::Swap { from, to, lines } => {
                replacements.insert(*from, (*to, lines.clone()));
            }
            EditOp::Delete { from, to } => {
                replacements.insert(*from, (*to, Vec::new()));
            }
            EditOp::InsertBefore { line, lines } => {
                before.entry(*line).or_default().extend(lines.clone());
            }
            EditOp::InsertAfter { line, lines } => {
                after.entry(*line).or_default().extend(lines.clone());
            }
            EditOp::InsertHead { lines } => head.extend(lines.clone()),
            EditOp::InsertTail { lines } => tail.extend(lines.clone()),
        }
    }

    let mut result = Vec::new();
    result.extend(head);

    let mut i = 1;
    while i <= n {
        if let Some((to, replacement)) = replacements.get(&i) {
            result.extend(replacement.clone());
            i = *to + 1;
            continue;
        }

        if let Some(inserted) = before.get(&i) {
            result.extend(inserted.clone());
        }
        result.push(lines[i - 1].to_string());
        if let Some(inserted) = after.get(&i) {
            result.extend(inserted.clone());
        }
        i += 1;
    }

    result.extend(tail);
    Ok(result.join("\n"))
}

fn validate_ops(ops: &[EditOp], n: usize) -> Result<(), String> {
    let mut ranges = Vec::new();
    let mut anchors = Vec::new();

    for op in ops {
        match op {
            EditOp::Swap { from, to, .. } | EditOp::Delete { from, to } => {
                validate_range(*from, *to, n)?;
                ranges.push((*from, *to));
            }
            EditOp::InsertBefore { line, lines } | EditOp::InsertAfter { line, lines } => {
                validate_anchor(*line, n)?;
                validate_insert_lines(lines)?;
                anchors.push(*line);
            }
            EditOp::InsertHead { lines } | EditOp::InsertTail { lines } => {
                validate_insert_lines(lines)?;
            }
        }
    }

    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        let (_, prev_to) = pair[0];
        let (next_from, _) = pair[1];
        if next_from <= prev_to {
            return Err(format!(
                "range {next_from} is already targeted by another edit"
            ));
        }
    }

    for anchor in anchors {
        if ranges
            .iter()
            .any(|(from, to)| *from <= anchor && anchor <= *to)
        {
            return Err(format!(
                "anchor line {anchor} is already targeted by another edit"
            ));
        }
    }

    Ok(())
}

fn validate_range(from: usize, to: usize, n: usize) -> Result<(), String> {
    if from > to {
        return Err(format!("range {from}..={to} ends before it starts"));
    }
    if from == 0 || to == 0 || from > n || to > n {
        return Err(format!("range {from}..={to} is out of range 1..={n}"));
    }
    Ok(())
}

fn validate_anchor(line: usize, n: usize) -> Result<(), String> {
    if line == 0 || line > n {
        return Err(format!("line {line} is out of range 1..={n}"));
    }
    Ok(())
}

fn validate_insert_lines(lines: &[String]) -> Result<(), String> {
    if lines.is_empty() {
        return Err("insert needs at least one line".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> &'static str {
        "line one\nline two\nline three"
    }

    #[test]
    fn swaps_single_line() {
        let edited = apply_line_edits(
            content(),
            &[EditOp::Swap {
                from: 2,
                to: 2,
                lines: vec!["LINE TWO".to_string()],
            }],
        )
        .unwrap();
        assert_eq!(edited, "line one\nLINE TWO\nline three");
    }

    #[test]
    fn deletes_single_line() {
        let edited = apply_line_edits(content(), &[EditOp::Delete { from: 1, to: 1 }]).unwrap();
        assert_eq!(edited, "line two\nline three");
    }

    #[test]
    fn inserts_after_tail_line() {
        let edited = apply_line_edits(
            content(),
            &[EditOp::InsertAfter {
                line: 3,
                lines: vec!["line four".to_string()],
            }],
        )
        .unwrap();
        assert_eq!(edited, "line one\nline two\nline three\nline four");
    }

    #[test]
    fn inserts_before_head_line() {
        let edited = apply_line_edits(
            content(),
            &[EditOp::InsertBefore {
                line: 1,
                lines: vec!["line zero".to_string()],
            }],
        )
        .unwrap();
        assert_eq!(edited, "line zero\nline one\nline two\nline three");
    }

    #[test]
    fn inserts_head_and_tail() {
        let edited = apply_line_edits(
            content(),
            &[
                EditOp::InsertHead {
                    lines: vec!["top".to_string()],
                },
                EditOp::InsertTail {
                    lines: vec!["bottom".to_string()],
                },
            ],
        )
        .unwrap();
        assert_eq!(edited, "top\nline one\nline two\nline three\nbottom");
    }

    #[test]
    fn applies_multiple_ops_against_original_indices() {
        let edited = apply_line_edits(
            content(),
            &[
                EditOp::Swap {
                    from: 1,
                    to: 1,
                    lines: vec!["A".to_string()],
                },
                EditOp::InsertAfter {
                    line: 2,
                    lines: vec!["X".to_string()],
                },
                EditOp::Delete { from: 3, to: 3 },
            ],
        )
        .unwrap();
        assert_eq!(edited, "A\nline two\nX");
    }

    #[test]
    fn expands_replacement_range() {
        let edited = apply_line_edits(
            content(),
            &[EditOp::Swap {
                from: 1,
                to: 2,
                lines: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            }],
        )
        .unwrap();
        assert_eq!(edited, "A\nB\nC\nline three");
    }

    #[test]
    fn rejects_overlapping_ranges() {
        let result = apply_line_edits(
            content(),
            &[
                EditOp::Swap {
                    from: 1,
                    to: 2,
                    lines: vec!["A".to_string()],
                },
                EditOp::Swap {
                    from: 2,
                    to: 3,
                    lines: vec!["B".to_string()],
                },
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_out_of_range() {
        let result = apply_line_edits(
            content(),
            &[EditOp::Swap {
                from: 5,
                to: 5,
                lines: vec!["x".to_string()],
            }],
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_range_ending_before_start() {
        let result = apply_line_edits(content(), &[EditOp::Delete { from: 3, to: 1 }]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_empty_ops() {
        let result = apply_line_edits(content(), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn compute_tag_is_stable_and_changes_with_content() {
        assert_eq!(compute_tag("abc"), compute_tag("abc"));
        assert_ne!(compute_tag("abc"), compute_tag("abd"));
    }
}
