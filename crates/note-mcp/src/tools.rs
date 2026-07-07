use note_pipelines::{
    delete_note, get_note, list_notes, save_note, search_notes, update_note, Context,
    ListNotesParams, SaveNoteInput as PipelineSaveNoteInput,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct LabelData {
    pub key: String,
    pub value: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct NoteData {
    pub id: String,
    pub title: String,
    pub content: String,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct SaveNoteToolInput {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
pub struct SaveNoteToolOutput {
    pub id: String,
}

pub async fn save_note_tool(
    ctx: &Context,
    input: SaveNoteToolInput,
) -> anyhow::Result<SaveNoteToolOutput> {
    let note = save_note(
        ctx,
        PipelineSaveNoteInput {
            title: input.title,
            content: input.content,
            labels: input.labels,
        },
    )
    .await?;
    Ok(SaveNoteToolOutput { id: note.id })
}

pub async fn get_note_tool(ctx: &Context, id: &str) -> anyhow::Result<Option<NoteData>> {
    let note = get_note(ctx, id).await?;
    Ok(note.map(|note| NoteData {
        id: note.id,
        title: note.title,
        content: note.content,
        labels: note
            .labels
            .into_iter()
            .map(|label| LabelData {
                key: label.key,
                value: label.value,
                description: label.description,
            })
            .collect(),
        created_at: note.created_at,
        updated_at: note.updated_at,
    }))
}

#[derive(Debug, Serialize)]
pub struct NoteLine {
    pub n: usize,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct NoteLinesData {
    pub id: String,
    pub tag: String,
    pub lines: Vec<NoteLine>,
}

pub async fn read_note_lines_tool(
    ctx: &Context,
    id: &str,
) -> anyhow::Result<Option<NoteLinesData>> {
    let note = get_note(ctx, id).await?;
    Ok(note.map(|note| NoteLinesData {
        id: note.id,
        tag: note_pipelines::compute_tag(&note.content),
        lines: note
            .content
            .split('\n')
            .enumerate()
            .map(|(idx, text)| NoteLine {
                n: idx + 1,
                text: text.to_string(),
            })
            .collect(),
    }))
}

pub async fn edit_note_tool(
    ctx: &Context,
    id: &str,
    tag: &str,
    ops: Vec<note_pipelines::EditOp>,
) -> anyhow::Result<Option<NoteLinesData>> {
    let note = note_pipelines::edit_note(ctx, id, tag, &ops).await?;
    Ok(note.map(|note| NoteLinesData {
        id: note.id,
        tag: note_pipelines::compute_tag(&note.content),
        lines: note
            .content
            .split('\n')
            .enumerate()
            .map(|(idx, text)| NoteLine {
                n: idx + 1,
                text: text.to_string(),
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateNoteToolInput {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

pub async fn update_note_tool(
    ctx: &Context,
    input: UpdateNoteToolInput,
) -> anyhow::Result<Option<NoteData>> {
    let note = update_note(
        ctx,
        &input.id,
        PipelineSaveNoteInput {
            title: input.title,
            content: input.content,
            labels: input.labels,
        },
    )
    .await?;
    Ok(note.map(|note| NoteData {
        id: note.id,
        title: note.title,
        content: note.content,
        labels: note
            .labels
            .into_iter()
            .map(|label| LabelData {
                key: label.key,
                value: label.value,
                description: label.description,
            })
            .collect(),
        created_at: note.created_at,
        updated_at: note.updated_at,
    }))
}

pub async fn delete_note_tool(ctx: &Context, id: &str) -> anyhow::Result<bool> {
    delete_note(ctx, id).await
}

pub async fn list_notes_tool(
    ctx: &Context,
    limit: Option<i64>,
    offset: Option<i64>,
    label: Option<String>,
) -> anyhow::Result<Vec<NoteData>> {
    let notes = list_notes(
        ctx,
        ListNotesParams {
            limit,
            offset,
            label,
        },
    )
    .await?;
    Ok(notes
        .into_iter()
        .map(|note| NoteData {
            id: note.id,
            title: note.title,
            content: note.content,
            labels: note
                .labels
                .into_iter()
                .map(|label| LabelData {
                    key: label.key,
                    value: label.value,
                    description: label.description,
                })
                .collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct SemanticSearchToolInput {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct SemanticSearchToolResult {
    pub id: String,
    pub title: String,
    pub score: f32,
}

pub async fn semantic_search_tool(
    ctx: &Context,
    input: SemanticSearchToolInput,
) -> anyhow::Result<Vec<SemanticSearchToolResult>> {
    let results = search_notes(ctx, &input.query, input.limit).await?;
    Ok(results
        .into_iter()
        .map(|r| SemanticSearchToolResult {
            id: r.note.id,
            title: r.note.title,
            score: r.score,
        })
        .collect())
}
