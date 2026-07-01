use note_pipelines::{save_note, search_notes, Context, SaveNoteInput as PipelineSaveNoteInput};
use serde::{Deserialize, Serialize};

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
