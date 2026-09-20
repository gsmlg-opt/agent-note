use agcodex_apply_patch::{parse_patch, Hunk};
use aho_corasick::AhoCorasick;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const UNIQUE_CONTEXT_ERROR: &str =
    "context must match exactly one location in the original content";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ContentUpdate {
    Replace(String),
    ApplyPatch(ContentPatch),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPatch {
    pub apply_patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentPatchError {
    pub message: String,
}

impl std::fmt::Display for ContentPatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ContentPatchError {}

pub fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn append_encoded_line(encoded: &mut Vec<u8>, line: &str) {
    encoded.extend_from_slice(&(line.len() as u64).to_be_bytes());
    encoded.extend_from_slice(line.as_bytes());
}

fn unique_pattern_locations(
    normalized_original: &[&str],
    patterns: &[Vec<u8>],
) -> Result<Vec<usize>, ContentPatchError> {
    let fail = || ContentPatchError {
        message: UNIQUE_CONTEXT_ERROR.to_owned(),
    };
    let mut encoded_original = Vec::new();
    let mut boundary_lines = HashMap::with_capacity(normalized_original.len().saturating_add(1));
    boundary_lines.insert(0, 0);
    for (line_index, line) in normalized_original.iter().enumerate() {
        append_encoded_line(&mut encoded_original, line);
        boundary_lines.insert(encoded_original.len(), line_index + 1);
    }

    let matcher = AhoCorasick::new(patterns).map_err(|_| fail())?;
    let mut locations = vec![None; patterns.len()];
    // Expected: O(encoded source + total encoded patterns + emitted matches).
    for found in matcher.find_overlapping_iter(&encoded_original) {
        let Some(&start_line) = boundary_lines.get(&found.start()) else {
            continue;
        };
        if !boundary_lines.contains_key(&found.end()) {
            continue;
        }
        let location = &mut locations[found.pattern().as_usize()];
        if location.replace(start_line).is_some() {
            return Err(fail());
        }
    }

    locations
        .into_iter()
        .map(|location| location.ok_or_else(&fail))
        .collect()
}

pub fn apply_content_patch(content: &str, patch: &str) -> Result<String, ContentPatchError> {
    let fail = |message: &str| ContentPatchError {
        message: message.to_owned(),
    };
    let patch_lines: Vec<_> = patch.lines().collect();
    if patch_lines.first() != Some(&"@@")
        || patch_lines
            .iter()
            .any(|line| *line != "@@" && !line.starts_with([' ', '+', '-']))
    {
        return Err(fail(
            "expected @@ hunks with context, - deletion, and + insertion lines; file directives are not supported",
        ));
    }
    let hunk_prefixes = patch_lines
        .iter()
        .skip(1)
        .fold(vec![Vec::new()], |mut hunks, line| {
            if *line == "@@" {
                hunks.push(Vec::new());
            } else {
                hunks
                    .last_mut()
                    .expect("a hunk exists after the initial @@")
                    .push(line.chars().next().expect("validated non-empty patch line"));
            }
            hunks
        });

    let body = patch.strip_suffix('\n').unwrap_or(patch);
    let envelope = format!("*** Begin Patch\n*** Update File: content.md\n{body}\n*** End Patch");
    let parsed = parse_patch(&envelope).map_err(|error| ContentPatchError {
        message: error.to_string(),
    })?;
    let [Hunk::UpdateFile {
        chunks,
        move_path: None,
        ..
    }] = parsed.hunks.as_slice()
    else {
        return Err(fail("expected a single content update"));
    };
    if chunks.is_empty() {
        return Err(fail("at least one hunk is required"));
    }

    let original: Vec<_> = content.split_inclusive('\n').collect();
    let normalized_original: Vec<_> = original
        .iter()
        .map(|raw| match raw.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => *raw,
        })
        .collect();

    if chunks.len() != hunk_prefixes.len() {
        return Err(fail("expected plain @@ hunks"));
    }
    if chunks.iter().any(|chunk| chunk.old_lines.is_empty()) {
        return Err(fail("each hunk needs existing context or deletion lines"));
    }

    let encoded_patterns: Vec<_> = chunks
        .iter()
        .map(|chunk| {
            let mut encoded = Vec::new();
            for line in &chunk.old_lines {
                append_encoded_line(&mut encoded, line);
            }
            encoded
        })
        .collect();
    let locations = unique_pattern_locations(&normalized_original, &encoded_patterns)?;
    let mut cursor = 0;
    let mut result = String::new();
    let mut removed_unterminated_eof = false;

    for ((chunk, prefixes), start) in chunks.iter().zip(hunk_prefixes).zip(locations) {
        if start < cursor {
            return Err(fail("hunks must be ordered and must not overlap"));
        }

        result.extend(original[cursor..start].iter().copied());
        let end = start + chunk.old_lines.len();
        if chunk.old_lines == chunk.new_lines {
            result.extend(original[start..end].iter().copied());
            cursor = end;
            continue;
        }

        let newline = if original[start].ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut old_line_index = 0;
        let mut new_line_index = 0;
        let mut output_after = vec![false; prefixes.len()];
        let mut has_output = false;
        for (index, prefix) in prefixes.iter().enumerate().rev() {
            output_after[index] = has_output;
            has_output |= matches!(prefix, ' ' | '+');
        }
        for (index, prefix) in prefixes.iter().enumerate() {
            let has_output_after = output_after[index];
            match prefix {
                ' ' => {
                    let raw = original[start + old_line_index];
                    result.push_str(raw);
                    if !raw.ends_with('\n') && has_output_after {
                        result.push_str(newline);
                    }
                    old_line_index += 1;
                    new_line_index += 1;
                }
                '-' => old_line_index += 1,
                '+' => {
                    result.push_str(&chunk.new_lines[new_line_index]);
                    new_line_index += 1;
                    if end != original.len() || content.ends_with('\n') || has_output_after {
                        result.push_str(newline);
                    }
                }
                _ => unreachable!("patch prefixes were validated before parsing"),
            }
        }
        removed_unterminated_eof =
            end == original.len() && !content.ends_with('\n') && prefixes.last() == Some(&'-');
        cursor = end;
    }
    result.extend(original[cursor..].iter().copied());
    if removed_unterminated_eof {
        if result.ends_with("\r\n") {
            result.truncate(result.len() - 2);
        } else if result.ends_with(['\n', '\r']) {
            result.pop();
        }
    }
    Ok(result)
}
