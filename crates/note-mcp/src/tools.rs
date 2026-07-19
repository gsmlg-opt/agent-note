use note_core::{Label, Note, NoteAttachment, NoteListItem};
use note_pipelines::{
    delete_note, delete_note_attachment as pipeline_delete_note_attachment,
    get_note_attachment_by_id, get_note_metadata, list_note_summaries,
    put_note_attachment as pipeline_put_note_attachment, save_note, search_notes_filtered,
    update_note_fields, Context, ListNotesParams, SaveNoteInput as PipelineSaveNoteInput,
    UpdateNoteFieldsInput,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabelData {
    pub key: String,
    pub value: String,
    pub description: String,
    pub value_type: String,
}

impl From<Label> for LabelData {
    fn from(label: Label) -> Self {
        Self {
            key: label.key,
            value: label.value,
            description: label.description,
            value_type: label.value_type.as_str().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttachmentMetadataData {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
}

impl From<NoteAttachment> for AttachmentMetadataData {
    fn from(attachment: NoteAttachment) -> Self {
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NoteSummaryData {
    pub id: String,
    pub title: String,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<NoteListItem> for NoteSummaryData {
    fn from(note: NoteListItem) -> Self {
        Self {
            id: note.id,
            title: note.title,
            labels: note.labels.into_iter().map(Into::into).collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NoteDetailData {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<AttachmentMetadataData>,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Note> for NoteDetailData {
    fn from(note: Note) -> Self {
        Self {
            id: note.id,
            title: note.title,
            content: note.content,
            attachments: note.attachments.into_iter().map(Into::into).collect(),
            labels: note.labels.into_iter().map(Into::into).collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
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
            attachments: vec![],
            labels: input.labels,
        },
    )
    .await?;
    Ok(SaveNoteToolOutput { id: note.id })
}

pub async fn get_note_tool(ctx: &Context, id: &str) -> anyhow::Result<Option<NoteDetailData>> {
    Ok(get_note_metadata(ctx, id).await?.map(Into::into))
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
    let note = get_note_metadata(ctx, id).await?;
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
) -> anyhow::Result<Option<NoteDetailData>> {
    Ok(update_note_fields(
        ctx,
        &input.id,
        UpdateNoteFieldsInput {
            title: input.title,
            content: input.content,
            labels: input.labels,
        },
    )
    .await?
    .map(Into::into))
}

pub async fn delete_note_tool(ctx: &Context, id: &str) -> anyhow::Result<bool> {
    delete_note(ctx, id).await
}

pub async fn list_notes_tool(
    ctx: &Context,
    limit: Option<i64>,
    offset: Option<i64>,
    label: Option<String>,
) -> anyhow::Result<Vec<NoteSummaryData>> {
    Ok(list_note_summaries(
        ctx,
        ListNotesParams {
            limit,
            offset,
            label,
        },
    )
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[derive(Debug, Deserialize)]
pub struct SemanticSearchToolInput {
    pub query: String,
    pub limit: usize,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SemanticSearchToolResult {
    pub id: String,
    pub title: String,
    pub score: f32,
    pub labels: Vec<LabelData>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub async fn semantic_search_tool(
    ctx: &Context,
    input: SemanticSearchToolInput,
) -> anyhow::Result<Vec<SemanticSearchToolResult>> {
    let results = search_notes_filtered(ctx, &input.query, input.limit, input.label).await?;
    Ok(results
        .into_iter()
        .map(|result| SemanticSearchToolResult {
            id: result.note.id,
            title: result.note.title,
            score: result.score,
            labels: result.note.labels.into_iter().map(Into::into).collect(),
            created_at: result.note.created_at,
            updated_at: result.note.updated_at,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct PutNoteAttachmentToolInput {
    pub note_id: String,
    pub attachment_id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct PutNoteAttachmentToolOutput {
    pub created: bool,
    pub attachment: AttachmentMetadataData,
}

pub async fn put_note_attachment_tool(
    ctx: &Context,
    input: PutNoteAttachmentToolInput,
) -> anyhow::Result<PutNoteAttachmentToolOutput> {
    let result = pipeline_put_note_attachment(
        ctx,
        &input.note_id,
        NoteAttachment {
            id: input.attachment_id,
            path: input.path,
            mime: input.mime,
            description: input.description,
            content: input.content,
        },
    )
    .await?;
    Ok(PutNoteAttachmentToolOutput {
        created: result.created,
        attachment: result.attachment.into(),
    })
}

#[derive(Debug, Deserialize)]
pub struct GetNoteAttachmentContentToolInput {
    pub note_id: String,
    pub attachment_id: String,
}

#[derive(Debug, Serialize)]
pub struct AttachmentContentData {
    pub attachment: AttachmentMetadataData,
    pub content: Vec<u8>,
}

pub async fn get_note_attachment_content_tool(
    ctx: &Context,
    input: GetNoteAttachmentContentToolInput,
) -> anyhow::Result<Option<AttachmentContentData>> {
    Ok(
        get_note_attachment_by_id(ctx, &input.note_id, &input.attachment_id)
            .await?
            .map(|attachment| {
                let NoteAttachment {
                    id,
                    path,
                    mime,
                    description,
                    content,
                } = attachment;
                AttachmentContentData {
                    attachment: AttachmentMetadataData {
                        id,
                        path,
                        mime,
                        description,
                    },
                    content,
                }
            }),
    )
}

pub async fn delete_note_attachment_tool(
    ctx: &Context,
    note_id: &str,
    attachment_id: &str,
) -> anyhow::Result<bool> {
    pipeline_delete_note_attachment(ctx, note_id, attachment_id).await
}
