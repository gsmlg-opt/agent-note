//! MCP server exposing note tools over stdio and Streamable HTTP transports.
//!
//! Transport-specific request/response marshalling lives here. The handlers
//! delegate to transport-independent wrappers in [`crate::tools`].

use std::{borrow::Cow, sync::Arc};

use note_pipelines::{AttachmentMutationError, Context};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::io::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::{
    delete_note_attachment_tool, delete_note_tool, edit_note_tool,
    get_note_attachment_content_tool, get_note_tool, list_notes_tool, put_note_attachment_tool,
    read_note_lines_tool, save_note_tool, semantic_search_tool, update_note_tool,
    AttachmentContentData, AttachmentMetadataData, GetNoteAttachmentContentToolInput, LabelData,
    NoteDetailData, NoteLine, NoteLinesData, NoteSummaryData, PutNoteAttachmentToolInput,
    PutNoteAttachmentToolOutput, SaveNoteToolInput, SaveNoteToolOutput, SemanticSearchToolInput,
    SemanticSearchToolResult, UpdateNoteToolInput,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveNoteRequest {
    /// Note title.
    pub title: String,
    /// Note body, formatted as Markdown.
    pub content: String,
    /// Existing label keys to attach, as `(key, value)` pairs.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SaveNoteResponse {
    /// Id of the newly saved note.
    pub id: String,
}

impl From<SaveNoteRequest> for SaveNoteToolInput {
    fn from(request: SaveNoteRequest) -> Self {
        Self {
            title: request.title,
            content: request.content,
            labels: request.labels,
        }
    }
}

impl From<SaveNoteToolOutput> for SaveNoteResponse {
    fn from(output: SaveNoteToolOutput) -> Self {
        Self { id: output.id }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetNoteRequest {
    /// Note id.
    pub id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct LabelSchema {
    /// Label key.
    pub key: String,
    /// Label value attached to the note.
    pub value: String,
    /// Label key description.
    pub description: String,
    /// Label value type used for comparisons.
    pub value_type: String,
}

impl From<LabelData> for LabelSchema {
    fn from(label: LabelData) -> Self {
        Self {
            key: label.key,
            value: label.value,
            description: label.description,
            value_type: label.value_type,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AttachmentMetadataSchema {
    /// Unique attachment id within the note.
    pub id: String,
    /// Relative path that note Markdown can reference.
    pub path: String,
    /// MIME type of the attachment content.
    pub mime: String,
    /// Human-readable attachment description.
    pub description: String,
}

impl From<AttachmentMetadataData> for AttachmentMetadataSchema {
    fn from(attachment: AttachmentMetadataData) -> Self {
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteSummaryResponse {
    /// Note id.
    pub id: String,
    /// Note title.
    pub title: String,
    /// Labels attached to the note.
    pub labels: Vec<LabelSchema>,
    /// Unix timestamp when the note was created.
    pub created_at: i64,
    /// Unix timestamp when the note was last updated.
    pub updated_at: i64,
}

impl From<NoteSummaryData> for NoteSummaryResponse {
    fn from(note: NoteSummaryData) -> Self {
        Self {
            id: note.id,
            title: note.title,
            labels: note.labels.into_iter().map(Into::into).collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteDetailResponse {
    /// Note id.
    pub id: String,
    /// Note title.
    pub title: String,
    /// Note body, formatted as Markdown.
    pub content: String,
    /// Attachment metadata. Content is fetched with `get_note_attachment_content`.
    pub attachments: Vec<AttachmentMetadataSchema>,
    /// Labels attached to the note.
    pub labels: Vec<LabelSchema>,
    /// Unix timestamp when the note was created.
    pub created_at: i64,
    /// Unix timestamp when the note was last updated.
    pub updated_at: i64,
}

impl From<NoteDetailData> for NoteDetailResponse {
    fn from(note: NoteDetailData) -> Self {
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadNoteLinesRequest {
    /// Note id.
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteLineSchema {
    /// 1-indexed line number.
    pub n: usize,
    /// Line text without the line separator.
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteLinesResponse {
    /// Note id.
    pub id: String,
    /// Content hash tag used to guard subsequent `edit_note` calls.
    pub tag: String,
    /// Numbered note body lines.
    pub lines: Vec<NoteLineSchema>,
}

impl From<NoteLine> for NoteLineSchema {
    fn from(line: NoteLine) -> Self {
        Self {
            n: line.n,
            text: line.text,
        }
    }
}

impl From<NoteLinesData> for NoteLinesResponse {
    fn from(note: NoteLinesData) -> Self {
        Self {
            id: note.id,
            tag: note.tag,
            lines: note.lines.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOpSchema {
    /// Replace an original line range with zero or more lines.
    Swap {
        from: usize,
        to: usize,
        lines: Vec<String>,
    },
    /// Delete an original line range.
    Delete { from: usize, to: usize },
    /// Insert lines before an original line.
    InsertBefore { line: usize, lines: Vec<String> },
    /// Insert lines after an original line.
    InsertAfter { line: usize, lines: Vec<String> },
    /// Insert lines at the start of the note body.
    InsertHead { lines: Vec<String> },
    /// Insert lines at the end of the note body.
    InsertTail { lines: Vec<String> },
}

impl From<EditOpSchema> for note_pipelines::EditOp {
    fn from(op: EditOpSchema) -> Self {
        match op {
            EditOpSchema::Swap { from, to, lines } => Self::Swap { from, to, lines },
            EditOpSchema::Delete { from, to } => Self::Delete { from, to },
            EditOpSchema::InsertBefore { line, lines } => Self::InsertBefore { line, lines },
            EditOpSchema::InsertAfter { line, lines } => Self::InsertAfter { line, lines },
            EditOpSchema::InsertHead { lines } => Self::InsertHead { lines },
            EditOpSchema::InsertTail { lines } => Self::InsertTail { lines },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditNoteRequest {
    /// Note id.
    pub id: String,
    /// Tag from `read_note_lines`.
    pub tag: String,
    /// Operations anchored to the original line numbers.
    pub edits: Vec<EditOpSchema>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateNoteRequest {
    /// Note id.
    pub id: String,
    /// New note title.
    pub title: String,
    /// New note body, formatted as Markdown.
    pub content: String,
    /// Existing label keys to attach, as `(key, value)` pairs.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

impl From<UpdateNoteRequest> for UpdateNoteToolInput {
    fn from(request: UpdateNoteRequest) -> Self {
        Self {
            id: request.id,
            title: request.title,
            content: request.content,
            labels: request.labels,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteNoteRequest {
    /// Note id.
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DeleteNoteResponse {
    /// Whether a note was deleted.
    pub deleted: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListNotesRequest {
    /// Max notes to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of notes to skip.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Label selector, with `&`-separated terms ANDed.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteListResponse {
    /// Saved note summaries.
    pub notes: Vec<NoteSummaryResponse>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticSearchRequest {
    /// Natural-language query.
    pub query: String,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Optional label selector.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SemanticSearchHit {
    /// Id of the matching note.
    pub id: String,
    /// Title of the matching note.
    pub title: String,
    /// Relevance score.
    pub score: f32,
    /// Labels attached to the matching note.
    pub labels: Vec<LabelSchema>,
    /// Unix timestamp when the note was created.
    pub created_at: i64,
    /// Unix timestamp when the note was last updated.
    pub updated_at: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SemanticSearchResponse {
    /// Matching notes, best first.
    pub results: Vec<SemanticSearchHit>,
}

impl From<SemanticSearchRequest> for SemanticSearchToolInput {
    fn from(request: SemanticSearchRequest) -> Self {
        Self {
            query: request.query,
            limit: request.limit,
            label: request.label,
        }
    }
}

impl From<SemanticSearchToolResult> for SemanticSearchHit {
    fn from(result: SemanticSearchToolResult) -> Self {
        Self {
            id: result.id,
            title: result.title,
            score: result.score,
            labels: result.labels.into_iter().map(Into::into).collect(),
            created_at: result.created_at,
            updated_at: result.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "anyOf" = [
        {"required": ["content"]},
        {"required": ["content_base64"]}
    ]
))]
pub struct PutNoteAttachmentRequest {
    /// Note id.
    pub note_id: String,
    /// Attachment id unique within the note.
    pub attachment_id: String,
    /// Relative attachment path.
    pub path: String,
    /// MIME type.
    pub mime: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// UTF-8 attachment content. Provide this or `content_base64`.
    #[serde(default)]
    pub content: Option<String>,
    /// Base64-encoded bytes. Provide this or `content`.
    #[serde(default)]
    pub content_base64: Option<String>,
}

impl TryFrom<PutNoteAttachmentRequest> for PutNoteAttachmentToolInput {
    type Error = ErrorData;

    fn try_from(request: PutNoteAttachmentRequest) -> Result<Self, Self::Error> {
        let content = note_core::decode_attachment_content(
            request.content.as_deref(),
            request.content_base64.as_deref(),
        )
        .map_err(|error| {
            ErrorData::invalid_params(
                format!(
                    "invalid attachment {} at {}: {error}",
                    request.attachment_id, request.path
                ),
                None,
            )
        })?;
        Ok(Self {
            note_id: request.note_id,
            attachment_id: request.attachment_id,
            path: request.path,
            mime: request.mime,
            description: request.description,
            content,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PutNoteAttachmentResponse {
    /// Whether a new attachment id was created.
    pub created: bool,
    /// Stored attachment metadata.
    pub attachment: AttachmentMetadataSchema,
}

impl From<PutNoteAttachmentToolOutput> for PutNoteAttachmentResponse {
    fn from(output: PutNoteAttachmentToolOutput) -> Self {
        Self {
            created: output.created,
            attachment: output.attachment.into(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetNoteAttachmentContentRequest {
    /// Note id.
    pub note_id: String,
    /// Attachment id within the note.
    pub attachment_id: String,
}

#[derive(Debug, Serialize)]
pub struct GetNoteAttachmentContentResponse {
    /// Attachment metadata.
    pub attachment: AttachmentMetadataSchema,
    /// Exactly one UTF-8 or Base64 content representation.
    #[serde(flatten)]
    pub content: AttachmentContentRepresentation,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AttachmentContentRepresentation {
    /// UTF-8 attachment content.
    Text { content: String },
    /// Canonical padded Base64 for non-UTF-8 bytes.
    Base64 { content_base64: String },
}

impl JsonSchema for GetNoteAttachmentContentResponse {
    fn schema_name() -> Cow<'static, str> {
        "GetNoteAttachmentContentResponse".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::GetNoteAttachmentContentResponse").into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let attachment = generator.subschema_for::<AttachmentMetadataSchema>();
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "attachment": attachment,
                "content": {"type": "string"},
                "content_base64": {"type": "string"}
            },
            "required": ["attachment"],
            "additionalProperties": false,
            "oneOf": [
                {
                    "required": ["content"],
                    "not": {"required": ["content_base64"]}
                },
                {
                    "required": ["content_base64"],
                    "not": {"required": ["content"]}
                }
            ]
        })
    }
}

impl From<AttachmentContentData> for GetNoteAttachmentContentResponse {
    fn from(data: AttachmentContentData) -> Self {
        let content = match String::from_utf8(data.content) {
            Ok(content) => AttachmentContentRepresentation::Text { content },
            Err(error) => AttachmentContentRepresentation::Base64 {
                content_base64: note_core::encode_attachment_content(error.as_bytes()),
            },
        };
        Self {
            attachment: data.attachment.into(),
            content,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteNoteAttachmentRequest {
    /// Note id.
    pub note_id: String,
    /// Attachment id within the note.
    pub attachment_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DeleteNoteAttachmentResponse {
    /// Whether the attachment existed and was deleted.
    pub deleted: bool,
}

#[derive(Clone)]
pub struct NoteMcpServer {
    ctx: Arc<Context>,
    tool_router: ToolRouter<Self>,
}

impl NoteMcpServer {
    pub fn new(ctx: Arc<Context>) -> Self {
        Self {
            ctx,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl NoteMcpServer {
    #[tool(
        name = "save_note",
        description = "Save a note with a title, Markdown content, and optional labels."
    )]
    pub async fn save_note(
        &self,
        params: Parameters<SaveNoteRequest>,
    ) -> Result<Json<SaveNoteResponse>, ErrorData> {
        let output = save_note_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        Ok(Json(output.into()))
    }

    #[tool(
        name = "get_note",
        description = "Fetch note content, labels, timestamps, and attachment metadata by note id."
    )]
    pub async fn get_note(
        &self,
        params: Parameters<GetNoteRequest>,
    ) -> Result<Json<NoteDetailResponse>, ErrorData> {
        let id = params.0.id;
        let note = get_note_tool(&self.ctx, &id)
            .await
            .map_err(to_error_data)?
            .ok_or_else(|| ErrorData::resource_not_found(format!("note not found: {id}"), None))?;
        Ok(Json(note.into()))
    }

    #[tool(
        name = "read_note_lines",
        description = "Read a note's Markdown body as numbered lines with a content tag for editing."
    )]
    pub async fn read_note_lines(
        &self,
        params: Parameters<ReadNoteLinesRequest>,
    ) -> Result<Json<NoteLinesResponse>, ErrorData> {
        let id = params.0.id;
        let note = read_note_lines_tool(&self.ctx, &id)
            .await
            .map_err(to_error_data)?
            .ok_or_else(|| ErrorData::resource_not_found(format!("note not found: {id}"), None))?;
        Ok(Json(note.into()))
    }

    #[tool(
        name = "edit_note",
        description = "Edit a note's Markdown body with line-range operations anchored by a content tag."
    )]
    pub async fn edit_note(
        &self,
        params: Parameters<EditNoteRequest>,
    ) -> Result<Json<NoteLinesResponse>, ErrorData> {
        let request = params.0;
        let id = request.id;
        let edits = request.edits.into_iter().map(Into::into).collect();
        let note = edit_note_tool(&self.ctx, &id, &request.tag, edits)
            .await
            .map_err(to_error_data)?
            .ok_or_else(|| ErrorData::resource_not_found(format!("note not found: {id}"), None))?;
        Ok(Json(note.into()))
    }

    #[tool(
        name = "update_note",
        description = "Update a note's title, Markdown content, and labels while preserving attachments."
    )]
    pub async fn update_note(
        &self,
        params: Parameters<UpdateNoteRequest>,
    ) -> Result<Json<NoteDetailResponse>, ErrorData> {
        let id = params.0.id.clone();
        let note = update_note_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?
            .ok_or_else(|| ErrorData::resource_not_found(format!("note not found: {id}"), None))?;
        Ok(Json(note.into()))
    }

    #[tool(name = "delete_note", description = "Delete a note by id.")]
    pub async fn delete_note(
        &self,
        params: Parameters<DeleteNoteRequest>,
    ) -> Result<Json<DeleteNoteResponse>, ErrorData> {
        let deleted = delete_note_tool(&self.ctx, &params.0.id)
            .await
            .map_err(to_error_data)?;
        Ok(Json(DeleteNoteResponse { deleted }))
    }

    #[tool(
        name = "list_notes",
        description = "List note summaries with optional limit, offset, and label selector filters."
    )]
    pub async fn list_notes(
        &self,
        params: Parameters<ListNotesRequest>,
    ) -> Result<Json<NoteListResponse>, ErrorData> {
        let request = params.0;
        let notes = list_notes_tool(
            &self.ctx,
            request.limit.map(i64::from),
            request.offset.map(i64::from),
            request.label,
        )
        .await
        .map_err(to_error_data)?;
        Ok(Json(NoteListResponse {
            notes: notes.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        name = "semantic_search",
        description = "Search note summaries semantically with an optional label selector."
    )]
    pub async fn semantic_search(
        &self,
        params: Parameters<SemanticSearchRequest>,
    ) -> Result<Json<SemanticSearchResponse>, ErrorData> {
        let results = semantic_search_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_error_data)?;
        Ok(Json(SemanticSearchResponse {
            results: results.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        name = "put_note_attachment",
        description = "Add or replace one note attachment by attachment id."
    )]
    pub async fn put_note_attachment(
        &self,
        params: Parameters<PutNoteAttachmentRequest>,
    ) -> Result<Json<PutNoteAttachmentResponse>, ErrorData> {
        let input = params.0.try_into()?;
        let output = put_note_attachment_tool(&self.ctx, input)
            .await
            .map_err(to_put_error_data)?;
        Ok(Json(output.into()))
    }

    #[tool(
        name = "get_note_attachment_content",
        description = "Fetch one note attachment's content by attachment id."
    )]
    pub async fn get_note_attachment_content(
        &self,
        params: Parameters<GetNoteAttachmentContentRequest>,
    ) -> Result<Json<GetNoteAttachmentContentResponse>, ErrorData> {
        let request = params.0;
        let note_id = request.note_id.clone();
        let attachment_id = request.attachment_id.clone();
        let output = get_note_attachment_content_tool(
            &self.ctx,
            GetNoteAttachmentContentToolInput {
                note_id,
                attachment_id,
            },
        )
        .await
        .map_err(to_error_data)?
        .ok_or_else(|| {
            ErrorData::resource_not_found(
                format!(
                    "note attachment not found: note {}, attachment {}",
                    request.note_id, request.attachment_id
                ),
                None,
            )
        })?;
        Ok(Json(output.into()))
    }

    #[tool(
        name = "delete_note_attachment",
        description = "Delete one note attachment by attachment id."
    )]
    pub async fn delete_note_attachment(
        &self,
        params: Parameters<DeleteNoteAttachmentRequest>,
    ) -> Result<Json<DeleteNoteAttachmentResponse>, ErrorData> {
        let deleted =
            delete_note_attachment_tool(&self.ctx, &params.0.note_id, &params.0.attachment_id)
                .await
                .map_err(to_error_data)?;
        Ok(Json(DeleteNoteAttachmentResponse { deleted }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NoteMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(
            "Note server exposing note CRUD, summary/list/search, tagged line editing, and standalone attachment put/get/delete tools over MCP."
                .to_string(),
        );
        info
    }
}

fn to_error_data(error: anyhow::Error) -> ErrorData {
    if error.downcast_ref::<note_core::ValidationError>().is_some() {
        return ErrorData::invalid_params(error.to_string(), None);
    }
    if let Some(error_kind) = error.downcast_ref::<AttachmentMutationError>() {
        return match error_kind {
            AttachmentMutationError::NoteNotFound(_)
            | AttachmentMutationError::AttachmentNotFound { .. } => {
                ErrorData::resource_not_found(error.to_string(), None)
            }
            AttachmentMutationError::AttachmentPathChange { .. }
            | AttachmentMutationError::AttachmentPathCollision { .. } => {
                ErrorData::internal_error(error.to_string(), None)
            }
        };
    }
    ErrorData::internal_error(error.to_string(), None)
}

fn to_put_error_data(error: anyhow::Error) -> ErrorData {
    if matches!(
        error.downcast_ref::<AttachmentMutationError>(),
        Some(
            AttachmentMutationError::AttachmentPathChange { .. }
                | AttachmentMutationError::AttachmentPathCollision { .. }
        )
    ) {
        return ErrorData::invalid_params(error.to_string(), None);
    }
    to_error_data(error)
}

pub async fn run_stdio(ctx: Arc<Context>) -> anyhow::Result<()> {
    let server = NoteMcpServer::new(ctx);
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::StorageBackend;
    use note_storage_turso::TursoStorage;
    use rmcp::model::ErrorCode;
    use serde_json::json;
    use tempfile::TempDir;

    async fn test_context() -> (Context, Arc<dyn StorageBackend>, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> = Arc::new(
            TursoStorage::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let ctx = Context::new(
            backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("attachments"),
            )),
        );
        (ctx, backend, dir)
    }

    fn put_request(
        note_id: &str,
        attachment_id: &str,
        path: &str,
        content: Option<&str>,
        content_base64: Option<&str>,
    ) -> PutNoteAttachmentRequest {
        PutNoteAttachmentRequest {
            note_id: note_id.into(),
            attachment_id: attachment_id.into(),
            path: path.into(),
            mime: "application/octet-stream".into(),
            description: format!("{attachment_id} description"),
            content: content.map(str::to_owned),
            content_base64: content_base64.map(str::to_owned),
        }
    }

    fn tool_schema(server: &NoteMcpServer, name: &str, output: bool) -> serde_json::Value {
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name.as_ref() == name)
            .unwrap_or_else(|| panic!("tool missing: {name}"));
        if output {
            serde_json::Value::Object(
                tool.output_schema
                    .unwrap_or_else(|| panic!("{name} output schema missing"))
                    .as_ref()
                    .clone(),
            )
        } else {
            serde_json::Value::Object(tool.input_schema.as_ref().clone())
        }
    }

    fn property_names(schema: &serde_json::Value) -> Vec<String> {
        let mut names = schema["properties"]
            .as_object()
            .expect("schema properties")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn resolve_schema<'a>(
        root: &'a serde_json::Value,
        schema: &'a serde_json::Value,
    ) -> &'a serde_json::Value {
        match schema.get("$ref").and_then(serde_json::Value::as_str) {
            Some(reference) => root
                .pointer(reference.strip_prefix('#').expect("local schema reference"))
                .unwrap_or_else(|| panic!("unresolved schema reference: {reference}")),
            None => schema,
        }
    }

    fn required_names(schema: &serde_json::Value) -> Vec<String> {
        let mut names = schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|name| name.as_str().expect("required property name").to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn assert_exact_closed_object(
        root: &serde_json::Value,
        schema: &serde_json::Value,
        expected_properties: &[&str],
    ) {
        let schema = resolve_schema(root, schema);
        assert_eq!(schema["type"], "object", "{schema}");
        assert_eq!(schema["additionalProperties"], false, "{schema}");
        assert_eq!(
            property_names(schema),
            expected_properties
                .iter()
                .map(|property| (*property).to_owned())
                .collect::<Vec<_>>(),
            "{schema}"
        );
    }

    fn assert_attachment_content_requirement(schema: &serde_json::Value) {
        assert!(schema.pointer("/properties/content").is_some());
        assert!(schema.pointer("/properties/content_base64").is_some());
        assert!(schema.get("oneOf").is_none());
        let mut required = schema["anyOf"]
            .as_array()
            .expect("attachment schema anyOf")
            .iter()
            .map(|branch| branch["required"].clone())
            .collect::<Vec<_>>();
        required.sort_by_key(ToString::to_string);
        assert_eq!(
            required,
            vec![json!(["content"]), json!(["content_base64"])]
        );
    }

    fn expect_error<T>(result: Result<T, ErrorData>) -> ErrorData {
        match result {
            Ok(_) => panic!("request unexpectedly succeeded"),
            Err(error) => error,
        }
    }

    fn schema_allows_null(schema: &serde_json::Value) -> bool {
        schema["type"] == "null"
            || schema["type"]
                .as_array()
                .is_some_and(|types| types.iter().any(|value| value == "null"))
            || ["anyOf", "oneOf"].into_iter().any(|keyword| {
                schema[keyword]
                    .as_array()
                    .is_some_and(|branches| branches.iter().any(schema_allows_null))
            })
    }

    #[tokio::test]
    async fn server_builds_and_lists_exactly_eleven_tools() {
        let (ctx, _backend, _dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let mut names = server
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            vec![
                "delete_note",
                "delete_note_attachment",
                "edit_note",
                "get_note",
                "get_note_attachment_content",
                "list_notes",
                "put_note_attachment",
                "read_note_lines",
                "save_note",
                "semantic_search",
                "update_note",
            ]
        );
    }

    #[tokio::test]
    async fn advertised_schemas_are_bounded_and_keep_nullable_filters() {
        let (ctx, _backend, _dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));

        for tool in server.tool_router.list_all() {
            let schema = serde_json::Value::Object(tool.input_schema.as_ref().clone());
            assert_eq!(schema["type"], "object", "{}: {schema}", tool.name);
            assert_eq!(
                schema["additionalProperties"], false,
                "{}: {schema}",
                tool.name
            );
        }

        let save_input = tool_schema(&server, "save_note", false);
        assert_exact_closed_object(&save_input, &save_input, &["content", "labels", "title"]);
        let update_input = tool_schema(&server, "update_note", false);
        assert_exact_closed_object(
            &update_input,
            &update_input,
            &["content", "id", "labels", "title"],
        );

        let put_schema = tool_schema(&server, "put_note_attachment", false);
        assert_exact_closed_object(
            &put_schema,
            &put_schema,
            &[
                "attachment_id",
                "content",
                "content_base64",
                "description",
                "mime",
                "note_id",
                "path",
            ],
        );
        assert_attachment_content_requirement(&put_schema);
        let list_input = tool_schema(&server, "list_notes", false);
        assert!(schema_allows_null(&list_input["properties"]["label"]));

        let list_output = tool_schema(&server, "list_notes", true);
        assert_exact_closed_object(&list_output, &list_output, &["notes"]);
        let summary = resolve_schema(&list_output, &list_output["properties"]["notes"]["items"]);
        assert_exact_closed_object(
            &list_output,
            summary,
            &["created_at", "id", "labels", "title", "updated_at"],
        );
        let summary_label = resolve_schema(&list_output, &summary["properties"]["labels"]["items"]);
        assert_exact_closed_object(
            &list_output,
            summary_label,
            &["description", "key", "value", "value_type"],
        );

        let detail_output = tool_schema(&server, "get_note", true);
        assert_exact_closed_object(
            &detail_output,
            &detail_output,
            &[
                "attachments",
                "content",
                "created_at",
                "id",
                "labels",
                "title",
                "updated_at",
            ],
        );
        let attachment = resolve_schema(
            &detail_output,
            &detail_output["properties"]["attachments"]["items"],
        );
        assert_exact_closed_object(
            &detail_output,
            attachment,
            &["description", "id", "mime", "path"],
        );

        let search_output = tool_schema(&server, "semantic_search", true);
        assert_exact_closed_object(&search_output, &search_output, &["results"]);
        let hit = resolve_schema(
            &search_output,
            &search_output["properties"]["results"]["items"],
        );
        assert_exact_closed_object(
            &search_output,
            hit,
            &["created_at", "id", "labels", "score", "title", "updated_at"],
        );

        let attachment_output = tool_schema(&server, "get_note_attachment_content", true);
        assert_exact_closed_object(
            &attachment_output,
            &attachment_output,
            &["attachment", "content", "content_base64"],
        );
        let metadata = resolve_schema(
            &attachment_output,
            &attachment_output["properties"]["attachment"],
        );
        assert_exact_closed_object(
            &attachment_output,
            metadata,
            &["description", "id", "mime", "path"],
        );
        assert_eq!(attachment_output["properties"]["content"]["type"], "string");
        assert_eq!(
            attachment_output["properties"]["content_base64"]["type"],
            "string"
        );
        assert_eq!(required_names(&attachment_output), vec!["attachment"]);
        let mut exclusive_variants = attachment_output["oneOf"]
            .as_array()
            .expect("exclusive attachment content variants")
            .iter()
            .map(|variant| (required_names(variant), required_names(&variant["not"])))
            .collect::<Vec<_>>();
        exclusive_variants.sort();
        assert_eq!(
            exclusive_variants,
            vec![
                (
                    vec!["content".to_owned()],
                    vec!["content_base64".to_owned()]
                ),
                (
                    vec!["content_base64".to_owned()],
                    vec!["content".to_owned()]
                ),
            ]
        );
    }

    #[test]
    fn response_schema_types_have_exact_fields() {
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(AttachmentMetadataSchema)).unwrap()
            ),
            vec!["description", "id", "mime", "path"]
        );
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(NoteSummaryResponse)).unwrap()
            ),
            vec!["created_at", "id", "labels", "title", "updated_at"]
        );
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(NoteDetailResponse)).unwrap()
            ),
            vec![
                "attachments",
                "content",
                "created_at",
                "id",
                "labels",
                "title",
                "updated_at",
            ]
        );
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(SemanticSearchHit)).unwrap()
            ),
            vec!["created_at", "id", "labels", "score", "title", "updated_at"]
        );
    }

    #[test]
    fn save_and_update_deserialization_rejects_inline_attachments() {
        let save = serde_json::from_value::<SaveNoteRequest>(json!({
            "title": "Legacy",
            "content": "Body",
            "attachments": []
        }));
        assert!(save.is_err());

        let update = serde_json::from_value::<UpdateNoteRequest>(json!({
            "id": "note-1",
            "title": "Legacy",
            "content": "Body",
            "attachments": []
        }));
        assert!(update.is_err());
    }

    #[test]
    fn only_put_maps_attachment_path_faults_to_invalid_params() {
        let path_change = || {
            anyhow::Error::new(AttachmentMutationError::AttachmentPathChange {
                attachment_id: "blob".into(),
            })
        };
        assert_eq!(
            to_put_error_data(path_change()).code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(to_error_data(path_change()).code, ErrorCode::INTERNAL_ERROR);

        let collision = anyhow::Error::new(AttachmentMutationError::AttachmentPathCollision {
            path: "blob.bin".into(),
        });
        assert_eq!(to_put_error_data(collision).code, ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn standalone_handlers_use_metadata_and_exclusive_content_shapes() {
        let (ctx, _backend, dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Standalone".into(),
                content: "searchable body".into(),
                labels: vec![("topic".into(), "rust".into())],
            }))
            .await
            .unwrap()
            .0;
        let text = server
            .put_note_attachment(Parameters(put_request(
                &saved.id,
                "text",
                "./text.txt",
                Some("plain text\n"),
                None,
            )))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(text).unwrap(),
            json!({
                "created": true,
                "attachment": {
                    "id": "text",
                    "path": "./text.txt",
                    "mime": "application/octet-stream",
                    "description": "text description"
                }
            })
        );
        server
            .put_note_attachment(Parameters(put_request(
                &saved.id,
                "blob",
                "./blob.bin",
                None,
                Some("AJ+Slv8="),
            )))
            .await
            .unwrap();

        let detail = server
            .get_note(Parameters(GetNoteRequest {
                id: saved.id.clone(),
            }))
            .await
            .unwrap()
            .0;
        let detail_json = serde_json::to_value(detail).unwrap();
        assert_eq!(
            detail_json
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "attachments",
                "content",
                "created_at",
                "id",
                "labels",
                "title",
                "updated_at"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        assert_eq!(
            detail_json["attachments"][0],
            json!({
                "id": "text",
                "path": "./text.txt",
                "mime": "application/octet-stream",
                "description": "text description"
            })
        );

        let text_content = server
            .get_note_attachment_content(Parameters(GetNoteAttachmentContentRequest {
                note_id: saved.id.clone(),
                attachment_id: "text".into(),
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(text_content).unwrap(),
            json!({
                "attachment": {
                    "id": "text",
                    "path": "./text.txt",
                    "mime": "application/octet-stream",
                    "description": "text description"
                },
                "content": "plain text\n"
            })
        );
        let binary_content = server
            .get_note_attachment_content(Parameters(GetNoteAttachmentContentRequest {
                note_id: saved.id.clone(),
                attachment_id: "blob".into(),
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(binary_content).unwrap(),
            json!({
                "attachment": {
                    "id": "blob",
                    "path": "./blob.bin",
                    "mime": "application/octet-stream",
                    "description": "blob description"
                },
                "content_base64": "AJ+Slv8="
            })
        );

        std::fs::remove_file(
            dir.path()
                .join("attachments")
                .join(&saved.id)
                .join("blob.bin"),
        )
        .unwrap();
        let updated = server
            .update_note(Parameters(UpdateNoteRequest {
                id: saved.id.clone(),
                title: "Standalone updated".into(),
                content: "updated searchable body".into(),
                labels: vec![("topic".into(), "rust".into())],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(updated.attachments.len(), 2);

        let listed = server
            .list_notes(Parameters(ListNotesRequest {
                limit: None,
                offset: None,
                label: None,
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(&listed.notes[0])
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            ["created_at", "id", "labels", "title", "updated_at"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );

        note_pipelines::drain_embedding_jobs(&server.ctx, 10)
            .await
            .unwrap();
        let hit = server
            .semantic_search(Parameters(SemanticSearchRequest {
                query: "updated searchable body".into(),
                limit: 5,
                label: None,
            }))
            .await
            .unwrap()
            .0
            .results
            .into_iter()
            .find(|hit| hit.id == saved.id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(hit)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            ["created_at", "id", "labels", "score", "title", "updated_at"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );

        for expected in [true, false] {
            let deleted = server
                .delete_note_attachment(Parameters(DeleteNoteAttachmentRequest {
                    note_id: saved.id.clone(),
                    attachment_id: "text".into(),
                }))
                .await
                .unwrap()
                .0;
            assert_eq!(
                serde_json::to_value(deleted).unwrap(),
                json!({"deleted": expected})
            );
        }
    }

    #[tokio::test]
    async fn invalid_attachment_inputs_are_invalid_params_without_writes() {
        let (ctx, _backend, dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Rejected".into(),
                content: "Body".into(),
                labels: vec![],
            }))
            .await
            .unwrap()
            .0;

        for (request, expected) in [
            (
                put_request(&saved.id, "blob", "./blob.bin", None, Some("not base64 !!")),
                "invalid attachment content_base64",
            ),
            (
                put_request(&saved.id, "blob", "./blob.bin", None, None),
                "attachment content or content_base64 is required",
            ),
            (
                put_request(&saved.id, "blob", "./blob.bin", Some("abc"), Some("eHl6")),
                "attachment content and content_base64 do not match",
            ),
            (
                put_request(&saved.id, "blob", "../blob.bin", Some("abc"), None),
                "attachment path must be relative",
            ),
        ] {
            let error = expect_error(server.put_note_attachment(Parameters(request)).await);
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert!(error.message.contains(expected), "{}", error.message);
        }

        let note_dir = dir.path().join("attachments").join(saved.id);
        assert!(!note_dir.exists() || std::fs::read_dir(note_dir).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn mutation_faults_and_missing_attachments_have_precise_errors() {
        let (ctx, _backend, _dir) = test_context().await;
        let server = NoteMcpServer::new(Arc::new(ctx));
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Faults".into(),
                content: "Body".into(),
                labels: vec![],
            }))
            .await
            .unwrap()
            .0;
        server
            .put_note_attachment(Parameters(put_request(
                &saved.id,
                "first",
                "./first.txt",
                Some("first"),
                None,
            )))
            .await
            .unwrap();

        for request in [
            put_request(&saved.id, "first", "./moved.txt", Some("moved"), None),
            put_request(&saved.id, "second", "./first.txt", Some("collision"), None),
        ] {
            let error = expect_error(server.put_note_attachment(Parameters(request)).await);
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        }

        let missing_put = expect_error(
            server
                .put_note_attachment(Parameters(put_request(
                    "missing-note",
                    "blob",
                    "./blob.bin",
                    Some("bytes"),
                    None,
                )))
                .await,
        );
        assert_eq!(missing_put.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert!(missing_put.message.contains("note not found: missing-note"));

        let missing_get = expect_error(
            server
                .get_note_attachment_content(Parameters(GetNoteAttachmentContentRequest {
                    note_id: saved.id,
                    attachment_id: "missing".into(),
                }))
                .await,
        );
        assert_eq!(missing_get.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert!(missing_get.message.contains("note attachment not found"));
    }
}
