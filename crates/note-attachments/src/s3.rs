const S3_PAGE_SIZE: i32 = 1_000;
const S3_DELETE_BATCH_SIZE: usize = 1_000;
const S3_MAX_ATTEMPTS: u32 = 4;

fn normalize_prefix(prefix: &str) -> anyhow::Result<String> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    if prefix.contains('\\')
        || prefix
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        anyhow::bail!("invalid S3 attachment prefix");
    }
    Ok(prefix.to_string())
}

fn validate_note_id(note_id: &str) -> anyhow::Result<()> {
    if note_id.is_empty()
        || note_id == "."
        || note_id == ".."
        || note_id == ".staging"
        || note_id.contains('/')
        || note_id.contains('\\')
    {
        anyhow::bail!("invalid note id for attachment key");
    }
    Ok(())
}

fn join_key(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{prefix}/{suffix}")
    }
}

fn final_prefix(prefix: &str, note_id: &str) -> anyhow::Result<String> {
    validate_note_id(note_id)?;
    let prefix = normalize_prefix(prefix)?;
    Ok(format!("{}/", join_key(&prefix, note_id)))
}

fn final_key(prefix: &str, note_id: &str, path: &str) -> anyhow::Result<String> {
    let relative = crate::path::canonical_relative_path(path)?;
    Ok(format!("{}{relative}", final_prefix(prefix, note_id)?))
}

fn staging_prefix(prefix: &str, note_id: &str, stage_id: &str) -> anyhow::Result<String> {
    validate_note_id(note_id)?;
    validate_note_id(stage_id)?;
    let prefix = normalize_prefix(prefix)?;
    Ok(format!(
        "{}/",
        join_key(&prefix, &format!(".staging/{note_id}/{stage_id}"))
    ))
}

fn copy_source(bucket: &str, key: &str) -> String {
    std::iter::once(bucket)
        .chain(key.split('/'))
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_limits_match_s3_api_boundaries() {
        assert_eq!(S3_PAGE_SIZE, 1_000);
        assert_eq!(S3_DELETE_BATCH_SIZE, 1_000);
        assert_eq!(S3_MAX_ATTEMPTS, 4);
    }

    #[test]
    fn key_composition_is_canonical_with_or_without_a_configured_prefix() {
        assert_eq!(
            final_key("attachments", "note-1", "./docs/read me.txt").unwrap(),
            "attachments/note-1/docs/read me.txt"
        );
        assert_eq!(
            final_key("", "note-1", "file.txt").unwrap(),
            "note-1/file.txt"
        );
        assert_eq!(
            staging_prefix("attachments/", "note-1", "stage-1").unwrap(),
            "attachments/.staging/note-1/stage-1/"
        );
    }

    #[test]
    fn prefix_and_note_id_cannot_escape_the_owned_namespace() {
        for prefix in [
            "../attachments",
            "safe//unsafe",
            r"safe\unsafe",
            "safe/./unsafe",
        ] {
            assert!(normalize_prefix(prefix).is_err(), "{prefix:?} must fail");
        }
        for note_id in [
            "",
            ".",
            "..",
            "../note",
            "note/other",
            r"note\other",
            ".staging",
        ] {
            assert!(final_prefix("", note_id).is_err(), "{note_id:?} must fail");
        }
    }

    #[test]
    fn stage_id_cannot_escape_the_owned_namespace() {
        for stage_id in [
            "",
            ".",
            "..",
            "../stage",
            "stage/other",
            r"stage\other",
            ".staging",
        ] {
            assert!(
                staging_prefix("", "note-1", stage_id).is_err(),
                "{stage_id:?} must fail"
            );
        }
    }

    #[test]
    fn copy_source_percent_encodes_segments_but_preserves_path_separators() {
        assert_eq!(
            copy_source("agent-note", "attachments/note-1/read me.txt"),
            "agent-note/attachments/note-1/read%20me.txt"
        );
        assert_eq!(
            copy_source("agent note", "attachments/note+1/100%.txt"),
            "agent%20note/attachments/note%2B1/100%25.txt"
        );
    }
}
