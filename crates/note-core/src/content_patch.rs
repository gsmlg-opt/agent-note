use agcodex_apply_patch::{parse_patch, Hunk};
use serde::{Deserialize, Serialize};

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
    let text = |raw: &str| match raw.strip_suffix('\n') {
        Some(line) => line.strip_suffix('\r').unwrap_or(line).to_owned(),
        None => raw.to_owned(),
    };
    let mut cursor = 0;
    let mut result = String::new();
    let mut removed_unterminated_eof = false;

    for chunk in chunks {
        if chunk.old_lines.is_empty() {
            return Err(fail("each hunk needs existing context or deletion lines"));
        }
        let matches: Vec<_> = original
            .windows(chunk.old_lines.len())
            .enumerate()
            .filter(|(_, window)| {
                window
                    .iter()
                    .map(|line| text(line))
                    .eq(chunk.old_lines.iter().cloned())
            })
            .map(|(start, _)| start)
            .collect();
        let [start] = matches.as_slice() else {
            return Err(fail(
                "context must match exactly one location in the original content",
            ));
        };
        if *start < cursor {
            return Err(fail("hunks must be ordered and must not overlap"));
        }

        result.extend(original[cursor..*start].iter().copied());
        let end = start + chunk.old_lines.len();
        if chunk.old_lines == chunk.new_lines {
            result.extend(original[*start..end].iter().copied());
            cursor = end;
            continue;
        }

        let newline = if original[*start].ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        for (index, line) in chunk.new_lines.iter().enumerate() {
            result.push_str(line);
            if end != original.len()
                || content.ends_with('\n')
                || index + 1 != chunk.new_lines.len()
            {
                result.push_str(newline);
            }
        }
        removed_unterminated_eof =
            end == original.len() && !content.ends_with('\n') && chunk.new_lines.is_empty();
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
