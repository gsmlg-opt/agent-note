use std::path::{Component, Path, PathBuf};

pub(crate) fn canonical_relative_path(value: &str) -> anyhow::Result<String> {
    if value.is_empty() || value.contains('\\') {
        anyhow::bail!("invalid attachment path");
    }

    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("attachment path is not UTF-8"))?;
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("invalid attachment path");
            }
        }
    }

    if parts.is_empty() {
        anyhow::bail!("attachment path is empty");
    }
    Ok(parts.join("/"))
}

pub(crate) fn note_directory(root: &Path, note_id: &str) -> anyhow::Result<PathBuf> {
    let mut components = Path::new(note_id).components();
    let Some(Component::Normal(note_id)) = components.next() else {
        anyhow::bail!("invalid note id");
    };
    if components.next().is_some() {
        anyhow::bail!("invalid note id");
    }
    Ok(root.join(note_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_relative_paths_drop_only_current_directory_components() {
        assert_eq!(
            canonical_relative_path("./docs/read me.txt").unwrap(),
            "docs/read me.txt"
        );
        assert_eq!(
            canonical_relative_path("docs/./read.md").unwrap(),
            "docs/read.md"
        );
    }

    #[test]
    fn canonical_relative_paths_reject_unsafe_or_empty_inputs() {
        for value in [
            "",
            ".",
            "../secret",
            "/absolute",
            r"C:\secret",
            r"dir\secret",
        ] {
            assert!(
                canonical_relative_path(value).is_err(),
                "{value:?} must fail"
            );
        }
    }
}
