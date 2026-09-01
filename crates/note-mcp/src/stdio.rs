//! MCP server exposing note tools over stdio and Streamable HTTP transports.
//!
//! Transport-specific request/response marshalling lives here. The handlers
//! delegate to transport-independent wrappers in [`crate::tools`].

use std::{borrow::Cow, sync::Arc};

use note_pipelines::{
    normalized_list_limit, normalized_list_offset, org::OrgContext, AttachmentMutationError,
    BulkUpdateNoteLabelsValidationError, Context,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::{io::stdio, IntoTransport},
    ErrorData, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::{
    bulk_update_note_labels_tool, delete_note_attachment_tool, delete_note_tool, edit_note_tool,
    get_note_attachment_content_tool, get_note_tool, list_notes_tool, put_note_attachment_tool,
    read_note_lines_tool, save_note_tool, semantic_search_tool, update_note_tool,
    AttachmentContentData, AttachmentMetadataData, BulkUpdateNoteLabelsToolInput,
    BulkUpdateNoteLabelsToolOutput, GetNoteAttachmentContentToolInput, LabelData, NoteDetailData,
    NoteLine, NoteLinesData, NoteSummaryData, PutNoteAttachmentToolInput,
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
    /// Label `(key, value)` pairs to attach. Missing keys are auto-created, but keys cannot contain
    /// selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SaveNoteResponse {
    /// Id of the newly saved note.
    pub id: String,
    /// Initial revision for subsequent writes.
    pub revision: i64,
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
        Self {
            id: output.id,
            revision: output.revision,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsRequest {
    /// Label selector used to choose active notes. Exact raw equality is `==` with
    /// `~<percent-encoded-key>==<percent-encoded-value>` so reserved, empty, and
    /// outer-whitespace operands round-trip exactly.
    pub selector: String,
    /// Label `(key, value)` pairs to set or replace on every matching note.
    #[serde(default)]
    pub set: Vec<(String, String)>,
    /// Label keys to remove from every matching note.
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BulkUpdateNoteLabelsResponse {
    /// Number of active notes matching the selector.
    pub matched: usize,
    /// Number of matching notes whose labels changed.
    pub updated: usize,
    /// Number of matching notes already in the requested state.
    pub unchanged: usize,
}

impl From<BulkUpdateNoteLabelsRequest> for BulkUpdateNoteLabelsToolInput {
    fn from(request: BulkUpdateNoteLabelsRequest) -> Self {
        Self {
            selector: request.selector,
            set: request.set,
            remove: request.remove,
        }
    }
}

impl From<BulkUpdateNoteLabelsToolOutput> for BulkUpdateNoteLabelsResponse {
    fn from(output: BulkUpdateNoteLabelsToolOutput) -> Self {
        Self {
            matched: output.matched,
            updated: output.updated,
            unchanged: output.unchanged,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
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
    /// Optimistic concurrency revision for subsequent writes.
    pub revision: i64,
}

impl From<NoteSummaryData> for NoteSummaryResponse {
    fn from(note: NoteSummaryData) -> Self {
        Self {
            id: note.id,
            title: note.title,
            labels: note.labels.into_iter().map(Into::into).collect(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            revision: note.revision,
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
    /// Optimistic concurrency revision for subsequent writes.
    pub revision: i64,
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
            revision: note.revision,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
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
    /// Optimistic concurrency revision required by `edit_note`.
    pub revision: i64,
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
            revision: note.revision,
            tag: note.tag,
            lines: note.lines.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
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
pub struct EditNoteRequest {
    /// Note id.
    pub id: String,
    /// Revision returned by `read_note_lines`.
    pub expected_revision: i64,
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
    /// Revision returned by the latest read.
    pub expected_revision: i64,
    /// New note title.
    pub title: String,
    /// New note body, formatted as Markdown.
    pub content: String,
    /// Label `(key, value)` pairs to attach. Missing keys are auto-created, but keys cannot contain
    /// selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`.
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

impl From<UpdateNoteRequest> for UpdateNoteToolInput {
    fn from(request: UpdateNoteRequest) -> Self {
        Self {
            id: request.id,
            expected_revision: request.expected_revision,
            title: request.title,
            content: request.content,
            labels: request.labels,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteNoteRequest {
    /// Note id.
    pub id: String,
    /// Revision returned by the latest read.
    pub expected_revision: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DeleteNoteResponse {
    /// Whether a note was deleted.
    pub deleted: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNotesRequest {
    /// Maximum notes to return; omission returns 10 notes and the maximum is 1,000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of notes to skip.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex). Exact raw equality is
    /// `==` with `~<percent-encoded-key>==<percent-encoded-value>`; the reserved prefix
    /// preserves legacy selector meanings while reserved characters, empty values, and whitespace round-trip.
    #[serde(default)]
    pub label: Option<String>,
}

fn normalized_list_notes_request(request: ListNotesRequest) -> (i64, i64, Option<String>) {
    (
        normalized_list_limit(request.limit.map(i64::from)),
        normalized_list_offset(request.offset.map(i64::from)),
        request.label,
    )
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct NoteListResponse {
    /// Saved note summaries.
    pub notes: Vec<NoteSummaryResponse>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SemanticSearchRequest {
    /// Natural-language query.
    pub query: String,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Label selector: `&`-separated terms are ANDed; bare-key presence is supported;
    /// operators are `=`, `!=`, `>`, `>=`, `<`, `<=`; case-insensitive operators are
    /// `^=` (starts-with), `$=` (ends-with), and `~=` (regex). Exact raw equality is
    /// `==` with `~<percent-encoded-key>==<percent-encoded-value>`; the reserved prefix
    /// preserves legacy selector meanings while reserved characters, empty values, and whitespace round-trip.
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
    /// Optimistic concurrency revision for subsequent writes.
    pub revision: i64,
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
            revision: result.revision,
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
    /// Revision returned by the latest note read.
    pub expected_revision: i64,
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
            expected_revision: request.expected_revision,
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
    /// New owning-note revision.
    pub revision: i64,
    /// Stored attachment metadata.
    pub attachment: AttachmentMetadataSchema,
}

impl From<PutNoteAttachmentToolOutput> for PutNoteAttachmentResponse {
    fn from(output: PutNoteAttachmentToolOutput) -> Self {
        Self {
            created: output.created,
            revision: output.revision,
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
    /// Revision returned by the latest note read.
    pub expected_revision: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DeleteNoteAttachmentResponse {
    /// Whether the attachment existed and was deleted.
    pub deleted: bool,
    /// New owning-note revision, or null when the attachment was already absent.
    pub revision: Option<i64>,
}

#[derive(Clone)]
pub struct NoteMcpServer {
    ctx: Arc<Context>,
    org_ctx: Arc<OrgContext>,
    tool_router: ToolRouter<Self>,
}

impl NoteMcpServer {
    pub fn new(ctx: Arc<Context>, org_ctx: Arc<OrgContext>) -> Self {
        Self {
            ctx,
            org_ctx,
            tool_router: crate::org::checked_tool_router(Self::tool_router())
                .expect("static MCP tool names are unique"),
        }
    }

    pub fn registered_tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all().to_vec()
    }

    pub(crate) fn org_context(&self) -> &Arc<OrgContext> {
        &self.org_ctx
    }
}

#[tool_router(router = tool_router)]
impl NoteMcpServer {
    #[tool(
        name = "bulk_update_note_labels",
        description = "Atomically set, replace, or remove labels on active notes matching a label selector while preserving unrelated labels."
    )]
    pub async fn bulk_update_note_labels(
        &self,
        params: Parameters<BulkUpdateNoteLabelsRequest>,
    ) -> Result<Json<BulkUpdateNoteLabelsResponse>, ErrorData> {
        let output = bulk_update_note_labels_tool(&self.ctx, params.0.into())
            .await
            .map_err(to_bulk_update_note_labels_error_data)?;
        Ok(Json(output.into()))
    }

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
        let note = edit_note_tool(
            &self.ctx,
            &id,
            request.expected_revision,
            &request.tag,
            edits,
        )
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
        let deleted = delete_note_tool(&self.ctx, &params.0.id, params.0.expected_revision)
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
        let (limit, offset, label) = normalized_list_notes_request(params.0);
        let notes = list_notes_tool(&self.ctx, Some(limit), Some(offset), label)
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
        let deleted = delete_note_attachment_tool(
            &self.ctx,
            &params.0.note_id,
            &params.0.attachment_id,
            params.0.expected_revision,
        )
        .await
        .map_err(to_error_data)?;
        let (deleted, revision) = match deleted {
            note_pipelines::DeleteNoteAttachmentResult::Applied { revision } => {
                (true, Some(revision))
            }
            note_pipelines::DeleteNoteAttachmentResult::Absent => (false, None),
        };
        Ok(Json(DeleteNoteAttachmentResponse { deleted, revision }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NoteMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.instructions = Some(
            "Agent Note and Org orchestration server. Org actor_id values are caller-asserted audit metadata only; they are not authenticated identities."
                .to_string(),
        );
        info
    }
}

fn to_error_data(error: anyhow::Error) -> ErrorData {
    if error.downcast_ref::<note_core::ValidationError>().is_some()
        || error
            .downcast_ref::<note_core::LabelKeyValidationError>()
            .is_some()
        || error
            .downcast_ref::<note_core::LabelSelectorParseError>()
            .is_some()
    {
        return ErrorData::invalid_params(error.to_string(), None);
    }
    if let Some(error) = error.downcast_ref::<note_pipelines::NoteMutationError>() {
        let message = match error {
            note_pipelines::NoteMutationError::NotFound(_) => "note not found",
            note_pipelines::NoteMutationError::StaleRevision { .. } => {
                "the note changed after it was read"
            }
            note_pipelines::NoteMutationError::StaleContentTag { .. } => {
                "the note content changed after it was read"
            }
        };
        let data = serde_json::json!({
            "code": error.code(),
            "message": message,
            "details": error.details(),
            "retryable": error.retryable(),
        });
        return match error {
            note_pipelines::NoteMutationError::NotFound(_) => {
                ErrorData::resource_not_found(message, Some(data))
            }
            note_pipelines::NoteMutationError::StaleRevision { .. }
            | note_pipelines::NoteMutationError::StaleContentTag { .. } => {
                ErrorData::invalid_request(message, Some(data))
            }
        };
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
    ErrorData::internal_error("note operation failed", None)
}

fn to_bulk_update_note_labels_error_data(error: anyhow::Error) -> ErrorData {
    let caller_message = error
        .downcast_ref::<BulkUpdateNoteLabelsValidationError>()
        .map(ToString::to_string)
        .or_else(|| {
            error
                .downcast_ref::<note_core::ValidationError>()
                .map(ToString::to_string)
        })
        .or_else(|| {
            error
                .downcast_ref::<note_core::LabelKeyValidationError>()
                .map(ToString::to_string)
        });
    if caller_message
        .as_ref()
        .is_some_and(|message| error.to_string() == *message)
    {
        return ErrorData::invalid_params(caller_message.unwrap(), None);
    }
    ErrorData::internal_error("bulk note label update failed", None)
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

pub async fn serve_stdio_transport<T, E, A>(
    ctx: Arc<Context>,
    org_ctx: Arc<OrgContext>,
    transport: T,
) -> anyhow::Result<()>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let server = NoteMcpServer::new(ctx, org_ctx);
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}

pub async fn run_stdio(ctx: Arc<Context>, org_ctx: Arc<OrgContext>) -> anyhow::Result<()> {
    serve_stdio_transport(ctx, org_ctx, stdio()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_attachments::FilesystemAttachmentStore;
    use note_embedding::StubEmbedder;
    use note_storage::{
        ActiveNoteSource, AttachmentMetadataUpdate, AttachmentOperation,
        AttachmentOperationRepository, AttachmentOperationStatus, BackendInfo,
        EmbeddingDashboardStatus, EmbeddingJob, EmbeddingRepository, LabelRepository,
        NewAttachmentOperation, NewNote, NoteChunk, NoteFieldsUpdate, NoteMutationResult,
        NoteUpdate, NotesRepository, OrgRepository, RetrievalRepository, SettingsRepository,
        StorageBackend, StorageError, StorageErrorKind, StorageResult, StorageSession,
        StorageTransaction, TransactionMode, UpsertNoteChunk,
    };
    use note_storage_turso::TursoStorage;
    use rmcp::model::ErrorCode;
    use serde_json::json;
    use tempfile::TempDir;

    struct FailingBeginStorageBackend {
        inner: Arc<dyn StorageBackend>,
    }

    struct RollbackFailingStorageBackend {
        inner: Arc<dyn StorageBackend>,
    }

    struct RollbackFailingTransaction {
        inner: Box<dyn StorageTransaction>,
    }

    #[async_trait::async_trait]
    impl StorageBackend for RollbackFailingStorageBackend {
        async fn session(&self) -> StorageResult<Box<dyn StorageSession>> {
            self.inner.session().await
        }

        async fn begin(&self, mode: TransactionMode) -> StorageResult<Box<dyn StorageTransaction>> {
            Ok(Box::new(RollbackFailingTransaction {
                inner: self.inner.begin(mode).await?,
            }))
        }

        async fn info(&self) -> StorageResult<BackendInfo> {
            self.inner.info().await
        }
    }

    macro_rules! impl_forward_repository {
        ($repository:path { $(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty;)* }) => {
            #[async_trait::async_trait]
            impl $repository for RollbackFailingTransaction {
                $(
                    async fn $name(&self, $($arg: $ty),*) -> StorageResult<$result> {
                        self.inner.$name($($arg),*).await
                    }
                )*
            }
        };
    }

    impl_forward_repository! {
        NotesRepository {
            fn insert_note(note: NewNote<'_>) -> ();
            fn get_note_revision(id: &str) -> Option<i64>;
            fn note_exists(id: &str) -> bool;
            fn get_note(id: &str) -> Option<note_core::Note>;
            fn get_note_content(id: &str) -> Option<String>;
            fn update_note(note: NoteUpdate<'_>) -> NoteMutationResult<()>;
            fn update_note_fields(note: NoteFieldsUpdate<'_>) -> NoteMutationResult<()>;
            fn update_note_attachments(note: AttachmentMetadataUpdate<'_>) -> NoteMutationResult<()>;
            fn soft_delete_note(id: &str, expected_revision: i64, deleted_at: i64) -> NoteMutationResult<()>;
            fn get_deleted_note_content_and_revision(id: &str) -> Option<(String, i64)>;
            fn get_deleted_note_snapshot(id: &str) -> Option<note_storage::DeletedNoteSnapshot>;
            fn restore_note(id: &str, expected_revision: i64) -> NoteMutationResult<()>;
            fn permanently_delete_note(id: &str, expected_revision: i64) -> NoteMutationResult<()>;
            fn list_expired_deleted_note_ids(cutoff: i64) -> Vec<String>;
            fn clear_note_search_data(id: &str) -> ();
            fn clear_note_labels(id: &str) -> ();
            fn clear_note_chunk_derived(id: &str, chunk_idx: i64) -> ();
            fn clear_note_chunks_from_derived(id: &str, min_chunk_idx: i64) -> ();
            fn list_notes(
                selectors: &[note_core::LabelSelector],
                limit: Option<i64>,
                offset: Option<i64>,
            ) -> Vec<note_core::Note>;
            fn list_all_notes() -> Vec<note_core::Note>;
            fn list_note_summaries(
                selectors: &[note_core::LabelSelector],
                limit: Option<i64>,
                offset: Option<i64>,
            ) -> Vec<note_core::NoteListItem>;
            fn list_deleted_note_summaries() -> Vec<note_core::NoteListItem>;
            fn count_notes(selectors: &[note_core::LabelSelector]) -> usize;
            fn matching_note_ids(selectors: &[note_core::LabelSelector]) -> Vec<String>;
            fn matching_note_ids_for_update(selectors: &[note_core::LabelSelector]) -> Vec<String>;
            fn active_note_revisions_for_update(ids: &[String]) -> Vec<(String, i64)>;
            fn advance_note_updated_at(id: &str, now: i64) -> u64;
            fn list_active_note_sources() -> Vec<ActiveNoteSource>;
        }
    }

    impl_forward_repository! {
        AttachmentOperationRepository {
            fn insert_attachment_operation(operation: NewAttachmentOperation) -> AttachmentOperation;
            fn get_attachment_operation(id: &str) -> Option<AttachmentOperation>;
            fn list_attachment_operations_for_note(note_id: &str) -> Vec<AttachmentOperation>;
            fn claim_attachment_operations(
                owner: &str,
                now: i64,
                lease_expires_at: i64,
                limit: i64,
            ) -> Vec<AttachmentOperation>;
            fn complete_attachment_operation(
                id: &str,
                owner: &str,
                expected_attempt: i64,
                updated_at: i64,
            ) -> bool;
            fn fail_attachment_operation(
                id: &str,
                owner: &str,
                expected_attempt: i64,
                status: AttachmentOperationStatus,
                next_attempt_at: Option<i64>,
                last_error: &str,
                updated_at: i64,
            ) -> bool;
        }
    }

    impl_forward_repository! {
        LabelRepository {
            fn insert_label_key(key: &str, description: &str) -> ();
            fn insert_label_key_if_missing(key: &str, description: &str) -> ();
            fn insert_label_key_with_type(
                key: &str,
                description: &str,
                value_type: note_core::LabelValueType,
            ) -> ();
            fn list_label_keys() -> Vec<note_core::LabelKey>;
            fn update_label_key(key: &str, description: &str) -> ();
            fn update_label_key_with_type(
                key: &str,
                description: &str,
                value_type: note_core::LabelValueType,
            ) -> ();
            fn delete_label_key(key: &str) -> ();
            fn attach_label(note_id: &str, key: &str, value: &str) -> ();
            fn set_note_label(note_id: &str, key: &str, value: &str) -> bool;
            fn remove_note_label(note_id: &str, key: &str) -> bool;
            fn labels_for_note(note_id: &str) -> Vec<note_core::Label>;
            fn label_note_counts() -> Vec<(String, usize)>;
            fn label_value_counts(keys: &[String]) -> Vec<note_storage::LabelValueCount>;
            fn find_note_with_labels(labels: &[(String, String)]) -> Option<String>;
        }
    }

    impl_forward_repository! {
        EmbeddingRepository {
            fn embedding_dashboard_status() -> EmbeddingDashboardStatus;
            fn list_note_chunks(note_id: &str) -> Vec<NoteChunk>;
            fn get_note_chunk(note_id: &str, chunk_idx: i64) -> Option<NoteChunk>;
            fn upsert_note_chunk(chunk: UpsertNoteChunk<'_>) -> ();
            fn mark_note_chunk_status(
                note_id: &str,
                chunk_idx: i64,
                content_hash: &str,
                note_revision: i64,
                status: &str,
                updated_at: i64,
            ) -> u64;
            fn delete_note_chunks_from(note_id: &str, min_chunk_idx: i64) -> u64;
            fn chunk_embedding_exists(note_id: &str, chunk_idx: i64) -> bool;
            fn enqueue_embedding_job(
                note_id: &str,
                chunk_idx: i64,
                content_hash: &str,
                content: &str,
                note_revision: i64,
                now: i64,
            ) -> ();
            fn delete_stale_embedding_jobs_for_chunk(
                note_id: &str,
                chunk_idx: i64,
                current_hash: &str,
            ) -> u64;
            fn delete_embedding_jobs_from_chunk(note_id: &str, min_chunk_idx: i64) -> u64;
            fn claim_pending_embedding_jobs(limit: usize, now: i64) -> Vec<EmbeddingJob>;
            fn delete_embedding_job(id: i64) -> u64;
            fn fail_embedding_job(
                id: i64,
                attempts: i64,
                max_attempts: i64,
                error: &str,
                now: i64,
            ) -> u64;
            fn requeue_processing_embedding_jobs(now: i64) -> u64;
            fn reset_embeddings_for_regeneration(now: i64) -> u64;
        }
    }

    impl_forward_repository! {
        RetrievalRepository {
            fn insert_chunk_embedding(note_id: &str, chunk_idx: i64, embedding: &[f32]) -> ();
            fn dense_search(
                query: &[f32],
                limit: usize,
                allowed_note_ids: Option<&[String]>,
            ) -> Vec<String>;
            fn title_search(
                query: &str,
                limit: usize,
                allowed_note_ids: Option<&[String]>,
            ) -> Vec<String>;
        }
    }

    impl_forward_repository! {
        SettingsRepository {
            fn get_system_config() -> note_core::SystemConfig;
            fn set_system_config(config: &note_core::SystemConfig) -> ();
            fn get_embedding_fingerprint() -> Option<String>;
            fn set_embedding_fingerprint(fingerprint: &str) -> ();
        }
    }

    impl OrgRepository for RollbackFailingTransaction {}

    #[async_trait::async_trait]
    impl StorageTransaction for RollbackFailingTransaction {
        async fn commit(self: Box<Self>) -> StorageResult<()> {
            self.inner.commit().await
        }

        async fn rollback(self: Box<Self>) -> StorageResult<()> {
            self.inner.rollback().await?;
            Err(StorageError::new(
                StorageErrorKind::Transaction,
                "ROLLBACK-SECRET-42",
            ))
        }
    }

    impl StorageBackend for FailingBeginStorageBackend {
        fn session<'life0, 'async_trait>(
            &'life0 self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = StorageResult<Box<dyn StorageSession>>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.session().await })
        }

        fn begin<'life0, 'async_trait>(
            &'life0 self,
            _mode: TransactionMode,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = StorageResult<Box<dyn StorageTransaction>>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async {
                Err(StorageError::new(
                    StorageErrorKind::Unavailable,
                    "private repository failure detail",
                ))
            })
        }

        fn info<'life0, 'async_trait>(
            &'life0 self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = StorageResult<BackendInfo>> + Send + 'async_trait>,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.info().await })
        }
    }

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

    fn test_server(ctx: Context, backend: Arc<dyn StorageBackend>) -> NoteMcpServer {
        let org_ctx = Arc::new(OrgContext::new(
            backend,
            Arc::new(note_pipelines::org::SystemOrgClock),
        ));
        NoteMcpServer::new(Arc::new(ctx), org_ctx)
    }

    async fn seed_summary_notes(backend: &dyn note_storage::StorageBackend, count: usize) {
        let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
        for index in 0..count {
            let id = format!("mcp-list-{index:04}");
            let timestamp = i64::try_from(index).unwrap();
            transaction
                .insert_note(note_storage::NewNote {
                    id: &id,
                    title: &id,
                    content: "summary-only body",
                    attachments: &[],
                    created_at: timestamp,
                    updated_at: timestamp,
                    note_revision: 1,
                    deleted_at: None,
                })
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();
    }

    fn put_request(
        note_id: &str,
        expected_revision: i64,
        attachment_id: &str,
        path: &str,
        content: Option<&str>,
        content_base64: Option<&str>,
    ) -> PutNoteAttachmentRequest {
        PutNoteAttachmentRequest {
            note_id: note_id.into(),
            expected_revision,
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

    fn assert_exact_open_object(schema: &serde_json::Value, expected_properties: &[&str]) {
        assert_eq!(schema["type"], "object", "{schema}");
        assert!(schema.get("additionalProperties").is_none(), "{schema}");
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
    async fn server_builds_and_lists_all_note_and_org_tools() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);
        let mut names = server
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 52);
        names.sort();
        for name in crate::org::NOTE_TOOL_NAMES {
            assert!(names.iter().any(|registered| registered == name));
        }
        assert_eq!(
            names.iter().filter(|name| name.starts_with("org_")).count(),
            40
        );
    }

    #[tokio::test]
    async fn server_keeps_the_exact_injected_context_arcs() {
        let (ctx, backend, _dir) = test_context().await;
        let note_ctx = Arc::new(ctx);
        let org_ctx = Arc::new(OrgContext::new(
            backend,
            Arc::new(note_pipelines::org::SystemOrgClock),
        ));
        let server = NoteMcpServer::new(note_ctx.clone(), org_ctx.clone());

        assert!(Arc::ptr_eq(&server.ctx, &note_ctx));
        assert!(Arc::ptr_eq(&server.org_ctx, &org_ctx));
        assert!(Arc::ptr_eq(server.org_context(), &org_ctx));
    }

    #[test]
    fn list_notes_request_adapter_clamps_max_limit() {
        let (limit, offset, label) = normalized_list_notes_request(ListNotesRequest {
            limit: Some(u32::MAX),
            offset: Some(7),
            label: Some("topic=rust".into()),
        });

        assert_eq!(limit, 1_000);
        assert_eq!(offset, 7);
        assert_eq!(label.as_deref(), Some("topic=rust"));
    }

    #[tokio::test]
    async fn list_notes_defaults_to_ten() {
        let (ctx, backend, _dir) = test_context().await;
        seed_summary_notes(backend.as_ref(), 12).await;
        let server = test_server(ctx, backend.clone());

        let response = server
            .list_notes(Parameters(ListNotesRequest {
                limit: None,
                offset: None,
                label: None,
            }))
            .await
            .unwrap()
            .0;

        assert_eq!(response.notes.len(), 10);
        assert_eq!(response.notes.first().unwrap().id, "mcp-list-0011");
        assert_eq!(response.notes.last().unwrap().id, "mcp-list-0002");
    }

    #[tokio::test]
    async fn list_notes_honors_zero_limit_and_explicit_offset() {
        let (ctx, backend, _dir) = test_context().await;
        seed_summary_notes(backend.as_ref(), 4).await;
        let server = test_server(ctx, backend.clone());

        let empty = server
            .list_notes(Parameters(ListNotesRequest {
                limit: Some(0),
                offset: None,
                label: None,
            }))
            .await
            .unwrap()
            .0;
        assert!(empty.notes.is_empty());

        let page = server
            .list_notes(Parameters(ListNotesRequest {
                limit: Some(2),
                offset: Some(1),
                label: None,
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            page.notes
                .iter()
                .map(|note| note.id.as_str())
                .collect::<Vec<_>>(),
            ["mcp-list-0002", "mcp-list-0001"]
        );
    }

    #[tokio::test]
    async fn advertised_schemas_are_bounded_and_keep_nullable_filters() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);

        let save_input = tool_schema(&server, "save_note", false);
        assert_exact_closed_object(&save_input, &save_input, &["content", "labels", "title"]);
        let update_input = tool_schema(&server, "update_note", false);
        assert_exact_closed_object(
            &update_input,
            &update_input,
            &["content", "expected_revision", "id", "labels", "title"],
        );

        let bulk_input = tool_schema(&server, "bulk_update_note_labels", false);
        assert_exact_closed_object(&bulk_input, &bulk_input, &["remove", "selector", "set"]);
        assert_eq!(required_names(&bulk_input), vec!["selector"]);
        assert_eq!(bulk_input["properties"]["selector"]["type"], "string");
        let assignments = &bulk_input["properties"]["set"];
        assert_eq!(assignments["type"], "array");
        assert_eq!(assignments["items"]["type"], "array");
        assert_eq!(assignments["items"]["minItems"], 2);
        assert_eq!(assignments["items"]["maxItems"], 2);
        assert_eq!(
            assignments["items"]["prefixItems"],
            json!([{"type": "string"}, {"type": "string"}])
        );
        let removals = &bulk_input["properties"]["remove"];
        assert_eq!(removals["type"], "array");
        assert_eq!(removals["items"]["type"], "string");

        let bulk_output = tool_schema(&server, "bulk_update_note_labels", true);
        assert_exact_closed_object(
            &bulk_output,
            &bulk_output,
            &["matched", "unchanged", "updated"],
        );
        assert_eq!(
            required_names(&bulk_output),
            vec!["matched", "unchanged", "updated"]
        );
        for field in ["matched", "updated", "unchanged"] {
            assert_eq!(bulk_output["properties"][field]["type"], "integer");
        }

        let put_schema = tool_schema(&server, "put_note_attachment", false);
        assert_exact_closed_object(
            &put_schema,
            &put_schema,
            &[
                "attachment_id",
                "content",
                "content_base64",
                "description",
                "expected_revision",
                "mime",
                "note_id",
                "path",
            ],
        );
        assert_attachment_content_requirement(&put_schema);

        let attachment_get_input = tool_schema(&server, "get_note_attachment_content", false);
        assert_exact_closed_object(
            &attachment_get_input,
            &attachment_get_input,
            &["attachment_id", "note_id"],
        );
        let attachment_delete_input = tool_schema(&server, "delete_note_attachment", false);
        assert_exact_closed_object(
            &attachment_delete_input,
            &attachment_delete_input,
            &["attachment_id", "expected_revision", "note_id"],
        );

        let list_input = tool_schema(&server, "list_notes", false);
        assert_exact_open_object(&list_input, &["label", "limit", "offset"]);
        assert!(schema_allows_null(&list_input["properties"]["label"]));
        assert!(required_names(&list_input).is_empty());
        let search_input = tool_schema(&server, "semantic_search", false);
        assert_exact_open_object(&search_input, &["label", "limit", "query"]);
        assert!(schema_allows_null(&search_input["properties"]["label"]));
        assert_eq!(
            list_input["properties"]["label"]["type"],
            search_input["properties"]["label"]["type"]
        );
        for label in [
            &list_input["properties"]["label"],
            &search_input["properties"]["label"],
        ] {
            let description = label["description"].as_str().unwrap();
            for operator in ["==", "^=", "$=", "~="] {
                assert!(description.contains(operator), "{description}");
            }
            assert!(description.contains("case-insensitive"), "{description}");
            assert!(description.contains("percent-encoded"), "{description}");
        }
        assert_eq!(required_names(&search_input), vec!["limit", "query"]);

        let list_output = tool_schema(&server, "list_notes", true);
        assert_exact_closed_object(&list_output, &list_output, &["notes"]);
        let summary = resolve_schema(&list_output, &list_output["properties"]["notes"]["items"]);
        assert_exact_closed_object(
            &list_output,
            summary,
            &[
                "created_at",
                "id",
                "labels",
                "revision",
                "title",
                "updated_at",
            ],
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
                "revision",
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
            &[
                "created_at",
                "id",
                "labels",
                "revision",
                "score",
                "title",
                "updated_at",
            ],
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
            property_names(&serde_json::to_value(schemars::schema_for!(SaveNoteResponse)).unwrap()),
            vec!["id", "revision"]
        );
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
            vec![
                "created_at",
                "id",
                "labels",
                "revision",
                "title",
                "updated_at"
            ]
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
                "revision",
                "title",
                "updated_at",
            ]
        );
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(SemanticSearchHit)).unwrap()
            ),
            vec![
                "created_at",
                "id",
                "labels",
                "revision",
                "score",
                "title",
                "updated_at"
            ]
        );
        assert_eq!(
            property_names(
                &serde_json::to_value(schemars::schema_for!(DeleteNoteAttachmentResponse)).unwrap()
            ),
            vec!["deleted", "revision"]
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
    fn bulk_update_note_labels_request_rejects_unknown_fields() {
        assert!(
            serde_json::from_value::<BulkUpdateNoteLabelsRequest>(json!({
                "selector": "type=ietf-rfc",
                "set": [["project", "IETF-RFC"]],
                "unknown": true
            }))
            .is_err()
        );
    }

    #[test]
    fn bulk_update_note_labels_request_defaults_omitted_mutations() {
        let request = serde_json::from_value::<BulkUpdateNoteLabelsRequest>(json!({
            "selector": "type=ietf-rfc"
        }))
        .unwrap();

        assert!(request.set.is_empty());
        assert!(request.remove.is_empty());
    }

    #[tokio::test]
    async fn bulk_update_note_labels_handler_adds_replaces_and_reports_noops() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);
        for (title, project) in [
            ("Add", None),
            ("Replace", Some("old")),
            ("Noop", Some("new")),
        ] {
            let mut labels = vec![
                ("type".into(), "ietf-rfc".into()),
                ("owner".into(), "protocols".into()),
            ];
            if let Some(project) = project {
                labels.push(("project".into(), project.into()));
            }
            server
                .save_note(Parameters(SaveNoteRequest {
                    title: title.into(),
                    content: "Body".into(),
                    labels,
                }))
                .await
                .unwrap();
        }

        let response = server
            .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                selector: "type=ietf-rfc".into(),
                set: vec![("project".into(), "new".into())],
                remove: vec![],
            }))
            .await
            .unwrap()
            .0;

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"matched": 3, "updated": 2, "unchanged": 1})
        );
    }

    #[tokio::test]
    async fn bulk_update_note_labels_handler_removes_labels_and_reports_absent_removals_as_noops() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);
        server
            .save_note(Parameters(SaveNoteRequest {
                title: "Remove owner".into(),
                content: "Body".into(),
                labels: vec![
                    ("type".into(), "ietf-rfc".into()),
                    ("owner".into(), "protocols".into()),
                ],
            }))
            .await
            .unwrap();

        let removed = server
            .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                selector: "type=ietf-rfc".into(),
                set: vec![],
                remove: vec!["owner".into()],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(removed).unwrap(),
            json!({"matched": 1, "updated": 1, "unchanged": 0})
        );

        let absent = server
            .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                selector: "type=ietf-rfc".into(),
                set: vec![],
                remove: vec!["owner".into(), "never-known".into()],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::to_value(absent).unwrap(),
            json!({"matched": 1, "updated": 0, "unchanged": 1})
        );
    }

    #[tokio::test]
    async fn bulk_update_note_labels_handler_returns_zero_counts_for_no_matches() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);

        let response = server
            .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                selector: "type=not-present".into(),
                set: vec![("project".into(), "new".into())],
                remove: vec![],
            }))
            .await
            .unwrap()
            .0;

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"matched": 0, "updated": 0, "unchanged": 0})
        );
    }

    #[tokio::test]
    async fn bulk_update_note_labels_maps_caller_faults_to_invalid_params() {
        let (ctx, backend, _dir) = test_context().await;
        let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
        transaction
            .insert_label_key_with_type("priority", "", note_core::LabelValueType::Number)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let server = test_server(ctx, backend);
        server
            .save_note(Parameters(SaveNoteRequest {
                title: "Typed".into(),
                content: "Body".into(),
                labels: vec![("type".into(), "ietf-rfc".into())],
            }))
            .await
            .unwrap();

        for (request, expected_message) in [
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "   ".into(),
                    set: vec![("project".into(), "new".into())],
                    remove: vec![],
                },
                "selector must not be empty",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc&&owner=protocols".into(),
                    set: vec![("project".into(), "new".into())],
                    remove: vec![],
                },
                "label selector is malformed",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![],
                    remove: vec![],
                },
                "at least one label set or remove mutation is required",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![
                        ("project".into(), "new".into()),
                        ("project".into(), "again".into()),
                    ],
                    remove: vec![],
                },
                "duplicate label assignment key: project",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![],
                    remove: vec!["owner".into(), "owner".into()],
                },
                "duplicate label removal key: owner",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![("owner".into(), "new".into())],
                    remove: vec!["owner".into()],
                },
                "label key cannot be both set and removed: owner",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![("bad$key".into(), "new".into())],
                    remove: vec![],
                },
                "label key must not contain selector-reserved character: $",
            ),
            (
                BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![("priority".into(), "urgent".into())],
                    remove: vec![],
                },
                "invalid value for label priority: urgent is not number",
            ),
        ] {
            let error = expect_error(server.bulk_update_note_labels(Parameters(request)).await);
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(error.message, expected_message);
        }
    }

    #[tokio::test]
    async fn note_mutation_schemas_require_expected_revision() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);

        for tool_name in ["update_note", "edit_note"] {
            let schema = tool_schema(&server, tool_name, false);
            assert_eq!(
                schema["properties"]["expected_revision"]["type"], "integer",
                "{tool_name}: {schema}"
            );
            assert!(
                required_names(&schema).contains(&"expected_revision".to_owned()),
                "{tool_name}: {schema}"
            );
        }
    }

    #[tokio::test]
    async fn bulk_update_note_labels_sanitizes_repository_failures() {
        let (_ctx, backend, dir) = test_context().await;
        let failing_backend: Arc<dyn StorageBackend> =
            Arc::new(FailingBeginStorageBackend { inner: backend });
        let failing_ctx = Context::new(
            failing_backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("failing-attachments"),
            )),
        );
        let server = test_server(failing_ctx, failing_backend);

        let error = expect_error(
            server
                .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![("project".into(), "new".into())],
                    remove: vec![],
                }))
                .await,
        );

        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, "bulk note label update failed");
        assert!(!error.message.contains("private repository failure detail"));
    }

    #[tokio::test]
    async fn bulk_update_note_labels_sanitizes_typed_validation_with_rollback_failure() {
        let (ctx, backend, dir) = test_context().await;
        let transaction = backend.begin(TransactionMode::Immediate).await.unwrap();
        transaction
            .insert_label_key_with_type("priority", "", note_core::LabelValueType::Number)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let base_server = test_server(ctx, backend.clone());
        base_server
            .save_note(Parameters(SaveNoteRequest {
                title: "Typed rollback".into(),
                content: "Body".into(),
                labels: vec![("type".into(), "ietf-rfc".into())],
            }))
            .await
            .unwrap();

        let failing_backend: Arc<dyn StorageBackend> =
            Arc::new(RollbackFailingStorageBackend { inner: backend });
        let failing_ctx = Context::new(
            failing_backend.clone(),
            Arc::new(StubEmbedder),
            Arc::new(FilesystemAttachmentStore::new(
                dir.path().join("rollback-failing-attachments"),
            )),
        );
        let server = test_server(failing_ctx, failing_backend);

        let error = expect_error(
            server
                .bulk_update_note_labels(Parameters(BulkUpdateNoteLabelsRequest {
                    selector: "type=ietf-rfc".into(),
                    set: vec![("priority".into(), "urgent".into())],
                    remove: vec![],
                }))
                .await,
        );

        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, "bulk note label update failed");
        assert!(!error.message.contains("ROLLBACK-SECRET-42"));
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

    #[test]
    fn label_key_validation_faults_are_invalid_params() {
        let error = anyhow::Error::new(note_core::LabelKeyValidationError::ReservedCharacter('$'));
        assert_eq!(to_error_data(error).code, ErrorCode::INVALID_PARAMS);
        let error = anyhow::Error::new(note_core::LabelSelectorParseError::MalformedExactSelector);
        assert_eq!(to_error_data(error).code, ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn malformed_exact_selectors_are_invalid_params_for_read_tools() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);

        let list_error = expect_error(
            server
                .list_notes(Parameters(ListNotesRequest {
                    limit: None,
                    offset: None,
                    label: Some("status=ready&~==secret".into()),
                }))
                .await,
        );
        assert_eq!(list_error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(list_error.message, "malformed exact label selector");

        for (query, limit) in [("anything", 10), ("", 10), ("anything", 0)] {
            let search_error = expect_error(
                server
                    .semantic_search(Parameters(SemanticSearchRequest {
                        query: query.into(),
                        limit,
                        label: Some("~project==%ZZ".into()),
                    }))
                    .await,
            );
            assert_eq!(search_error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(search_error.message, "malformed exact label selector");
        }
    }

    #[tokio::test]
    async fn semantic_search_uses_saved_minimum_score() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend.clone());
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Threshold marker".into(),
                content: "unrelated body".into(),
                labels: vec![],
            }))
            .await
            .unwrap()
            .0;
        note_pipelines::drain_embedding_jobs(&server.ctx, 10)
            .await
            .unwrap();

        let baseline = server
            .semantic_search(Parameters(SemanticSearchRequest {
                query: "Threshold marker".into(),
                limit: 5,
                label: None,
            }))
            .await
            .unwrap()
            .0;
        let score = baseline
            .results
            .iter()
            .find(|result| result.id == saved.id)
            .unwrap()
            .score;
        {
            let session = backend.session().await.unwrap();
            let mut config = session.get_system_config().await.unwrap();
            config.search.minimum_score = f32::from_bits(score.to_bits() + 1);
            session.set_system_config(&config).await.unwrap();
        }

        let response = server
            .semantic_search(Parameters(SemanticSearchRequest {
                query: "Threshold marker".into(),
                limit: 5,
                label: None,
            }))
            .await
            .unwrap()
            .0;
        assert!(response.results.is_empty());
    }

    #[test]
    fn note_conflicts_preserve_the_shared_structured_error_fields() {
        let mapped = to_error_data(anyhow::Error::new(
            note_pipelines::NoteMutationError::StaleRevision {
                note_id: "note-1".into(),
                expected_revision: 5,
                current_revision: 6,
            },
        ));
        assert_eq!(mapped.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(
            mapped.data,
            Some(json!({
                "code": "stale_revision",
                "message": "the note changed after it was read",
                "details": {
                    "note_id": "note-1",
                    "expected_revision": 5,
                    "current_revision": 6
                },
                "retryable": false
            }))
        );
    }

    #[tokio::test]
    async fn edit_note_uses_the_read_revision_and_returns_structured_stale_conflicts() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Editable".into(),
                content: "first".into(),
                labels: vec![],
            }))
            .await
            .unwrap()
            .0;
        let read = server
            .read_note_lines(Parameters(ReadNoteLinesRequest {
                id: saved.id.clone(),
            }))
            .await
            .unwrap()
            .0;

        let edited = server
            .edit_note(Parameters(EditNoteRequest {
                id: saved.id.clone(),
                expected_revision: read.revision,
                tag: read.tag.clone(),
                edits: vec![EditOpSchema::InsertTail {
                    lines: vec!["second".into()],
                }],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(edited.revision, read.revision + 1);
        assert_ne!(edited.tag, read.tag);

        let stale = expect_error(
            server
                .edit_note(Parameters(EditNoteRequest {
                    id: saved.id.clone(),
                    expected_revision: read.revision,
                    tag: edited.tag.clone(),
                    edits: vec![EditOpSchema::InsertTail {
                        lines: vec!["stale".into()],
                    }],
                }))
                .await,
        );
        assert_eq!(stale.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(
            stale.data,
            Some(json!({
                "code": "stale_revision",
                "message": "the note changed after it was read",
                "details": {
                    "note_id": saved.id,
                    "expected_revision": read.revision,
                    "current_revision": edited.revision
                },
                "retryable": false
            }))
        );
    }

    #[tokio::test]
    async fn standalone_handlers_use_metadata_and_exclusive_content_shapes() {
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend.clone());
        let saved = server
            .save_note(Parameters(SaveNoteRequest {
                title: "Standalone".into(),
                content: "searchable body".into(),
                labels: vec![("topic".into(), "rust".into())],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(saved.revision, 1);
        let text = server
            .put_note_attachment(Parameters(put_request(
                &saved.id,
                1,
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
                "revision": 2,
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
                2,
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
                "revision",
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

        let transaction = backend
            .begin(note_storage::TransactionMode::Deferred)
            .await
            .unwrap();
        let note = transaction.get_note(&saved.id).await.unwrap().unwrap();
        let blob_key = note
            .attachments
            .iter()
            .find(|a| a.id == "blob")
            .unwrap()
            .storage
            .as_ref()
            .unwrap()
            .object_key
            .clone();
        server
            .ctx
            .attachments()
            .delete_object(&blob_key)
            .await
            .unwrap();
        let updated = server
            .update_note(Parameters(UpdateNoteRequest {
                id: saved.id.clone(),
                expected_revision: 3,
                title: "Standalone updated".into(),
                content: "updated searchable body".into(),
                labels: vec![("topic".into(), "rust".into())],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(updated.attachments.len(), 2);

        let stale = expect_error(
            server
                .update_note(Parameters(UpdateNoteRequest {
                    id: saved.id.clone(),
                    expected_revision: 3,
                    title: "Stale".into(),
                    content: "stale body".into(),
                    labels: vec![],
                }))
                .await,
        );
        assert_eq!(stale.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(
            stale.data,
            Some(json!({
                "code": "stale_revision",
                "message": "the note changed after it was read",
                "details": {
                    "note_id": saved.id.clone(),
                    "expected_revision": 3,
                    "current_revision": 4
                },
                "retryable": false
            }))
        );

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
            [
                "created_at",
                "id",
                "labels",
                "revision",
                "title",
                "updated_at"
            ]
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
            [
                "created_at",
                "id",
                "labels",
                "revision",
                "score",
                "title",
                "updated_at",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );

        for expected in [true, false] {
            let deleted = server
                .delete_note_attachment(Parameters(DeleteNoteAttachmentRequest {
                    note_id: saved.id.clone(),
                    attachment_id: "text".into(),
                    expected_revision: if expected { 4 } else { 5 },
                }))
                .await
                .unwrap()
                .0;
            assert_eq!(
                serde_json::to_value(deleted).unwrap(),
                json!({
                    "deleted": expected,
                    "revision": if expected { Some(5) } else { None },
                })
            );
        }
    }

    #[tokio::test]
    async fn invalid_attachment_inputs_are_invalid_params_without_writes() {
        let (ctx, backend, dir) = test_context().await;
        let server = test_server(ctx, backend);
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
                put_request(
                    &saved.id,
                    1,
                    "blob",
                    "./blob.bin",
                    None,
                    Some("not base64 !!"),
                ),
                "invalid attachment content_base64",
            ),
            (
                put_request(&saved.id, 1, "blob", "./blob.bin", None, None),
                "attachment content or content_base64 is required",
            ),
            (
                put_request(
                    &saved.id,
                    1,
                    "blob",
                    "./blob.bin",
                    Some("abc"),
                    Some("eHl6"),
                ),
                "attachment content and content_base64 do not match",
            ),
            (
                put_request(&saved.id, 1, "blob", "../blob.bin", Some("abc"), None),
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
        let (ctx, backend, _dir) = test_context().await;
        let server = test_server(ctx, backend);
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
                1,
                "first",
                "./first.txt",
                Some("first"),
                None,
            )))
            .await
            .unwrap();

        for request in [
            put_request(&saved.id, 2, "first", "./moved.txt", Some("moved"), None),
            put_request(
                &saved.id,
                2,
                "second",
                "./first.txt",
                Some("collision"),
                None,
            ),
        ] {
            let error = expect_error(server.put_note_attachment(Parameters(request)).await);
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        }

        let missing_put = expect_error(
            server
                .put_note_attachment(Parameters(put_request(
                    "missing-note",
                    1,
                    "blob",
                    "./blob.bin",
                    Some("bytes"),
                    None,
                )))
                .await,
        );
        assert_eq!(missing_put.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(missing_put.message, "note not found");
        assert_eq!(
            missing_put.data,
            Some(json!({
                "code": "not_found",
                "message": "note not found",
                "details": {"note_id": "missing-note"},
                "retryable": false
            }))
        );

        let missing_delete = expect_error(
            server
                .delete_note_attachment(Parameters(DeleteNoteAttachmentRequest {
                    note_id: "missing-note".into(),
                    attachment_id: "blob".into(),
                    expected_revision: 1,
                }))
                .await,
        );
        assert_eq!(missing_delete.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(missing_delete.message, "note not found");
        assert_eq!(
            missing_delete.data,
            Some(json!({
                "code": "not_found",
                "message": "note not found",
                "details": {"note_id": "missing-note"},
                "retryable": false
            }))
        );

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
