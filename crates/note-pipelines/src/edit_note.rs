use crate::{get_note_metadata, update_note_fields, Context, UpdateNoteFieldsInput};
use note_core::Note;

pub use note_core::{compute_tag, EditOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLine {
    pub n: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLines {
    pub id: String,
    pub revision: i64,
    pub tag: String,
    pub lines: Vec<NoteLine>,
}

pub async fn read_note_lines(ctx: &Context, id: &str) -> anyhow::Result<Option<NoteLines>> {
    Ok(get_note_metadata(ctx, id).await?.map(|note| NoteLines {
        id: note.id,
        revision: note.revision,
        tag: compute_tag(&note.content),
        lines: note
            .content
            .split('\n')
            .enumerate()
            .map(|(index, text)| NoteLine {
                n: index + 1,
                text: text.to_string(),
            })
            .collect(),
    }))
}

pub async fn edit_note(
    ctx: &Context,
    id: &str,
    expected_revision: i64,
    expected_tag: &str,
    ops: &[EditOp],
) -> anyhow::Result<Option<Note>> {
    let note = match get_note_metadata(ctx, id).await? {
        Some(note) => note,
        None => return Err(crate::NoteMutationError::NotFound(id.to_string()).into()),
    };

    let current = compute_tag(&note.content);
    if expected_tag != current {
        return Err(crate::NoteMutationError::StaleContentTag {
            note_id: id.to_string(),
            expected_tag: expected_tag.to_string(),
            current_tag: current,
        }
        .into());
    }

    let new_content =
        note_core::apply_line_edits(&note.content, ops).map_err(|e| anyhow::anyhow!(e))?;
    let labels: Vec<(String, String)> = note
        .labels
        .iter()
        .map(|label| (label.key.clone(), label.value.clone()))
        .collect();

    update_note_fields(
        ctx,
        id,
        UpdateNoteFieldsInput {
            expected_revision,
            title: note.title.clone(),
            content: new_content,
            labels,
        },
    )
    .await
}
