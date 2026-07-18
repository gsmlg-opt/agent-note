use std::path::{Component, Path, PathBuf};

pub(crate) fn canonical_attachment_path(
    root: &Path,
    attachment_path: &str,
) -> anyhow::Result<PathBuf> {
    let bytes = attachment_path.as_bytes();
    let has_windows_drive_prefix =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if has_windows_drive_prefix || attachment_path.starts_with(r"\\") {
        anyhow::bail!("invalid attachment path");
    }

    let mut path = root.to_path_buf();
    for component in Path::new(attachment_path).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => anyhow::bail!("invalid attachment path"),
        }
    }
    Ok(path)
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
