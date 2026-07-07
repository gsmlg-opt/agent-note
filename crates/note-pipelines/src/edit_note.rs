use crate::{get_note, update_note, Context, SaveNoteInput};
use note_core::Note;

pub use note_core::{compute_tag, EditOp};

pub async fn edit_note(
    ctx: &Context,
    id: &str,
    expected_tag: &str,
    ops: &[EditOp],
) -> anyhow::Result<Option<Note>> {
    let note = match get_note(ctx, id).await? {
        Some(note) => note,
        None => return Ok(None),
    };

    let current = compute_tag(&note.content);
    if expected_tag != current {
        anyhow::bail!(
            "stale tag: note {id} changed since it was read (current tag {current}); re-read with read_note_lines before editing"
        );
    }

    let new_content =
        note_core::apply_line_edits(&note.content, ops).map_err(|e| anyhow::anyhow!(e))?;
    let labels: Vec<(String, String)> = note
        .labels
        .iter()
        .map(|label| (label.key.clone(), label.value.clone()))
        .collect();

    update_note(
        ctx,
        id,
        SaveNoteInput {
            title: note.title.clone(),
            content: new_content,
            labels,
        },
    )
    .await
}
